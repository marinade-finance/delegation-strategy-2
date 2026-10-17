use crate::directory::Directory;
use crate::docs::{
    block_reward_key, epoch_doc_path, merge_block_rewards, merge_into, BlockRewardEntry,
    BlockRewardsDoc, BLOCK_REWARDS_DIR,
};
use crate::dto::{ValidatorBlockReward, ValidatorBlockRewardsRecord};
use crate::warehouse::Warehouse;
use chrono::{DateTime, Utc};
use clap::Parser;
use collect::validators_block_rewards::ValidatorsBlockRewardsSnapshot;
use log::info;
use serde_yaml;
use std::collections::HashMap;

#[derive(Debug, Parser)]
pub struct StoreBlockRewardsParams {
    #[arg(long = "snapshot-file")]
    snapshot_path: String,
}

pub async fn store_block_rewards(
    params: StoreBlockRewardsParams,
    directory: &Directory,
) -> anyhow::Result<()> {
    info!("Storing block rewards snapshot...");

    let path = params.snapshot_path;
    let snapshot_file = std::fs::File::open(&path)
        .map_err(|e| anyhow::anyhow!("Failed to open snapshot block rewards file '{path}': {e}"))?;
    let snapshot: ValidatorsBlockRewardsSnapshot =
        serde_yaml::from_reader(snapshot_file).map_err(|e| {
            anyhow::anyhow!("Failed to parse snapshot block rewards file '{path}': {e}")
        })?;
    let created_at: DateTime<Utc> = snapshot.created_at.parse()?;

    info!(
        "Loaded the snapshot for epoch {}. Snapshot created at {} loaded at epoch {}, slot index {}",
        snapshot.epoch, created_at, snapshot.loaded_at_epoch, snapshot.loaded_at_slot_index
    );

    let mut block_rewards = BlockRewardsDoc::new();
    for reward in snapshot.block_rewards.iter() {
        let key = block_reward_key(&reward.identity_account, &reward.vote_account);
        let reward = ValidatorBlockReward::from_snapshot(reward, snapshot.epoch);
        block_rewards.insert(key, BlockRewardEntry::new(&reward, created_at)?);
    }

    info!("Processing block rewards records {}", block_rewards.len());

    let path = epoch_doc_path(BLOCK_REWARDS_DIR, snapshot.epoch);
    merge_into(directory, &path, block_rewards, merge_block_rewards).await?;

    info!("Stored the block rewards snapshot at {path}");

    Ok(())
}

/// The newest block reward per (identity, vote account) inside the window.
pub fn get_last_block_rewards(
    warehouse: &Warehouse,
    epochs: u64,
) -> anyhow::Result<Vec<ValidatorBlockRewardsRecord>> {
    let first_epoch = warehouse.window_start(epochs);
    let mut latest: HashMap<&String, &BlockRewardEntry> = Default::default();

    for (_, rewards) in warehouse.block_rewards.range(first_epoch..) {
        for (key, reward) in rewards.iter() {
            latest.insert(key, reward);
        }
    }

    let mut records: Vec<_> = latest.into_values().map(to_record).collect();
    records.sort_by_key(|record| record.epoch);

    Ok(records)
}

pub fn get_block_rewards_by_epoch(
    warehouse: &Warehouse,
    epoch: u64,
) -> anyhow::Result<Vec<ValidatorBlockRewardsRecord>> {
    let Some(rewards) = warehouse.block_rewards.get(&epoch) else {
        return Ok(Default::default());
    };

    let mut records: Vec<_> = rewards.values().map(to_record).collect();
    records.sort_by(|a, b| a.vote_account.cmp(&b.vote_account));

    Ok(records)
}

fn to_record(reward: &BlockRewardEntry) -> ValidatorBlockRewardsRecord {
    ValidatorBlockRewardsRecord {
        epoch: reward.epoch,
        identity_account: reward.identity_account.clone(),
        vote_account: reward.vote_account.clone(),
        authorized_voter: reward.authorized_voter.clone(),
        amount: reward.amount,
    }
}
