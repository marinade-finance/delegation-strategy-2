use chrono::{DateTime, Utc};
use collect::validators_slot_times::ValidatorsSlotTimesSnapshot;
use log::info;
use rust_decimal::prelude::*;
use std::collections::BTreeMap;
use tokio_postgres::Client;

pub const VALIDATORS_SLOT_TIMES_TABLE: &str = "validators_slot_times";

const SNAPSHOT_VERSION: u16 = 1;
const DEFAULT_CHUNK_SIZE: usize = 500;

#[derive(Debug, clap::Parser)]
pub struct StoreSlotTimesParams {
    #[arg(long = "snapshot-file")]
    snapshot_path: String,
}

pub async fn store_slot_times(
    params: StoreSlotTimesParams,
    psql_client: &mut Client,
) -> anyhow::Result<()> {
    info!("Storing validator slot times snapshot...");

    let path = params.snapshot_path;
    let snapshot_file = std::fs::File::open(&path)
        .map_err(|e| anyhow::anyhow!("Failed to open snapshot slot times file '{path}': {e}"))?;
    let snapshot: ValidatorsSlotTimesSnapshot = serde_yaml::from_reader(snapshot_file)
        .map_err(|e| anyhow::anyhow!("Failed to parse snapshot slot times file '{path}': {e}"))?;
    anyhow::ensure!(
        snapshot.version == SNAPSHOT_VERSION,
        "Unsupported slot times snapshot version {} (expected {SNAPSHOT_VERSION})",
        snapshot.version
    );

    let snapshot_created_at: DateTime<Utc> = snapshot.created_at.parse()?;

    info!(
        "Loaded the {} slot times snapshot from epoch {}. Snapshot created at {} loaded at epoch {}, slot index {}. {} records.",
        snapshot.source,
        snapshot.from_epoch,
        snapshot_created_at,
        snapshot.loaded_at_epoch,
        snapshot.loaded_at_slot_index,
        snapshot.slot_times.len()
    );

    // One upsert statement cannot touch the same (epoch, vote_account) twice. The last row wins.
    let slot_times: Vec<_> = snapshot
        .slot_times
        .iter()
        .map(|r| ((r.epoch, r.vote_account.as_str()), r))
        .collect::<BTreeMap<_, _>>()
        .into_values()
        .collect();

    let mut total_upserted = 0;
    // One transaction: the blacklist judges an epoch once, so it must never see half of one.
    let tx = psql_client.transaction().await?;

    for chunk in slot_times.chunks(DEFAULT_CHUNK_SIZE) {
        let epochs: Vec<Decimal> = chunk.iter().map(|r| Decimal::from(r.epoch)).collect();
        let vote_accounts: Vec<&str> = chunk.iter().map(|r| r.vote_account.as_str()).collect();
        let sources: Vec<&str> = vec![snapshot.source.as_str(); chunk.len()];
        let medians: Vec<f64> = chunk.iter().map(|r| r.slot_duration_median_ms).collect();
        let ibrl_scores: Vec<Option<f64>> = chunk.iter().map(|r| r.ibrl_score).collect();
        let updated_ats: Vec<&DateTime<Utc>> = vec![&snapshot_created_at; chunk.len()];
        let created_ats = updated_ats.clone();

        let query = format!(
            "INSERT INTO {VALIDATORS_SLOT_TIMES_TABLE} (
            epoch,
            vote_account,
            source,
            slot_duration_median_ms,
            ibrl_score,
            created_at,
            updated_at
        )
        SELECT * FROM UNNEST(
            $1::NUMERIC[],
            $2::TEXT[],
            $3::TEXT[],
            $4::DOUBLE PRECISION[],
            $5::DOUBLE PRECISION[],
            $6::TIMESTAMP WITH TIME ZONE[],
            $7::TIMESTAMP WITH TIME ZONE[]
        )
        ON CONFLICT (epoch, vote_account, source)
        DO UPDATE SET
            slot_duration_median_ms = EXCLUDED.slot_duration_median_ms,
            ibrl_score = EXCLUDED.ibrl_score,
            updated_at = EXCLUDED.updated_at"
        );

        let rows_affected = tx
            .execute(
                &query,
                &[
                    &epochs,
                    &vote_accounts,
                    &sources,
                    &medians,
                    &ibrl_scores,
                    &created_ats,
                    &updated_ats,
                ],
            )
            .await?;

        total_upserted += rows_affected;

        info!("Upserted {rows_affected} slot time records in this chunk");
    }
    tx.commit().await?;

    info!("Stored slot times snapshot: {total_upserted} total records upserted");

    Ok(())
}
