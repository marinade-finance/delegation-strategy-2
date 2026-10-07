use crate::directory::Directory;
use crate::docs::{
    epoch_doc_path, merge_into, merge_validator_rewards, ValidatorRewardsDoc,
    ValidatorRewardsEntry, VALIDATOR_REWARDS_DIR,
};
use crate::dto::TakeRateRecord;
use crate::utils::{expected_take_rate, worst_known_commission, RewardMixShares};
use crate::warehouse::Warehouse;
use chrono::{DateTime, Utc};
use clap::Parser;
use collect::take_rates::ValidatorRewardsSnapshot;
use log::info;
use rust_decimal::prelude::*;
use serde_yaml;
use std::collections::{BTreeMap, HashMap};

const SUPPORTED_DATA_VERSION: u16 = 1;

#[derive(Debug, Parser)]
pub struct StoreTakeRatesParams {
    #[arg(long = "snapshot-file")]
    snapshot_path: String,
}

pub async fn store_take_rates(
    params: StoreTakeRatesParams,
    directory: &Directory,
) -> anyhow::Result<()> {
    info!("Storing take rates snapshot...");

    let path = params.snapshot_path;
    let snapshot_file = std::fs::File::open(&path)
        .map_err(|e| anyhow::anyhow!("Failed to open snapshot take rates file '{path}': {e}"))?;
    let snapshot: ValidatorRewardsSnapshot = serde_yaml::from_reader(snapshot_file)
        .map_err(|e| anyhow::anyhow!("Failed to parse snapshot take rates file '{path}': {e}"))?;

    anyhow::ensure!(
        snapshot.version == SUPPORTED_DATA_VERSION,
        "Snapshot take rates file '{path}' has version {}, expected {SUPPORTED_DATA_VERSION}",
        snapshot.version
    );

    let created_at: DateTime<Utc> = snapshot.created_at.parse()?;

    info!(
        "Loaded the snapshot from epoch {}. Snapshot created at {} loaded at epoch {}, slot index {}",
        snapshot.from_epoch, created_at, snapshot.loaded_at_epoch, snapshot.loaded_at_slot_index
    );

    // One snapshot reaches back over several epochs; each has its own
    // document. A validator that earned nothing has no rate to store.
    let mut rewards_by_epoch: BTreeMap<u64, ValidatorRewardsDoc> = Default::default();
    for rewards in snapshot.rewards.iter().filter(|r| r.total_rewards > 0) {
        rewards_by_epoch.entry(rewards.epoch).or_default().insert(
            rewards.vote_account.clone(),
            ValidatorRewardsEntry {
                validator_rewards: Decimal::from(rewards.validator_rewards),
                total_rewards: Decimal::from(rewards.total_rewards),
                inflation_rewards: Decimal::from(rewards.inflation_rewards),
                mev_rewards: Decimal::from(rewards.mev_rewards),
                block_rewards: Decimal::from(rewards.block_rewards),
                take_rate: rewards.validator_rewards as f64 / rewards.total_rewards as f64,
                created_at,
                updated_at: created_at,
            },
        );
    }

    let mut total = 0;
    for (epoch, rewards) in rewards_by_epoch {
        let path = epoch_doc_path(VALIDATOR_REWARDS_DIR, epoch);
        total += rewards.len();
        merge_into(directory, &path, rewards, merge_validator_rewards).await?;
        info!("Stored take rate records at {path}");
    }

    info!("Stored take rates snapshot: {total} total records");

    Ok(())
}

/// Cluster reward mix per epoch. Epochs that paid no inflation are absent: the
/// in-progress one has only accruing block rewards, and nothing else pays out
/// before the epoch closes.
pub fn load_epoch_reward_mix(warehouse: &Warehouse) -> HashMap<u64, RewardMixShares> {
    let mut mix = HashMap::with_capacity(warehouse.validator_rewards.len());
    for (epoch, rewards) in warehouse.validator_rewards.iter() {
        let inflation: Decimal = rewards.values().map(|r| r.inflation_rewards).sum();
        let mev: Decimal = rewards.values().map(|r| r.mev_rewards).sum();
        let block: Decimal = rewards.values().map(|r| r.block_rewards).sum();
        if inflation <= Decimal::ZERO {
            continue;
        }

        let total = (inflation + mev + block).to_f64().unwrap_or_default();
        mix.insert(
            *epoch,
            RewardMixShares {
                inflation: inflation.to_f64().unwrap_or_default() / total,
                mev: mev.to_f64().unwrap_or_default() / total,
                block: block.to_f64().unwrap_or_default() / total,
            },
        );
    }

    mix
}

/// One validator's take rate per epoch, oldest first, over the epochs the
/// warehouse holds. `epoch_start_at` and `epoch_end_at` are null where the
/// epoch is not sealed yet.
pub fn get_take_rate_series(
    warehouse: &Warehouse,
    vote_account: &str,
    from_epoch: Option<u64>,
    reward_mix: &HashMap<u64, RewardMixShares>,
) -> Vec<TakeRateRecord> {
    let from_epoch = from_epoch.unwrap_or_default();
    let mut records = Vec::new();
    for (epoch, rewards) in warehouse.validator_rewards.range(from_epoch..) {
        let Some(entry) = rewards.get(vote_account) else {
            continue;
        };
        let epoch_record = warehouse.epochs.get(epoch);
        let validator = warehouse
            .snapshots
            .get(epoch)
            .and_then(|snapshot| snapshot.get(vote_account));
        let mev_commission_bps = warehouse
            .mev
            .get(epoch)
            .and_then(|mev| mev.get(vote_account))
            .map(|mev| mev.mev_commission);
        let priority_commission_bps = warehouse
            .priority_fees
            .get(epoch)
            .and_then(|fees| fees.get(vote_account))
            .map(|fees| fees.priority_commission);

        records.push(TakeRateRecord {
            epoch: *epoch,
            epoch_start_at: epoch_record.map(|record| record.start_at),
            epoch_end_at: epoch_record.map(|record| record.end_at),
            realized_take_rate: entry.take_rate,
            expected_take_rate: reward_mix.get(epoch).and_then(|shares| {
                expected_take_rate(
                    *shares,
                    worst_known_commission(
                        validator.and_then(|v| v.commission_max_observed),
                        validator.and_then(|v| v.commission_advertised),
                    ),
                    mev_commission_bps,
                    priority_commission_bps,
                )
            }),
            created_at: entry.created_at,
        });
    }

    records
}
