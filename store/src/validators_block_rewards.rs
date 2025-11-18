use crate::dto::{ValidatorBlockReward, ValidatorBlockRewardsRecord};
use chrono::{DateTime, Utc};
use collect::validators_block_rewards::ValidatorsBlockRewardsSnapshot;
use log::info;
use rust_decimal::prelude::*;
use serde_yaml;
use std::collections::HashMap;
use structopt::StructOpt;
use tokio_postgres::Client;

pub const VALIDATORS_BLOCK_REWARDS_TABLE: &str = "validators_block_rewards";

#[derive(Debug, StructOpt)]
pub struct StoreBlockRewardsParams {
    #[structopt(long = "snapshot-file")]
    snapshot_path: String,
}

const DEFAULT_CHUNK_SIZE: usize = 500;

pub async fn store_block_rewards(
    params: StoreBlockRewardsParams,
    psql_client: &mut Client,
) -> anyhow::Result<()> {
    info!("Storing block rewards snapshot...");

    let path = params.snapshot_path;
    let snapshot_file = std::fs::File::open(&path)
        .map_err(|e| anyhow::anyhow!("Failed to open snapshot block rewards file '{path}': {e}"))?;
    let snapshot: ValidatorsBlockRewardsSnapshot =
        serde_yaml::from_reader(snapshot_file).map_err(|e| {
            anyhow::anyhow!("Failed to parse snapshot block rewards file '{path}': {e}")
        })?;

    let snapshot_created_at: DateTime<Utc> = snapshot.created_at.parse()?;
    let snapshot_epoch = Decimal::from(snapshot.epoch);

    info!(
        "Loaded the snapshot for epoch {}. Snapshot created at {} loaded at epoch {}, slot index {}",
        snapshot_epoch,
        snapshot_created_at,
        snapshot.loaded_at_epoch,
        snapshot.loaded_at_slot_index
    );

    let block_rewards: HashMap<_, _> = snapshot
        .block_rewards
        .iter()
        .map(|r| {
            let key = (r.identity_account.clone(), r.vote_account.clone());
            let reward = ValidatorBlockReward::from_snapshot(r, snapshot.epoch);
            (key, reward)
        })
        .collect();

    info!(
        "Processing snapshot loaded block rewards records {}",
        block_rewards.keys().len()
    );

    let records: Vec<_> = block_rewards.values().collect();

    let mut total_upserted = 0;

    for chunk in records.chunks(DEFAULT_CHUNK_SIZE) {
        // Build arrays for each column
        let identity_accounts: Vec<&str> =
            chunk.iter().map(|r| r.identity_account.as_str()).collect();
        let vote_accounts: Vec<&str> = chunk.iter().map(|r| r.vote_account.as_str()).collect();
        let authorized_voters: Vec<&str> =
            chunk.iter().map(|r| r.authorized_voter.as_str()).collect();
        let amounts: Vec<&Decimal> = chunk.iter().map(|r| &r.amount).collect();
        let epochs: Vec<&Decimal> = chunk.iter().map(|r| &r.epoch).collect();
        let updated_ats: Vec<&DateTime<Utc>> = vec![&snapshot_created_at; chunk.len()];
        let created_ats = updated_ats.clone();

        let query = format!(
            "INSERT INTO {VALIDATORS_BLOCK_REWARDS_TABLE} (
            identity_account,
            vote_account,
            authorized_voter,
            amount,
            epoch,
            created_at,
            updated_at
        )
        SELECT * FROM UNNEST(
            $1::TEXT[],
            $2::TEXT[],
            $3::TEXT[],
            $4::NUMERIC[],
            $5::NUMERIC[],
            $6::TIMESTAMP WITH TIME ZONE[],
            $7::TIMESTAMP WITH TIME ZONE[]
        )
        ON CONFLICT (epoch, identity_account, vote_account)
        DO UPDATE SET
            authorized_voter = EXCLUDED.authorized_voter,
            amount = EXCLUDED.amount,
            updated_at = EXCLUDED.updated_at"
        );

        let rows_affected = psql_client
            .execute(
                &query,
                &[
                    &identity_accounts,
                    &vote_accounts,
                    &authorized_voters,
                    &amounts,
                    &epochs,
                    &created_ats,
                    &updated_ats,
                ],
            )
            .await?;

        total_upserted += rows_affected;

        info!("Upserted {rows_affected} block rewards records in this chunk",);
    }

    info!("Stored block rewards snapshot: {total_upserted} total records upserted",);

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
