use anyhow::{bail, Context};
use google_cloud_bigquery::client::{Client as BqClient, ClientConfig as BqClientConfig};
use google_cloud_bigquery::http::job::query::QueryRequest;
use google_cloud_bigquery::query::row::Row;
use log::{info, warn};
use solana_commitment_config::CommitmentConfig;
use solana_rpc_client::rpc_client::RpcClient;
use solana_rpc_client_api::response::RpcVoteAccountStatus;
use solana_sdk::{
    account::from_account,
    clock::{Epoch, Slot},
    slot_history::{self, SlotHistory},
    sysvar,
};
use std::collections::{HashMap, HashSet};

const GOOGLE_BQ_PROJECT_ID: &str = "data-store-406413";
const GOOGLE_BQ_DATASET: &str = "mainnet_beta_stakes";

/// Vote account -> absolute slots it leads in the epoch.
pub type VoteLeaderSlots = HashMap<String, Vec<Slot>>;

async fn create_bigquery_client() -> anyhow::Result<BqClient> {
    let (config, _) = BqClientConfig::new_with_auth().await?;
    Ok(BqClient::new(config).await?)
}

/// `None` while the ETL has not loaded the epoch yet.
async fn query_vote_leader_slots(
    bq_client: &BqClient,
    epoch: Epoch,
    slots_in_epoch: u64,
) -> anyhow::Result<Option<VoteLeaderSlots>> {
    let query = format!(
        "SELECT vote_pubkey, CAST(slot AS STRING) AS slot \
         FROM `{GOOGLE_BQ_PROJECT_ID}.{GOOGLE_BQ_DATASET}.leader_schedule` \
         WHERE epoch = {epoch}"
    );
    let request = QueryRequest {
        query,
        use_legacy_sql: false,
        ..Default::default()
    };
    let mut iter = bq_client
        .query::<Row>(GOOGLE_BQ_PROJECT_ID, request)
        .await
        .context("Failed to query the leader schedule")?;

    let mut schedule = VoteLeaderSlots::new();
    let mut row_count = 0u64;
    let mut seen_slots = HashSet::new();
    while let Some(row) = iter.next().await? {
        row_count += 1;
        let vote_pubkey = row
            .column::<String>(0)
            .context("Failed to read vote_pubkey")?;
        let raw_slot = row.column::<String>(1).context("Failed to read slot")?;
        let slot: Slot = raw_slot
            .parse()
            .context(format!("Failed to parse slot '{raw_slot}'"))?;
        if !seen_slots.insert(slot) {
            bail!("Leader schedule of epoch {epoch} has slot {slot} more than once");
        }
        schedule.entry(vote_pubkey).or_default().push(slot);
    }

    match row_count {
        0 => Ok(None),
        n if n == slots_in_epoch => Ok(Some(schedule)),
        n => bail!(
            "Leader schedule of epoch {epoch} has {n} slots in BigQuery, expected {slots_in_epoch}"
        ),
    }
}

/// Correct only for vote accounts that kept their identity since the schedule was built. An
/// identity that more than one vote account shares is left out, so those vote accounts stay unresolved.
pub fn vote_keyed_from_identity_schedule(
    identity_schedule: HashMap<String, Vec<usize>>,
    first_slot_in_epoch: Slot,
    vote_accounts: &RpcVoteAccountStatus,
) -> VoteLeaderSlots {
    let mut votes_by_identity: HashMap<&str, Vec<&str>> = HashMap::new();
    for vote_account in vote_accounts
        .current
        .iter()
        .chain(vote_accounts.delinquent.iter())
    {
        votes_by_identity
            .entry(vote_account.node_pubkey.as_str())
            .or_default()
            .push(vote_account.vote_pubkey.as_str());
    }

    identity_schedule
        .into_iter()
        .filter_map(|(identity, relative_slots)| {
            match votes_by_identity.get(identity.as_str()).map(Vec::as_slice) {
                Some([vote_pubkey]) => Some((
                    vote_pubkey.to_string(),
                    relative_slots
                        .into_iter()
                        .map(|relative_slot| first_slot_in_epoch + relative_slot as Slot)
                        .collect(),
                )),
                _ => None,
            }
        })
        .collect()
}

/// Vote account -> (leader slots, blocks produced), counting only slots up to `last_slot`.
pub fn count_block_production(
    schedule: &VoteLeaderSlots,
    last_slot: Slot,
    has_block: impl Fn(Slot) -> bool,
) -> HashMap<String, (usize, usize)> {
    schedule
        .iter()
        .map(|(vote_pubkey, slots)| {
            let (leader_slots, blocks_produced) = slots
                .iter()
                .filter(|slot| **slot <= last_slot)
                .fold((0, 0), |(leader_slots, blocks_produced), slot| {
                    (
                        leader_slots + 1,
                        blocks_produced + has_block(*slot) as usize,
                    )
                });
            (vote_pubkey.clone(), (leader_slots, blocks_produced))
        })
        .collect()
}

/// Vote account -> (leader slots, blocks produced). A vote account missing from the map has no
/// resolved slots. For the open epoch only the slots up to the newest finalized slot count.
pub fn get_block_production_by_vote_account(
    rpc_client: &RpcClient,
    epoch: Epoch,
    current_epoch: Epoch,
    vote_accounts: &RpcVoteAccountStatus,
) -> anyhow::Result<HashMap<String, (usize, usize)>> {
    info!("Getting block production by vote account for epoch {epoch}");
    let epoch_schedule = rpc_client.get_epoch_schedule()?;
    let first_slot_in_epoch = epoch_schedule.get_first_slot_in_epoch(epoch);
    let last_slot_in_epoch = epoch_schedule.get_last_slot_in_epoch(epoch);
    let slots_in_epoch = epoch_schedule.get_slots_in_epoch(epoch);

    let runtime = tokio::runtime::Runtime::new().context("Failed to create tokio runtime")?;
    let bq_schedule = runtime.block_on(async {
        let bq_client = create_bigquery_client().await?;
        query_vote_leader_slots(&bq_client, epoch, slots_in_epoch).await
    })?;

    let schedule = match bq_schedule {
        Some(schedule) => schedule,
        None if epoch < current_epoch => {
            bail!("Leader schedule of closed epoch {epoch} is not in BigQuery yet")
        }
        None => {
            warn!("Leader schedule of epoch {epoch} is not in BigQuery yet, mapping the RPC schedule by current identity");
            let identity_schedule = rpc_client
                .get_leader_schedule_with_commitment(
                    Some(first_slot_in_epoch),
                    CommitmentConfig::finalized(),
                )?
                .context(format!("RPC has no leader schedule for epoch {epoch}"))?;
            vote_keyed_from_identity_schedule(identity_schedule, first_slot_in_epoch, vote_accounts)
        }
    };

    let slot_history_account = rpc_client
        .get_account_with_commitment(&sysvar::slot_history::id(), CommitmentConfig::finalized())?
        .value
        .context("SlotHistory sysvar not found")?;
    let slot_history: SlotHistory = from_account(&slot_history_account)
        .context("Failed to deserialize the SlotHistory sysvar")?;
    if first_slot_in_epoch < slot_history.oldest() {
        bail!(
            "Epoch {epoch} starts at slot {first_slot_in_epoch}, before the oldest SlotHistory slot {}",
            slot_history.oldest()
        );
    }
    let last_slot = last_slot_in_epoch.min(slot_history.newest());
    info!("Counting blocks in slot range [{first_slot_in_epoch},{last_slot}]");

    Ok(count_block_production(&schedule, last_slot, |slot| {
        slot_history.check(slot) == slot_history::Check::Found
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use solana_rpc_client_api::response::RpcVoteAccountInfo;

    fn vote_account(vote_pubkey: &str, node_pubkey: &str) -> RpcVoteAccountInfo {
        RpcVoteAccountInfo {
            vote_pubkey: vote_pubkey.into(),
            node_pubkey: node_pubkey.into(),
            activated_stake: 1,
            commission: 0,
            epoch_vote_account: true,
            epoch_credits: vec![],
            last_vote: 0,
            root_slot: 0,
        }
    }

    fn vote_accounts(accounts: Vec<RpcVoteAccountInfo>) -> RpcVoteAccountStatus {
        RpcVoteAccountStatus {
            current: accounts,
            delinquent: vec![],
        }
    }

    #[test]
    fn slots_follow_the_vote_account_across_an_identity_switch() {
        let schedule = VoteLeaderSlots::from([("vote".into(), vec![100, 101, 102, 103])]);
        let production = count_block_production(&schedule, 1_000, |slot| slot != 101);
        assert_eq!(production.get("vote"), Some(&(4, 3)));
    }

    #[test]
    fn the_open_epoch_counts_only_slots_up_to_the_last_one() {
        let schedule = VoteLeaderSlots::from([("vote".into(), vec![100, 101, 200, 201])]);
        let production = count_block_production(&schedule, 150, |_| true);
        assert_eq!(production.get("vote"), Some(&(2, 2)));
    }

    #[test]
    fn a_vote_account_whose_identity_switched_stays_unresolved_in_the_fallback() {
        let identity_schedule = HashMap::from([("old".to_string(), vec![0, 1, 2, 3])]);
        let schedule = vote_keyed_from_identity_schedule(
            identity_schedule,
            1_000,
            &vote_accounts(vec![vote_account("vote", "new")]),
        );
        assert!(schedule.is_empty());
    }

    #[test]
    fn the_fallback_maps_an_unchanged_identity_to_absolute_slots() {
        let identity_schedule = HashMap::from([("node".to_string(), vec![0, 1])]);
        let schedule = vote_keyed_from_identity_schedule(
            identity_schedule,
            1_000,
            &vote_accounts(vec![vote_account("vote", "node")]),
        );
        assert_eq!(schedule.get("vote"), Some(&vec![1_000, 1_001]));
    }

    #[test]
    fn the_fallback_leaves_out_an_identity_shared_by_two_vote_accounts() {
        let identity_schedule = HashMap::from([("node".to_string(), vec![0, 1])]);
        let schedule = vote_keyed_from_identity_schedule(
            identity_schedule,
            1_000,
            &vote_accounts(vec![vote_account("a", "node"), vote_account("b", "node")]),
        );
        assert!(schedule.is_empty());
    }
}
