use crate::dto::TakeRateRecord;
use chrono::{DateTime, Utc};
use collect::take_rates::ValidatorRewardsSnapshot;
use log::info;
use rust_decimal::prelude::*;
use serde_yaml;
use std::collections::HashMap;
use structopt::StructOpt;
use tokio_postgres::Client;

pub const VALIDATORS_REWARDS_TABLE: &str = "validators_rewards";

#[derive(Debug, StructOpt)]
pub struct StoreTakeRatesParams {
    #[structopt(long = "snapshot-file")]
    snapshot_path: String,
}

const DEFAULT_CHUNK_SIZE: usize = 500;

struct ValidatorRewardsRow {
    vote_account: String,
    epoch: Decimal,
    validator_rewards: Decimal,
    total_rewards: Decimal,
    inflation_rewards: Decimal,
    mev_rewards: Decimal,
    block_rewards: Decimal,
    take_rate: f64,
}

pub async fn store_take_rates(
    params: StoreTakeRatesParams,
    psql_client: &mut Client,
) -> anyhow::Result<()> {
    info!("Storing take rates snapshot...");

    let path = params.snapshot_path;
    let snapshot_file = std::fs::File::open(&path)
        .map_err(|e| anyhow::anyhow!("Failed to open snapshot take rates file '{path}': {e}"))?;
    let snapshot: ValidatorRewardsSnapshot = serde_yaml::from_reader(snapshot_file)
        .map_err(|e| anyhow::anyhow!("Failed to parse snapshot take rates file '{path}': {e}"))?;

    let snapshot_created_at: DateTime<Utc> = snapshot.created_at.parse()?;

    info!(
        "Loaded the snapshot from epoch {}. Snapshot created at {} loaded at epoch {}, slot index {}",
        snapshot.from_epoch,
        snapshot_created_at,
        snapshot.loaded_at_epoch,
        snapshot.loaded_at_slot_index
    );

    // The BigQuery query already emits one row per (vote_account, epoch); dedup defensively so a
    // single upsert statement can never touch the same conflict target twice.
    let rows: HashMap<(String, u64), ValidatorRewardsRow> = snapshot
        .rewards
        .iter()
        .filter(|r| r.total_rewards > 0)
        .map(|r| {
            let key = (r.vote_account.clone(), r.epoch);
            let take_rate = r.validator_rewards as f64 / r.total_rewards as f64;
            (
                key,
                ValidatorRewardsRow {
                    vote_account: r.vote_account.clone(),
                    epoch: Decimal::from(r.epoch),
                    validator_rewards: Decimal::from(r.validator_rewards),
                    total_rewards: Decimal::from(r.total_rewards),
                    inflation_rewards: Decimal::from(r.inflation_rewards),
                    mev_rewards: Decimal::from(r.mev_rewards),
                    block_rewards: Decimal::from(r.block_rewards),
                    take_rate,
                },
            )
        })
        .collect();

    info!(
        "Processing snapshot loaded take rate records {}",
        rows.len()
    );

    let records: Vec<_> = rows.values().collect();
    let mut total_upserted = 0;

    for chunk in records.chunks(DEFAULT_CHUNK_SIZE) {
        let vote_accounts: Vec<&str> = chunk.iter().map(|r| r.vote_account.as_str()).collect();
        let epochs: Vec<&Decimal> = chunk.iter().map(|r| &r.epoch).collect();
        let validator_rewards: Vec<&Decimal> = chunk.iter().map(|r| &r.validator_rewards).collect();
        let total_rewards: Vec<&Decimal> = chunk.iter().map(|r| &r.total_rewards).collect();
        let inflation_rewards: Vec<&Decimal> = chunk.iter().map(|r| &r.inflation_rewards).collect();
        let mev_rewards: Vec<&Decimal> = chunk.iter().map(|r| &r.mev_rewards).collect();
        let block_rewards: Vec<&Decimal> = chunk.iter().map(|r| &r.block_rewards).collect();
        let take_rates: Vec<f64> = chunk.iter().map(|r| r.take_rate).collect();
        let updated_ats: Vec<&DateTime<Utc>> = vec![&snapshot_created_at; chunk.len()];
        let created_ats = updated_ats.clone();

        let query = format!(
            "INSERT INTO {VALIDATORS_REWARDS_TABLE} (
            vote_account,
            epoch,
            validator_rewards,
            total_rewards,
            inflation_rewards,
            mev_rewards,
            block_rewards,
            take_rate,
            created_at,
            updated_at
        )
        SELECT * FROM UNNEST(
            $1::TEXT[],
            $2::NUMERIC[],
            $3::NUMERIC[],
            $4::NUMERIC[],
            $5::NUMERIC[],
            $6::NUMERIC[],
            $7::NUMERIC[],
            $8::DOUBLE PRECISION[],
            $9::TIMESTAMP WITH TIME ZONE[],
            $10::TIMESTAMP WITH TIME ZONE[]
        )
        ON CONFLICT (vote_account, epoch)
        DO UPDATE SET
            validator_rewards = EXCLUDED.validator_rewards,
            total_rewards = EXCLUDED.total_rewards,
            inflation_rewards = EXCLUDED.inflation_rewards,
            mev_rewards = EXCLUDED.mev_rewards,
            block_rewards = EXCLUDED.block_rewards,
            take_rate = EXCLUDED.take_rate,
            updated_at = EXCLUDED.updated_at"
        );

        let rows_affected = psql_client
            .execute(
                &query,
                &[
                    &vote_accounts,
                    &epochs,
                    &validator_rewards,
                    &total_rewards,
                    &inflation_rewards,
                    &mev_rewards,
                    &block_rewards,
                    &take_rates,
                    &created_ats,
                    &updated_ats,
                ],
            )
            .await?;

        total_upserted += rows_affected;

        info!("Upserted {rows_affected} take rate records in this chunk");
    }

    info!("Stored take rates snapshot: {total_upserted} total records upserted");

    Ok(())
}

pub async fn get_take_rate_series(
    psql_client: &Client,
    vote_account: &str,
    from_epoch: Option<u64>,
) -> anyhow::Result<Vec<TakeRateRecord>> {
    let from_epoch = from_epoch.map(Decimal::from);
    let query = format!(
        "
        SELECT
            {VALIDATORS_REWARDS_TABLE}.epoch, take_rate, created_at,
            epochs.start_at AS epoch_start, epochs.end_at AS epoch_end
        FROM {VALIDATORS_REWARDS_TABLE}
        LEFT JOIN epochs ON {VALIDATORS_REWARDS_TABLE}.epoch = epochs.epoch
        WHERE vote_account = $1
          AND ($2::NUMERIC IS NULL OR {VALIDATORS_REWARDS_TABLE}.epoch >= $2::NUMERIC)
        ORDER BY {VALIDATORS_REWARDS_TABLE}.epoch ASC
        "
    );
    let rows = psql_client
        .query(&query, &[&vote_account, &from_epoch])
        .await?;

    let mut records = Vec::with_capacity(rows.len());
    for row in rows {
        // Null until `store close-epoch` writes the `epochs` row, so the open epoch has no boundaries.
        records.push(TakeRateRecord {
            epoch: row.get::<_, Decimal>("epoch").try_into()?,
            epoch_start_at: row.get::<_, Option<DateTime<Utc>>>("epoch_start"),
            epoch_end_at: row.get::<_, Option<DateTime<Utc>>>("epoch_end"),
            take_rate: row.get("take_rate"),
            created_at: row.get("created_at"),
        })
    }

    Ok(records)
}
