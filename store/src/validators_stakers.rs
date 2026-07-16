use crate::dto::StakersRecord;
use chrono::{DateTime, Utc};
use collect::validators_stakers::ValidatorsStakersSnapshot;
use log::info;
use rust_decimal::prelude::*;
use serde_yaml;
use std::collections::HashMap;
use structopt::StructOpt;
use tokio_postgres::Client;

pub const VALIDATORS_STAKERS_TABLE: &str = "validators_stakers";

#[derive(Debug, StructOpt)]
pub struct StoreStakersParams {
    #[structopt(long = "snapshot-file")]
    snapshot_path: String,
}

const DEFAULT_CHUNK_SIZE: usize = 500;

pub async fn store_stakers(
    params: StoreStakersParams,
    psql_client: &mut Client,
) -> anyhow::Result<()> {
    info!("Storing stakers snapshot...");

    let path = params.snapshot_path;
    let snapshot_file = std::fs::File::open(&path)
        .map_err(|e| anyhow::anyhow!("Failed to open snapshot stakers file '{path}': {e}"))?;
    let snapshot: ValidatorsStakersSnapshot = serde_yaml::from_reader(snapshot_file)
        .map_err(|e| anyhow::anyhow!("Failed to parse snapshot stakers file '{path}': {e}"))?;

    let snapshot_created_at: DateTime<Utc> = snapshot.created_at.parse()?;

    info!(
        "Loaded the stakers snapshot from epoch {}. Snapshot created at {} loaded at epoch {}, slot index {}. {} records.",
        snapshot.from_epoch,
        snapshot_created_at,
        snapshot.loaded_at_epoch,
        snapshot.loaded_at_slot_index,
        snapshot.stakers.len()
    );

    let mut total_upserted = 0;

    for chunk in snapshot.stakers.chunks(DEFAULT_CHUNK_SIZE) {
        let vote_accounts: Vec<&str> = chunk.iter().map(|r| r.vote_account.as_str()).collect();
        let unique_stakers: Vec<Decimal> =
            chunk.iter().map(|r| Decimal::from(r.unique_stakers)).collect();
        let active_stakes: Vec<Decimal> =
            chunk.iter().map(|r| Decimal::from(r.active_stake)).collect();
        let epochs: Vec<Decimal> = chunk.iter().map(|r| Decimal::from(r.epoch)).collect();
        let updated_ats: Vec<&DateTime<Utc>> = vec![&snapshot_created_at; chunk.len()];
        let created_ats = updated_ats.clone();

        let query = format!(
            "INSERT INTO {VALIDATORS_STAKERS_TABLE} (
            vote_account,
            unique_stakers,
            active_stake,
            epoch,
            created_at,
            updated_at
        )
        SELECT * FROM UNNEST(
            $1::TEXT[],
            $2::NUMERIC[],
            $3::NUMERIC[],
            $4::NUMERIC[],
            $5::TIMESTAMP WITH TIME ZONE[],
            $6::TIMESTAMP WITH TIME ZONE[]
        )
        ON CONFLICT (epoch, vote_account)
        DO UPDATE SET
            unique_stakers = EXCLUDED.unique_stakers,
            active_stake = EXCLUDED.active_stake,
            updated_at = EXCLUDED.updated_at"
        );

        let rows_affected = psql_client
            .execute(
                &query,
                &[
                    &vote_accounts,
                    &unique_stakers,
                    &active_stakes,
                    &epochs,
                    &created_ats,
                    &updated_ats,
                ],
            )
            .await?;

        total_upserted += rows_affected;

        info!("Upserted {rows_affected} stakers records in this chunk");
    }

    info!("Stored stakers snapshot: {total_upserted} total records upserted");

    Ok(())
}

pub async fn load_stakers(
    psql_client: &Client,
    epochs: u64,
) -> anyhow::Result<HashMap<String, Vec<StakersRecord>>> {
    let rows = psql_client
        .query(
            "
            WITH cluster AS (
                SELECT MAX(epoch) AS last_epoch
                FROM cluster_info
            )
            SELECT
                vote_account,
                unique_stakers,
                active_stake,
                validators_stakers.epoch,
                epochs.end_at AS epoch_end
            FROM validators_stakers
            LEFT JOIN epochs ON validators_stakers.epoch = epochs.epoch
            CROSS JOIN cluster
            WHERE validators_stakers.epoch > cluster.last_epoch - $1::NUMERIC",
            &[&Decimal::from(epochs)],
        )
        .await?;

    let mut records: HashMap<_, Vec<_>> = Default::default();
    for row in rows {
        let vote_account: String = row.get("vote_account");
        let stakers = records
            .entry(vote_account.clone())
            .or_insert(Default::default());
        stakers.push(StakersRecord {
            epoch: row.get::<_, Decimal>("epoch").try_into()?,
            epoch_end_at: row.get::<_, Option<DateTime<Utc>>>("epoch_end"),
            unique_stakers: row.get::<_, Decimal>("unique_stakers").try_into()?,
            active_stake: row.get::<_, Decimal>("active_stake"),
        })
    }

    Ok(records)
}
