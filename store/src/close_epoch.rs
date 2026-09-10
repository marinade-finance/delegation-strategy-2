use crate::dto::{COMMISSION_EFFECTIVE_SOURCE_REWARD_ROW, COMMISSION_EFFECTIVE_SOURCE_VOTE_STATE};
use crate::utils::UpdateQueryCombiner;
use chrono::{DateTime, Utc};
use clap::Parser;
use collect::solana_service::bps_to_percent;
use collect::validators_performance::{ClusterInflation, ValidatorsPerformanceSnapshot};
use log::info;
use rust_decimal::prelude::*;
use serde_yaml;
use std::collections::{HashMap, HashSet};
use tokio_postgres::{types::ToSql, Client};

#[derive(Debug, Parser)]
pub struct CloseEpochParams {
    #[arg(long = "snapshot-file")]
    snapshot_path: String,
}

const DEFAULT_CHUNK_SIZE: usize = 500;

// A reward row still wins where one exists, so pre-1031 epochs reprocess to the same values.
fn resolve_commission_effective(
    from_reward_row: Option<u8>,
    sampled_bps: Option<u16>,
) -> (Option<i32>, Option<&'static str>) {
    if let Some(commission) = from_reward_row {
        return (
            Some(i32::from(commission)),
            Some(COMMISSION_EFFECTIVE_SOURCE_REWARD_ROW),
        );
    }
    match sampled_bps {
        Some(bps) => (
            Some(i32::from(bps_to_percent(bps))),
            Some(COMMISSION_EFFECTIVE_SOURCE_VOTE_STATE),
        ),
        None => (None, None),
    }
}

// Written hourly by store validators over the open epoch, so by close this is the last sample taken.
async fn load_sampled_commission_bps(
    psql_client: &Client,
    epoch: &Decimal,
) -> anyhow::Result<HashMap<String, u16>> {
    let rows = psql_client
        .query(
            "SELECT vote_account, inflation_rewards_commission_bps
             FROM validators
             WHERE epoch = $1 AND inflation_rewards_commission_bps IS NOT NULL",
            &[epoch],
        )
        .await?;

    let mut sampled = HashMap::with_capacity(rows.len());
    for row in rows {
        let bps: i32 = row.get("inflation_rewards_commission_bps");
        sampled.insert(row.get("vote_account"), u16::try_from(bps)?);
    }
    Ok(sampled)
}

pub async fn create_epoch_record(
    psql_client: &Client,
    epoch: u64,
    cluster_inflation: ClusterInflation,
    slots_per_year: f64,
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
            inflation_taper,
            slots_per_year
        ) SELECT
            $1,
            COALESCE(previous_epoch.end_at, epoch_cluster_info.start_at) start_at,
            epoch_cluster_info.end_at,
            transaction_count,
            $2,
            $3,
            $4,
            $5
        FROM epoch_cluster_info, previous_epoch
    ",
            &[
                &Decimal::from(epoch),
                &Decimal::from(cluster_inflation.sol_total_supply),
                &cluster_inflation.inflation,
                &cluster_inflation.inflation_taper,
                &slots_per_year,
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
    commission_effective_source: Option<&'static str>,
    credits: Decimal,
    leader_slots: Decimal,
    blocks_produced: Decimal,
    skip_rate: f64,
    updated_at: DateTime<Utc>,
}

pub async fn close_epoch(
    epoch_params: CloseEpochParams,
    psql_client: &mut Client,
) -> anyhow::Result<()> {
    info!("Finalizing validators snapshot...");

    let snapshot_file = std::fs::File::open(epoch_params.snapshot_path)?;
    let snapshot: ValidatorsPerformanceSnapshot = serde_yaml::from_reader(snapshot_file)?;
    let snapshot_created_at: DateTime<Utc> = snapshot.created_at.parse().unwrap();
    let snapshot_epoch: Decimal = snapshot.epoch.into();
    let rewards = snapshot.rewards.unwrap();

    create_epoch_record(
        psql_client,
        snapshot.epoch,
        snapshot.cluster_inflation.unwrap(),
        snapshot.slots_per_year,
    )
    .await?;

    let mut updated_identities: HashSet<_> = Default::default();

    info!("Loaded the snapshot");

    let sampled_commission_bps = load_sampled_commission_bps(psql_client, &snapshot_epoch).await?;

    let validator_update_records: Vec<_> = snapshot
        .validators
        .iter()
        .map(|(vote_account, v)| {
            let (commission_effective, commission_effective_source) = resolve_commission_effective(
                rewards
                    .get(vote_account)
                    .and_then(|r| r.commission_effective),
                sampled_commission_bps.get(vote_account).copied(),
            );
            ValidatorUpdateRecord {
                vote_account: vote_account.clone(),
                epoch: snapshot_epoch,
                commission_effective,
                commission_effective_source,
                credits: v.credits.into(),
                leader_slots: v.leader_slots.into(),
                blocks_produced: v.blocks_produced.into(),
                skip_rate: v.skip_rate,
                updated_at: snapshot_created_at,
            }
        })
        .collect();

    let from_vote_state = validator_update_records
        .iter()
        .filter(|record| {
            record.commission_effective_source == Some(COMMISSION_EFFECTIVE_SOURCE_VOTE_STATE)
        })
        .count();
    let unresolved = validator_update_records
        .iter()
        .filter(|record| record.commission_effective_source.is_none())
        .count();
    info!(
        "Effective commission for {} validators: {} from a reward row, {from_vote_state} from sampled vote state, {unresolved} unresolved",
        validator_update_records.len(),
        validator_update_records.len() - from_vote_state - unresolved
    );

    for chunk in validator_update_records.chunks(DEFAULT_CHUNK_SIZE) {
        let mut query = UpdateQueryCombiner::new(
            "validators".to_string(),
            "
            commission_effective = u.commission_effective,
            commission_effective_source = u.commission_effective_source,
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
                commission_effective_source,
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
                &v.commission_effective_source,
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
                    (3, "TEXT".into()),                     // commission_effective_source
                    (4, "NUMERIC".into()),                  // credits
                    (5, "NUMERIC".into()),                  // leader_slots
                    (6, "NUMERIC".into()),                  // blocks_produced
                    (7, "DOUBLE PRECISION".into()),         // skip_rate
                    (8, "TIMESTAMP WITH TIME ZONE".into()), // updated_at
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reward_row_still_wins_so_closed_epochs_reprocess_unchanged() {
        assert_eq!(
            resolve_commission_effective(Some(7), Some(300)),
            (Some(7), Some(COMMISSION_EFFECTIVE_SOURCE_REWARD_ROW))
        );
    }

    #[test]
    fn a_missing_reward_row_falls_back_to_the_sampled_vote_state() {
        assert_eq!(
            resolve_commission_effective(None, Some(700)),
            (Some(7), Some(COMMISSION_EFFECTIVE_SOURCE_VOTE_STATE))
        );
    }

    // Same div_ceil rule as every other projection of these basis points, so a validator just over
    // the 10% eligibility cap cannot round down onto it.
    #[test]
    fn the_fallback_projects_basis_points_the_way_agave_does() {
        assert_eq!(resolve_commission_effective(None, Some(1_001)).0, Some(11));
        assert_eq!(resolve_commission_effective(None, Some(1_000)).0, Some(10));
        assert_eq!(
            resolve_commission_effective(None, Some(25_600)).0,
            Some(100)
        );
    }

    #[test]
    fn neither_source_leaves_the_rate_unknown_rather_than_zero() {
        assert_eq!(resolve_commission_effective(None, None), (None, None));
    }

    #[test]
    fn a_genuine_zero_is_resolved_not_missing() {
        assert_eq!(
            resolve_commission_effective(None, Some(0)),
            (Some(0), Some(COMMISSION_EFFECTIVE_SOURCE_VOTE_STATE))
        );
        assert_eq!(
            resolve_commission_effective(Some(0), None),
            (Some(0), Some(COMMISSION_EFFECTIVE_SOURCE_REWARD_ROW))
        );
    }
}
