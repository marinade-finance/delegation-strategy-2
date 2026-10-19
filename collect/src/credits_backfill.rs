use crate::common::CommonParams;
use crate::solana_service::*;
use clap::Parser;
use log::info;
use serde::{Deserialize, Serialize};
use solana_rpc_client::rpc_client::RpcClient;
use solana_rpc_client_api::config::RpcGetVoteAccountsConfig;
use solana_rpc_client_api::response::{RpcVoteAccountInfo, RpcVoteAccountStatus};
use solana_sdk::clock::Epoch;
use std::collections::{BTreeMap, HashMap};
use std::ops::RangeInclusive;
use std::time::Duration;

#[derive(Debug, Parser)]
pub struct CreditsBackfillParams {
    #[arg(long = "from-epoch")]
    from_epoch: Epoch,

    #[arg(long = "to-epoch")]
    to_epoch: Epoch,

    #[arg(
        long = "rpc-timeout",
        help = "How long to wait for RPC response (seconds).",
        default_value = "300"
    )]
    rpc_timeout: u64,
}

// agave MAX_EPOCH_CREDITS_HISTORY, the migration marker takes one of the entries
const MAX_EPOCH_CREDITS_HISTORY: usize = 64;

#[derive(Debug, Serialize, Deserialize)]
pub struct CreditsBackfill {
    pub epochs: BTreeMap<Epoch, HashMap<String, EpochCredits>>,
}

/// Each vote account with a history gets an entry in each epoch, (None, None) where it has no value.
pub fn credits_backfill(
    vote_accounts: &RpcVoteAccountStatus,
    histories: &HashMap<String, Vec<EpochCreditsEntry>>,
    epochs: RangeInclusive<Epoch>,
    alpenglow_migration_epoch: Option<Epoch>,
) -> CreditsBackfill {
    let mut backfill = CreditsBackfill {
        epochs: epochs
            .clone()
            .map(|epoch| (epoch, HashMap::new()))
            .collect(),
    };
    for vote_account in vote_accounts
        .current
        .iter()
        .chain(vote_accounts.delinquent.iter())
    {
        let Some(history) = histories.get(&vote_account.vote_pubkey) else {
            continue;
        };
        // split_epoch_credits gives a 0 reward for an epoch with no entry only when activated_stake > 0.
        // The current stake is not the stake of a past epoch, so 1 always gives the 0, and store clears it for rows with no stake.
        let vote_account = RpcVoteAccountInfo {
            epoch_credits: history.clone(),
            activated_stake: 1,
            ..vote_account.clone()
        };
        for epoch in epochs.clone() {
            let credits = split_epoch_credits(&vote_account, epoch, alpenglow_migration_epoch)
                .unwrap_or(EpochCredits {
                    tower_credits: None,
                    vote_reward_lamports: None,
                });
            backfill
                .epochs
                .get_mut(&epoch)
                .unwrap()
                .insert(vote_account.vote_pubkey.clone(), credits);
        }
    }
    backfill
}

/// Vote accounts whose history is full and starts after `from_epoch`, so their entries of `from_epoch` are gone.
pub fn truncated_histories(
    histories: &HashMap<String, Vec<EpochCreditsEntry>>,
    from_epoch: Epoch,
) -> Vec<&String> {
    histories
        .iter()
        .filter(|(_, history)| {
            history.len() >= MAX_EPOCH_CREDITS_HISTORY
                && history
                    .first()
                    .is_some_and(|(first_epoch, _, _)| *first_epoch > from_epoch)
        })
        .map(|(vote_pubkey, _)| vote_pubkey)
        .collect()
}

fn get_all_vote_accounts(client: &RpcClient) -> anyhow::Result<RpcVoteAccountStatus> {
    Ok(
        client.get_vote_accounts_with_config(RpcGetVoteAccountsConfig {
            keep_unstaked_delinquents: Some(true),
            ..Default::default()
        })?,
    )
}

pub fn collect_credits_backfill(
    common_params: CommonParams,
    params: CreditsBackfillParams,
) -> anyhow::Result<()> {
    let (from_epoch, to_epoch) = (params.from_epoch, params.to_epoch);
    info!("Collecting the credits backfill of epochs {from_epoch}..={to_epoch}");
    anyhow::ensure!(
        from_epoch <= to_epoch,
        "--from-epoch {from_epoch} is after --to-epoch {to_epoch}"
    );
    let client = solana_client_with_timeout(
        common_params.rpc_url,
        Duration::from_secs(params.rpc_timeout),
        common_params.commitment,
    );
    let current_epoch = client.get_epoch_info()?.epoch;
    anyhow::ensure!(
        to_epoch < current_epoch,
        "--to-epoch {to_epoch} is not closed, the current epoch is {current_epoch}"
    );
    let oldest_epoch = current_epoch.saturating_sub(MAX_EPOCH_CREDITS_HISTORY as Epoch - 1);
    anyhow::ensure!(
        from_epoch >= oldest_epoch,
        "--from-epoch {from_epoch} is older than {oldest_epoch}, the oldest epoch that epochCredits can still hold"
    );
    let vote_accounts = get_all_vote_accounts(&client)?;
    let vote_pubkeys: Vec<String> = vote_accounts
        .current
        .iter()
        .chain(vote_accounts.delinquent.iter())
        .map(|vote_account| vote_account.vote_pubkey.clone())
        .collect();
    let histories = get_full_epoch_credits(&client, &vote_pubkeys)?;
    info!(
        "Read the epochCredits of {} of {} vote accounts",
        histories.len(),
        vote_pubkeys.len()
    );
    let truncated = truncated_histories(&histories, from_epoch);
    anyhow::ensure!(
        truncated.is_empty(),
        "{} vote accounts no longer hold the epochCredits of epoch {from_epoch}, for example {}",
        truncated.len(),
        truncated
            .first()
            .map_or("", |vote_pubkey| vote_pubkey.as_str())
    );
    let backfill = credits_backfill(
        &vote_accounts,
        &histories,
        from_epoch..=to_epoch,
        is_alpenglow_active(&client)?,
    );
    for (epoch, credits) in &backfill.epochs {
        let tower = credits
            .values()
            .filter(|c| c.tower_credits.is_some())
            .count();
        let rewards = credits
            .values()
            .filter(|c| c.vote_reward_lamports.is_some())
            .count();
        info!(
            "Epoch {epoch}: {} vote accounts, {tower} with tower credits, {rewards} with vote reward lamports",
            credits.len()
        );
    }
    serde_yaml::to_writer(std::io::stdout(), &backfill)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn history(first_epoch: Epoch, len: usize) -> Vec<EpochCreditsEntry> {
        (0..len as Epoch)
            .map(|i| (first_epoch + i, 100 * (i + 1), 100 * i))
            .collect()
    }

    #[test]
    fn a_full_history_that_starts_after_from_epoch_is_truncated() {
        let histories = HashMap::from([
            (
                "full_after".to_string(),
                history(1043, MAX_EPOCH_CREDITS_HISTORY),
            ),
            (
                "full_before".to_string(),
                history(1003, MAX_EPOCH_CREDITS_HISTORY),
            ),
            // A vote account created after from_epoch has no older entries to lose
            ("young".to_string(), history(1050, 10)),
        ]);
        assert_eq!(truncated_histories(&histories, 1042), vec!["full_after"]);
    }
}
