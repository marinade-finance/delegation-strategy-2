use crate::dto::{COMMISSION_EFFECTIVE_SOURCE_REWARD_ROW, COMMISSION_EFFECTIVE_SOURCE_VOTE_STATE};
use crate::utils::UpdateQueryCombiner;
use chrono::{DateTime, Utc};
use clap::Parser;
use collect::solana_service::bps_to_percent;
use collect::validators_performance::{ClusterInflation, ValidatorsPerformanceSnapshot};
use log::{info, warn};
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

#[derive(Clone, Copy, Debug, PartialEq)]
enum SampledCommission {
    Bps(u16),
    Percent(u8),
}

// A reward row still wins where one exists, so pre-1030 epochs reprocess to the same values.
fn resolve_commission_effective(
    from_reward_row: Option<u8>,
    sampled: Option<SampledCommission>,
) -> (Option<i32>, Option<i32>, Option<&'static str>) {
    if let Some(commission) = from_reward_row {
        return (
            Some(i32::from(commission)),
            None,
            Some(COMMISSION_EFFECTIVE_SOURCE_REWARD_ROW),
        );
    }
    match sampled {
        Some(SampledCommission::Bps(bps)) => (
            Some(i32::from(bps_to_percent(bps))),
            Some(i32::from(bps)),
            Some(COMMISSION_EFFECTIVE_SOURCE_VOTE_STATE),
        ),
        Some(SampledCommission::Percent(percent)) => (
            Some(i32::from(percent)),
            None,
            Some(COMMISSION_EFFECTIVE_SOURCE_VOTE_STATE),
        ),
        None => (None, None, None),
    }
}

// Agave's order for epoch E: epoch_stakes(E) frozen at the close of E-2, then the close of E-1, then live.
// An unparsed vote state keeps its row's vintage through the advertised percent, as agave falls back only on absence.
async fn load_sampled_commission(
    psql_client: &Client,
    epoch: &Decimal,
) -> anyhow::Result<HashMap<String, SampledCommission>> {
    let rows = psql_client
        .query(
            "SELECT DISTINCT ON (vote_account)
                 vote_account, inflation_rewards_commission_bps, commission_advertised
             FROM validators
             WHERE epoch BETWEEN $1::NUMERIC - 2 AND $1::NUMERIC
               AND (inflation_rewards_commission_bps IS NOT NULL OR commission_advertised IS NOT NULL)
             ORDER BY vote_account, epoch",
            &[epoch],
        )
        .await?;

    let mut sampled = HashMap::with_capacity(rows.len());
    for row in rows {
        let commission = match row.get::<_, Option<i32>>("inflation_rewards_commission_bps") {
            Some(bps) => SampledCommission::Bps(u16::try_from(bps)?),
            None => SampledCommission::Percent(u8::try_from(
                row.get::<_, i32>("commission_advertised"),
            )?),
        };
        sampled.insert(row.get("vote_account"), commission);
    }
    Ok(sampled)
}

// A row the snapshot never listed needs its own write, and only where nothing resolved the rate yet
async fn store_commission_outside_the_snapshot(
    psql_client: &Client,
    epoch: &Decimal,
    vote_accounts: &[&str],
    rates: &[i32],
    bps: &[Option<i32>],
    updated_at: &DateTime<Utc>,
) -> anyhow::Result<u64> {
    let updated = psql_client
        .execute(
            "UPDATE validators
             SET commission_effective = u.commission_effective,
                 commission_effective_bps = u.commission_effective_bps,
                 commission_effective_source = $4,
                 updated_at = $6
             FROM UNNEST($1::TEXT[], $2::INTEGER[], $3::INTEGER[])
                 AS u(vote_account, commission_effective, commission_effective_bps)
             WHERE validators.vote_account = u.vote_account AND validators.epoch = $5
               AND validators.commission_effective IS NULL",
            &[
                &vote_accounts,
                &rates,
                &bps,
                &COMMISSION_EFFECTIVE_SOURCE_VOTE_STATE,
                epoch,
                updated_at,
            ],
        )
        .await?;
    Ok(updated)
}

// The in-memory tally sees only snapshot validators, so re-count in the DB.
async fn warn_on_unresolved_commission(psql_client: &Client, epoch: u64) -> anyhow::Result<()> {
    let row = psql_client
        .query_one(
            "SELECT COUNT(*) FROM validators WHERE epoch = $1 AND commission_effective IS NULL",
            &[&Decimal::from(epoch)],
        )
        .await?;
    let unresolved: i64 = row.get(0);
    if unresolved > 0 {
        warn!("Epoch {epoch} closed with {unresolved} validator rows still without commission_effective");
    }
    Ok(())
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
    commission_effective_bps: Option<i32>,
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

    let sampled_commission = load_sampled_commission(psql_client, &snapshot_epoch).await?;

    let validator_update_records: Vec<_> = snapshot
        .validators
        .iter()
        .map(|(vote_account, v)| {
            let (commission_effective, commission_effective_bps, commission_effective_source) =
                resolve_commission_effective(
                    rewards
                        .get(vote_account)
                        .and_then(|r| r.commission_effective),
                    sampled_commission.get(vote_account).copied(),
                );
            ValidatorUpdateRecord {
                vote_account: vote_account.clone(),
                epoch: snapshot_epoch,
                commission_effective,
                commission_effective_source,
                commission_effective_bps,
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
            commission_effective_bps = u.commission_effective_bps,
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
                commission_effective_bps,
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
                &v.commission_effective_bps,
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
                    (4, "INTEGER".into()),                  // commission_effective_bps
                    (5, "NUMERIC".into()),                  // credits
                    (6, "NUMERIC".into()),                  // leader_slots
                    (7, "NUMERIC".into()),                  // blocks_produced
                    (8, "DOUBLE PRECISION".into()),         // skip_rate
                    (9, "TIMESTAMP WITH TIME ZONE".into()), // updated_at
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

    let mut outside_vote_accounts: Vec<&str> = vec![];
    let mut outside_rates: Vec<i32> = vec![];
    let mut outside_bps: Vec<Option<i32>> = vec![];
    for (vote_account, sampled) in sampled_commission
        .iter()
        .filter(|(vote_account, _)| !snapshot.validators.contains_key(*vote_account))
    {
        let (rate, bps, _) = resolve_commission_effective(None, Some(*sampled));
        outside_vote_accounts.push(vote_account.as_str());
        outside_rates.extend(rate);
        outside_bps.push(bps);
    }
    // A closed epoch is never re-listed, so the floor below must land even when this write fails.
    if !outside_vote_accounts.is_empty() {
        match store_commission_outside_the_snapshot(
            psql_client,
            &snapshot_epoch,
            &outside_vote_accounts,
            &outside_rates,
            &outside_bps,
            &snapshot_created_at,
        )
        .await
        {
            Ok(updated) => info!(
                "Effective commission from sampled vote state for {updated} validators the snapshot did not list"
            ),
            Err(err) => warn!(
                "Could not store effective commission for validators outside the snapshot: {err}"
            ),
        }
    }

    update_uptimes(psql_client, snapshot.epoch).await?;
    update_observed_commission(psql_client, snapshot.epoch).await?;
    if let Err(err) = warn_on_unresolved_commission(psql_client, snapshot.epoch).await {
        warn!("Could not count validator rows without commission_effective: {err}");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reward_row_still_wins_so_closed_epochs_reprocess_unchanged() {
        assert_eq!(
            resolve_commission_effective(Some(7), Some(SampledCommission::Bps(300))),
            (Some(7), None, Some(COMMISSION_EFFECTIVE_SOURCE_REWARD_ROW))
        );
    }

    #[test]
    fn a_missing_reward_row_falls_back_to_the_sampled_vote_state() {
        assert_eq!(
            resolve_commission_effective(None, Some(SampledCommission::Bps(700))),
            (
                Some(7),
                Some(700),
                Some(COMMISSION_EFFECTIVE_SOURCE_VOTE_STATE)
            )
        );
    }

    #[test]
    fn the_fallback_rounds_basis_points_up_so_the_eligibility_cap_stays_strict() {
        assert_eq!(
            resolve_commission_effective(None, Some(SampledCommission::Bps(1_001))).0,
            Some(11)
        );
        assert_eq!(
            resolve_commission_effective(None, Some(SampledCommission::Bps(1_000))).0,
            Some(10)
        );
        assert_eq!(
            resolve_commission_effective(None, Some(SampledCommission::Bps(1_001))).1,
            Some(1_001)
        );
        assert_eq!(
            resolve_commission_effective(None, Some(SampledCommission::Bps(25_600))).0,
            Some(100)
        );
    }

    #[test]
    fn an_unparsed_vote_state_resolves_from_its_advertised_percent_without_bps() {
        assert_eq!(
            resolve_commission_effective(None, Some(SampledCommission::Percent(7))),
            (Some(7), None, Some(COMMISSION_EFFECTIVE_SOURCE_VOTE_STATE))
        );
    }

    #[test]
    fn neither_source_leaves_the_rate_unknown_rather_than_zero() {
        assert_eq!(resolve_commission_effective(None, None), (None, None, None));
    }

    #[test]
    fn a_genuine_zero_is_resolved_not_missing() {
        assert_eq!(
            resolve_commission_effective(None, Some(SampledCommission::Bps(0))),
            (
                Some(0),
                Some(0),
                Some(COMMISSION_EFFECTIVE_SOURCE_VOTE_STATE)
            )
        );
        assert_eq!(
            resolve_commission_effective(Some(0), None),
            (Some(0), None, Some(COMMISSION_EFFECTIVE_SOURCE_REWARD_ROW))
        );
    }
}
