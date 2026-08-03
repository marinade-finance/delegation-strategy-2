use chrono::{DateTime, Utc};
use collect::take_rates::TakeRatesSnapshot;
use log::info;
use rust_decimal::prelude::*;
use serde_yaml;
use std::collections::HashMap;
use structopt::StructOpt;
use tokio_postgres::Client;

pub const TAKE_RATES_TABLE: &str = "take_rates";

#[derive(Debug, StructOpt)]
pub struct StoreTakeRatesParams {
    #[structopt(long = "snapshot-file")]
    snapshot_path: String,
}

const DEFAULT_CHUNK_SIZE: usize = 500;

struct TakeRateRow {
    vote_account: String,
    epoch: Decimal,
    validator_rewards: Decimal,
    total_rewards: Decimal,
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
    let snapshot: TakeRatesSnapshot = serde_yaml::from_reader(snapshot_file)
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
    let rows: HashMap<(String, u64), TakeRateRow> = snapshot
        .take_rates
        .iter()
        .filter(|r| r.total_rewards > 0)
        .map(|r| {
            let key = (r.vote_account.clone(), r.epoch);
            let take_rate = r.validator_rewards as f64 / r.total_rewards as f64;
            (
                key,
                TakeRateRow {
                    vote_account: r.vote_account.clone(),
                    epoch: Decimal::from(r.epoch),
                    validator_rewards: Decimal::from(r.validator_rewards),
                    total_rewards: Decimal::from(r.total_rewards),
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
        let take_rates: Vec<f64> = chunk.iter().map(|r| r.take_rate).collect();
        let updated_ats: Vec<&DateTime<Utc>> = vec![&snapshot_created_at; chunk.len()];
        let created_ats = updated_ats.clone();

        let query = format!(
            "INSERT INTO {TAKE_RATES_TABLE} (
            vote_account,
            epoch,
            validator_rewards,
            total_rewards,
            take_rate,
            created_at,
            updated_at
        )
        SELECT * FROM UNNEST(
            $1::TEXT[],
            $2::NUMERIC[],
            $3::NUMERIC[],
            $4::NUMERIC[],
            $5::DOUBLE PRECISION[],
            $6::TIMESTAMP WITH TIME ZONE[],
            $7::TIMESTAMP WITH TIME ZONE[]
        )
        ON CONFLICT (vote_account, epoch)
        DO UPDATE SET
            validator_rewards = EXCLUDED.validator_rewards,
            total_rewards = EXCLUDED.total_rewards,
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
