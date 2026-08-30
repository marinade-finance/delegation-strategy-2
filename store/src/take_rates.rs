use crate::dto::TakeRateRecord;
use crate::utils::{expected_take_rate, worst_known_commission, RewardMixShares};
use chrono::{DateTime, Utc};
use clap::Parser;
use collect::take_rates::ValidatorRewardsSnapshot;
use log::info;
use rust_decimal::prelude::*;
use serde_yaml;
use std::collections::HashMap;
use tokio_postgres::Client;

pub const VALIDATORS_REWARDS_TABLE: &str = "validators_rewards";

const SUPPORTED_DATA_VERSION: u16 = 1;

#[derive(Debug, Parser)]
pub struct StoreTakeRatesParams {
    #[arg(long = "snapshot-file")]
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

    anyhow::ensure!(
        snapshot.version == SUPPORTED_DATA_VERSION,
        "Snapshot take rates file '{path}' has version {}, expected {SUPPORTED_DATA_VERSION}",
        snapshot.version
    );

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

/// Cluster reward mix per epoch. Epochs that paid no inflation are absent: the in-progress one has
/// only accruing block rewards, and nothing else pays out before the epoch closes.
pub async fn load_epoch_reward_mix(
    psql_client: &Client,
) -> anyhow::Result<HashMap<u64, RewardMixShares>> {
    let rows = psql_client
        .query(
            &format!(
                "
        SELECT
            epoch,
            SUM(inflation_rewards) AS inflation,
            SUM(mev_rewards) AS mev,
            SUM(block_rewards) AS block
        FROM {VALIDATORS_REWARDS_TABLE}
        GROUP BY epoch
        HAVING SUM(inflation_rewards) > 0
        "
            ),
            &[],
        )
        .await?;

    let mut mix = HashMap::with_capacity(rows.len());
    for row in rows {
        let epoch: u64 = row.get::<_, Decimal>("epoch").try_into()?;
        let inflation: Decimal = row.get("inflation");
        let mev: Decimal = row.get("mev");
        let block: Decimal = row.get("block");

        let total = (inflation + mev + block).to_f64().unwrap_or_default();

        mix.insert(
            epoch,
            RewardMixShares {
                inflation: inflation.to_f64().unwrap_or_default() / total,
                mev: mev.to_f64().unwrap_or_default() / total,
                block: block.to_f64().unwrap_or_default() / total,
            },
        );
    }

    Ok(mix)
}

pub async fn get_take_rate_series(
    psql_client: &Client,
    vote_account: &str,
    from_epoch: Option<u64>,
    reward_mix: &HashMap<u64, RewardMixShares>,
) -> anyhow::Result<Vec<TakeRateRecord>> {
    let from_epoch = from_epoch.map(Decimal::from);
    let query = format!(
        "
        SELECT
            {VALIDATORS_REWARDS_TABLE}.epoch, take_rate AS realized_take_rate, created_at,
            epochs.start_at AS epoch_start, epochs.end_at AS epoch_end,
            validators.commission_max_observed, validators.commission_advertised,
            mev.mev_commission AS mev_commission_bps,
            jpf.validator_commission AS priority_commission_bps
        FROM {VALIDATORS_REWARDS_TABLE}
        LEFT JOIN epochs ON {VALIDATORS_REWARDS_TABLE}.epoch = epochs.epoch
        LEFT JOIN validators
            ON validators.vote_account = {VALIDATORS_REWARDS_TABLE}.vote_account
            AND validators.epoch = {VALIDATORS_REWARDS_TABLE}.epoch
        -- Both filters stay inside the subqueries; moved to the join they scan every validator's snapshots.
        LEFT JOIN (
            SELECT DISTINCT ON (epoch) epoch, mev_commission
            FROM mev WHERE vote_account = $1 ORDER BY epoch, created_at DESC
        ) mev ON mev.epoch = {VALIDATORS_REWARDS_TABLE}.epoch
        LEFT JOIN (
            SELECT DISTINCT ON (epoch) epoch, validator_commission
            FROM jito_priority_fee WHERE vote_account = $1 ORDER BY epoch, created_at DESC
        ) jpf ON jpf.epoch = {VALIDATORS_REWARDS_TABLE}.epoch
        WHERE {VALIDATORS_REWARDS_TABLE}.vote_account = $1
          AND ($2::NUMERIC IS NULL OR {VALIDATORS_REWARDS_TABLE}.epoch >= $2::NUMERIC)
        ORDER BY {VALIDATORS_REWARDS_TABLE}.epoch ASC
        "
    );
    let rows = psql_client
        .query(&query, &[&vote_account, &from_epoch])
        .await?;

    let mut records = Vec::with_capacity(rows.len());
    for row in rows {
        let epoch: u64 = row.get::<_, Decimal>("epoch").try_into()?;
        // Null where `epochs` has no row: `store close-epoch` has not run for it yet, or a
        // `--from-epoch` backfill reached past the stored epoch history.
        records.push(TakeRateRecord {
            epoch,
            epoch_start_at: row.get::<_, Option<DateTime<Utc>>>("epoch_start"),
            epoch_end_at: row.get::<_, Option<DateTime<Utc>>>("epoch_end"),
            realized_take_rate: row.get("realized_take_rate"),
            expected_take_rate: reward_mix.get(&epoch).and_then(|shares| {
                expected_take_rate(
                    *shares,
                    worst_known_commission(
                        row.get::<_, Option<i32>>("commission_max_observed"),
                        row.get::<_, Option<i32>>("commission_advertised"),
                    ),
                    row.get::<_, Option<i32>>("mev_commission_bps"),
                    row.get::<_, Option<i32>>("priority_commission_bps"),
                )
            }),
            created_at: row.get("created_at"),
        })
    }

    Ok(records)
}
