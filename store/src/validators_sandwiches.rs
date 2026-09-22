use chrono::{DateTime, Utc};
use collect::validators_sandwiches::ValidatorsSandwichesSnapshot;
use log::info;
use rust_decimal::prelude::*;
use serde_yaml;
use tokio_postgres::Client;

pub const VALIDATORS_SANDWICHES_TABLE: &str = "validators_sandwiches";

#[derive(Debug, clap::Parser)]
pub struct StoreSandwichesParams {
    #[arg(long = "snapshot-file")]
    snapshot_path: String,
}

const DEFAULT_CHUNK_SIZE: usize = 500;

pub async fn store_sandwiches(
    params: StoreSandwichesParams,
    psql_client: &mut Client,
) -> anyhow::Result<()> {
    info!("Storing validator sandwich rates snapshot...");

    let path = params.snapshot_path;
    let snapshot_file = std::fs::File::open(&path)
        .map_err(|e| anyhow::anyhow!("Failed to open snapshot sandwiches file '{path}': {e}"))?;
    let snapshot: ValidatorsSandwichesSnapshot = serde_yaml::from_reader(snapshot_file)
        .map_err(|e| anyhow::anyhow!("Failed to parse snapshot sandwiches file '{path}': {e}"))?;

    let snapshot_created_at: DateTime<Utc> = snapshot.created_at.parse()?;

    info!(
        "Loaded the sandwiches snapshot from epoch {}. Snapshot created at {} loaded at epoch {}, slot index {}. {} records.",
        snapshot.from_epoch,
        snapshot_created_at,
        snapshot.loaded_at_epoch,
        snapshot.loaded_at_slot_index,
        snapshot.sandwiches.len()
    );

    let mut total_upserted = 0;

    for chunk in snapshot.sandwiches.chunks(DEFAULT_CHUNK_SIZE) {
        let epochs: Vec<Decimal> = chunk.iter().map(|r| Decimal::from(r.epoch)).collect();
        let vote_accounts: Vec<&str> = chunk.iter().map(|r| r.vote_account.as_str()).collect();
        let blocks_produced: Vec<Decimal> = chunk
            .iter()
            .map(|r| Decimal::from(r.blocks_produced))
            .collect();
        let blocks_with_sandwiches: Vec<Decimal> = chunk
            .iter()
            .map(|r| Decimal::from(r.blocks_with_sandwiches))
            .collect();
        let rates_30d: Vec<f64> = chunk.iter().map(|r| r.sandwich_rate_30d).collect();
        let rates_60d: Vec<Option<f64>> = chunk.iter().map(|r| r.sandwich_rate_60d).collect();
        let updated_ats: Vec<&DateTime<Utc>> = vec![&snapshot_created_at; chunk.len()];
        let created_ats = updated_ats.clone();

        let query = format!(
            "INSERT INTO {VALIDATORS_SANDWICHES_TABLE} (
            epoch,
            vote_account,
            blocks_produced,
            blocks_with_sandwiches,
            sandwich_rate_30d,
            sandwich_rate_60d,
            created_at,
            updated_at
        )
        SELECT * FROM UNNEST(
            $1::NUMERIC[],
            $2::TEXT[],
            $3::NUMERIC[],
            $4::NUMERIC[],
            $5::DOUBLE PRECISION[],
            $6::DOUBLE PRECISION[],
            $7::TIMESTAMP WITH TIME ZONE[],
            $8::TIMESTAMP WITH TIME ZONE[]
        )
        ON CONFLICT (epoch, vote_account)
        DO UPDATE SET
            blocks_produced = EXCLUDED.blocks_produced,
            blocks_with_sandwiches = EXCLUDED.blocks_with_sandwiches,
            sandwich_rate_30d = EXCLUDED.sandwich_rate_30d,
            sandwich_rate_60d = EXCLUDED.sandwich_rate_60d,
            updated_at = EXCLUDED.updated_at"
        );

        let rows_affected = psql_client
            .execute(
                &query,
                &[
                    &epochs,
                    &vote_accounts,
                    &blocks_produced,
                    &blocks_with_sandwiches,
                    &rates_30d,
                    &rates_60d,
                    &created_ats,
                    &updated_ats,
                ],
            )
            .await?;

        total_upserted += rows_affected;

        info!("Upserted {rows_affected} sandwich records in this chunk");
    }

    info!("Stored sandwiches snapshot: {total_upserted} total records upserted");

    Ok(())
}
