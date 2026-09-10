use crate::directory::Directory;
use crate::docs::{
    block_reward_key, epoch_doc_path, merge_block_rewards, merge_into, BlockRewardEntry,
    BlockRewardsDoc, BLOCK_REWARDS_DIR,
};
use crate::dto::{ValidatorBlockReward, ValidatorBlockRewardsRecord};
use chrono::{DateTime, Utc};
use collect::validators_block_rewards::ValidatorsBlockRewardsSnapshot;
use log::info;
use rust_decimal::prelude::*;
use serde_yaml;
use structopt::StructOpt;
use tokio_postgres::Client;

pub const VALIDATORS_BLOCK_REWARDS_TABLE: &str = "validators_block_rewards";

#[derive(Debug, StructOpt)]
pub struct StoreBlockRewardsParams {
    #[structopt(long = "snapshot-file")]
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

pub async fn get_last_block_rewards(
    psql_client: &Client,
    epochs: u64,
    table_name: &str,
) -> anyhow::Result<Vec<ValidatorBlockRewardsRecord>> {
    let query = format!(
        "WITH cluster AS (
            SELECT MAX(epoch) AS last_epoch
            FROM cluster_info
        ),
        filtered_data AS (
            SELECT
                epoch,
                identity_account,
                vote_account,
                authorized_voter,
                amount,
                ROW_NUMBER() OVER (PARTITION BY identity_account, vote_account ORDER BY epoch DESC) AS rn
            FROM {table_name}
            CROSS JOIN cluster
            WHERE epoch > cluster.last_epoch - $1::NUMERIC
        )
        SELECT identity_account, vote_account, authorized_voter, amount, epoch
        FROM filtered_data
        WHERE rn = 1
        ORDER BY epoch ASC;"
    );

    let rows = psql_client.query(&query, &[&Decimal::from(epochs)]).await?;

    let mut results = Vec::new();
    for row in rows {
        results.push(ValidatorBlockRewardsRecord {
            epoch: row.get::<_, Decimal>("epoch").try_into()?,
            identity_account: row.get("identity_account"),
            vote_account: row.get("vote_account"),
            authorized_voter: row.get("authorized_voter"),
            amount: row.get("amount"),
        });
    }

    Ok(results)
}

pub async fn get_block_rewards_by_epoch(
    psql_client: &Client,
    epoch: u64,
    table_name: &str,
) -> anyhow::Result<Vec<ValidatorBlockRewardsRecord>> {
    let query = format!(
        "SELECT epoch,identity_account, vote_account, authorized_voter, amount
         FROM {table_name}
         WHERE epoch = $1
         ORDER BY vote_account ASC;"
    );

    let rows = psql_client
        .query(&query, &[&Decimal::from(epoch)])
        .await
        .map_err(|e| {
            anyhow::anyhow!("Failed to get block rewards for epoch {epoch}: {e} [{e:?}]")
        })?;

    let mut results = Vec::new();
    for row in rows {
        results.push(ValidatorBlockRewardsRecord {
            epoch: row.get::<_, Decimal>("epoch").try_into()?,
            identity_account: row.get("identity_account"),
            vote_account: row.get("vote_account"),
            authorized_voter: row.get("authorized_voter"),
            amount: row.get("amount"),
        });
    }

    Ok(results)
}
