use crate::utils::UpdateQueryCombiner;
use chrono::{DateTime, Utc};
use collect::validators_performance::{ClusterInflation, ValidatorsPerformanceSnapshot};
use log::info;
use rust_decimal::prelude::*;
use serde_yaml;
use std::collections::{HashMap, HashSet};
use structopt::StructOpt;
use tokio_postgres::{types::ToSql, Client};

#[derive(Debug, StructOpt)]
pub struct CloseEpochOptions {
    #[structopt(long = "snapshot-file")]
    snapshot_path: String,
}

const DEFAULT_CHUNK_SIZE: usize = 500;

pub async fn create_epoch_record(
    psql_client: &Client,
    epoch: u64,
    cluster_inflation: ClusterInflation,
) -> anyhow::Result<()> {
    psql_client
        .execute(
            "
        WITH
            epoch_cluster_info AS (
                SELECT
                    MAX(transaction_count) - MIN(transaction_count) transaction_count,
                    MIN(created_at) AS start_at,
                    MAX(created_at) AS end_at
                FROM cluster_info
                WHERE epoch = $1
            ),
            previous_epoch AS (
                SELECT
                    MAX(end_at) end_at
                FROM epochs
                WHERE epoch = $1 - 1
            )
        INSERT INTO epochs (
            epoch,
            start_at,
            end_at,
            transaction_count,
            supply,
            inflation,
            inflation_taper
        ) SELECT
            $1,
            COALESCE(previous_epoch.end_at, epoch_cluster_info.start_at) start_at,
            epoch_cluster_info.end_at,
            transaction_count,
            $2,
            $3,
            $4
        FROM epoch_cluster_info, previous_epoch
    ",
            &[
                &Decimal::from(epoch),
                &Decimal::from(cluster_inflation.sol_total_supply),
                &cluster_inflation.inflation,
                &cluster_inflation.inflation_taper,
            ],
        )
        .await?;

    Ok(())
}

pub async fn update_observed_commission(psql_client: &Client, epoch: u64) -> anyhow::Result<()> {
    psql_client
            .execute("
                WITH grouped_commissions AS (
                    WITH
                        commissions AS (SELECT vote_account, MIN(commission) AS commission_min, MAX(commission) AS commission_max FROM commissions WHERE epoch = $1 GROUP BY vote_account)
                    SELECT
                        commissions.commission_min,
                        commissions.commission_max,
                        validators.vote_account
                    FROM
                        validators
                        LEFT JOIN commissions ON validators.vote_account = commissions.vote_account
                    WHERE validators.epoch = $1
                )
                UPDATE validators
                SET
                    commission_max_observed = GREATEST(commission_max, commission_advertised, commission_effective),
                    commission_min_observed = LEAST(commission_min, commission_advertised, commission_effective)
                FROM grouped_commissions
                WHERE grouped_commissions.vote_account = validators.vote_account AND validators.epoch = $1
                "
,
        &[
            &Decimal::from(epoch),
        ],
    )
    .await?;

    Ok(())
}

pub async fn update_uptimes(psql_client: &Client, epoch: u64) -> anyhow::Result<()> {
    psql_client
            .execute("
                WITH uptimes AS (
                    WITH
                        vars AS (SELECT epoch, end_at - start_at AS epoch_duration FROM epochs WHERE epoch = $1),
                        downtimes AS (SELECT vote_account, SUM(end_at - start_at) AS downtime FROM uptimes WHERE epoch = $1 AND status = 'DOWN' GROUP BY vote_account)
                    SELECT
                        LEAST(GREATEST(COALESCE(1 - EXTRACT('epoch' FROM downtimes.downtime) / EXTRACT('epoch' FROM vars.epoch_duration), 1), 0), 1) uptime_pct,
                        EXTRACT('epoch' FROM GREATEST(COALESCE(vars.epoch_duration - downtimes.downtime, vars.epoch_duration), '0 seconds')) uptime,
                        EXTRACT('epoch' FROM COALESCE(downtimes.downtime, '0 seconds')) downtime,
                        validators.vote_account,
                        vars.epoch
                    FROM
                        validators
                        INNER JOIN vars ON validators.epoch = vars.epoch
                        LEFT JOIN downtimes ON validators.vote_account = downtimes.vote_account
                    WHERE validators.epoch = $1
                )
                UPDATE validators
                SET uptime_pct = uptimes.uptime_pct, uptime = uptimes.uptime, downtime = uptimes.downtime
                FROM uptimes
                WHERE uptimes.vote_account = validators.vote_account AND uptimes.epoch = validators.epoch
                "
,
        &[
            &Decimal::from(epoch),
        ],
    )
    .await?;

    Ok(())
}

struct ValidatorUpdateRecord {
    vote_account: String,
    epoch: Decimal,
    commission_effective: Option<i32>,
    credits: Decimal,
    leader_slots: Decimal,
    blocks_produced: Decimal,
    skip_rate: f64,
    updated_at: DateTime<Utc>,
}

pub async fn close_epoch(
    options: CloseEpochOptions,
    psql_client: &mut Client,
) -> anyhow::Result<()> {
    info!("Finalizing validators snapshot...");

    let snapshot_file = std::fs::File::open(options.snapshot_path)?;
    let snapshot: ValidatorsPerformanceSnapshot = serde_yaml::from_reader(snapshot_file)?;
    let snapshot_created_at = snapshot.created_at.parse::<DateTime<Utc>>().unwrap();
    let snapshot_epoch: Decimal = snapshot.epoch.into();
    let rewards = snapshot.rewards.unwrap();

    create_epoch_record(
        psql_client,
        snapshot.epoch,
        snapshot.cluster_inflation.unwrap(),
    )
    .await?;

    let mut updated_identities: HashSet<_> = Default::default();

    info!("Loaded the snapshot");

    let validator_update_records: Vec<_> = snapshot
        .validators
        .iter()
        .map(|(vote_account, v)| ValidatorUpdateRecord {
            vote_account: vote_account.clone(),
            epoch: snapshot_epoch,
            commission_effective: rewards
                .get(vote_account)
                .and_then(|r| r.commission_effective.map(|c| c as i32)),
            credits: v.credits.into(),
            leader_slots: v.leader_slots.into(),
            blocks_produced: v.blocks_produced.into(),
            skip_rate: v.skip_rate,
            updated_at: snapshot_created_at,
        })
        .collect();

    for chunk in validator_update_records.chunks(DEFAULT_CHUNK_SIZE) {
        let mut query = UpdateQueryCombiner::new(
            "validators".to_string(),
            "
            commission_effective = u.commission_effective,
            credits = u.credits,
            leader_slots = u.leader_slots,
            blocks_produced = u.blocks_produced,
            skip_rate = u.skip_rate,
            updated_at = u.updated_at
            "
            .to_string(),
            "u(
                vote_account,
                epoch,
                commission_effective,
                credits,
                leader_slots,
                blocks_produced,
                skip_rate,
                updated_at
            )"
            .to_string(),
            "validators.vote_account = u.vote_account AND validators.epoch = u.epoch".to_string(),
        );
        for v in chunk {
            let mut params: Vec<&(dyn ToSql + Sync)> = vec![
                &v.vote_account,
                &v.epoch,
                &v.commission_effective,
                &v.credits,
                &v.leader_slots,
                &v.blocks_produced,
                &v.skip_rate,
                &v.updated_at,
            ];
            query.add(
                &mut params,
                HashMap::from_iter([
                    (1, "NUMERIC".into()),                  // epoch
                    (2, "INTEGER".into()),                  // commission_effective
                    (3, "NUMERIC".into()),                  // credits
                    (4, "NUMERIC".into()),                  // leader_slots
                    (5, "NUMERIC".into()),                  // blocks_produced
                    (6, "DOUBLE PRECISION".into()),         // skip_rate
                    (7, "TIMESTAMP WITH TIME ZONE".into()), // updated_at
                ]),
            );
            updated_identities.insert(v.vote_account.clone());
        }
        query.execute(psql_client).await?;
        info!(
            "Updated previously existing validator records: {}",
            updated_identities.len()
        );
    }

    update_uptimes(psql_client, snapshot.epoch).await?;
    update_observed_commission(psql_client, snapshot.epoch).await?;

    Ok(())
}
