use crate::docs::{SnapshotDoc, UptimeStatus, VersionSample};
use crate::dto::{
    client_label, client_lineage, client_name, client_vendor, effective_client_id,
    BlockProductionStats, ClientDiversityStats, ClientLineageStats, ClusterStats, CommissionRecord,
    DCConcentrationStats, FeatureSetStats, RugInfo, RuggerRecord, ScoringRunRecord, UptimeRecord,
    Validator, ValidatorAggregatedFlat, ValidatorEpochStats, ValidatorRecord, ValidatorScoreRecord,
    ValidatorWarning, ValidatorsAggregated, VersionRecord,
};
use crate::incidents::{
    CommissionRaise, DowntimeInterval, EpochBlockProduction, ValidatorIncidents,
    COMMISSION_SPIKE_THRESHOLD_PERCENTAGE,
};
use crate::validators_jito::get_last_jito_info;
use crate::validators_sandwiches::load_validator_sandwiches;
use crate::warehouse::Warehouse;
use chrono::{DateTime, Utc};
use collect::take_rates::query_validator_rewards;
use google_cloud_bigquery::client::{Client as BqClient, ClientConfig as BqClientConfig};
use google_cloud_bigquery::http::job::query::QueryRequest;
use google_cloud_bigquery::query::row::Row;
use rust_decimal::prelude::*;
use std::{
    collections::{HashMap, HashSet},
    ops::RangeInclusive,
    time::Duration,
};

#[cfg(test)]
#[path = "utils_test.rs"]
mod utils_test;

/// Default number of recent epochs the API loads/serves (validators, uptimes, events, ...).
pub const DEFAULT_CACHE_EPOCHS: u64 = 90;

/// Agave's year: the same one every `slots_per_year` row annualises to, so nominal and measured stay comparable.
const SECONDS_IN_YEAR: f64 = 31556925.9936;
pub use collect::slot_params::SLOTS_IN_EPOCH;
/// Timeout for outbound HTTP calls to sibling services (scoring, validator-bonds). Without it a
/// hung upstream would stall the whole cache-warmer loop, freezing every cache type's refresh.
const HTTP_TIMEOUT_S: u64 = 60;

pub fn to_fixed(a: f64, decimals: i32) -> u64 {
    (a * 10f64.powi(decimals)).round() as u64
}

// Guarding the scaled value, not the input: the multiplication is what overflows, and the u64 cast then saturates to either end and reads as a genuine rank.
pub fn to_fixed_for_sort(a: f64) -> Option<u64> {
    let scaled = (a * 10f64.powi(4)).round();
    (scaled >= 0.0 && scaled < u64::MAX as f64).then_some(scaled as u64)
}

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

        let calculator = InflationApyCalculator {
            supply: match epoch_record.supply.try_into() {
                Ok(supply) => supply,
                Err(err) => {
                    log::warn!("Skipping APY calculator for epoch {epoch}: supply: {err:#}");
                    continue;
                }
            },
            duration: match (epoch_record.end_at - epoch_record.start_at)
                .num_seconds()
                .try_into()
            {
                Ok(duration) => duration,
                Err(err) => {
                    log::warn!("Skipping APY calculator for epoch {epoch}: duration: {err:#}");
                    continue;
                }
            },
            inflation: epoch_record.inflation,
            slots_per_year: epoch_record.slots_per_year,
            total_weighted_credits,
        };
        result.insert(*epoch, calculator);
    }

    Ok(result)
}

/// How far back to accept a validator's latest Jito commissions. Wide enough to survive an epoch
/// with no distribution account written, short enough that a long-departed validator reads as absent.
const DEFAULT_JITO_COMMISSION_EPOCHS: u64 = 10;

/// Every raise over the bar per vote account. Reads one epoch further back than the window: a raise
/// in its first epoch needs the rate it moved from, and the commission stream carries a sample per
/// vote account per epoch even where nothing changed. A validator that never raised is left out, so
/// the caller does not mint a cache entry for every vote account the stream ever saw.
fn load_commission_raises(
    warehouse: &Warehouse,
    epochs: RangeInclusive<u64>,
) -> anyhow::Result<HashMap<String, Vec<CommissionRaise>>> {
    let (from_epoch, last_epoch) = (*epochs.start(), *epochs.end());
    let mut samples = warehouse.commission_changes(from_epoch.saturating_sub(1)..=last_epoch);
    samples.sort_by_key(|(vote_account, change)| {
        (
            *vote_account,
            change.epoch,
            change.epoch_slot,
            change.created_at,
        )
    });

    let mut raises: HashMap<String, Vec<CommissionRaise>> = Default::default();
    let mut previous: Option<(&String, u8)> = None;
    // Vote account, epoch and index of the raise whose peak later samples of the same epoch still lift.
    let mut peak: Option<(&String, u64, usize)> = None;
    for (vote_account, change) in samples {
        let commission: u8 = change.commission.try_into()?;
        let epoch = change.epoch;

        // The epoch under the window is read for its rate alone: a raise there sits before it.
        let crossed = previous.is_some_and(|(before_account, before)| {
            before_account == vote_account
                && before < COMMISSION_SPIKE_THRESHOLD_PERCENTAGE
                && commission >= COMMISSION_SPIKE_THRESHOLD_PERCENTAGE
                && epoch >= from_epoch
        });

        if crossed {
            let commission_before = previous.map_or(0, |(_, before)| before);
            let raised = raises.entry(vote_account.clone()).or_default();
            raised.push(CommissionRaise {
                epoch,
                epoch_slot: change.epoch_slot,
                changed_at: change.created_at,
                commission_before,
                commission_after: commission,
            });
            peak = Some((vote_account, epoch, raised.len() - 1));
        } else if commission < COMMISSION_SPIKE_THRESHOLD_PERCENTAGE {
            peak = None;
        } else if let Some((peak_account, peak_epoch, index)) = peak {
            if peak_account == vote_account && peak_epoch == epoch {
                if let Some(raise) = raises.get_mut(peak_account).and_then(|r| r.get_mut(index)) {
                    raise.commission_after = raise.commission_after.max(commission);
                }
            } else {
                peak = None;
            }
        }

        previous = Some((vote_account, commission));
    }

    Ok(raises)
}

/// The raw material of every incident kind, per vote account, over `epochs`.
/// The block production and client version kinds are read off the same
/// epoch stats the incidents are handed back for, so the cluster figure and
/// the validator it judges come from one snapshot.
pub fn load_validator_incidents(
    warehouse: &Warehouse,
    epochs: RangeInclusive<u64>,
    records: &HashMap<String, ValidatorRecord>,
) -> anyhow::Result<ValidatorIncidents> {
    let mut incidents = ValidatorIncidents::default();

    let mut downtimes: Vec<(&String, &crate::docs::UptimeInterval)> = warehouse
        .uptime_intervals(epochs.clone())
        .into_iter()
        .filter(|(_, interval)| interval.status == UptimeStatus::Down)
        .collect();
    downtimes.sort_by_key(|(_, interval)| interval.start_at);
    for (vote_account, interval) in downtimes {
        incidents
            .records(vote_account)
            .downtimes
            .push(DowntimeInterval {
                epoch: interval.epoch,
                start_at: interval.start_at,
                end_at: interval.end_at,
                downtime_seconds: (interval.end_at - interval.start_at)
                    .num_seconds()
                    .try_into()?,
            });
    }

    for (vote_account, raises) in load_commission_raises(warehouse, epochs.clone())? {
        incidents.records(&vote_account).commission_raises = raises;
    }

    for (vote_account, sandwiches) in load_validator_sandwiches(warehouse, epochs.clone()) {
        incidents.records(&vote_account).sandwiches = sandwiches;
    }

    let cluster_skip_rates = crate::incidents::cluster_skip_rates(records.values());
    let newer_stake_shares = crate::incidents::newer_stake_shares(records.values());

    for (vote_account, record) in records {
        for stats in record
            .epoch_stats
            .iter()
            .filter(|stats| epochs.contains(&stats.epoch))
        {
            // No cluster figure, no bar to measure against, so the epoch stays unrecorded.
            let Some(production) = cluster_skip_rates
                .get(&stats.epoch)
                .and_then(|cluster_skip_rate| EpochBlockProduction::new(stats, *cluster_skip_rate))
            else {
                continue;
            };
            incidents
                .records(vote_account)
                .block_production
                .push(production);
        }

        let late_patch = crate::incidents::running_late_client_version_incidents(
            crate::incidents::epochs_running_late_client_version(record, &newer_stake_shares),
            epochs.clone(),
        );
        if !late_patch.is_empty() {
            incidents.records(vote_account).running_late_client_versions = late_patch;
        }
    }

    Ok(incidents)
}

pub fn load_uptimes(
    warehouse: &Warehouse,
    epochs: u64,
) -> anyhow::Result<HashMap<String, Vec<UptimeRecord>>> {
    let mut records: HashMap<String, Vec<UptimeRecord>> = Default::default();

    for (vote_account, interval) in warehouse.uptime_intervals(warehouse.window(epochs)) {
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

    for (vote_account, change) in warehouse.version_changes(warehouse.window(epochs)) {
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

/// One epoch of a validator's commission as the rug rule reads it.
struct CommissionPoint {
    epoch: u64,
    effective: Option<i32>,
    min_observed: Option<i32>,
    /// The lowest rate the validator carried into the epoch it was paid at:
    /// the smaller of E-2's sample floor and advertised rate, where the
    /// validator has a record at exactly E-2. From 1030 on
    /// `commission_effective` is close-epoch's vote-state sample at the
    /// epoch_stakes vintage, so a cut it still lags must not read as a rug.
    vintage_floor: Option<i32>,
}

/// A rug is a rise above the validator's own floor to over 10%, a dip to 10%
/// or under between two epochs over it, or a spike over 10% between two
/// epochs at or under it. Every rule needs the epoch's observed floor.
pub fn load_ruggers(warehouse: &Warehouse) -> HashMap<String, RuggerRecord> {
    let mut sample_floors: HashMap<(&String, u64), i32> = Default::default();
    for (vote_account, change) in warehouse.commission_changes(0..=u64::MAX) {
        let floor = sample_floors
            .entry((vote_account, change.epoch))
            .or_insert(change.commission);
        *floor = (*floor).min(change.commission);
    }

    let mut series: HashMap<&String, Vec<CommissionPoint>> = Default::default();
    for (epoch, snapshot) in warehouse.snapshots.iter() {
        for (vote_account, validator) in snapshot.iter() {
            let vintage_floor = epoch.checked_sub(2).and_then(|vintage| {
                let carried = warehouse.snapshots.get(&vintage)?.get(vote_account)?;
                [
                    sample_floors.get(&(vote_account, vintage)).copied(),
                    carried.commission_advertised,
                ]
                .into_iter()
                .flatten()
                .min()
            });
            series
                .entry(vote_account)
                .or_default()
                .push(CommissionPoint {
                    epoch: *epoch,
                    effective: validator.commission_effective,
                    min_observed: validator.commission_min_observed,
                    vintage_floor,
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
            let (Some(effective), Some(min_observed)) = (point.effective, point.min_observed)
            else {
                continue;
            };
            let above_its_own_floor = effective > min_observed
                && effective > point.vintage_floor.unwrap_or(-1)
                && effective > 10
                && min_observed <= 10;
            let dipped =
                previous.is_some_and(|p| p > 10) && effective <= 10 && next.is_some_and(|n| n > 10);
            let spiked = previous.is_some_and(|p| p <= 10)
                && effective > 10
                && next.is_some_and(|n| n <= 10);
            if above_its_own_floor || dipped || spiked {
                rugs.push((point.epoch, effective, min_observed));
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

    for (vote_account, change) in warehouse.commission_changes(warehouse.window(epochs)) {
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
        // SIMD-0232 nulled commission_effective; folding that into 0 read as a free validator.
        let max_known_commission = validator
            .epoch_stats
            .iter()
            .filter(|stat| epochs_range.contains(&stat.epoch))
            .filter_map(|epoch_stats: &ValidatorEpochStats| {
                worst_known_commission(
                    epoch_stats.commission_max_observed.map(i32::from),
                    epoch_stats.commission_advertised.map(i32::from),
                )
            })
            .max();
        if max_known_commission.is_some_and(|commission| commission > 10) {
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

/// Per-validator take rate over the last `TAKE_RATE_WINDOW_DAYS`, collapsed from the per-epoch rows
/// `query_validator_rewards` returns, plus the cluster reward mix those same rows sum to. Windowed by
/// `epochs.epoch_end_time` (same as apy-api).
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

    let rows = query_validator_rewards(&bq_client, min_epoch).await?;

    // Lamports summed over every validator and epoch in the window; u128 so the cluster-wide
    // accumulation is not what overflows.
    let mut per_validator: HashMap<String, (u128, u128)> = Default::default();
    let mut cluster_inflation: u128 = 0;
    let mut cluster_mev: u128 = 0;
    let mut cluster_block: u128 = 0;

    for row in &rows {
        let (validator_rewards, total_rewards) =
            per_validator.entry(row.vote_account.clone()).or_default();
        *validator_rewards += u128::from(row.validator_rewards);
        *total_rewards += u128::from(row.total_rewards);
        cluster_inflation += u128::from(row.inflation_rewards);
        cluster_mev += u128::from(row.mev_rewards);
        cluster_block += u128::from(row.block_rewards);
    }

    // Ratio of sums, not a mean of the per-epoch rates: a big epoch has to weigh more than a small one.
    let measured = per_validator
        .into_iter()
        .filter(|(_, (_, total_rewards))| *total_rewards > 0)
        .map(|(vote_account, (validator_rewards, total_rewards))| {
            (
                vote_account,
                validator_rewards as f64 / total_rewards as f64,
            )
        })
        .collect();

    let cluster_total = cluster_inflation + cluster_mev + cluster_block;
    let shares = (cluster_total > 0).then(|| RewardMixShares {
        inflation: cluster_inflation as f64 / cluster_total as f64,
        mev: cluster_mev as f64 / cluster_total as f64,
        block: cluster_block as f64 / cluster_total as f64,
    });

    Ok(TakeRates { measured, shares })
}

/// What the validator's own fee settings imply it keeps, weighted by the cluster reward mix.
/// Renormalized over the components it actually earns: a validator not running Jito receives no MEV
/// at all, so crediting it a 0% MEV commission would dilute the rate it takes on what it does earn.
/// None when the inflation commission is unknown, or when the mix carries no inflation to weight it by.
/// Inflation is the one component no validator can opt out of.
pub fn expected_take_rate(
    shares: RewardMixShares,
    inflation_commission_pct: Option<i32>,
    mev_commission_bps: Option<i32>,
    priority_commission_bps: Option<i32>,
) -> Option<f64> {
    // An in-progress epoch has paid no inflation or MEV yet, leaving a mix of pure block rewards.
    if shares.inflation <= 0.0 {
        return None;
    }

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

    Some(weighted / weight)
}

/// The higher of the last closed epoch's observed ceiling and what is advertised now: a validator moving its commission inside an epoch advertises the low end, so a rise counts at once while a cut waits for the epoch to close.
pub fn worst_known_commission(
    commission_max_observed: Option<i32>,
    commission_advertised: Option<i32>,
) -> Option<i32> {
    commission_max_observed.max(commission_advertised)
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

    let mut first_epochs: HashMap<&String, u64> = Default::default();
    for (epoch, snapshot) in warehouse.snapshots.iter() {
        for vote_account in snapshot.keys() {
            let first_epoch = first_epochs.entry(vote_account).or_insert(*epoch);
            *first_epoch = (*first_epoch).min(*epoch);
        }
    }

    log::info!("Aggregating validator records...");
    let mut records: HashMap<String, ValidatorRecord> = Default::default();
    let mut seeding_epochs: HashMap<&String, u64> = Default::default();
    let mut projected_node_metadata: HashSet<&String> = Default::default();
    let window_start = warehouse.window_start(display_epochs);

    for (epoch, snapshot) in warehouse.snapshots.range(window_start..).rev() {
        let epoch = *epoch;
        let epoch_record = warehouse.epochs.get(&epoch);
        let mev = warehouse.mev.get(&epoch);
        let priority_fees = warehouse.priority_fees.get(&epoch);
        let collectors = SharedCollectors::of(snapshot);

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
            let collector_fields = collectors.fields_of(validator);

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
                    commission_effective_source: validator.commission_effective_source.clone(),
                    commission_effective_bps: validator.commission_effective_bps,
                    inflation_rewards_commission_bps: validator.inflation_rewards_commission_bps,
                    inflation_rewards_commission_bps_is_v4: validator
                        .inflation_rewards_commission_bps_is_v4,
                    inflation_rewards_collector: validator.inflation_rewards_collector.clone(),
                    inflation_rewards_collector_redirected: collector_fields
                        .inflation_rewards_collector_redirected,
                    inflation_rewards_collector_shared_count: collector_fields
                        .inflation_rewards_collector_shared_count,
                    inflation_rewards_collector_healthy: validator
                        .inflation_rewards_collector_healthy,
                    block_revenue_collector: validator.block_revenue_collector.clone(),
                    block_revenue_collector_is_identity: collector_fields
                        .block_revenue_collector_is_identity,
                    block_revenue_collector_shared_count: collector_fields
                        .block_revenue_collector_shared_count,
                    block_revenue_collector_healthy: validator.block_revenue_collector_healthy,
                    block_revenue_commission_bps: validator.block_revenue_commission_bps,
                    pending_delegator_rewards: validator.pending_delegator_rewards,
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
                    activating_stake: validator.activating_stake,
                    deactivating_stake: validator.deactivating_stake,
                    direct_stake: validator.direct_stake,
                    direct_activating_stake: validator.direct_activating_stake,
                    direct_deactivating_stake: validator.direct_deactivating_stake,
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
                    operator: None,
                    stake_delta_7d: None,
                    stake_delta_30d: None,
                    verified: false,
                    protected: false,
                    has_last_epoch_stats: false,
                    rugged_commission: false,
                    rugged_commission_info: Vec::new(),
                    rugged_commission_occurrences: 0,
                });

            // Epochs run newest-first, so the node metadata is projected from
            // the first epoch that reported any, under the identity the
            // record was seeded with.
            let has_node_metadata = validator.rpc_public.is_some()
                || validator.pubsub_public.is_some()
                || validator.gossip_port.is_some()
                || validator.version.is_some()
                || validator.client_id.is_some()
                || validator.client_id_raw.is_some()
                || validator.feature_set.is_some()
                || validator.shred_version.is_some();
            if has_node_metadata
                && validator.identity == record.identity
                && !projected_node_metadata.contains(vote_account)
            {
                record.version = validator.version.clone();
                record.client_id = client_id;
                record.client_name = client_name(client_id);
                record.client_label = client_label(client_id);
                record.client_vendor = client_vendor(client_id);
                record.client_lineage = client_lineage(client_id);
                record.client_id_raw = validator.client_id_raw.clone();
                record.feature_set = validator.feature_set.map(|set| set as u32);
                record.shred_version = validator.shred_version.map(|version| version as u16);
                record.gossip_port = validator.gossip_port.map(|port| port as u16);
                record.rpc_public = validator.rpc_public;
                record.pubsub_public = validator.pubsub_public;
                projected_node_metadata.insert(vote_account);
            }

            // Gating all three on max_observed keeps them from one epoch; stopping one epoch below
            // the record's own is what makes that epoch the newest closed one rather than whichever
            // older epoch still happens to carry the field. commission_advertised deliberately stays
            // on the open epoch that seeded the record.
            let seeding_epoch = *seeding_epochs.entry(vote_account).or_insert(epoch);
            if record.commission_max_observed.is_none() && epoch + 1 >= seeding_epoch {
                record.commission_max_observed = validator.commission_max_observed;
                record.commission_min_observed = validator.commission_min_observed;
                record.commission_effective = validator.commission_effective;
                record.commission_effective_source = validator.commission_effective_source.clone();
                record.commission_effective_bps = validator.commission_effective_bps;
            }

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
                commission_effective_source: validator.commission_effective_source.clone(),
                inflation_rewards_commission_bps: validator.inflation_rewards_commission_bps,
                inflation_rewards_commission_bps_is_v4: validator
                    .inflation_rewards_commission_bps_is_v4,
                inflation_rewards_collector: validator.inflation_rewards_collector.clone(),
                inflation_rewards_collector_redirected: collector_fields
                    .inflation_rewards_collector_redirected,
                inflation_rewards_collector_shared_count: collector_fields
                    .inflation_rewards_collector_shared_count,
                inflation_rewards_collector_healthy: validator.inflation_rewards_collector_healthy,
                block_revenue_collector: validator.block_revenue_collector.clone(),
                block_revenue_collector_is_identity: collector_fields
                    .block_revenue_collector_is_identity,
                block_revenue_collector_shared_count: collector_fields
                    .block_revenue_collector_shared_count,
                block_revenue_collector_healthy: validator.block_revenue_collector_healthy,
                block_revenue_commission_bps: validator.block_revenue_commission_bps,
                pending_delegator_rewards: validator.pending_delegator_rewards,
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
                direct_stake: validator.direct_stake,
                direct_activating_stake: validator.direct_activating_stake,
                direct_deactivating_stake: validator.direct_deactivating_stake,
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

    log::info!("Updating operators...");
    crate::operators::stamp_operators(records.values_mut());

    log::info!("Updating stake deltas...");
    crate::stake_deltas::stamp_stake_deltas(records.values_mut());

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
                worst_known_commission(
                    record.commission_max_observed,
                    record.commission_advertised,
                ),
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

/// The SIMD-0232 collector fields that are derived against the epoch rather
/// than read off the validator: whether a collector was moved, and how many
/// vote accounts name the same one.
#[derive(Default)]
struct CollectorFields {
    inflation_rewards_collector_redirected: Option<bool>,
    inflation_rewards_collector_shared_count: Option<i64>,
    block_revenue_collector_is_identity: Option<bool>,
    block_revenue_collector_shared_count: Option<i64>,
}

/// How many vote accounts of one epoch name each collector. A pre-v4 vote
/// state names none, and is counted nowhere rather than pooled under null.
struct SharedCollectors<'a> {
    inflation_rewards: HashMap<&'a String, i64>,
    block_revenue: HashMap<&'a String, i64>,
}

impl<'a> SharedCollectors<'a> {
    fn of(snapshot: &'a SnapshotDoc) -> Self {
        let mut inflation_rewards: HashMap<&String, i64> = Default::default();
        let mut block_revenue: HashMap<&String, i64> = Default::default();
        for validator in snapshot.values() {
            if let Some(collector) = &validator.inflation_rewards_collector {
                *inflation_rewards.entry(collector).or_default() += 1;
            }
            if let Some(collector) = &validator.block_revenue_collector {
                *block_revenue.entry(collector).or_default() += 1;
            }
        }
        Self {
            inflation_rewards,
            block_revenue,
        }
    }

    fn fields_of(&self, validator: &Validator) -> CollectorFields {
        CollectorFields {
            // Only UpdateCommissionCollector moves this, so a mismatch is a deliberate redirect.
            inflation_rewards_collector_redirected: validator
                .inflation_rewards_collector
                .as_ref()
                .map(|collector| *collector != validator.vote_account),
            inflation_rewards_collector_shared_count: validator
                .inflation_rewards_collector
                .as_ref()
                .map(|collector| self.inflation_rewards.get(collector).copied().unwrap_or(0)),
            // Against the identity, its default: SIMD-0232 stopped agave re-syncing the two.
            block_revenue_collector_is_identity: validator
                .block_revenue_collector
                .as_ref()
                .map(|collector| *collector == validator.identity),
            block_revenue_collector_shared_count: validator
                .block_revenue_collector
                .as_ref()
                .map(|collector| self.block_revenue.get(collector).copied().unwrap_or(0)),
        }
    }
}

/// Unknown parts are empty, and the key exists either way.
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
    for (epoch, snapshot) in warehouse.snapshots.range(first_epoch..).rev() {
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
    let by_client_id = load_stake_distribution(warehouse, epochs, client_id_group)?;
    Ok(ClusterStats {
        block_production_stats: load_block_production_stats(warehouse, epochs)?,
        dc_concentration_stats: load_dc_concentration_stats(warehouse, epochs)?,
        client_diversity_stats: client_diversity_stats(&by_client_id),
        client_lineage_stats: client_lineage_stats(&by_client_id),
        feature_set_stats: load_feature_set_stats(warehouse, epochs)?,
    })
}

const MIN_REQUIRED_EPOCHS_IN_THE_PAST: u64 = 1;
const MIN_REQUIRED_EPOCHS_WITH_CREDITS_OR_STAKE: u64 = 1;

/// Whether a validator is one the API should return at all: present in the last two epochs, and voting or
/// holding stake in them.
pub fn is_eligible_validator(validator: &ValidatorRecord, last_epoch: u64) -> bool {
    let min_required_epoch = last_epoch.saturating_sub(MIN_REQUIRED_EPOCHS_IN_THE_PAST);
    let credits_or_stake_from =
        last_epoch.saturating_sub(MIN_REQUIRED_EPOCHS_WITH_CREDITS_OR_STAKE);

    let has_min_epoch_stats = (min_required_epoch..=last_epoch).all(|epoch| {
        validator
            .epoch_stats
            .iter()
            .any(|epoch_stat| epoch_stat.epoch == epoch)
    });
    if !has_min_epoch_stats {
        return false;
    }

    (credits_or_stake_from..=last_epoch).all(|epoch| {
        validator
            .epoch_stats
            .iter()
            .find(|&epoch_stat| epoch_stat.epoch == epoch)
            .is_some_and(|epoch_stat| {
                epoch_stat.activated_stake > Decimal::from(0) || epoch_stat.credits > 0
            })
    })
}

/// The newest epoch any validator reports, which every eligibility check is relative to.
pub fn last_reported_epoch<'a>(
    validators: impl IntoIterator<Item = &'a ValidatorRecord>,
) -> Option<u64> {
    validators
        .into_iter()
        .flat_map(|validator| &validator.epoch_stats)
        .map(|epoch_stat| epoch_stat.epoch)
        .max()
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

/// What ds-sam scores from: one row per validator, averaged over the window.
pub fn load_validators_aggregated_flat(
    warehouse: &Warehouse,
    last_epoch: u64,
    epochs: u64,
) -> anyhow::Result<Vec<ValidatorAggregatedFlat>> {
    let epochs = epochs.max(1);
    let first_epoch = last_epoch - u64::min(last_epoch, epochs - 1);

    let mut accumulators: HashMap<&String, FlatAccumulator> = Default::default();
    for (epoch, snapshot) in warehouse.snapshots.range(first_epoch..=last_epoch) {
        let cluster = ClusterEpoch::of(snapshot);
        for (vote_account, validator) in snapshot.iter() {
            accumulators
                .entry(vote_account)
                .or_default()
                .add(*epoch, validator, &cluster);
        }
    }

    // The client columns are bounded to this call's window; last_version is not,
    // and sees every change the warehouse holds, which is the warm window rather
    // than all of history. Tightening it to the call's window changes what
    // historical scoring runs see, so it needs a ds-sam side check.
    let versions = version_changes_by_validator(warehouse);

    let mut validators: Vec<ValidatorAggregatedFlat> = Default::default();
    for (vote_account, accumulator) in accumulators {
        if accumulator.count != epochs || accumulator.epochs_with_credits < 7 {
            continue;
        }
        let changes = versions.get(vote_account);
        let last_version = changes
            .and_then(|changes| {
                changes
                    .iter()
                    .rev()
                    .find_map(|change| change.version.clone())
            })
            .unwrap_or_else(|| "0.0.0".to_string());
        // The shared filter plus the position tiebreak make both aggregates read one change.
        let last_client = changes.and_then(|changes| {
            changes.iter().rev().find(|change| {
                (change.client_id.is_some() || change.client_id_raw.is_some())
                    && change.epoch <= last_epoch
            })
        });
        let last_client_id = last_client.and_then(|change| {
            effective_client_id(
                change.client_id.map(|id| id as u16),
                change.client_id_raw.as_deref(),
            )
        });

        let max_inflation_rewards_commission_bps =
            accumulator.max_inflation_rewards_commission_bps();
        validators.push(ValidatorAggregatedFlat {
            vote_account: vote_account.clone(),
            minimum_stake: accumulator.minimum_stake,
            avg_stake: accumulator.stake / accumulator.count as f64,
            avg_dc_concentration: accumulator.average_dc_concentration(),
            avg_skip_rate: accumulator.skip_rate / accumulator.count as f64,
            avg_grace_skip_rate: accumulator.grace_skip_rate / accumulator.count as f64,
            max_commission: accumulator.max_commission.try_into()?,
            avg_adjusted_credits: accumulator.adjusted_credits / accumulator.count as f64 / 100f64,
            dc_aso: accumulator.dc_aso.unwrap_or_else(|| "Unknown".to_string()),
            marinade_stake: accumulator.marinade_stake,
            version: last_version,
            client_vendor: client_vendor(last_client_id)
                .unwrap_or_else(|| UNKNOWN_CLIENT_GROUP.to_string()),
            client_lineage: client_lineage(last_client_id)
                .unwrap_or_else(|| UNKNOWN_CLIENT_GROUP.to_string()),
            max_inflation_rewards_commission_bps,
        });
    }

    validators.sort_by(|a, b| b.avg_adjusted_credits.total_cmp(&a.avg_adjusted_credits));

    Ok(validators)
}

struct ClusterEpoch {
    weighted_skip_rate: Option<f64>,
    concentration_by_aso: HashMap<String, f64>,
}

impl ClusterEpoch {
    fn of(snapshot: &SnapshotDoc) -> Self {
        let mut stake = 0f64;
        let mut weighted_skip_rate = 0f64;
        let mut stake_by_aso: HashMap<String, f64> = Default::default();

        for validator in snapshot.values() {
            let activated_stake = validator.activated_stake.to_f64().unwrap_or_default();
            stake += activated_stake;
            weighted_skip_rate += validator.skip_rate * activated_stake;
            if let Some(aso) = validator.dc_aso.clone() {
                *stake_by_aso.entry(aso).or_default() += activated_stake;
            }
        }

        Self {
            weighted_skip_rate: (stake > 0f64).then(|| weighted_skip_rate / stake),
            concentration_by_aso: stake_by_aso
                .into_iter()
                .map(|(aso, aso_stake)| (aso, aso_stake / stake))
                .collect(),
        }
    }
}

#[derive(Default)]
struct FlatAccumulator {
    count: u64,
    epochs_with_credits: u64,
    minimum_stake: f64,
    stake: f64,
    dc_concentration: f64,
    epochs_with_aso: u64,
    skip_rate: f64,
    grace_skip_rate: f64,
    max_commission: i32,
    /// `None` once any epoch of the window carried no basis points: a
    /// maximum over a partial series would understate the rate.
    max_inflation_rewards_commission_bps: Option<i32>,
    epochs_without_bps: u64,
    adjusted_credits: f64,
    newest_epoch: u64,
    dc_aso: Option<String>,
    marinade_stake: f64,
}

impl FlatAccumulator {
    fn max_inflation_rewards_commission_bps(&self) -> Option<i32> {
        (self.epochs_without_bps == 0)
            .then_some(self.max_inflation_rewards_commission_bps)
            .flatten()
    }

    fn add(&mut self, epoch: u64, validator: &Validator, cluster: &ClusterEpoch) {
        let stake = to_sol(validator.activated_stake);
        if self.count == 0 || stake < self.minimum_stake {
            self.minimum_stake = stake;
        }
        self.count += 1;
        self.stake += stake;
        if validator.credits > Decimal::ZERO {
            self.epochs_with_credits += 1;
        }

        if let Some(concentration) = validator
            .dc_aso
            .as_ref()
            .and_then(|aso| cluster.concentration_by_aso.get(aso))
        {
            self.dc_concentration += concentration;
            self.epochs_with_aso += 1;
        }

        self.skip_rate += validator.skip_rate;
        // A validator with few leader slots is judged no worse than the cluster.
        let leader_slots = validator.leader_slots.to_u64().unwrap_or_default();
        self.grace_skip_rate += match (leader_slots < 200, cluster.weighted_skip_rate) {
            (true, Some(cluster_skip_rate)) => validator.skip_rate.min(cluster_skip_rate),
            _ => validator.skip_rate,
        };

        let commission = validator
            .commission_effective
            .or(validator.commission_advertised)
            .unwrap_or(100);
        self.max_commission = self.max_commission.max(commission);
        match validator.inflation_rewards_commission_bps {
            Some(bps) => {
                self.max_inflation_rewards_commission_bps = Some(
                    self.max_inflation_rewards_commission_bps
                        .map_or(bps, |max| max.max(bps)),
                )
            }
            None => self.epochs_without_bps += 1,
        }
        self.adjusted_credits +=
            validator.credits.to_f64().unwrap_or_default() * 0f64.max((100 - commission) as f64);

        if epoch >= self.newest_epoch {
            self.newest_epoch = epoch;
            self.dc_aso = validator.dc_aso.clone();
            self.marinade_stake = to_sol(validator.marinade_stake);
        }
    }

    fn average_dc_concentration(&self) -> f64 {
        match self.epochs_with_aso {
            0 => 0f64,
            epochs => self.dc_concentration / epochs as f64,
        }
    }
}

fn to_sol(lamports: Decimal) -> f64 {
    (lamports / Decimal::from(1_000_000_000u64))
        .to_f64()
        .unwrap_or_default()
}

/// Every version change held, oldest first, so the last one is the newest.
fn version_changes_by_validator(warehouse: &Warehouse) -> HashMap<&String, Vec<&VersionSample>> {
    let mut changes: HashMap<&String, Vec<&VersionSample>> = Default::default();

    for sealed in warehouse.versions.values() {
        for (vote_account, sealed) in sealed.iter() {
            changes.entry(vote_account).or_default().extend(sealed);
        }
    }
    for (vote_account, state) in warehouse.live.versions.iter() {
        changes
            .entry(vote_account)
            .or_default()
            .extend(state.changes.iter());
    }
    for changes in changes.values_mut() {
        changes.sort_by_key(|change| change.created_at);
    }

    changes
}
