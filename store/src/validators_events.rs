use crate::dto::{EventEpochRecord, PerformanceRecord, SettlementRecord};
use chrono::{DateTime, Utc};
use collect::validators_events::ValidatorsEventsSnapshot;
use log::info;
use rust_decimal::prelude::*;
use serde_yaml;
use std::collections::HashMap;
use tokio_postgres::Client;

pub const VALIDATORS_EVENTS_TABLE: &str = "validators_events";

#[derive(Debug, structopt::StructOpt)]
pub struct StoreEventsParams {
    #[structopt(long = "snapshot-file")]
    snapshot_path: String,
}

const DEFAULT_CHUNK_SIZE: usize = 500;

pub async fn store_events(
    params: StoreEventsParams,
    psql_client: &mut Client,
) -> anyhow::Result<()> {
    info!("Storing events (PSR settlements) snapshot...");

    let path = params.snapshot_path;
    let snapshot_file = std::fs::File::open(&path)
        .map_err(|e| anyhow::anyhow!("Failed to open snapshot events file '{path}': {e}"))?;
    let snapshot: ValidatorsEventsSnapshot = serde_yaml::from_reader(snapshot_file)
        .map_err(|e| anyhow::anyhow!("Failed to parse snapshot events file '{path}': {e}"))?;

    let snapshot_created_at: DateTime<Utc> = snapshot.created_at.parse()?;

    info!(
        "Loaded the events snapshot from epoch {}. Snapshot created at {} loaded at epoch {}, slot index {}. {} records.",
        snapshot.from_epoch,
        snapshot_created_at,
        snapshot.loaded_at_epoch,
        snapshot.loaded_at_slot_index,
        snapshot.events.len()
    );

    let mut total_upserted = 0;

    for chunk in snapshot.events.chunks(DEFAULT_CHUNK_SIZE) {
        let epochs: Vec<Decimal> = chunk.iter().map(|r| Decimal::from(r.epoch)).collect();
        let vote_accounts: Vec<&str> = chunk.iter().map(|r| r.vote_account.as_str()).collect();
        let reasons: Vec<&str> = chunk.iter().map(|r| r.reason.as_str()).collect();
        let metas: Vec<&str> = chunk.iter().map(|r| r.meta.as_str()).collect();
        let amounts: Vec<Decimal> = chunk.iter().map(|r| Decimal::from(r.amount)).collect();
        let updated_ats: Vec<&DateTime<Utc>> = vec![&snapshot_created_at; chunk.len()];
        let created_ats = updated_ats.clone();

        let query = format!(
            "INSERT INTO {VALIDATORS_EVENTS_TABLE} (
            epoch,
            vote_account,
            reason,
            meta,
            amount,
            created_at,
            updated_at
        )
        SELECT * FROM UNNEST(
            $1::NUMERIC[],
            $2::TEXT[],
            $3::TEXT[],
            $4::TEXT[],
            $5::NUMERIC[],
            $6::TIMESTAMP WITH TIME ZONE[],
            $7::TIMESTAMP WITH TIME ZONE[]
        )
        ON CONFLICT (epoch, vote_account, reason, meta)
        DO UPDATE SET
            amount = EXCLUDED.amount,
            updated_at = EXCLUDED.updated_at"
        );

        let rows_affected = psql_client
            .execute(
                &query,
                &[
                    &epochs,
                    &vote_accounts,
                    &reasons,
                    &metas,
                    &amounts,
                    &created_ats,
                    &updated_ats,
                ],
            )
            .await?;

        total_upserted += rows_affected;

        info!("Upserted {rows_affected} events records in this chunk");
    }

    info!("Stored events snapshot: {total_upserted} total records upserted");

    Ok(())
}

pub const DEFAULT_EVENTS_WINDOW_EPOCHS: u64 = 80;

/// `from = true` -> smallest epoch ending on/after `date`; else largest ending on/before.
pub async fn resolve_epoch_for_date(
    psql_client: &Client,
    date: DateTime<Utc>,
    from: bool,
) -> anyhow::Result<Option<u64>> {
    let (cmp, order) = if from { (">=", "ASC") } else { ("<=", "DESC") };
    let query = format!(
        "SELECT epoch FROM epochs WHERE end_at {cmp} $1 ORDER BY epoch {order} LIMIT 1"
    );
    let row = psql_client.query_opt(&query, &[&date]).await?;
    match row {
        Some(row) => Ok(Some(row.get::<_, Decimal>("epoch").try_into()?)),
        None => Ok(None),
    }
}

pub async fn get_events_with_context(
    psql_client: &Client,
    vote_account: &str,
    from_epoch: Option<u64>,
) -> anyhow::Result<Vec<EventEpochRecord>> {
    let from_epoch = match from_epoch {
        Some(from_epoch) => from_epoch,
        None => {
            let last_epoch: Option<Decimal> = psql_client
                .query_one("SELECT MAX(epoch) AS last_epoch FROM cluster_info", &[])
                .await?
                .get("last_epoch");
            last_epoch
                .and_then(|e| e.to_u64())
                .unwrap_or(0)
                .saturating_sub(DEFAULT_EVENTS_WINDOW_EPOCHS)
        }
    };
    let from_epoch = Decimal::from(from_epoch);

    let settlement_rows = psql_client
        .query(
            "SELECT epoch, reason, meta, amount
             FROM validators_events
             WHERE vote_account = $1 AND epoch >= $2::NUMERIC
             ORDER BY epoch ASC",
            &[&vote_account, &from_epoch],
        )
        .await?;

    let mut settlements_by_epoch: HashMap<u64, Vec<SettlementRecord>> = Default::default();
    for row in settlement_rows {
        let epoch: u64 = row.get::<_, Decimal>("epoch").try_into()?;
        settlements_by_epoch
            .entry(epoch)
            .or_default()
            .push(SettlementRecord {
                reason: row.get("reason"),
                meta: row.get("meta"),
                amount: row.get::<_, Decimal>("amount"),
            });
    }

    let perf_rows = psql_client
        .query(
            "SELECT
                validators.epoch,
                epochs.end_at AS epoch_end,
                blocks_produced,
                leader_slots,
                skip_rate,
                credits,
                uptime_pct,
                downtime
            FROM validators
            LEFT JOIN epochs ON validators.epoch = epochs.epoch
            WHERE validators.vote_account = $1 AND validators.epoch >= $2::NUMERIC
            ORDER BY validators.epoch ASC",
            &[&vote_account, &from_epoch],
        )
        .await?;

    let mut records = Vec::new();
    for row in perf_rows {
        let epoch: u64 = row.get::<_, Decimal>("epoch").try_into()?;
        records.push(EventEpochRecord {
            epoch,
            epoch_end_at: row.get::<_, Option<DateTime<Utc>>>("epoch_end"),
            performance: PerformanceRecord {
                blocks_produced: row.get::<_, Decimal>("blocks_produced").try_into()?,
                leader_slots: row.get::<_, Decimal>("leader_slots").try_into()?,
                skip_rate: row.get("skip_rate"),
                credits: row.get::<_, Decimal>("credits").try_into()?,
            },
            uptime_pct: row.get("uptime_pct"),
            downtime: row
                .get::<_, Option<Decimal>>("downtime")
                .map(|n| n.try_into())
                .transpose()?,
            settlements: settlements_by_epoch.remove(&epoch).unwrap_or_default(),
        });
    }

    Ok(records)
}
