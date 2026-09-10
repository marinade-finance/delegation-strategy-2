use crate::docs::UptimeStatus;
use crate::dto::{
    client_label, client_lineage, client_name, client_vendor, effective_client_id,
    BlockProductionStats, ClientDiversityStats, ClientLineageStats, ClusterStats, CommissionRecord,
    DCConcentrationStats, FeatureSetStats, IncidentRecord, RugInfo, RuggerRecord, ScoringRunRecord,
    UptimeRecord, Validator, ValidatorAggregatedFlat, ValidatorEpochStats, ValidatorRecord,
    ValidatorScoreRecord, ValidatorScoringCsvRow, ValidatorWarning, ValidatorsAggregated,
    VersionRecord,
};
use crate::validators_jito::get_last_jito_info;
use crate::warehouse::Warehouse;
use chrono::{DateTime, Utc};
use google_cloud_bigquery::client::{Client as BqClient, ClientConfig as BqClientConfig};
use google_cloud_bigquery::http::job::query::QueryRequest;
use google_cloud_bigquery::query::row::Row;
use rust_decimal::prelude::*;
use std::{
    collections::{HashMap, HashSet},
    ops::RangeInclusive,
    time::Duration,
};
use tokio_postgres::{types::ToSql, Client, GenericClient};

/// Default number of recent epochs the API loads/serves (validators, uptimes, events, ...).
pub const DEFAULT_CACHE_EPOCHS: u64 = 80;

/// Agave's year: the same one every `slots_per_year` row annualises to, so nominal and measured stay comparable.
const SECONDS_IN_YEAR: f64 = 31556925.9936;
pub use collect::slot_params::SLOTS_IN_EPOCH;
/// Timeout for outbound HTTP calls to sibling services (scoring, validator-bonds). Without it a
/// hung upstream would stall the whole cache-warmer loop, freezing every cache type's refresh.
const HTTP_TIMEOUT_S: u64 = 60;

pub struct InsertQueryCombiner<'a> {
    pub insertions: u64,
    statement: String,
    params: Vec<&'a (dyn ToSql + Sync)>,
}

pub fn to_fixed(a: f64, decimals: i32) -> u64 {
    (a * 10f64.powi(decimals)).round() as u64
}

// Guarding the scaled value, not the input: the multiplication is what overflows, and the u64 cast then saturates to either end and reads as a genuine rank.
pub fn to_fixed_for_sort(a: f64) -> Option<u64> {
    let scaled = (a * 10f64.powi(4)).round();
    (scaled >= 0.0 && scaled < u64::MAX as f64).then_some(scaled as u64)
}

impl<'a> InsertQueryCombiner<'a> {
    pub fn new(table_name: String, columns: String) -> Self {
        Self {
            insertions: 0,
            statement: format!("INSERT INTO {table_name} ({columns}) VALUES").to_string(),
            params: vec![],
        }
    }

    pub fn add(&mut self, values: &mut Vec<&'a (dyn ToSql + Sync)>) {
        let separator = if self.insertions == 0 { " " } else { "," };
        let mut query_end = "(".to_string();
        for i in 0..values.len() {
            if i > 0 {
                query_end.push(',');
            }
            query_end.push_str(&format!("${}", i + 1 + self.params.len()));
        }
        query_end.push(')');

        self.params.append(values);
        self.statement.push_str(&format!("{separator}{query_end}"));
        self.insertions += 1;
    }

    pub async fn execute(&self, client: &mut Client) -> anyhow::Result<Option<u64>> {
        self.execute_in(&*client).await
    }

    pub async fn execute_in(&self, client: &impl GenericClient) -> anyhow::Result<Option<u64>> {
        if self.insertions == 0 {
            return Ok(None);
        }

        // println!("{}", self.statement);
        // println!("{:?}", self.params);

        Ok(Some(client.execute(&self.statement, &self.params).await?))
    }
}

pub struct UpdateQueryCombiner<'a> {
    pub updates: u64,
    statement: String,
    values_names: String,
    where_condition: String,
    params: Vec<&'a (dyn ToSql + Sync)>,
}

impl<'a> UpdateQueryCombiner<'a> {
    pub fn new(
        table_name: String,
        updates: String,
        values_names: String,
        where_condition: String,
    ) -> Self {
        Self {
            updates: 0,
            statement: format!("UPDATE {table_name} SET {updates} FROM (VALUES").to_string(),
            values_names,
            where_condition,
            params: vec![],
        }
    }

    pub fn add(&mut self, values: &mut Vec<&'a (dyn ToSql + Sync)>, types: HashMap<usize, String>) {
        let separator = if self.updates == 0 { " " } else { "," };
        let mut query_end = "(".to_string();
        for i in 0..values.len() {
            if i > 0 {
                query_end.push(',');
            }
            query_end.push_str(&format!("${}", i + 1 + self.params.len()));
            if let Some(t) = types.get(&i) {
                query_end.push_str(&format!("::{t}"));
            };
        }
        query_end.push(')');

        self.params.append(values);
        self.statement.push_str(&format!("{separator}{query_end}"));
        self.updates += 1;
    }

    pub async fn execute(&mut self, client: &mut Client) -> anyhow::Result<Option<u64>> {
        if self.updates == 0 {
            return Ok(None);
        }

        self.statement.push_str(&format!(
            ") AS {} WHERE {}",
            self.values_names, self.where_condition
        ));

        // println!("{}", self.statement);
        // println!("{:?}", self.params);

        Ok(Some(client.execute(&self.statement, &self.params).await?))
    }
}

#[derive(Debug)]
struct InflationApyCalculator {
    supply: u64,
    duration: u64,
    inflation: f64,
    slots_per_year: f64,
    total_weighted_credits: u128,
}
impl InflationApyCalculator {
    fn estimate_yields(&self, credits: u64, commission: u8) -> (f64, f64) {
        if self.total_weighted_credits == 0 || self.duration == 0 {
            return (0.0, 0.0);
        }

        let commission = commission.clamp(0, 100) as f64 / 100.0;
        let staker_share = 1.0 - commission;
        let actual_epochs_per_year = SECONDS_IN_YEAR / self.duration as f64;

        // Nominal, not measured: it converts annual issuance into what the protocol mints per epoch.
        let nominal_epochs_per_year = self.slots_per_year / SLOTS_IN_EPOCH as f64;

        let cluster_rewards_per_year = self.supply as f64 * self.inflation;
        let cluster_rewards_per_nominal_epoch = cluster_rewards_per_year / nominal_epochs_per_year;

        let stake_fraction_per_epoch =
            staker_share * cluster_rewards_per_nominal_epoch * credits as f64
                / self.total_weighted_credits as f64;

        let apr = stake_fraction_per_epoch * actual_epochs_per_year;
        let apy = (1.0 + stake_fraction_per_epoch).powf(actual_epochs_per_year) - 1.0;

        (apr, apy)
    }
}
fn get_apy_calculators(
    warehouse: &Warehouse,
) -> anyhow::Result<HashMap<u64, InflationApyCalculator>> {
    let mut result: HashMap<_, _> = Default::default();

    for (epoch, epoch_record) in warehouse.epochs.iter() {
        // The epochs a snapshot never landed for carry no yield to estimate.
        let Some(snapshot) = warehouse.snapshots.get(epoch) else {
            continue;
        };
        let total_weighted_credits: u128 = snapshot
            .values()
            .map(|validator| {
                validator.credits.to_u128().unwrap_or_default()
                    * validator.activated_stake.to_u128().unwrap_or_default()
            })
            .sum();

        result.insert(
            *epoch,
            InflationApyCalculator {
                supply: epoch_record.supply.try_into()?,
                duration: (epoch_record.end_at - epoch_record.start_at)
                    .num_seconds()
                    .try_into()?,
                inflation: epoch_record.inflation,
                slots_per_year: epoch_record.slots_per_year,
                total_weighted_credits,
            },
        );
    }

    Ok(result)
}

/// Window (in epochs) over which per-validator downtime incidents are collected for the
/// `incidents` field on `/validators`.
const DEFAULT_INCIDENTS_WINDOW_EPOCHS: u64 = 90;

/// How far back to accept a validator's latest Jito commissions. Wide enough to survive an epoch
/// with no distribution account written, short enough that a long-departed validator reads as absent.
const DEFAULT_JITO_COMMISSION_EPOCHS: u64 = 10;

/// Loads all downtime incidents (each a distinct `DOWN` interval) per validator over the last
/// `epochs` epochs. Each `DOWN` interval is one incident and includes length of downtime.
pub fn load_incidents(
    warehouse: &Warehouse,
    epochs: u64,
) -> anyhow::Result<HashMap<String, Vec<IncidentRecord>>> {
    let mut records: HashMap<String, Vec<IncidentRecord>> = Default::default();

    for (vote_account, interval) in warehouse.uptime_intervals(epochs) {
        if interval.status != UptimeStatus::Down {
            continue;
        }
        records
            .entry(vote_account.clone())
            .or_default()
            .push(IncidentRecord {
                epoch: interval.epoch,
                start_at: interval.start_at,
                end_at: interval.end_at,
                downtime_seconds: (interval.end_at - interval.start_at)
                    .num_seconds()
                    .try_into()?,
            });
    }

    for incidents in records.values_mut() {
        incidents.sort_by_key(|incident| incident.start_at);
    }

    Ok(records)
}

pub fn load_uptimes(
    warehouse: &Warehouse,
    epochs: u64,
) -> anyhow::Result<HashMap<String, Vec<UptimeRecord>>> {
    let mut records: HashMap<String, Vec<UptimeRecord>> = Default::default();

    for (vote_account, interval) in warehouse.uptime_intervals(epochs) {
        let epoch_record = warehouse.epochs.get(&interval.epoch);
        records
            .entry(vote_account.clone())
            .or_default()
            .push(UptimeRecord {
                epoch: interval.epoch,
                epoch_start_at: epoch_record.map_or_else(Utc::now, |epoch| epoch.start_at),
                epoch_end_at: epoch_record.map_or_else(Utc::now, |epoch| epoch.end_at),
                status: interval.status.to_string(),
                start_at: interval.start_at,
                end_at: interval.end_at,
            });
    }

    Ok(records)
}

pub fn load_versions(
    warehouse: &Warehouse,
    epochs: u64,
) -> anyhow::Result<HashMap<String, Vec<VersionRecord>>> {
    let mut records: HashMap<String, Vec<VersionRecord>> = Default::default();

    for (vote_account, change) in warehouse.version_changes(epochs) {
        let client_id = effective_client_id(
            change.client_id.map(|id| id as u16),
            change.client_id_raw.as_deref(),
        );
        records
            .entry(vote_account.clone())
            .or_default()
            .push(VersionRecord {
                epoch: change.epoch,
                version: change.version.clone(),
                client_id,
                client_name: client_name(client_id),
                client_label: client_label(client_id),
                client_vendor: client_vendor(client_id),
                client_lineage: client_lineage(client_id),
                client_id_raw: change.client_id_raw.clone(),
                feature_set: change.feature_set.map(|set| set as u32),
                shred_version: change.shred_version.map(|version| version as u16),
                created_at: change.created_at,
            });
    }

    Ok(records)
}

/*
We are checking if:
- Current commission is greater than previous minimum, and it's above 10 OR
- Previous commission is more than 10, current commission is less than or equal to 10, and the next commission is more than 10 OR
- Previous commission is less than or equal to 10, current commission is more than 10, and the next commission is less than or equal to 10
 */
/// One epoch's commissions of one validator, as the rug rule reads them.
struct CommissionPoint {
    epoch: u64,
    effective: Option<i32>,
    min_observed: Option<i32>,
}

pub fn load_ruggers(warehouse: &Warehouse) -> HashMap<String, RuggerRecord> {
    let mut series: HashMap<&String, Vec<CommissionPoint>> = Default::default();
    for (epoch, snapshot) in warehouse.snapshots.iter() {
        for (vote_account, validator) in snapshot.iter() {
            series
                .entry(vote_account)
                .or_default()
                .push(CommissionPoint {
                    epoch: *epoch,
                    effective: validator.commission_effective,
                    min_observed: validator.commission_min_observed,
                });
        }
    }

    let mut records: HashMap<String, RuggerRecord> = Default::default();
    for (vote_account, mut epochs) in series {
        epochs.sort_by_key(|point| point.epoch);

        let mut rugs: Vec<(u64, i32, i32)> = Default::default();
        for (index, point) in epochs.iter().enumerate() {
            let previous = index.checked_sub(1).and_then(|i| epochs[i].effective);
            let next = epochs.get(index + 1).and_then(|point| point.effective);
            let Some(effective) = point.effective else {
                continue;
            };
            let above_its_own_floor = point
                .min_observed
                .is_some_and(|min| effective > min && effective > 10 && min <= 10);
            let dipped =
                previous.is_some_and(|p| p > 10) && effective <= 10 && next.is_some_and(|n| n > 10);
            let spiked = previous.is_some_and(|p| p <= 10)
                && effective > 10
                && next.is_some_and(|n| n <= 10);
            if above_its_own_floor || dipped || spiked {
                // A dip or spike is judged on its neighbours, so the floor it
                // is paired with may not have been observed at all.
                rugs.push((
                    point.epoch,
                    effective,
                    point.min_observed.unwrap_or_default(),
                ));
            }
        }

        if rugs.len() <= 1 {
            continue;
        }
        records.insert(
            vote_account.clone(),
            RuggerRecord {
                epochs: rugs.iter().map(|(epoch, _, _)| *epoch).collect(),
                occurrences: rugs.len() as u64,
                observed_commissions: rugs.iter().map(|(_, e, _)| *e as u64).collect(),
                min_commissions: rugs.iter().map(|(_, _, m)| *m as u64).collect(),
                created_at: Utc::now(),
            },
        );
    }

    records
}

pub fn load_commissions(
    warehouse: &Warehouse,
    epochs: u64,
) -> anyhow::Result<HashMap<String, Vec<CommissionRecord>>> {
    let mut records: HashMap<String, Vec<CommissionRecord>> = Default::default();

    for (vote_account, change) in warehouse.commission_changes(epochs) {
        let epoch_record = warehouse.epochs.get(&change.epoch);
        records
            .entry(vote_account.clone())
            .or_default()
            .push(CommissionRecord {
                epoch: change.epoch,
                epoch_start_at: epoch_record.map_or_else(Utc::now, |epoch| epoch.start_at),
                epoch_end_at: epoch_record.map_or_else(Utc::now, |epoch| epoch.end_at),
                epoch_slot: change.epoch_slot,
                commission: change.commission.try_into()?,
                created_at: change.created_at,
            });
    }

    // The effective commission of a closed epoch is a commission observation
    // of its own, at the slot the epoch ended on.
    let first_epoch = warehouse.window_start(epochs);
    for (epoch, snapshot) in warehouse.snapshots.range(first_epoch..) {
        let epoch_record = warehouse.epochs.get(epoch);
        for (vote_account, validator) in snapshot.iter() {
            let Some(commission_effective) = validator.commission_effective else {
                continue;
            };
            records
                .entry(vote_account.clone())
                .or_default()
                .push(CommissionRecord {
                    epoch: *epoch,
                    epoch_start_at: epoch_record.map_or_else(Utc::now, |epoch| epoch.start_at),
                    epoch_end_at: epoch_record.map_or_else(Utc::now, |epoch| epoch.end_at),
                    epoch_slot: SLOTS_IN_EPOCH,
                    commission: commission_effective.try_into()?,
                    created_at: validator.updated_at.unwrap_or_else(Utc::now),
                });
        }
    }

    Ok(records)
}

pub async fn update_with_warnings(
    validators: &mut HashMap<String, ValidatorRecord>,
    epochs_range: RangeInclusive<u64>,
) -> anyhow::Result<()> {
    log::info!("Updating validator records with warnings");

    for validator in validators.values_mut() {
        if validator.superminority {
            validator.warnings.push(ValidatorWarning::Superminority);
        }
        if validator.avg_uptime_pct.unwrap_or(0.0) < 0.9 {
            validator.warnings.push(ValidatorWarning::LowUptime);
        }
        let max_effective_commission = validator
            .epoch_stats
            .iter()
            .filter(|stat| epochs_range.contains(&stat.epoch))
            .fold(0, |max_commission, epoch_stats: &ValidatorEpochStats| {
                epoch_stats
                    .commission_effective
                    .unwrap_or(0)
                    .max(max_commission)
            });
        if max_effective_commission > 10 {
            validator.warnings.push(ValidatorWarning::HighCommission);
        }
    }

    Ok(())
}

fn average(numbers: &[f64]) -> Option<f64> {
    if numbers.is_empty() {
        return None;
    }
    let sum = numbers.iter().filter(|n| !n.is_nan()).sum::<f64>();
    let count = numbers.iter().filter(|n| !n.is_nan()).count() as f64;
    Some(sum / count)
}

pub fn update_validators_with_avgs(
    validators: &mut HashMap<String, ValidatorRecord>,
    epochs_range: RangeInclusive<u64>,
) {
    for (_, record) in validators.iter_mut() {
        record.avg_apy = average(
            &record
                .epoch_stats
                .iter()
                .filter(|stat| epochs_range.contains(&stat.epoch))
                .flat_map(|epoch| epoch.apy)
                .collect::<Vec<f64>>(),
        );
        record.avg_uptime_pct = average(
            &record
                .epoch_stats
                .iter()
                .filter(|stat| epochs_range.contains(&stat.epoch))
                .flat_map(|epoch| epoch.uptime_pct)
                .collect::<Vec<f64>>(),
        );
    }
}

// Validators without a value stay unranked; ranking them as 0 would contradict the list sort.
pub fn update_validators_ranks<T>(
    validators: &mut HashMap<String, ValidatorRecord>,
    field_extractor: fn(&ValidatorEpochStats) -> Option<T>,
    rank_updater: fn(&mut ValidatorEpochStats, usize) -> (),
) where
    T: Ord,
{
    let mut stats_by_epoch: HashMap<u64, Vec<(String, T)>> = Default::default();
    for (vote_account, record) in validators.iter() {
        for validator_epoch_stats in record.epoch_stats.iter() {
            if let Some(value) = field_extractor(validator_epoch_stats) {
                stats_by_epoch
                    .entry(validator_epoch_stats.epoch)
                    .or_default()
                    .push((vote_account.clone(), value));
            }
        }
    }

    for (epoch, stats) in stats_by_epoch.iter_mut() {
        stats.sort_by(|(_, stat_a), (_, stat_b)| stat_a.cmp(stat_b));
        let mut previous_value: Option<&T> = None;
        let mut same_ranks: usize = 0;
        for (index, (vote_account, stat)) in stats.iter().enumerate() {
            if let Some(some_previous_value) = previous_value {
                if some_previous_value == stat {
                    same_ranks += 1;
                } else {
                    same_ranks = 0;
                }
            }
            previous_value = Some(stat);

            let validator_epoch_stats = validators
                .get_mut(vote_account)
                .unwrap()
                .epoch_stats
                .iter_mut()
                .find(|a| a.epoch == *epoch)
                .unwrap();
            rank_updater(validator_epoch_stats, stats.len() - index + same_ranks);
        }
    }
}

const GOOGLE_BQ_PROJECT_ID: &str = "data-store-406413";
const GOOGLE_BQ_DATASET: &str = "mainnet_beta_stakes";
const STAKES_TABLE: &str = "stakes";

/// Latest epoch present in BigQuery; the gate for refreshing BigQuery-sourced caches.
pub async fn load_last_bigquery_epoch() -> anyhow::Result<Option<u64>> {
    let (config, _) = BqClientConfig::new_with_auth().await?;
    let bq_client = BqClient::new(config).await?;
    let ds = format!("{GOOGLE_BQ_PROJECT_ID}.{GOOGLE_BQ_DATASET}");
    scalar_u64(
        &bq_client,
        format!("SELECT CAST(MAX(epoch) AS STRING) FROM `{ds}.epochs`"),
    )
    .await
}

/// Latest-epoch unique delegator (distinct `withdraw_authority`) count per validator, from BigQuery.
pub async fn load_latest_unique_delegators() -> anyhow::Result<HashMap<String, u64>> {
    let (config, _) = BqClientConfig::new_with_auth().await?;
    let bq_client = BqClient::new(config).await?;

    let project_table = format!("{GOOGLE_BQ_PROJECT_ID}.{GOOGLE_BQ_DATASET}.{STAKES_TABLE}");

    // Resolve the latest epoch as a literal first so the main query prunes to a single partition.
    let max_epoch = match scalar_u64(
        &bq_client,
        format!("SELECT CAST(MAX(epoch) AS STRING) FROM `{project_table}`"),
    )
    .await?
    {
        Some(epoch) => epoch,
        None => return Ok(Default::default()),
    };

    let query = format!(
        "SELECT vote_account, \
                CAST(COUNT(DISTINCT withdraw_authority) AS INT64) AS unique_delegators \
         FROM `{project_table}` \
         WHERE active > 0 AND vote_account IS NOT NULL AND epoch = {max_epoch} \
         GROUP BY vote_account"
    );

    let request = QueryRequest {
        query,
        use_legacy_sql: false,
        ..Default::default()
    };

    let mut iter = bq_client
        .query::<Row>(GOOGLE_BQ_PROJECT_ID, request)
        .await?;

    let mut records: HashMap<String, u64> = Default::default();
    while let Some(row) = iter.next().await? {
        let vote_account = row.column::<String>(0)?;
        let unique_delegators_str = row.column::<String>(1)?;
        records.insert(vote_account, unique_delegators_str.parse()?);
    }

    Ok(records)
}

/// Rolling window (days) for the take-rate average. Mirrors apy-api's `DEFAULT_ROLLING_APY_WINDOW`.
const TAKE_RATE_WINDOW_DAYS: u64 = 30;

async fn scalar_u64(bq_client: &BqClient, query: String) -> anyhow::Result<Option<u64>> {
    let request = QueryRequest {
        query,
        use_legacy_sql: false,
        ..Default::default()
    };
    let mut iter = bq_client
        .query::<Row>(GOOGLE_BQ_PROJECT_ID, request)
        .await?;
    match iter.next().await? {
        Some(row) => Ok(row
            .column::<Option<String>>(0)?
            .map(|s| s.parse())
            .transpose()?),
        None => Ok(None),
    }
}

/// Cluster-wide split of the window's rewards across the three components, as fractions of the
/// total pot. Each counts both sides, so a validator choosing to share its block rewards moves
/// lamports between the sides without moving the weight every validator's take rate is scaled by.
#[derive(Default, Clone, Copy, Debug, PartialEq)]
pub struct RewardMixShares {
    pub inflation: f64,
    pub mev: f64,
    pub block: f64,
}

#[derive(Default, Clone, Debug)]
pub struct TakeRates {
    pub measured: HashMap<String, f64>,
    pub shares: Option<RewardMixShares>,
}

/// Per-validator take rate over the last `TAKE_RATE_WINDOW_DAYS`, computed directly from BigQuery
/// reward tables: `validator_rewards / total_rewards` where validator = inflation + MEV + block
/// commission and total = staker + validator rewards. Windowed by `epochs.epoch_end_time` (same as
/// apy-api). Reward tables are epoch-partitioned, so the resolved lower epoch is filtered on each.
/// Also returns the cluster reward mix, which the same scan already has to compute.
pub async fn load_take_rates() -> anyhow::Result<TakeRates> {
    let (config, _) = BqClientConfig::new_with_auth().await?;
    let bq_client = BqClient::new(config).await?;

    let ds = format!("{GOOGLE_BQ_PROJECT_ID}.{GOOGLE_BQ_DATASET}");

    let min_epoch = match scalar_u64(
        &bq_client,
        format!(
            "SELECT CAST(MIN(epoch) AS STRING) FROM `{ds}.epochs` \
             WHERE epoch_end_time >= TIMESTAMP_SUB( \
                 (SELECT MAX(epoch_end_time) FROM `{ds}.epochs`), INTERVAL {TAKE_RATE_WINDOW_DAYS} DAY)"
        ),
    )
    .await?
    {
        Some(min_epoch) => min_epoch,
        None => return Ok(Default::default()),
    };

    let query = format!(
        "SELECT
            vote_account,
            CAST(take_rate AS STRING) AS take_rate,
            CAST(inflation_share AS STRING) AS inflation_share,
            CAST(mev_share AS STRING) AS mev_share,
            CAST(block_share AS STRING) AS block_share
        FROM (
            WITH stakers AS (
                SELECT
                    stakes.vote_account AS vote_account,
                    stakes.epoch AS epoch,
                    SUM(COALESCE(inflation.amount, 0)) AS staker_inflation,
                    SUM(COALESCE(mev.amount, 0)) AS staker_mev,
                    SUM(COALESCE(prio.amount, 0)) AS staker_blocks
                FROM `{ds}.stakes` stakes
                LEFT JOIN `{ds}.rewards_inflation` inflation
                    ON stakes.stake_account = inflation.stake_account
                    AND stakes.epoch = inflation.epoch AND inflation.epoch >= {min_epoch}
                LEFT JOIN `{ds}.rewards_mev` mev
                    ON stakes.stake_account = mev.stake_account
                    AND stakes.epoch = mev.epoch AND mev.epoch >= {min_epoch}
                -- rewards_validators_blocks is gross, so what Jito's PriorityFeeDistribution passed through has to come off the validator's keep rather than add to the pot.
                LEFT JOIN `{ds}.rewards_jito_priority_fee` prio
                    ON stakes.stake_account = prio.stake_account
                    AND stakes.epoch = prio.epoch AND prio.epoch >= {min_epoch}
                WHERE stakes.vote_account IS NOT NULL AND stakes.epoch >= {min_epoch}
                GROUP BY stakes.vote_account, stakes.epoch
            ),
            per_validator AS (
                SELECT
                    stakers.vote_account AS vote_account,
                    SUM(staker_inflation + COALESCE(vi.amount, 0)) AS inflation_total,
                    SUM(staker_mev + COALESCE(vm.amount, 0)) AS mev_total,
                    SUM(COALESCE(vb.amount, 0)) AS block_total,
                    -- GREATEST guards the epochs where the two tables attribute one distribution to different sides of a boundary.
                    SUM(COALESCE(vi.amount, 0) + COALESCE(vm.amount, 0)
                        + GREATEST(COALESCE(vb.amount, 0) - staker_blocks, 0)) AS validator_total
                FROM stakers
                -- Pre-aggregate to one row per (vote_account, epoch) so raw duplicate keys can't fan out the SUM().
                LEFT JOIN (
                    SELECT vote_account, epoch, SUM(amount) AS amount
                    FROM `{ds}.rewards_validators_inflation`
                    WHERE epoch >= {min_epoch}
                    GROUP BY vote_account, epoch
                ) vi
                    ON stakers.vote_account = vi.vote_account AND stakers.epoch = vi.epoch
                LEFT JOIN (
                    SELECT vote_account, epoch, SUM(amount) AS amount
                    FROM `{ds}.rewards_validators_mev`
                    WHERE epoch >= {min_epoch}
                    GROUP BY vote_account, epoch
                ) vm
                    ON stakers.vote_account = vm.vote_account AND stakers.epoch = vm.epoch
                LEFT JOIN (
                    SELECT vote_account, epoch, SUM(amount) AS amount
                    FROM `{ds}.rewards_validators_blocks`
                    WHERE epoch >= {min_epoch}
                    GROUP BY vote_account, epoch
                ) vb
                    ON stakers.vote_account = vb.vote_account AND stakers.epoch = vb.epoch
                GROUP BY stakers.vote_account
            )
            SELECT
                vote_account,
                SAFE_DIVIDE(validator_total, inflation_total + mev_total + block_total) AS take_rate,
                -- Windowed over the already-grouped rows, so the cluster mix costs no extra scan.
                SAFE_DIVIDE(SUM(inflation_total) OVER (), SUM(inflation_total + mev_total + block_total) OVER ())
                    AS inflation_share,
                SAFE_DIVIDE(SUM(mev_total) OVER (), SUM(inflation_total + mev_total + block_total) OVER ())
                    AS mev_share,
                SAFE_DIVIDE(SUM(block_total) OVER (), SUM(inflation_total + mev_total + block_total) OVER ())
                    AS block_share
            FROM per_validator
        )
        WHERE take_rate IS NOT NULL"
    );

    let request = QueryRequest {
        query,
        use_legacy_sql: false,
        ..Default::default()
    };

    let mut iter = bq_client
        .query::<Row>(GOOGLE_BQ_PROJECT_ID, request)
        .await?;

    let mut measured: HashMap<String, f64> = Default::default();
    // Identical on every row by construction, so the last one read is the cluster mix.
    let mut shares = None;
    while let Some(row) = iter.next().await? {
        let vote_account = row.column::<String>(0)?;
        let take_rate_str = row.column::<String>(1)?;
        measured.insert(vote_account, take_rate_str.parse()?);
        shares = match (
            row.column::<Option<String>>(2)?,
            row.column::<Option<String>>(3)?,
            row.column::<Option<String>>(4)?,
        ) {
            (Some(inflation), Some(mev), Some(block)) => Some(RewardMixShares {
                inflation: inflation.parse()?,
                mev: mev.parse()?,
                block: block.parse()?,
            }),
            _ => shares,
        };
    }

    Ok(TakeRates { measured, shares })
}

/// What the validator's own fee settings imply it keeps, weighted by the cluster reward mix.
/// Renormalized over the components it actually earns: a validator not running Jito receives no MEV
/// at all, so crediting it a 0% MEV commission would dilute the rate it takes on what it does earn.
/// None when the inflation commission is unknown, which is the one component no validator can opt out of.
pub fn expected_take_rate(
    shares: RewardMixShares,
    inflation_commission_pct: Option<i32>,
    mev_commission_bps: Option<i32>,
    priority_commission_bps: Option<i32>,
) -> Option<f64> {
    let mut weighted = (f64::from(inflation_commission_pct?) / 100.0) * shares.inflation;
    let mut weight = shares.inflation;

    if let Some(bps) = mev_commission_bps {
        weighted += (f64::from(bps) / 10_000.0) * shares.mev;
        weight += shares.mev;
    }
    // Block rewards are always earned, and absent a Jito PriorityFeeDistribution account none of them
    // are shared: SIMD-0096 pays every priority fee to the block producer.
    weighted += priority_commission_bps.map_or(1.0, |bps| f64::from(bps) / 10_000.0) * shares.block;
    weight += shares.block;

    (weight > 0.0).then_some(weighted / weight)
}

#[derive(serde::Deserialize)]
struct VerifiedValidatorsResponse {
    verified_validators: Vec<String>,
}

#[derive(serde::Deserialize)]
struct ProtectedValidatorsResponse {
    protected_validators: Vec<String>,
}

pub async fn load_verified_validators(base: &str) -> anyhow::Result<HashSet<String>> {
    load_validator_flag(base, "verified", |resp: VerifiedValidatorsResponse| {
        resp.verified_validators
    })
    .await
}

pub async fn load_protected_validators(base: &str) -> anyhow::Result<HashSet<String>> {
    load_validator_flag(base, "protected", |resp: ProtectedValidatorsResponse| {
        resp.protected_validators
    })
    .await
}

#[derive(serde::Deserialize)]
struct LatestValidatorApyRecord {
    apy: f64,
}

// `what` names the upstream in the status error; it is the only part of this that differs per caller.
async fn fetch_json<T: serde::de::DeserializeOwned>(url: &str, what: &str) -> anyhow::Result<T> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(HTTP_TIMEOUT_S))
        .build()?;
    let resp = client.get(url).send().await?;
    anyhow::ensure!(
        resp.status().is_success(),
        "{what} returned {}",
        resp.status()
    );
    Ok(resp.json::<T>().await?)
}

// `base` is the apy-api base URL; the rolling window is fixed by that endpoint, so it is deliberately not a parameter here.
pub async fn load_validator_net_apy(base: &str) -> anyhow::Result<HashMap<String, f64>> {
    let url = format!(
        "{}/v1/rolling-apy/validator/latest/all",
        base.trim_end_matches('/')
    );
    Ok(fetch_json::<HashMap<String, LatestValidatorApyRecord>>(
        &url,
        "latest validator net APY endpoint",
    )
    .await?
    .into_iter()
    .map(|(vote_account, record)| (vote_account, record.apy))
    .collect())
}

// `base` is the validator-bonds API base URL; `/v1/validators/{flag}` is appended here.
async fn load_validator_flag<T, F>(
    base: &str,
    flag: &str,
    vote_accounts: F,
) -> anyhow::Result<HashSet<String>>
where
    T: serde::de::DeserializeOwned,
    F: FnOnce(T) -> Vec<String>,
{
    let url = format!("{}/v1/validators/{flag}", base.trim_end_matches('/'));
    let resp = fetch_json::<T>(&url, &format!("{flag} endpoint")).await?;
    Ok(vote_accounts(resp).into_iter().collect())
}

/// Per-record values the SQL query does not provide: the BigQuery-derived pair from the epoch cache, the apy-api net APY, and the validator-bonds flags the caller resolved.
#[derive(Default)]
pub struct ValidatorOverlays {
    pub unique_delegators: HashMap<String, u64>,
    pub take_rates: TakeRates,
    pub net_apy: HashMap<String, f64>,
    pub verified: HashSet<String>,
    pub protected: HashSet<String>,
}

pub async fn load_validators(
    warehouse: &Warehouse,
    display_epochs: u64,
    computing_epochs: u64,
    overlays: &ValidatorOverlays,
) -> anyhow::Result<HashMap<String, ValidatorRecord>> {
    let last_epoch = warehouse.last_epoch();
    if warehouse.snapshots.is_empty() {
        return Ok(Default::default());
    }
    let ruggers = load_ruggers(warehouse);
    let apy_calculators = get_apy_calculators(warehouse)?;
    let concentrations = load_dc_concentration_stats(warehouse, 1)?.first().cloned();

    // The epoch a validator was first seen in, over every epoch still held.
    let mut first_epochs: HashMap<&String, u64> = Default::default();
    for (epoch, snapshot) in warehouse.snapshots.iter() {
        for vote_account in snapshot.keys() {
            let first_epoch = first_epochs.entry(vote_account).or_insert(*epoch);
            *first_epoch = (*first_epoch).min(*epoch);
        }
    }

    log::info!("Aggregating validator records...");
    let mut records: HashMap<String, ValidatorRecord> = Default::default();
    let window_start = warehouse.window_start(display_epochs);

    for (epoch, snapshot) in warehouse.snapshots.range(window_start..).rev() {
        let epoch = *epoch;
        let epoch_record = warehouse.epochs.get(&epoch);
        let mev = warehouse.mev.get(&epoch);
        let priority_fees = warehouse.priority_fees.get(&epoch);

        for (vote_account, validator) in snapshot.iter() {
            let first_epoch = first_epochs.get(vote_account).copied().unwrap_or(epoch);
            let (apr, apy) = match apy_calculators.get(&epoch) {
                Some(calculator) => {
                    let (apr, apy) = calculator.estimate_yields(
                        validator.credits.try_into()?,
                        validator
                            .commission_effective
                            .map(|commission| commission.clamp(0, 100) as u8)
                            .unwrap_or(100),
                    );
                    (Some(apr), Some(apy))
                }
                None => (None, None),
            };

            let dc_full_city = full_city(validator);
            let dc_asn = validator
                .dc_asn
                .map(|asn| asn.to_string())
                .unwrap_or("Unknown".into());
            let dc_aso = validator.dc_aso.clone().unwrap_or("Unknown".into());
            let dc_country = validator.dc_country.clone().unwrap_or("Unknown".into());

            let client_id = effective_client_id(
                validator.client_id.map(|id| id as u16),
                validator.client_id_raw.as_deref(),
            );

            let record = records
                .entry(vote_account.clone())
                .or_insert_with(|| ValidatorRecord {
                    identity: validator.identity.clone(),
                    // Without the epoch's own record there is no date to start from.
                    start_epoch: match warehouse.epochs.contains_key(&first_epoch) {
                        true => first_epoch,
                        false => 0,
                    },
                    start_date: warehouse
                        .epochs
                        .get(&first_epoch)
                        .map(|epoch| epoch.start_at),
                    vote_account: vote_account.clone(),
                    info_name: validator.info_name.clone(),
                    info_url: validator.info_url.clone(),
                    info_keybase: validator.info_keybase.clone(),
                    info_icon_url: validator.info_icon_url.clone(),
                    node_ip: validator.node_ip.clone(),
                    dc_coordinates_lat: validator.dc_coordinates_lat,
                    dc_coordinates_lon: validator.dc_coordinates_lon,
                    dc_continent: validator.dc_continent.clone(),
                    dc_country_iso: validator.dc_country_iso.clone(),
                    dc_country: validator.dc_country.clone(),
                    dc_city: validator.dc_city.clone(),
                    dc_full_city: Some(dc_full_city.clone()),
                    dc_asn: validator.dc_asn,
                    dc_aso: validator.dc_aso.clone(),
                    dcc_full_city: concentrations
                        .as_ref()
                        .and_then(|c| c.dc_concentration_by_city.get(&dc_full_city).copied()),
                    dcc_asn: concentrations
                        .as_ref()
                        .and_then(|c| c.dc_concentration_by_asn.get(&dc_asn).copied()),
                    dcc_aso: concentrations
                        .as_ref()
                        .and_then(|c| c.dc_concentration_by_aso.get(&dc_aso).copied()),
                    dcc_country: concentrations
                        .as_ref()
                        .and_then(|c| c.dc_concentration_by_country.get(&dc_country).copied()),
                    commission_max_observed: validator.commission_max_observed,
                    commission_min_observed: validator.commission_min_observed,
                    commission_advertised: validator.commission_advertised,
                    commission_effective: validator.commission_effective,
                    commission_aggregated: None,
                    version: validator.version.clone(),
                    client_id,
                    client_name: client_name(client_id),
                    client_label: client_label(client_id),
                    client_vendor: client_vendor(client_id),
                    client_lineage: client_lineage(client_id),
                    client_id_raw: validator.client_id_raw.clone(),
                    feature_set: validator.feature_set.map(|set| set as u32),
                    shred_version: validator.shred_version.map(|version| version as u16),
                    gossip_port: validator.gossip_port.map(|port| port as u16),
                    rpc_public: validator.rpc_public,
                    pubsub_public: validator.pubsub_public,
                    activated_stake: validator.activated_stake,
                    marinade_stake: validator.marinade_stake,
                    foundation_stake: validator.foundation_stake,
                    self_stake: validator.self_stake,
                    marinade_native_stake: validator.marinade_native_stake,
                    institutional_stake: validator.institutional_stake,
                    superminority: validator.superminority,
                    credits: validator.credits.try_into().unwrap_or_default(),
                    score: None,

                    epoch_stats: Vec::with_capacity(display_epochs as usize),

                    warnings: Default::default(),

                    epochs_count: epoch - first_epoch + 1,

                    avg_uptime_pct: None,
                    avg_apy: None,
                    unique_delegators: None,
                    avg_take_rate: None,
                    expected_take_rate: None,
                    net_apy: None,
                    incidents: Vec::new(),
                    verified: false,
                    protected: false,
                    has_last_epoch_stats: false,
                    rugged_commission: false,
                    rugged_commission_info: Vec::new(),
                    rugged_commission_occurrences: 0,
                });

            if let Some(rugger) = ruggers.get(vote_account) {
                record.rugged_commission = true;
                record.rugged_commission_occurrences = rugger.occurrences;
                record.rugged_commission_info = rugger
                    .epochs
                    .iter()
                    .enumerate()
                    .map(|(index, &epoch)| RugInfo {
                        epoch,
                        after: rugger.observed_commissions[index],
                        before: rugger.min_commissions[index],
                    })
                    .collect()
            }
            if last_epoch == epoch {
                record.has_last_epoch_stats = true;
            }

            record.epoch_stats.push(ValidatorEpochStats {
                epoch,
                epoch_start_at: Some(epoch_record.map_or_else(Utc::now, |epoch| epoch.start_at)),
                epoch_end_at: epoch_record.map(|epoch| epoch.end_at),
                commission_max_observed: to_commission(validator.commission_max_observed)?,
                commission_min_observed: to_commission(validator.commission_min_observed)?,
                commission_advertised: to_commission(validator.commission_advertised)?,
                commission_effective: to_commission(validator.commission_effective)?,
                version: validator.version.clone(),
                mev_commission_bps: mev
                    .and_then(|mev| mev.get(vote_account))
                    .map(|entry| entry.mev_commission),
                priority_commission_bps: priority_fees
                    .and_then(|fees| fees.get(vote_account))
                    .map(|entry| entry.priority_commission),
                dc_asn: validator.dc_asn,
                dc_aso: validator.dc_aso.clone(),
                dc_city: validator.dc_city.clone(),
                dc_country: validator.dc_country.clone(),
                client_id,
                client_name: client_name(client_id),
                client_label: client_label(client_id),
                client_vendor: client_vendor(client_id),
                client_lineage: client_lineage(client_id),
                client_id_raw: validator.client_id_raw.clone(),
                feature_set: validator.feature_set.map(|set| set as u32),
                shred_version: validator.shred_version.map(|version| version as u16),
                gossip_port: validator.gossip_port.map(|port| port as u16),
                rpc_public: validator.rpc_public,
                pubsub_public: validator.pubsub_public,
                activated_stake: validator.activated_stake,
                marinade_stake: validator.marinade_stake,
                foundation_stake: validator.foundation_stake,
                self_stake: validator.self_stake,
                marinade_native_stake: validator.marinade_native_stake,
                institutional_stake: validator.institutional_stake,
                superminority: validator.superminority,
                stake_to_become_superminority: validator.stake_to_become_superminority,
                credits: validator.credits.try_into()?,
                leader_slots: validator.leader_slots.try_into()?,
                blocks_produced: validator.blocks_produced.try_into()?,
                skip_rate: validator.skip_rate,
                uptime_pct: validator.uptime_pct,
                uptime: validator.uptime.map(u64::try_from).transpose()?,
                downtime: validator.downtime.map(u64::try_from).transpose()?,
                apr,
                apy,
                score: None,
                rank_apy: None,
                rank_score: None,
                rank_activated_stake: None,
            });
        }
    }

    let mut first_epoch = last_epoch - display_epochs.min(last_epoch) + 1;
    let mut epochs_range = first_epoch..=last_epoch;

    log::info!("Updating with scores...");
    update_validators_with_scores(warehouse, &mut records, epochs_range.clone());

    first_epoch = last_epoch - computing_epochs.min(last_epoch) + 1;
    epochs_range = first_epoch..=last_epoch;

    log::info!("Updating averages...");
    update_validators_with_avgs(&mut records, epochs_range.clone());
    log::info!("Updating ranks...");
    update_validators_ranks(
        &mut records,
        |a: &ValidatorEpochStats| Some(a.activated_stake),
        |a: &mut ValidatorEpochStats, rank: usize| a.rank_activated_stake = Some(rank),
    );
    update_validators_ranks(
        &mut records,
        |a: &ValidatorEpochStats| a.score.and_then(Decimal::from_f64_retain),
        |a: &mut ValidatorEpochStats, rank: usize| a.rank_score = Some(rank),
    );
    update_validators_ranks(
        &mut records,
        |a: &ValidatorEpochStats| {
            a.apy
                .filter(|apy| *apy >= 0.0)
                .and_then(Decimal::from_f64_retain)
        },
        |a: &mut ValidatorEpochStats, rank: usize| a.rank_apy = Some(rank),
    );
    update_with_warnings(&mut records, epochs_range.clone()).await?;

    log::info!("Updating unique delegators...");
    for (vote_account, record) in records.iter_mut() {
        record.unique_delegators = overlays.unique_delegators.get(vote_account).copied();
    }

    log::info!("Updating incidents...");
    let incidents = load_incidents(warehouse, DEFAULT_INCIDENTS_WINDOW_EPOCHS)?;
    for (vote_account, record) in records.iter_mut() {
        record.incidents = incidents.get(vote_account).cloned().unwrap_or_default();
    }

    log::info!("Updating validator-bonds flags...");
    for (vote_account, record) in records.iter_mut() {
        record.verified = overlays.verified.contains(vote_account);
        record.protected = overlays.protected.contains(vote_account);
    }

    log::info!("Updating take rates...");
    let mut mev_commissions: HashMap<String, i32> = Default::default();
    let mut priority_commissions: HashMap<String, i32> = Default::default();
    // get_last_jito_info keys on (vote_account, epoch): the two distribution accounts resolve their last epoch separately, splitting one validator across two records.
    for jito in get_last_jito_info(warehouse, DEFAULT_JITO_COMMISSION_EPOCHS)? {
        if let Some(bps) = jito.mev_commission_bps {
            mev_commissions.insert(jito.vote_account.clone(), bps);
        }
        if let Some(bps) = jito.priority_commission_bps {
            priority_commissions.insert(jito.vote_account, bps);
        }
    }
    for (vote_account, record) in records.iter_mut() {
        record.avg_take_rate = overlays.take_rates.measured.get(vote_account).copied();
        record.expected_take_rate = overlays.take_rates.shares.and_then(|shares| {
            expected_take_rate(
                shares,
                record.commission_advertised,
                mev_commissions.get(vote_account).copied(),
                priority_commissions.get(vote_account).copied(),
            )
        });
    }

    log::info!("Updating net APY...");
    for (vote_account, record) in records.iter_mut() {
        record.net_apy = overlays.net_apy.get(vote_account).copied();
    }

    log::info!("Records prepared...");
    Ok(records)
}

/// What `CONCAT(dc_continent, '/', dc_country, '/', dc_city)` rendered: the
/// parts that are unknown are empty, and the key exists either way.
pub fn full_city(validator: &Validator) -> String {
    format!(
        "{}/{}/{}",
        validator.dc_continent.clone().unwrap_or_default(),
        validator.dc_country.clone().unwrap_or_default(),
        validator.dc_city.clone().unwrap_or_default()
    )
}

fn to_commission(commission: Option<i32>) -> anyhow::Result<Option<u8>> {
    Ok(commission.map(u8::try_from).transpose()?)
}

pub fn update_validators_with_scores(
    warehouse: &Warehouse,
    validators: &mut HashMap<String, ValidatorRecord>,
    epochs_range: RangeInclusive<u64>,
) {
    log::info!("Updating validator score with epochs range: {epochs_range:?}");
    let scores_per_epoch = load_scores_in_epochs(warehouse, epochs_range);

    let Some(latest_epoch_with_score) = scores_per_epoch.keys().max() else {
        return;
    };
    let latest_scores = &scores_per_epoch[latest_epoch_with_score];

    for validator in validators.values_mut() {
        for epoch_record in validator.epoch_stats.iter_mut() {
            if let Some(scores) = scores_per_epoch.get(&epoch_record.epoch) {
                epoch_record.score = scores.get(&validator.vote_account).copied();
            }
        }

        validator.score = latest_scores.get(&validator.vote_account).copied();
    }
}

pub fn load_scores_in_epochs(
    warehouse: &Warehouse,
    epochs: RangeInclusive<u64>,
) -> HashMap<u64, HashMap<String, f64>> {
    log::info!("Loading scores for epochs: {epochs:?}");

    warehouse
        .scoring
        .range(epochs)
        .map(|(epoch, breakdowns)| {
            let scores = breakdowns
                .scores
                .iter()
                .map(|score| (score.vote_account.clone(), score.score))
                .collect();
            (*epoch, scores)
        })
        .collect()
}

/// The newest epoch ds-scoring has published, in the version it serves now.
pub fn load_last_scoring_run(warehouse: &Warehouse) -> Option<ScoringRunRecord> {
    let Some((_, breakdowns)) = warehouse.scoring.iter().next_back() else {
        log::warn!("No scoring run was found!");
        return None;
    };

    Some(breakdowns.scoring_run())
}

pub fn load_scores(
    warehouse: &Warehouse,
    scoring_run_id: Decimal,
) -> HashMap<String, ValidatorScoreRecord> {
    warehouse
        .scoring
        .values()
        .find(|breakdowns| Decimal::from(breakdowns.scoring_run_id) == scoring_run_id)
        .map(|breakdowns| {
            breakdowns
                .scores
                .iter()
                .map(|score| (score.vote_account.clone(), score.clone()))
                .collect()
        })
        .unwrap_or_default()
}

pub fn get_last_epoch(warehouse: &Warehouse) -> Option<u64> {
    Some(warehouse.last_epoch())
}

pub fn load_dc_concentration_stats(
    warehouse: &Warehouse,
    epochs: u64,
) -> anyhow::Result<Vec<DCConcentrationStats>> {
    let mut stats: Vec<_> = Default::default();

    let map_stake_to_concentration =
        |stake: &HashMap<String, u64>, total_stake: u64| -> HashMap<_, _> {
            stake
                .iter()
                .map(|(key, stake)| (key.clone(), *stake as f64 / total_stake as f64))
                .collect()
        };

    for epoch in warehouse.epochs_window(epochs) {
        let mut dc_stake_by_aso: HashMap<String, u64> = Default::default();
        let mut dc_stake_by_asn: HashMap<String, u64> = Default::default();
        let mut dc_stake_by_city: HashMap<String, u64> = Default::default();
        let mut dc_stake_by_country: HashMap<String, u64> = Default::default();
        let mut total_active_stake = 0;

        for validator in snapshot_of(warehouse, epoch) {
            let activated_stake: u64 = validator.activated_stake.try_into()?;
            let dc_aso = validator.dc_aso.clone().unwrap_or("Unknown".to_string());
            let dc_asn = validator
                .dc_asn
                .map_or("Unknown".to_string(), |asn| asn.to_string());
            let dc_country = validator
                .dc_country
                .clone()
                .unwrap_or("Unknown".to_string());

            total_active_stake += activated_stake;
            *dc_stake_by_aso.entry(dc_aso).or_default() += activated_stake;
            *dc_stake_by_asn.entry(dc_asn).or_default() += activated_stake;
            *dc_stake_by_city.entry(full_city(validator)).or_default() += activated_stake;
            *dc_stake_by_country.entry(dc_country).or_default() += activated_stake;
        }

        stats.push(DCConcentrationStats {
            epoch,
            total_activated_stake: total_active_stake,
            dc_concentration_by_aso: map_stake_to_concentration(
                &dc_stake_by_aso,
                total_active_stake,
            ),
            dc_concentration_by_asn: map_stake_to_concentration(
                &dc_stake_by_asn,
                total_active_stake,
            ),
            dc_stake_by_asn,
            dc_stake_by_aso,
            dc_concentration_by_city: map_stake_to_concentration(
                &dc_stake_by_city,
                total_active_stake,
            ),
            dc_stake_by_city,
            dc_concentration_by_country: map_stake_to_concentration(
                &dc_stake_by_country,
                total_active_stake,
            ),
            dc_stake_by_country,
        })
    }

    Ok(stats)
}

pub fn load_block_production_stats(
    warehouse: &Warehouse,
    epochs: u64,
) -> anyhow::Result<Vec<BlockProductionStats>> {
    let last_epoch = warehouse.last_epoch();
    let first_epoch = last_epoch - epochs.min(last_epoch) + 1;

    let mut stats: Vec<_> = Default::default();
    // The SQL bound was exclusive and the series it feeds is drawn that way.
    for (epoch, snapshot) in warehouse.snapshots.range(first_epoch + 1..).rev() {
        let blocks_produced: u64 = snapshot
            .values()
            .map(|validator| u64::try_from(validator.blocks_produced).unwrap_or_default())
            .sum();
        let leader_slots: u64 = snapshot
            .values()
            .map(|validator| u64::try_from(validator.leader_slots).unwrap_or_default())
            .sum();

        stats.push(BlockProductionStats {
            epoch: *epoch,
            blocks_produced,
            leader_slots,
            avg_skip_rate: match leader_slots {
                0 => 1f64,
                _ => 1f64 - blocks_produced as f64 / leader_slots as f64,
            },
        })
    }

    Ok(stats)
}

struct StakeDistribution {
    epoch: u64,
    total_stake: u64,
    stake_by: HashMap<String, u64>,
    share_by: HashMap<String, f64>,
    count_by: HashMap<String, u64>,
}

/// The validators of one epoch, or none where no snapshot landed for it.
fn snapshot_of(warehouse: &Warehouse, epoch: u64) -> impl Iterator<Item = &Validator> {
    warehouse
        .snapshots
        .get(&epoch)
        .into_iter()
        .flat_map(|snapshot| snapshot.values())
}

fn load_stake_distribution(
    warehouse: &Warehouse,
    epochs: u64,
    grouping_key: fn(&Validator) -> String,
) -> anyhow::Result<Vec<StakeDistribution>> {
    let mut distributions: Vec<StakeDistribution> = Default::default();

    for epoch in warehouse.epochs_window(epochs) {
        let mut stake_by: HashMap<String, u64> = Default::default();
        let mut count_by: HashMap<String, u64> = Default::default();
        let mut total_stake: u64 = 0;

        for validator in snapshot_of(warehouse, epoch) {
            let key = grouping_key(validator);
            let stake: u64 = validator.activated_stake.try_into()?;
            total_stake += stake;
            *stake_by.entry(key.clone()).or_default() += stake;
            *count_by.entry(key).or_default() += 1;
        }

        let share_by: HashMap<String, f64> = stake_by
            .iter()
            .map(|(k, v)| {
                (
                    k.clone(),
                    if total_stake > 0 {
                        *v as f64 / total_stake as f64
                    } else {
                        0.0
                    },
                )
            })
            .collect();

        distributions.push(StakeDistribution {
            epoch,
            total_stake,
            stake_by,
            share_by,
            count_by,
        });
    }
    Ok(distributions)
}

const UNKNOWN_CLIENT_GROUP: &str = "unknown";

// Empty rather than a word, so the sentinel cannot collide with a client name in the registry.
fn client_id_group(validator: &Validator) -> String {
    validator
        .client_id
        .map(|id| id.to_string())
        .or_else(|| validator.client_id_raw.clone())
        .unwrap_or_default()
}

fn grouped_by(key: &str, map: fn(Option<u16>) -> Option<String>) -> String {
    map(effective_client_id(key.parse().ok(), Some(key)))
        .unwrap_or_else(|| UNKNOWN_CLIENT_GROUP.to_string())
}

// One entry per client id, but several ids share a vendor.
fn fold_distribution(
    distributions: &[StakeDistribution],
    map: fn(Option<u16>) -> Option<String>,
) -> Vec<StakeDistribution> {
    distributions
        .iter()
        .map(|distribution| {
            let mut stake_by: HashMap<String, u64> = Default::default();
            let mut count_by: HashMap<String, u64> = Default::default();
            for (key, stake) in distribution.stake_by.iter() {
                let folded = grouped_by(key, map);
                *stake_by.entry(folded.clone()).or_default() += stake;
                *count_by.entry(folded).or_default() +=
                    distribution.count_by.get(key).copied().unwrap_or_default();
            }
            let share_by = stake_by
                .iter()
                .map(|(key, stake)| {
                    let share = if distribution.total_stake > 0 {
                        *stake as f64 / distribution.total_stake as f64
                    } else {
                        0.0
                    };
                    (key.clone(), share)
                })
                .collect();
            StakeDistribution {
                epoch: distribution.epoch,
                total_stake: distribution.total_stake,
                stake_by,
                share_by,
                count_by,
            }
        })
        .collect()
}

fn client_diversity_stats(by_client_id: &[StakeDistribution]) -> Vec<ClientDiversityStats> {
    fold_distribution(by_client_id, client_vendor)
        .into_iter()
        .map(|distribution| ClientDiversityStats {
            epoch: distribution.epoch,
            total_activated_stake: distribution.total_stake,
            client_stake: distribution.stake_by,
            client_share: distribution.share_by,
            client_validator_count: distribution.count_by,
        })
        .collect()
}

fn client_lineage_stats(by_client_id: &[StakeDistribution]) -> Vec<ClientLineageStats> {
    fold_distribution(by_client_id, client_lineage)
        .into_iter()
        .map(|distribution| ClientLineageStats {
            epoch: distribution.epoch,
            total_activated_stake: distribution.total_stake,
            lineage_stake: distribution.stake_by,
            lineage_share: distribution.share_by,
            lineage_validator_count: distribution.count_by,
        })
        .collect()
}

pub fn load_client_diversity_stats(
    warehouse: &Warehouse,
    epochs: u64,
) -> anyhow::Result<Vec<ClientDiversityStats>> {
    let by_client_id = load_stake_distribution(warehouse, epochs, client_id_group)?;
    Ok(client_diversity_stats(&by_client_id))
}

pub fn load_client_lineage_stats(
    warehouse: &Warehouse,
    epochs: u64,
) -> anyhow::Result<Vec<ClientLineageStats>> {
    let by_client_id = load_stake_distribution(warehouse, epochs, client_id_group)?;
    Ok(client_lineage_stats(&by_client_id))
}

pub fn load_feature_set_stats(
    warehouse: &Warehouse,
    epochs: u64,
) -> anyhow::Result<Vec<FeatureSetStats>> {
    Ok(load_stake_distribution(warehouse, epochs, |validator| {
        validator
            .feature_set
            .map_or_else(|| UNKNOWN_CLIENT_GROUP.to_string(), |set| set.to_string())
    })?
    .into_iter()
    .map(|distribution| FeatureSetStats {
        epoch: distribution.epoch,
        total_activated_stake: distribution.total_stake,
        feature_set_stake: distribution.stake_by,
        feature_set_share: distribution.share_by,
        feature_set_validator_count: distribution.count_by,
    })
    .collect())
}

pub fn load_cluster_stats(warehouse: &Warehouse, epochs: u64) -> anyhow::Result<ClusterStats> {
    // Vendor and lineage are two folds of one per-client-id distribution.
    let by_client_id = load_stake_distribution(warehouse, epochs, client_id_group)?;
    Ok(ClusterStats {
        block_production_stats: load_block_production_stats(warehouse, epochs)?,
        dc_concentration_stats: load_dc_concentration_stats(warehouse, epochs)?,
        client_diversity_stats: client_diversity_stats(&by_client_id),
        client_lineage_stats: client_lineage_stats(&by_client_id),
        feature_set_stats: load_feature_set_stats(warehouse, epochs)?,
    })
}

pub fn aggregate_validators(validators: &[ValidatorRecord]) -> Vec<ValidatorsAggregated> {
    let mut epochs: HashSet<_> = Default::default();
    let mut epochs_start_dates: HashMap<u64, DateTime<Utc>> = Default::default();
    let mut marinade_scores: HashMap<u64, Vec<f64>> = Default::default();
    let mut apys: HashMap<u64, Vec<f64>> = Default::default();

    for validator in validators.iter() {
        for epoch_stats in validator.epoch_stats.iter() {
            epochs.insert(epoch_stats.epoch);
            epochs_start_dates.insert(
                epoch_stats.epoch,
                epoch_stats.epoch_start_at.unwrap_or(Utc::now()),
            );
            if let Some(score) = epoch_stats.score {
                marinade_scores
                    .entry(epoch_stats.epoch)
                    .or_default()
                    .push(score);
            }
            if let Some(apy) = epoch_stats.apy {
                apys.entry(epoch_stats.epoch).or_default().push(apy);
            }
        }
    }

    let mut agg: Vec<_> = epochs
        .into_iter()
        .map(|epoch| ValidatorsAggregated {
            epoch,
            epoch_start_date: epochs_start_dates.get(&epoch).copied(),
            avg_marinade_score: average(marinade_scores.get(&epoch).unwrap_or(&vec![])),
            avg_apy: average(apys.get(&epoch).unwrap_or(&vec![])),
        })
        .collect();

    agg.sort_by_key(|a| std::cmp::Reverse(a.epoch));

    agg
}

pub async fn load_validators_aggregated_flat(
    psql_client: &Client,
    last_epoch: u64,
    epochs: u64,
) -> anyhow::Result<Vec<ValidatorAggregatedFlat>> {
    let epochs = epochs.max(1);
    // last_version is deliberately left unbounded while the client columns are bounded to $2:
    // adding the bound changes what historical scoring runs see, so it needs a ds-sam side check.
    let rows = psql_client
            .query(
                "with
                cluster_stake AS (select epoch, sum(activated_stake) as stake from validators group by epoch),
                cluster_skip_rate AS (select epoch, sum(skip_rate * activated_stake) / sum(activated_stake) stake_weighted_skip_rate from validators group by epoch),
                dc AS (select validators.epoch, sum(activated_stake) / cluster_stake.stake as dc_concentration, dc_aso from validators LEFT JOIN cluster_stake ON validators.epoch = cluster_stake.epoch group by validators.epoch, dc_aso, cluster_stake.stake),
                agg_versions AS (select vote_account, (array_agg(version order by created_at desc, id desc) filter (where version is not null))[1] as last_version, (array_agg(client_id order by created_at desc, id desc) filter (where (client_id is not null or client_id_raw is not null) and epoch <= $2))[1] as last_client_id, (array_agg(client_id_raw order by created_at desc, id desc) filter (where (client_id is not null or client_id_raw is not null) and epoch <= $2))[1] as last_client_id_raw from versions group by vote_account)
                select
                    validators.vote_account,
                    min(activated_stake / 1e9)::double precision AS minimum_stake,
                    avg(activated_stake / 1e9)::double precision AS avg_stake,
                    coalesce(avg(dc_concentration), 0)::double precision AS avg_dc_concentration,
                    coalesce(avg(skip_rate), 1)::double precision AS avg_skip_rate,
                    coalesce(avg(case when leader_slots < 200 then least(skip_rate, cluster_skip_rate.stake_weighted_skip_rate) else skip_rate end), 1)::double precision AS avg_grace_skip_rate,
                    max(coalesce(commission_effective, commission_advertised, 100)) AS max_commission,
                    (coalesce(avg(credits * greatest(0, 100 - coalesce(commission_effective, commission_advertised, 100))), 0) / 100)::double precision AS avg_adjusted_credits,
                    coalesce((array_agg(validators.dc_aso ORDER BY validators.epoch DESC))[1], 'Unknown') dc_aso,
                    coalesce((array_agg((marinade_stake / 1e9)::double precision ORDER BY validators.epoch DESC))[1], 0) AS marinade_stake,
                    coalesce((array_agg(agg_versions.last_version))[1], '0.0.0') AS last_version,
                    (array_agg(agg_versions.last_client_id))[1] AS last_client_id,
                    (array_agg(agg_versions.last_client_id_raw))[1] AS last_client_id_raw
                FROM
                    validators
                    LEFT JOIN dc ON dc.dc_aso = validators.dc_aso AND dc.epoch = validators.epoch
                    LEFT JOIN cluster_skip_rate ON cluster_skip_rate.epoch = validators.epoch
                    LEFT JOIN agg_versions ON validators.vote_account = agg_versions.vote_account
                WHERE
                validators.epoch BETWEEN $1 AND $2
                GROUP BY validators.vote_account
                HAVING COUNT(*) = $3 AND COUNT(*) FILTER (WHERE credits > 0) >= 7
                ORDER BY avg_adjusted_credits DESC;
            ",
                &[&Decimal::from(last_epoch - u64::min(last_epoch, epochs - 1)), &Decimal::from(last_epoch), &i64::try_from(epochs).unwrap()],
            )
            .await?; // and vote_account not in ('3Z1N2Fkfha4ThNiRwN8RnU6U8dkFJ92DH2TFyLWJf8cj','2Dwg3x37yN4q8SyrrwDaRPGQTp14atcwMPewe3Y8FDoL','GkBrxrDjmx2kfTMUZJgYWAbar9fEpYJW7TgLatrZSjhN','5fdEXhCBKC7FRRsH64asZCSiwgNXRozxmzb1cFzfrtWM','7y4wStv8XxUkuBgwNkidfxdy1V6TMYr4UjaTDwcS3MUr','C33g1CBgcc47XFcrYksA3CEkBKaitKuhs9yD7LLtW98K','964w4qykexipZ7aCur1BEeJtexTMa1ehMUc9tCcxm9J3','Gj63nResvnBrKLw4GyyfWFpTudwQWDe9bExkE9Z1LvB1','9uygnf8zm2A88bS4tjqiYUPKAUuSWkJGeHxKLTndrs6v','A465fkGZut4A7FncUvzbCzGD8QE98yn2Lm8grr93c9dV','13zyX9jfGy1RvM28LcdqfLwR4VSowXx6whAL6AcFERCk','AUgLtpPVz6zL4iVCXZwi3cifLERdvnHsuVhNKzqmW45i','4R2eqfCDqN3UesKPW4kSTZVd55955V4awbof4vBuWibY','97AgcJPr1KGkwhq7tSD2LDMADreeCpGoFcX6hWjEuQpi','dVcK6ZibNvBiiwqxadXEGheznJFsWY9SHiMb8afTQns','C9pfCHG1zx5fTtmbsFwLG6yFoztyUVXoCmirUcCe2dt7','5hVPfoTZcfZTcyondKxjuVczaFap9pBGYBSPKXg9Jrg5','DXv73X82WCjVMsqDszK3z764tTJMU3nPXyCU3UktudBG','FvoZJRfV8LWMbkMeiswKzMHSZ2qvU8KVsEkUaW6MdN8m','6EftrAURp1rwpmy7Jeqem4kwWYeSnKmgYWKbdX5gEBHQ','HCZJjvZbaKaPTE96jz64HnBZTnXHBFv3pugqsBE5Z1D9','9fDyXmKS8Qgf9TNsRoDw8q2FJJL5J8LN7Y52sddigqyi','BHGHrKBJ9z6oE4Rjd7rBTsy9GLiFcTeDbkTkC5YmT5JG','CQCvXh6fDejoKVeMKXWirksnwCnhrLzb6XkyrBoQJzX5','Agsu9fcnH3rKBix59mktDRqJhjR8aStgLDd9njaddcdr','AUYVsW5ZGwPMAiFJUuAYPtCB9Xp5CVA1osJyAasj8CLe','FKCcfoLt2pq7boiNqRGucVq5LE1K5Dt4HALCx2WbEQkv','DHasctf9Gs2hRY2QzSoRiLuJnuEkRcGHSrh2JUxthxwa','467Bg8FwFFq5jqebWPnMQtdDRjpmHUqvCWBW1zYVMzHg','JCHsvHwF6TgeM1fapxgAkhVKDU5QtPox3bfCR5sjWirP','72i2i4D7ehg7k3rKUNZLFiHqvhVFv1UvdCQbiNTFudJa','G4RU9qUt7tG8M8E4L4ZfXtdnwPTcPpaWwLEvSxtdRNHF','JB8zjnRE6FeT8N6Yq2182vj69kKHGdeKJ7kBAhHKuHRq','Amhxcj1nt4BhnmTfy3ncqaoLzVr94QEfGMYY9Lqkg9en','783PrbTTsMojSJWv64ZCFnQ7mYoDj4oqdsAGXf22XVQs','FrWFgD5vfjJkKiCY4WeKBk65X4C7sDhi2X1DVMFCPfJ5','BeNvYv2pd3MRJBBSGiMPSRVYcafKAXocNNp79GoaHfoP','ZBfLjZjz48oS3ArtnjmPn4Fc1bd2VbKeBnxeCSrKE9S','DzhGmMUzpyQ5ruk5rRCfekTZMyvPXBXHtnn6aNnt94x4','8cUmk4UHZXFBXZJBnWnXd48iTSRMYikQ1QYbJddBfAxu','85qJ2DWmav9YgKLLdo6mrVAVLLKRH3fDuWPyiViA362n','5xk3gjstftRwZRqQdme4vTuZonpkgs2wsm734M68Bq1Y','7iC1Uu6QBqNG6oaBimnPgLmtoantH7Nc3RD7SoLHgVET','8sqpHTT3B8kLto6Vb98bNP93MVuRuPKdcUAwZaP6xuYs','4QMtvpJ2cFLWAa363dZsr46aBeDAnEsF66jomv4eVqu4','4rxFGSzXiTXuF9GveXbMr4fJAPPnQVjHmpEZbWV8jz9m','HC1NSDR9cbBeQ8V1XJ62VNceUAbjGdnCcH7f5wVFVZw3','Bm3rPaD62YWXJxvpW5viF9jUVdMmd7Q2HYA6eTbDhxxW','DeNodee9LR1WPokmRqidmAQEq8UbBqNCv6QfFTvU6k69','9xMyJXgxBABzV5bmiCuw4xZ8acao2xgvhC1G1yknW1Zj','9DYSMwSwMbQcckH1Zi7EQ3E6ipJKkChqRVJCQjF5FCWp','DqxQuDD9BZERufL2gTCipHhAqj7Bb2zoAEKfvHuXWNUL','FfE7rncxyYJvsqFu3Kn323sJpjBXkfMNXwd4d8kdURk9','9HvcpT4vGkgDU3TUXuJFtC5DPjMt5jb8MXFFTNTg9Mim','CrZEDyNQfbxakxdFYzMc8dtrYq4XDoRZ51xBa12skDpJ','D7wuZ935mznAM6hRJJQBpWcBWyvVgUK96pPDZT2uZq5g','9c5bpzVRbfsYY2fannb4hyX5CJUPg3BfH2cL6sR7kJM4','CnYYmAhuFcyocBbXxoVzPnu37a5ctpLaSr8ja1NGKNZ7','ESF3vCij1t6K437j7tzDyKspPeuMnYoEtooFN9Suzico','GHRvDXj9BfACkJ9CoLWbpi2UkMVti9DwXJGsaFT9XDcD','5HMtU9ngrq7vhQn4qPxFHzaVJRjbnT2VQxTTPdfwvbUL','3HSgNsx9rQsAFZrL7k2BAUuL8HpCjhgfxXjrPBK9cnjD','5GKFk6ptwtYTUVXZwofK3tgCJXRiQBfY6yS9w8dgZaSS','CZw1sSfjZbCccsk2kTjbFeVSgfzEgV1JxHEsEW69Qce9','6XimUrvgbdoAuavV9WGgSYdYKSw6ghajLGeQgZMG9aZU','13fUogQP3K8jAWgSW5gji5NyqHFprwoW3xVRs9MpLqdp','5KF6gMG6f5GCr4V6BXKzdroHxeXK68oKrLQdiujGsj9m','EAZpeduar1WoSCyR8W4YhurN3FfVmuKwdPx4ruy58VU8','4GhsFrzqekca4FiZQdwqbstzcEXsredqpemF9FdRQBqZ','BCFLyTNSoxQbVrTogK8n7ft1oYAgHYEdzafVfGgqz9WN','DREVB8Ce8nLp9Ha5m66sduRcjJtHeQo8B9BkYxjC4Zx3','A8XYMkTzKNceJT7BKtRwGrg5KGgaXcjyoAYuthrjfKUi','2e6hcXeqPMwskDfQXKuwVuHiFByEwaiG9ohgapNBk6qU','53gnaHMxDzGTZ9A58S4jbc1qzhYT4X51thUD4MdSBiyo','2kQZfvm5tqcBhXnscT3xe5SbCDttkipxgy1wCqhzqL2a','6vJGsbs5jYKEdQGUfMEYN4Nenwscgza1dBXB3WJraFyH','CGWR68eEdSDoj5LUn2MGKBgxRC3By64DCBFHCoHdSV21','DDmp7zGUzKhXsZhnUynohWrrKyWFf9gSJcGacihRRHuU','7yvrrixKhYrxMJHjzsPDz8tSAajLL3oD2arAsgeMdK9N','9DsSqMHnrSXkyHtG8sN4zPhjrsRUgfP9vBQ6hFEpEwM','5HedSkUKfYmusiV7rAppuHbz7fp8JmUoLLJjJqCQLS5j','725My2yzg5ZUpQtpEtivLT7JmRes2gGxF3KeGCbYACDe','9Mi8M1JnRmtcYpB42DxYPVmYy2safgdYFmeHmMgkW8TG','2jevuBmk1TrXA36bRZ4bhdJGPzGqcCDoVzRcyYtxzKHY','CHUF69YeA3gZv484izYuhKk1EjaJYjv1pNoJJ6QeFDQc','8b3JPQtHbw8MBJQNwUDVXC6xfaL26UNx6WA3GShGy5Vw','HHFqy4NJteQJScyoAsjwYjS8wCuV1AjNv4veoeKVACRi','DMYn88X6PkHAc2y5zDWm5jGZ2Tk2CyBUe8K1U2obF8jc','C31ocJKVAi8wxCvyAMjXte2fY9zECV2fKrn786F4WZ4N','6g6jypXGeavZPVkWSu4Ny5bfhTMLFnuSepfGMQkQpWV1','D6AkdRCEAvE8Rjh4AKDSCXZ5BaiKpp79da6XtUJotGq5','3Le35iTn2KXRfomruXiDLcMd4BVLKYVgQ7yssmrFJZXx','9TTpcbiTDUQH9goeRvhAhk4X3ahtZ6XttCjRyH8Pu7MP','7yXM5mUSAtBuh2TcCABvSJa3LouZ8wcLps5zTEMiwxvj','LSV1yYBUsxwY8y7AgL2RcJDVJjnwxfeShxaXm7Edrwr','FQY5UU6THEhRNZRg7YXfYGQhJi45TLXrHg76EsXJmESc','9TUJdBxnHvAapYoq8cVFgh1bMbTh6GYfY4etDqWVKXAT','FSH9xke8FBpx6YxEEzNXVWgjmT3G5SN9HpipmCSVamV','CFtrZKxqGfXSuZrM5G64prTfNM8GqWQFQa3GXq4tdzx2','BifNttkf51HzsPgUf2keDdVBL64YvnAVQGF3fkNDfB56','4e1B3jra6oS7nK5wLn9mPMtX1skUJsEvmhV9MscA6UA4','DZJWKjtj1fCJDWTYL1HvF9rLrxRRKKp6GQgyujEzqc22','3YKcH4c8eoAKkghQeGavg9HZ13fSe77RWM3QoFTCV2Gv','7r4qcfiaWaZ8i9zW2YjqgRLEpGGE7L5hfW8ncpF1HdTs','8aGU18Nxn99AEWEQNrBYy1ZsJBhiHVFrcqYqHQPNhmEv','5TFdzjKE6LnkhQArxWjt26yVCXskPo3fUXE8F351Cfn7','DpodNd2DWRLbNJVuV1R33xW2PkBJyRTFU5aZGmQrVtMi','Dpy1qt9MhoRD5YpmzfM9iBw2LuRvP1nZAa9La77nAHW5','8kcrp8M2c5LGYThHQxVgsp7BGfGjHZ9fLHa6YN3YpFNa','BgjQPDdsHeD9XXs7pYyHsmvKdkLR1A4SBNYs3mLmPUCD','9EzbogBnGi8hVeLXEyFu2xUo6qi5JdEELs4y3cQXQW33','9uASGafRPWpvpfXeuwcA3TzMUuP5BoHfQWtcdGMyYR9x','7aE66BtyfPELpp6hnb6qb3PjQzmzMqXRMKx9EU7tHak6','6JjWRGRM94G2cpnsqDD3KL8p4ravnFSpJP778V6M9LUS','AeHBkLDeWtMHqrM4uwuewwWKtyKd5aBZAygxZJ3MCjRp','6c2FJC1NfzNvivapAzPW8vj9TW63dpHCVh7zzehwnNLH','D7ZCDE1PHe8duMjNpxwHrYbrRzcnsS7p4nD2daLzWwtr','DaNexGpPeQChZTPZAn1BGmd4ASQpHE4hLDv7V4iAe2oA','8HciLEx6hGdb8mxaCx7ExFBxkcgdkpt43FhiJdvPA6XZ','ouLzBTp7vqzT1mhjtg1TpYHwACJpeB5akRC1zDVdg1N','AQB1eoovP55TyjkecjCPTvfXBEzS1JH1sxWguBo1gu9d','J9Go27V87fCdJtjMxmFJu48ctrHzFoe6xQpA6Ecq4Wkw','Fu4wz4US6dV6GzZrv9NnF18KeT47tdbDKRd7pA6DiyS4','CwEsA6kkUZHnuCK2HC1WVpriBpZWFJSKW9xxrdednm6J','5bjKPhoQDcpPVeMhu83SEtXqXA9vw62k7KhL9zpsK31b','2ikGwX24ATJQHPtWpHupEAJvAyp63niaFL5R2sGXwfnd','GAerry2FZncXgLJXohjgGmC3W4JKLDFxwhGz4beTgDeP','8sNLx7RinHfPWeoYE1N4dixtNACvgiUrj9Ty81p7iMhb','9EMmPd6zKqTnpj74rgmkTjkYAsZSZ42jBWcqu6iaoGTR','3v6FfdWMT2bcoQQ9hN4F2syu7qhRHzNuCPPQqV12hsw2','BYNXBFkB89FoRCJ4VxFE9Tfde3anECjZjasTP8qSYQUi','3iD36QhXqWzx5b4HHhkRAyUcbEgCaC42hi1GcBePNsp2','8hpaVczvUK24kogYWxV6s3hajDAbaHb6KGZsVRLDoksi','J61sYWwTT3Kfkjy3gJ1ViRwtfXVp7Bi89DLqvCp5WDgC','A9hwhEeQ7hNm8rPbRX7ZDAZRjTVrUCjgDEDD4Tt8rmT7','9b9F4xYHMenZfbD8pSLm45oJfoFYPQ9RVWPXSEmJQzVn','DawVi7TKkWS81ZKyGTmxLAabL1w5gcw8FhGgbHeGJGnj','5ycsa9WVK6zFcUR13m3yijxbevZehZeCbuSocSawsweW','4qS6unxhpNh6fp2rRU3nnyMZEYyZ4hUbjnP7iEN7Jx1w','BxTUwfMiokzimVDLDupGfVPmWXfLSGVpkGr9TUmetn6b','8J4xNmyAQskmPuyywPf1arig4X8hfza2xKBkKwz8E2gU','6zDrZWRXQ7GWi1W2fBTzSs59PSa2uj2k8w2qkc451rqG','74ibS6YRDBF3jMf3bxiLY1i3ohFhJySQwyeeMWaRsAk2','HTpinijYNYPe2UhfwoX7fHKC9j44QEJoVmStCmfvYZxA','76sb4FZPwewvxtST5tJMp9N43jj4hDC5DQ7bv8kBi1rA','FKyoehgzXD6KVSQoHJuTteXGCrChYe65k98wMckr9MN8','4qSZsB9QjXr97HzhzPd1zuvB8z7tqqDuM1xbxB5PcPFh','1M5USfamd1N4i1z6UZeECrWeu2VfrxjYMBSXThu6TqB','7LCnWqQGpNCiUvBLznYG9Q6Zo7mcLkhAHA7YBjbg8SET','AU4yDLbrnLzcjk2pnxvXwNeKJsj9CiUDRXWQbeSbk6Y9','AeSLUUNmADEM2xzfmWbRhfwomvJW3f3Rd1AdicXf27Gb','4RyNsFHDccFEEnbYJAFt2cNufFduh8Se8eKTqXDVr82h','5enTTfG63W4JUzCpwioeLte7827NrYXUgGr6z7Rm7xf5','8usnMxy6YunbfrjHDHPfRcpLWXigcSvrpVohv3F2v24H','J4ooR8AV8o5Ez2qN8ghQhR7YKhqRY5WEHfE8dTR2Yo6a','HFY5f6PF6cRyVAvVG1xV9X15q87qoZ1o6GDcyBzHSEnX')

    let mut validators: Vec<ValidatorAggregatedFlat> = Default::default();
    for row in rows.iter() {
        // The shared filter plus the id tiebreaker make both aggregates read off one versions row.
        let last_client_id_raw: Option<String> = row.get("last_client_id_raw");
        let last_client_id = effective_client_id(
            row.get::<_, Option<i32>>("last_client_id")
                .map(|n| n as u16),
            last_client_id_raw.as_deref(),
        );
        validators.push(ValidatorAggregatedFlat {
            vote_account: row.get("vote_account"),
            minimum_stake: row.get("minimum_stake"),
            avg_stake: row.get("avg_stake"),
            avg_dc_concentration: row.get("avg_dc_concentration"),
            avg_skip_rate: row.get("avg_skip_rate"),
            avg_grace_skip_rate: row.get("avg_grace_skip_rate"),
            max_commission: row.get::<_, i32>("max_commission").try_into()?,
            avg_adjusted_credits: row.get("avg_adjusted_credits"),
            dc_aso: row.get("dc_aso"),
            marinade_stake: row.get("marinade_stake"),
            version: row.get("last_version"),
            client_vendor: client_vendor(last_client_id)
                .unwrap_or_else(|| UNKNOWN_CLIENT_GROUP.to_string()),
            client_lineage: client_lineage(last_client_id)
                .unwrap_or_else(|| UNKNOWN_CLIENT_GROUP.to_string()),
        });
    }

    Ok(validators)
}

fn map_to_ordered_component_values(
    components: &Vec<&str>,
    row: &ValidatorScoringCsvRow,
) -> Vec<Option<String>> {
    components
        .iter()
        .map(|component| match *component {
            "COMMISSION_ADJUSTED_CREDITS" => Some(row.avg_adjusted_credits.to_string()),
            "GRACE_SKIP_RATE" => Some(row.avg_grace_skip_rate.to_string()),
            "DC_CONCENTRATION" => Some(row.avg_dc_concentration.to_string()),
            _ => None,
        })
        .collect()
}

pub async fn store_scoring(
    psql_client: &mut Client,
    epoch: i32,
    ui_id: String,
    components: Vec<&str>,
    component_weights: Vec<f64>,
    scores: Vec<ValidatorScoringCsvRow>,
) -> anyhow::Result<()> {
    // One transaction, so MAX(scoring_run_id) never becomes visible ahead of that run's scores.
    let tx = psql_client.transaction().await?;

    let scoring_run_result = tx
        .query_one(
            "INSERT INTO scoring_runs (created_at, epoch, components, component_weights, ui_id)
            VALUES (now(), $1, $2, $3, $4) RETURNING scoring_run_id;",
            &[&epoch, &components, &component_weights, &ui_id],
        )
        .await?;

    let scoring_run_id: i64 = scoring_run_result.get("scoring_run_id");

    log::info!("Stored scoring run: {scoring_run_id}");

    let component_scores_by_vote_account: HashMap<_, _> = scores
        .iter()
        .map(|row| {
            (
                row.vote_account.clone(),
                Vec::from([
                    row.normalized_adjusted_credits,
                    row.normalized_grace_skip_rate,
                    row.normalized_dc_concentration,
                ]),
            )
        })
        .collect();

    let component_ranks_by_vote_account: HashMap<_, _> = scores
        .iter()
        .map(|row| {
            (
                row.vote_account.clone(),
                Vec::from([
                    row.rank_adjusted_credits,
                    row.rank_grace_skip_rate,
                    row.rank_dc_concentration,
                ]),
            )
        })
        .collect();

    let component_values_by_vote_account: HashMap<_, _> = scores
        .iter()
        .map(|row| {
            (
                row.vote_account.clone(),
                map_to_ordered_component_values(&components, row),
            )
        })
        .collect();

    let ui_hints_parsed: HashMap<_, Vec<&str>> = scores
        .iter()
        .map(|row| {
            (
                row.vote_account.clone(),
                if row.ui_hints.is_empty() {
                    Default::default()
                } else {
                    row.ui_hints.split(",").collect()
                },
            )
        })
        .collect();

    for chunk in scores.chunks(500) {
        let mut query = InsertQueryCombiner::new(
            "scores".to_string(),
            "vote_account, score, component_scores, component_ranks, component_values, vemnde_votes, msol_votes, rank, ui_hints, eligible_stake_algo, eligible_stake_vemnde, eligible_stake_msol, target_stake_algo, target_stake_vemnde, target_stake_msol, scoring_run_id".to_string(),
        );
        for row in chunk {
            let mut params: Vec<&(dyn ToSql + Sync)> = vec![
                &row.vote_account,
                &row.score,
                component_scores_by_vote_account
                    .get(&row.vote_account)
                    .unwrap(),
                component_ranks_by_vote_account
                    .get(&row.vote_account)
                    .unwrap(),
                component_values_by_vote_account
                    .get(&row.vote_account)
                    .unwrap(),
                &row.vemnde_votes,
                &row.msol_votes,
                &row.rank,
                ui_hints_parsed.get(&row.vote_account).unwrap(),
                &row.eligible_stake_algo,
                &row.eligible_stake_vemnde,
                &row.eligible_stake_msol,
                &row.target_stake_algo,
                &row.target_stake_vemnde,
                &row.target_stake_msol,
                &scoring_run_id,
            ];
            query.add(&mut params);
        }
        query.execute_in(&tx).await?;
    }

    tx.commit().await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_fixed_for_sort_scales_usable_values() {
        assert_eq!(to_fixed_for_sort(0.0), Some(0));
        assert_eq!(to_fixed_for_sort(0.05), Some(500));
        assert_eq!(to_fixed_for_sort(1.0), Some(10_000));
    }

    #[test]
    fn to_fixed_for_sort_rejects_values_that_would_saturate_to_zero() {
        assert_eq!(to_fixed_for_sort(-0.01), None);
        assert_eq!(to_fixed_for_sort(f64::NAN), None);
        assert_eq!(to_fixed_for_sort(f64::NEG_INFINITY), None);
    }

    #[test]
    fn to_fixed_for_sort_rejects_values_that_would_saturate_to_max() {
        assert_eq!(to_fixed_for_sort(f64::INFINITY), None);
        assert_eq!(to_fixed_for_sort(f64::MAX), None);
        assert_eq!(to_fixed_for_sort(1e30), None);
    }

    const BASELINE_SLOTS_PER_YEAR: f64 = 78_892_314.984;
    const SLOTS_PER_YEAR_350MS: f64 = 90_162_645.696;

    const CREDITS: u64 = 400_000;

    /// Mainnet-scale: 600M SOL supply, ~400k credits per validator, ~400M SOL staked cluster-wide.
    fn calculator(slots_per_year: f64) -> InflationApyCalculator {
        InflationApyCalculator {
            supply: 600_000_000_000_000_000,
            duration: 182_400,
            inflation: 0.043,
            slots_per_year,
            total_weighted_credits: 160_000_000_000_000_000_000_000,
        }
    }

    /// Relative, because these quantities span 1e-2 to 1e15 and a fixed epsilon fits neither end.
    fn assert_close(left: f64, right: f64) {
        assert!(
            (left - right).abs() / right.abs() < 1e-12,
            "{left} != {right}"
        );
    }

    fn rate_per_epoch(calculator: &InflationApyCalculator) -> f64 {
        let (apr, _) = calculator.estimate_yields(CREDITS, 5);
        apr / (SECONDS_IN_YEAR / calculator.duration as f64)
    }

    #[test]
    fn per_epoch_issuance_tracks_the_protocol_slot_time() {
        let baseline = calculator(BASELINE_SLOTS_PER_YEAR);
        let stage_1 = calculator(SLOTS_PER_YEAR_350MS);
        let (_, apy_baseline) = baseline.estimate_yields(CREDITS, 5);
        let (_, apy_350) = stage_1.estimate_yields(CREDITS, 5);

        // Guards the fixture: an implausible one overflows to inf, where every ratio below matches.
        assert!((0.03..0.12).contains(&apy_baseline), "{apy_baseline}");

        // Shorter slots mint proportionally less per epoch, so the rate scales by exactly 350/400.
        assert_close(
            rate_per_epoch(&stage_1) / rate_per_epoch(&baseline),
            350.0 / 400.0,
        );

        let epochs_per_year = SECONDS_IN_YEAR / stage_1.duration as f64;
        assert_close(
            1.0 + apy_350,
            (1.0 + rate_per_epoch(&stage_1)).powf(epochs_per_year),
        );
        assert!(apy_350 < apy_baseline);
    }

    #[test]
    fn measured_epoch_length_does_not_move_per_epoch_issuance() {
        let short = InflationApyCalculator {
            duration: 151_200,
            ..calculator(BASELINE_SLOTS_PER_YEAR)
        };
        let long = InflationApyCalculator {
            duration: 182_400,
            ..calculator(BASELINE_SLOTS_PER_YEAR)
        };

        // Only the compounding exponent may depend on the measured epoch, never the minted amount.
        assert_close(rate_per_epoch(&short), rate_per_epoch(&long));
    }

    // Roughly mainnet's mix at epoch 1015, so the numbers below read against something real.
    const MIX: RewardMixShares = RewardMixShares {
        inflation: 0.90,
        mev: 0.044,
        block: 0.056,
    };

    fn approx(actual: Option<f64>, expected: f64) {
        let actual = actual.expect("expected a rate");
        assert!(
            (actual - expected).abs() < 1e-12,
            "expected {expected}, got {actual}"
        );
    }

    #[test]
    fn expected_take_rate_floors_at_the_block_share_for_a_zero_fee_validator() {
        // HelixNode's shape: 0% inflation, 0% MEV, no priority-fee account. It measures ~5.6%.
        approx(expected_take_rate(MIX, Some(0), Some(0), None), MIX.block);
    }

    #[test]
    fn expected_take_rate_reaches_one_when_every_component_is_fully_taken() {
        approx(
            expected_take_rate(MIX, Some(100), Some(10_000), Some(10_000)),
            1.0,
        );
    }

    #[test]
    fn expected_take_rate_weights_each_commission_by_its_component() {
        approx(
            expected_take_rate(MIX, Some(5), Some(1_000), None),
            0.05 * MIX.inflation + 0.1 * MIX.mev + MIX.block,
        );
    }

    #[test]
    fn expected_take_rate_drops_below_the_floor_when_block_rewards_are_shared() {
        // The two validators on Jito's PriorityFeeDistribution at 0 bps keep none of their priority fees.
        approx(expected_take_rate(MIX, Some(0), Some(0), Some(0)), 0.0);
    }

    #[test]
    fn expected_take_rate_renormalizes_when_the_validator_earns_no_mev() {
        // Without Jito there is no MEV to take a cut of, so the MEV weight must leave the denominator
        // rather than count as a 0% commission and dilute the rate.
        let no_jito = expected_take_rate(MIX, Some(10), None, None);
        approx(
            no_jito,
            (0.1 * MIX.inflation + MIX.block) / (MIX.inflation + MIX.block),
        );

        let diluted = 0.1 * MIX.inflation + MIX.block;
        assert!(
            no_jito.unwrap() > diluted,
            "renormalizing must read higher than crediting a 0% MEV commission"
        );
    }

    #[test]
    fn expected_take_rate_is_unknown_without_an_inflation_commission() {
        // Inflation rewards are earned by every validator, so a missing commission cannot renormalize away the way a missing MEV one does.
        assert_eq!(expected_take_rate(MIX, None, Some(0), Some(0)), None);
    }

    #[test]
    fn expected_take_rate_needs_at_least_one_component_to_weigh() {
        let empty = RewardMixShares {
            inflation: 0.0,
            mev: 0.0,
            block: 0.0,
        };
        assert_eq!(expected_take_rate(empty, Some(5), Some(1_000), None), None);
    }
}
