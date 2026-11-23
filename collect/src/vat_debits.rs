use crate::common::{with_bigquery_timeout, CommonParams, BIGQUERY_READ_TIMEOUT};
use crate::leader_schedule::{bigquery_dataset, create_bigquery_client, GOOGLE_BQ_PROJECT_ID};
use crate::slot_params::is_known_vat_lamports;
use crate::solana_service::{is_alpenglow_active, solana_client_with_timeout};
use anyhow::{bail, Context};
use clap::Parser;
use google_cloud_bigquery::client::Client as BqClient;
use google_cloud_bigquery::http::job::query::QueryRequest;
use google_cloud_bigquery::query::row::Row;
use log::{error, info};
use serde::{Deserialize, Serialize};
use serde_json::json;
use solana_clock::Epoch;
use solana_rpc_client::rpc_client::RpcClient;
use solana_rpc_client_api::request::RpcRequest;
use std::collections::{BTreeSet, HashSet};
use std::time::Duration;

#[derive(Debug, Parser)]
pub struct VatDebitsParams {
    #[arg(
        long = "epoch",
        help = "Epoch whose payout, in the first block of the next epoch, is read."
    )]
    epoch: Epoch,

    #[arg(
        long = "rpc-timeout",
        help = "How long to wait for RPC response (seconds).",
        default_value = "300"
    )]
    rpc_timeout: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct VatDebitsSnapshot {
    pub epoch: Epoch,
    pub slot: u64,
    pub created_at: String,
    pub vote_accounts: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BlockRewards {
    parent_slot: u64,
    num_reward_partitions: Option<u64>,
    rewards: Option<Vec<BlockReward>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BlockReward {
    pubkey: String,
    lamports: i64,
    reward_type: Option<String>,
}

/// Agave burns the VAT only under Alpenglow; the first debits are those of the migration epoch.
fn chain_charges_vat(epoch: Epoch, alpenglow_migration_epoch: Option<Epoch>) -> bool {
    alpenglow_migration_epoch.is_some_and(|migration_epoch| epoch >= migration_epoch)
}

/// Vote accounts stakes-etl saw charged the SIMD-0357 VAT for the epoch; empty while not loaded.
async fn query_vat_debited(
    bq_client: &BqClient,
    dataset: &str,
    epoch: Epoch,
) -> anyhow::Result<Vec<String>> {
    let query = format!(
        "SELECT vote_account \
         FROM `{GOOGLE_BQ_PROJECT_ID}.{dataset}.rewards_validators_inflation_vat` \
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
        .context("Failed to query the VAT debits")?;

    let mut debited = vec![];
    let mut seen = HashSet::new();
    while let Some(row) = iter.next().await? {
        let vote_account = row
            .column::<String>(0)
            .context("Failed to read vote_account")?;
        if !seen.insert(vote_account.clone()) {
            bail!("VAT debits of epoch {epoch} hold vote account {vote_account} more than once");
        }
        debited.push(vote_account);
    }
    Ok(debited)
}

/// The stakes-etl debits of a closed epoch for the cross-check at close; `None` whenever they cannot be read.
pub fn bigquery_vat_debited(rpc_client: &RpcClient, epoch: Epoch) -> Option<Vec<String>> {
    let read = || -> anyhow::Result<Option<Vec<String>>> {
        if !chain_charges_vat(epoch, is_alpenglow_active(rpc_client)?) {
            return Ok(None);
        }
        let Some(dataset) = bigquery_dataset(rpc_client)? else {
            return Ok(None);
        };
        let runtime = tokio::runtime::Runtime::new().context("Failed to create tokio runtime")?;
        let debited = runtime.block_on(with_bigquery_timeout(
            "VAT debits",
            BIGQUERY_READ_TIMEOUT,
            async {
                let bq_client = create_bigquery_client().await?;
                query_vat_debited(&bq_client, dataset, epoch).await
            },
        ))?;
        Ok((!debited.is_empty()).then_some(debited))
    };
    match read() {
        Ok(debited) => debited,
        Err(err) => {
            error!("Could not read the stakes-etl VAT debits of epoch {epoch}: {err:#}");
            None
        }
    }
}

/// The checks of stakes-etl `ensure_vat_debits`, plus the anchors proving the block opens the payout.
fn vat_debited(
    epoch: Epoch,
    first_slot_of_next_epoch: u64,
    block: BlockRewards,
) -> anyhow::Result<Vec<String>> {
    anyhow::ensure!(
        block.parent_slot < first_slot_of_next_epoch,
        "The block read for the payout of epoch {epoch} has its parent at slot {} inside the next epoch starting at slot {first_slot_of_next_epoch}, so it is not the block opening the payout",
        block.parent_slot
    );
    anyhow::ensure!(
        block.num_reward_partitions.is_some(),
        "The block opening the payout of epoch {epoch} carries no numRewardPartitions"
    );
    let debits: Vec<BlockReward> = block
        .rewards
        .unwrap_or_default()
        .into_iter()
        .filter(|reward| reward.reward_type.as_deref() == Some("VATDebit"))
        .collect();
    anyhow::ensure!(
        !debits.is_empty(),
        "The block opening the payout of epoch {epoch} carries no VATDebit rewards"
    );
    anyhow::ensure!(
        debits.iter().all(|debit| debit.lamports < 0),
        "A VATDebit reward of epoch {epoch} credits lamports instead of burning them"
    );
    let amounts: BTreeSet<u64> = debits
        .iter()
        .map(|debit| debit.lamports.unsigned_abs())
        .collect();
    anyhow::ensure!(
        amounts.len() == 1 && amounts.iter().all(|amount| is_known_vat_lamports(*amount)),
        "The VATDebit rewards of epoch {epoch} burn {amounts:?} lamports, expected one known VAT amount"
    );
    let mut seen = HashSet::new();
    for debit in &debits {
        anyhow::ensure!(
            seen.insert(debit.pubkey.as_str()),
            "Vote account {} is debited the VAT of epoch {epoch} more than once",
            debit.pubkey
        );
    }
    Ok(debits.into_iter().map(|debit| debit.pubkey).collect())
}

pub fn collect_vat_debits(
    common_params: CommonParams,
    params: VatDebitsParams,
) -> anyhow::Result<()> {
    let epoch = params.epoch;
    let client = solana_client_with_timeout(
        common_params.rpc_url()?,
        Duration::from_secs(params.rpc_timeout),
        common_params.commitment,
    );
    anyhow::ensure!(
        client.commitment().is_finalized(),
        "The VAT verdict is read only from a finalized block"
    );
    if !chain_charges_vat(epoch, is_alpenglow_active(&client)?) {
        info!("Epoch {epoch} predates Alpenglow and has no VAT debits");
        return Ok(());
    }
    let current_epoch = client.get_epoch_info()?.epoch;
    anyhow::ensure!(
        epoch < current_epoch,
        "Epoch {epoch} is not over, the current epoch is {current_epoch}"
    );

    let first_slot = client
        .get_epoch_schedule()?
        .get_first_slot_in_epoch(epoch + 1);
    let slot = *client
        .get_blocks_with_limit(first_slot, 1)?
        .first()
        .with_context(|| format!("No finalized block yet in epoch {}", epoch + 1))?;
    let block: Option<BlockRewards> = client.send(
        RpcRequest::GetBlock,
        json!([slot, {
            "encoding": "json",
            "transactionDetails": "none",
            "rewards": true,
            "commitment": "finalized",
            "maxSupportedTransactionVersion": 0
        }]),
    )?;
    let block = block.with_context(|| format!("RPC returned no block at slot {slot}"))?;
    let vote_accounts = vat_debited(epoch, first_slot, block)?;
    info!(
        "Epoch {epoch}: {} vote accounts paid the VAT in block {slot}",
        vote_accounts.len()
    );

    serde_yaml::to_writer(
        std::io::stdout(),
        &VatDebitsSnapshot {
            epoch,
            slot,
            created_at: chrono::Utc::now().to_string(),
            vote_accounts,
        },
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    const FIRST_SLOT: u64 = 1_000;

    fn reward(pubkey: &str, lamports: i64, reward_type: &str) -> Value {
        json!({"pubkey": pubkey, "lamports": lamports, "postBalance": 5_000_000_000u64, "rewardType": reward_type})
    }

    fn block(parent_slot: u64, partitions: Option<u64>, rewards: Vec<Value>) -> BlockRewards {
        serde_json::from_value(json!({
            "parentSlot": parent_slot,
            "numRewardPartitions": partitions,
            "rewards": rewards,
        }))
        .unwrap()
    }

    fn opening_block(rewards: Vec<Value>) -> BlockRewards {
        block(FIRST_SLOT - 1, Some(4), rewards)
    }

    fn error_of(block: BlockRewards) -> String {
        vat_debited(10, FIRST_SLOT, block).unwrap_err().to_string()
    }

    #[test]
    fn the_opening_block_yields_the_debited_vote_accounts() {
        let debited = vat_debited(
            10,
            FIRST_SLOT,
            opening_block(vec![
                reward("voteA", 1_000, "Voting"),
                reward("voteA", -800_000_000, "VATDebit"),
                reward("voteB", -800_000_000, "VATDebit"),
                reward("stakeC", 500, "Staking"),
            ]),
        )
        .unwrap();
        assert_eq!(debited, vec!["voteA".to_string(), "voteB".to_string()]);
    }

    #[test]
    fn a_block_after_the_opening_one_is_refused() {
        let err = error_of(block(
            FIRST_SLOT,
            Some(4),
            vec![reward("voteA", -800_000_000, "VATDebit")],
        ));
        assert!(err.contains("not the block opening the payout"), "{err}");
    }

    #[test]
    fn a_block_without_reward_partitions_is_refused() {
        let err = error_of(block(
            FIRST_SLOT - 1,
            None,
            vec![reward("voteA", -800_000_000, "VATDebit")],
        ));
        assert!(err.contains("no numRewardPartitions"), "{err}");
    }

    #[test]
    fn a_block_without_vat_debits_is_refused() {
        let err = error_of(opening_block(vec![reward("voteA", 1_000, "Voting")]));
        assert!(err.contains("no VATDebit rewards"), "{err}");
    }

    #[test]
    fn a_positive_vat_debit_is_refused() {
        let err = error_of(opening_block(vec![reward(
            "voteA",
            800_000_000,
            "VATDebit",
        )]));
        assert!(err.contains("credits lamports"), "{err}");
    }

    #[test]
    fn two_vat_amounts_are_refused() {
        let err = error_of(opening_block(vec![
            reward("voteA", -800_000_000, "VATDebit"),
            reward("voteB", -1_000_000_000, "VATDebit"),
        ]));
        assert!(err.contains("expected one known VAT amount"), "{err}");
    }

    #[test]
    fn an_unknown_vat_amount_is_refused() {
        let err = error_of(opening_block(vec![reward(
            "voteA",
            -900_000_000,
            "VATDebit",
        )]));
        assert!(err.contains("expected one known VAT amount"), "{err}");
    }

    #[test]
    fn a_vote_account_debited_twice_is_refused() {
        let err = error_of(opening_block(vec![
            reward("voteA", -800_000_000, "VATDebit"),
            reward("voteA", -800_000_000, "VATDebit"),
        ]));
        assert!(err.contains("more than once"), "{err}");
    }

    #[test]
    fn the_chain_charges_vat_from_the_alpenglow_migration_epoch_on() {
        assert!(!chain_charges_vat(1062, Some(1063)));
        assert!(chain_charges_vat(1063, Some(1063)));
        assert!(chain_charges_vat(1085, Some(1063)));
        assert!(!chain_charges_vat(1085, None));
    }
}
