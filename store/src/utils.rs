use crate::dto::{
    BlockProductionStats, ClusterStats, CommissionRecord, DCConcentrationStats, RugInfo,
    RuggerRecord, ScoringRunRecord, UptimeRecord, ValidatorAggregatedFlat, ValidatorEpochStats,
    ValidatorRecord, ValidatorScoreRecord, ValidatorScoringCsvRow, ValidatorWarning,
    ValidatorsAggregated, VersionRecord,
};
use chrono::{DateTime, Utc};
use rust_decimal::prelude::*;
use std::{
    collections::{HashMap, HashSet},
    ops::RangeInclusive,
};
use tokio_postgres::{types::ToSql, Client};

const SECONDS_IN_YEAR: f64 = 365.25 * 24f64 * 3600f64;
const IDEAL_SLOT_DURATION_MS: u64 = 400;
const SLOTS_IN_EPOCH: u64 = 432000;
const SECONDS_IN_IDEAL_EPOCH: u64 = SLOTS_IN_EPOCH * IDEAL_SLOT_DURATION_MS / 1000;
const IDEAL_EPOCHS_PER_YEAR: f64 = SECONDS_IN_YEAR / SECONDS_IN_IDEAL_EPOCH as f64;

pub struct InsertQueryCombiner<'a> {
    pub insertions: u64,
    statement: String,
    params: Vec<&'a (dyn ToSql + Sync)>,
}

pub fn to_fixed(a: f64, decimals: i32) -> u64 {
    (a * 10f64.powi(decimals)).round() as u64
}

pub fn to_fixed_for_sort(a: f64) -> u64 {
    to_fixed(a, 4)
}

impl<'a> InsertQueryCombiner<'a> {
    pub fn new(table_name: String, columns: String) -> Self {
        Self {
            insertions: 0,
            statement: format!("INSERT INTO {} ({}) VALUES", table_name, columns).to_string(),
            params: vec![],
        }
    }

    pub fn add(&mut self, values: &mut Vec<&'a (dyn ToSql + Sync)>) {
        let separator = if self.insertions == 0 { " " } else { "," };
        let mut query_end = "(".to_string();
        for i in 0..values.len() {
            if i > 0 {
                query_end.push_str(",");
            }
            query_end.push_str(&format!("${}", i + 1 + self.params.len()));
        }
        query_end.push_str(")");

        self.params.append(values);
        self.statement
            .push_str(&format!("{}{}", separator, query_end));
        self.insertions += 1;
    }

    pub async fn execute(&self, client: &mut Client) -> anyhow::Result<Option<u64>> {
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
            statement: format!("UPDATE {} SET {} FROM (VALUES", table_name, updates).to_string(),
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
                query_end.push_str(",");
            }
            query_end.push_str(&format!("${}", i + 1 + self.params.len()));
            if let Some(t) = types.get(&i) {
                query_end.push_str(&format!("::{}", t));
            };
        }
        query_end.push_str(")");

        self.params.append(values);
        self.statement
            .push_str(&format!("{}{}", separator, query_end));
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

        let cluster_rewards_per_year = self.supply as f64 * self.inflation;
        let cluster_rewards_per_ideal_epoch = cluster_rewards_per_year / IDEAL_EPOCHS_PER_YEAR;

        let stake_fraction_per_epoch =
            staker_share * cluster_rewards_per_ideal_epoch * credits as f64
                / self.total_weighted_credits as f64;

        let apr = stake_fraction_per_epoch * actual_epochs_per_year;
        let apy = (1.0 + stake_fraction_per_epoch).powf(actual_epochs_per_year) - 1.0;

        (apr, apy)
    }
}
async fn get_apy_calculators(
    psql_client: &Client,
) -> anyhow::Result<HashMap<u64, InflationApyCalculator>> {
    let apy_info_rows = psql_client
        .query(
            "SELECT
                    epochs.epoch,
                    (EXTRACT('epoch' FROM end_at) - EXTRACT('epoch' FROM start_at))::INTEGER as duration,
                    supply,
                    inflation,
                    SUM(validators.credits * validators.activated_stake) total_weighted_credits
                FROM
                epochs
                INNER JOIN validators ON epochs.epoch = validators.epoch
                GROUP BY epochs.epoch",
            &[],
        )
        .await?;

    let mut result: HashMap<_, _> = Default::default();
    for row in apy_info_rows {
        result.insert(
            row.get::<_, Decimal>("epoch").try_into()?,
            InflationApyCalculator {
                supply: row.get::<_, Decimal>("supply").try_into()?,
                duration: row.get::<_, i32>("duration").try_into()?,
                inflation: row.get("inflation"),
                total_weighted_credits: row
                    .get::<_, Decimal>("total_weighted_credits")
                    .try_into()?,
            },
        );
    }

    Ok(result)
}

pub async fn load_uptimes(
    psql_client: &Client,
    epochs: u64,
) -> anyhow::Result<HashMap<String, Vec<UptimeRecord>>> {
    let rows = psql_client
        .query(
            "
            WITH cluster AS (
                SELECT MAX(epoch) as last_epoch 
                FROM cluster_info
            )
            SELECT
                vote_account, 
                status, 
                uptimes.epoch, 
                epochs.start_at AS epoch_start,
                epochs.end_at AS epoch_end,
                uptimes.start_at,
                uptimes.end_at
            FROM uptimes
            LEFT JOIN epochs ON uptimes.epoch = epochs.epoch
            CROSS JOIN cluster
            WHERE uptimes.epoch > cluster.last_epoch - $1::NUMERIC",
            &[&Decimal::from(epochs)],
        )
        .await?;

    let mut records: HashMap<_, Vec<_>> = Default::default();
    for row in rows {
        let vote_account: String = row.get("vote_account");
        let uptimes = records
            .entry(vote_account.clone())
            .or_insert(Default::default());
        let epoch_start_at: Option<DateTime<Utc>> = row
            .get::<_, Option<DateTime<Utc>>>("epoch_start")
            .try_into()
            .unwrap();
        let epoch_end_at: Option<DateTime<Utc>> = row
            .get::<_, Option<DateTime<Utc>>>("epoch_end")
            .try_into()
            .unwrap();
        uptimes.push(UptimeRecord {
            epoch: row.get::<_, Decimal>("epoch").try_into()?,
            epoch_end_at: epoch_end_at.unwrap_or(Utc::now()),
            epoch_start_at: epoch_start_at.unwrap_or(Utc::now()),
            status: row.get("status"),
            start_at: row.get("start_at"),
            end_at: row.get("end_at"),
        })
    }

    Ok(records)
}

pub async fn load_versions(
    psql_client: &Client,
    epochs: u64,
) -> anyhow::Result<HashMap<String, Vec<VersionRecord>>> {
    let rows = psql_client
        .query(
            "
            WITH cluster AS (SELECT MAX(epoch) as last_epoch FROM cluster_info)
            SELECT
                vote_account, version, epoch, created_at
            FROM versions, cluster WHERE epoch > cluster.last_epoch - $1::NUMERIC",
            &[&Decimal::from(epochs)],
        )
        .await?;

    let mut records: HashMap<_, Vec<_>> = Default::default();
    for row in rows {
        let vote_account: String = row.get("vote_account");
        let versions = records
            .entry(vote_account.clone())
            .or_insert(Default::default());
        versions.push(VersionRecord {
            epoch: row.get::<_, Decimal>("epoch").try_into()?,
            version: row.get("version"),
            created_at: row.get("created_at"),
        })
    }

    Ok(records)
}
/*
We are checking if:
- Current commission is greater than previous minimum and it's above 10 OR
- Previous commission is more than 10, current commission is less than or equal to 10, and the next commission is more than 10 OR
- Previous commission is less than or equal to 10, current commission is more than 10, and the next commission is less than or equal to 10 OR
 */
pub async fn load_ruggers(psql_client: &Client) -> anyhow::Result<HashMap<String, RuggerRecord>> {
    let rows = psql_client
        .query(
            "
            WITH commission_changes AS (
                SELECT 
                    vote_account, 
                    epoch,
                    commission_effective,
                    commission_min_observed,
                    LAG(commission_effective) OVER(PARTITION BY vote_account ORDER BY epoch) AS prev_commission,
                    LEAD(commission_effective) OVER(PARTITION BY vote_account ORDER BY epoch) AS next_commission
                FROM 
                    validators
            ),
            filtered_commissions AS (
                SELECT 
                    vote_account, 
                    epoch, 
                    commission_effective, 
                    commission_min_observed
                FROM 
                    commission_changes
                WHERE 
                    (commission_effective > commission_min_observed AND commission_effective > 10 AND commission_min_observed <= 10)
                    OR
                    (prev_commission > 10 AND commission_effective <= 10 AND next_commission > 10)
                    OR
                    (prev_commission <= 10 AND commission_effective > 10 AND next_commission <= 10)
            )
            SELECT 
                vote_account, 
                COUNT(*) AS events_count, 
                ARRAY_AGG(epoch) AS epochs,
                ARRAY_AGG(commission_effective) AS commission_observed_values,
                ARRAY_AGG(commission_min_observed) AS commission_min_observed_values
            FROM 
                filtered_commissions
            GROUP BY 
                vote_account
            HAVING 
                COUNT(*) > 1
            ORDER BY 
                events_count DESC;
            ",
            &[],
        )
        .await?;

    let mut records: HashMap<String, RuggerRecord> = Default::default();
    for row in rows {
        let vote_account: String = row.get("vote_account");
        let occurrences: u64 = row.get::<_, i64>("events_count").try_into()?;
        let epochs: Vec<u64> = row
            .get::<_, Vec<Decimal>>("epochs")
            .into_iter()
            .map(|val| val.to_u64().unwrap_or_default())
            .collect();
        let observed_commissions: Vec<u64> = row
            .get::<_, Vec<i32>>("commission_observed_values")
            .into_iter()
            .map(|val| val as u64)
            .collect();
        let min_commissions: Vec<u64> = row
            .get::<_, Vec<i32>>("commission_min_observed_values")
            .into_iter()
            .map(|val| val as u64)
            .collect();

        records.insert(
            vote_account,
            RuggerRecord {
                epochs,
                occurrences,
                observed_commissions,
                min_commissions,
                created_at: Utc::now(),
            },
        );
    }
    Ok(records)
}

pub async fn load_commissions(
    psql_client: &Client,
    epochs: u64,
) -> anyhow::Result<HashMap<String, Vec<CommissionRecord>>> {
    let rows = psql_client
        .query(
            "
            WITH cluster AS (SELECT MAX(epoch) as last_epoch FROM cluster_info)
            SELECT
                vote_account, commission, commissions.epoch, epochs.start_at as epoch_start,
				epochs.end_at as epoch_end,
				epoch_slot, created_at
            FROM commissions
            LEFT JOIN epochs ON commissions.epoch = epochs.epoch
            CROSS JOIN cluster
            WHERE commissions.epoch > cluster.last_epoch - $1::NUMERIC
            UNION
            SELECT
                vote_account, commission_effective, validators.epoch, epochs.start_at as epoch_start,
				epochs.end_at as epoch_end, 432000, updated_at
            FROM validators
            LEFT JOIN epochs ON validators.epoch = epochs.epoch
            CROSS JOIN cluster
            WHERE validators.epoch > cluster.last_epoch - $1::NUMERIC AND commission_effective IS NOT NULL
            ",
            &[&Decimal::from(epochs)],
        )
        .await?;

    let mut records: HashMap<_, Vec<_>> = Default::default();
    for row in rows {
        let vote_account: String = row.get("vote_account");
        let commissions = records
            .entry(vote_account.clone())
            .or_insert(Default::default());
        let epoch_start_at: Option<DateTime<Utc>> = row
            .get::<_, Option<DateTime<Utc>>>("epoch_start")
            .try_into()
            .unwrap();
        let epoch_end_at: Option<DateTime<Utc>> = row
            .get::<_, Option<DateTime<Utc>>>("epoch_end")
            .try_into()
            .unwrap();
        commissions.push(CommissionRecord {
            epoch: row.get::<_, Decimal>("epoch").try_into()?,
            epoch_end_at: epoch_end_at.unwrap_or(Utc::now()),
            epoch_start_at: epoch_start_at.unwrap_or(Utc::now()),
            epoch_slot: row.get::<_, Decimal>("epoch_slot").try_into()?,
            commission: row.get::<_, i32>("commission").try_into()?,
            created_at: row.get("created_at"),
        })
    }

    Ok(records)
}

pub async fn update_with_warnings(
    validators: &mut HashMap<String, ValidatorRecord>,
    epochs_range: RangeInclusive<u64>,
) -> anyhow::Result<()> {
    log::info!("Updating validator records with warnings");

    for (_, validator) in validators {
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

fn average(numbers: &Vec<f64>) -> Option<f64> {
    if numbers.len() == 0 {
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
                .collect(),
        );
        record.avg_uptime_pct = average(
            &record
                .epoch_stats
                .iter()
                .filter(|stat| epochs_range.contains(&stat.epoch))
                .flat_map(|epoch| epoch.uptime_pct)
                .collect(),
        );
    }
}

pub fn update_validators_ranks<T>(
    validators: &mut HashMap<String, ValidatorRecord>,
    field_extractor: fn(&ValidatorEpochStats) -> T,
    rank_updater: fn(&mut ValidatorEpochStats, usize) -> (),
) where
    T: Ord,
{
    let mut stats_by_epoch: HashMap<u64, Vec<(String, T)>> = Default::default();
    for (vote_account, record) in validators.iter() {
        for validator_epoch_stats in record.epoch_stats.iter() {
            stats_by_epoch
                .entry(validator_epoch_stats.epoch)
                .or_insert(Default::default())
                .push((vote_account.clone(), field_extractor(validator_epoch_stats)));
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

pub async fn load_validators(
    psql_client: &Client,
    display_epochs: u64,
    computing_epochs: u64,
) -> anyhow::Result<HashMap<String, ValidatorRecord>> {
    let last_epoch = match get_last_epoch(psql_client).await? {
        Some(last_epoch) => last_epoch,
        _ => return Ok(Default::default()),
    };
    let ruggers = load_ruggers(psql_client).await?;
    let apy_calculators = get_apy_calculators(psql_client).await?;
    let concentrations = load_dc_concentration_stats(psql_client, 1)
        .await?
        .first()
        .cloned();

    log::info!("Querying validators...");
    let rows = psql_client
        .query(
            "
            WITH
                validators_aggregated AS (SELECT vote_account, MIN(epoch) first_epoch FROM validators GROUP BY vote_account),
                cluster AS (SELECT MAX(epoch) as last_epoch FROM cluster_info),
                epochs_dates AS (SELECT vote_account, starting_epoch, start_at FROM (SELECT vote_account, MIN(validators.epoch) as starting_epoch FROM validators WHERE credits > 0 GROUP BY vote_account) AS s JOIN epochs ON s.starting_epoch = epochs.epoch)
            SELECT
                validators.identity,
                validators.vote_account,
                validators.epoch,
                epochs.start_at AS epoch_start,
				epochs.end_at AS epoch_end,
                COALESCE(epochs_dates.starting_epoch, 0) AS starting_epoch,
                epochs_dates.start_at AS starting_epoch_date,
                info_name,
                info_url,
                info_keybase,
                node_ip,
                dc_coordinates_lat,
                dc_coordinates_lon,
                dc_continent,
                dc_country_iso,
                dc_country,
                dc_city,
                dc_asn,
                dc_aso,
                CONCAT(dc_continent, '/', dc_country, '/', dc_city) dc_full_city,

                commission_max_observed,
                commission_min_observed,
                commission_advertised,
                commission_effective,
                version,
                activated_stake,
                marinade_stake,
                decentralizer_stake,
                superminority,
                stake_to_become_superminority,
                credits,
                leader_slots,
                blocks_produced,
                skip_rate,
                uptime_pct,
                uptime,
                downtime,

                validators_aggregated.first_epoch AS first_epoch
            FROM validators
                LEFT JOIN cluster ON 1 = 1
                LEFT JOIN validators_aggregated ON validators_aggregated.vote_account = validators.vote_account
                LEFT JOIN epochs_dates ON validators.vote_account = epochs_dates.vote_account
                LEFT JOIN epochs ON epochs.epoch = validators.epoch
            WHERE validators.epoch > cluster.last_epoch - $1::NUMERIC
            ORDER BY epoch DESC",
            &[&Decimal::from(display_epochs)],
        )
        .await?;

    let mut records: HashMap<_, _> = tokio::task::spawn_blocking(move || {
        log::info!("Aggregating validator records...");
        let mut records: HashMap<_, _> = Default::default();
        for row in rows {
            let vote_account: String = row.get("vote_account");
            let epoch: u64 = row.get::<_, Decimal>("epoch").try_into().unwrap();
            let starting_epoch_date: Option<DateTime<Utc>> = row
                .get::<_, Option<DateTime<Utc>>>("starting_epoch_date")
                .try_into()
                .unwrap();
            let mut epoch_start_at: Option<DateTime<Utc>> = row
                .get::<_, Option<DateTime<Utc>>>("epoch_start")
                .try_into()
                .unwrap();
            let epoch_end_at: Option<DateTime<Utc>> = row
                .get::<_, Option<DateTime<Utc>>>("epoch_end")
                .try_into()
                .unwrap();
            let first_epoch: u64 = row.get::<_, Decimal>("first_epoch").try_into().unwrap();
            let starting_epoch: u64 = row.get::<_, Decimal>("starting_epoch").try_into().unwrap();

            let (apr, apy) = if let Some(c) = apy_calculators.get(&epoch) {
                let (apr, apy) = c.estimate_yields(
                    row.get::<_, Decimal>("credits").try_into().unwrap(),
                    row.get::<_, Option<i32>>("commission_effective")
                        .map(|n| n.try_into().unwrap())
                        .unwrap_or(100),
                );
                (Some(apr), Some(apy))
            } else {
                (None, None)
            };

            let dc_full_city = row
                .get::<_, Option<String>>("dc_full_city")
                .unwrap_or("Unknown".into());
            let dc_asn = row
                .get::<_, Option<i32>>("dc_asn")
                .map(|dc_asn| dc_asn.to_string())
                .unwrap_or("Unknown".into());
            let dc_aso = row
                .get::<_, Option<String>>("dc_aso")
                .unwrap_or("Unknown".into());

            let dcc_full_city = concentrations
                .clone()
                .and_then(|c| c.dc_concentration_by_city.get(&dc_full_city).cloned());
            let dcc_asn = concentrations
                .clone()
                .and_then(|c| c.dc_concentration_by_asn.get(&dc_asn).cloned());
            let dcc_aso = concentrations
                .clone()
                .and_then(|c| c.dc_concentration_by_aso.get(&dc_aso).cloned());

            let record = records
                .entry(vote_account.clone())
                .or_insert_with(|| ValidatorRecord {
                    identity: row.get("identity"),
                    start_epoch: starting_epoch.clone(),
                    start_date: starting_epoch_date.clone(),
                    vote_account: vote_account.clone(),
                    info_name: row.get("info_name"),
                    info_url: row.get("info_url"),
                    info_keybase: row.get("info_keybase"),
                    node_ip: row.get("node_ip"),
                    dc_coordinates_lat: row.get("dc_coordinates_lat"),
                    dc_coordinates_lon: row.get("dc_coordinates_lon"),
                    dc_continent: row.get("dc_continent"),
                    dc_country_iso: row.get("dc_country_iso"),
                    dc_country: row.get("dc_country"),
                    dc_city: row.get("dc_city"),
                    dc_full_city: row.get("dc_full_city"),
                    dc_asn: row.get("dc_asn"),
                    dc_aso: row.get("dc_aso"),
                    dcc_full_city,
                    dcc_asn,
                    dcc_aso,
                    commission_max_observed: row
                        .get::<_, Option<i32>>("commission_max_observed")
                        .map(|n| n.try_into().unwrap()),
                    commission_min_observed: row
                        .get::<_, Option<i32>>("commission_min_observed")
                        .map(|n| n.try_into().unwrap()),
                    commission_advertised: row
                        .get::<_, Option<i32>>("commission_advertised")
                        .map(|n| n.try_into().unwrap()),
                    commission_effective: row
                        .get::<_, Option<i32>>("commission_effective")
                        .map(|n| n.try_into().unwrap()),
                    commission_aggregated: None,
                    version: row.get("version"),
                    activated_stake: row.get::<_, Decimal>("activated_stake").try_into().unwrap(),
                    marinade_stake: row.get::<_, Decimal>("marinade_stake").try_into().unwrap(),
                    decentralizer_stake: row
                        .get::<_, Decimal>("decentralizer_stake")
                        .try_into()
                        .unwrap(),
                    superminority: row.get("superminority"),
                    credits: row.get::<_, Decimal>("credits").try_into().unwrap(),
                    score: None,

                    epoch_stats: Default::default(),

                    warnings: Default::default(),

                    epochs_count: epoch - first_epoch + 1,

                    avg_uptime_pct: None,
                    avg_apy: None,
                    has_last_epoch_stats: false,
                    rugged_commission: false,
                    rugged_commission_info: Vec::new(),
                    rugged_commission_occurrences: 0,
                });

            let rug_info = ruggers.get(&vote_account);
            if let Some(rugger_info) = rug_info {
                record.rugged_commission = true;
                record.rugged_commission_occurrences = rugger_info.occurrences;
                record.rugged_commission_info = rugger_info
                    .epochs
                    .iter()
                    .enumerate()
                    .map(|(index, &epoch)| RugInfo {
                        epoch,
                        after: rugger_info.observed_commissions[index],
                        before: rugger_info.min_commissions[index]
                    })
                    .collect()
            }
            if last_epoch == epoch {
                record.has_last_epoch_stats = true;
            }
            if let None = epoch_start_at {
                epoch_start_at = Some(Utc::now());
            }
            record.epoch_stats.push(ValidatorEpochStats {
                epoch,
                epoch_start_at: epoch_start_at,
                epoch_end_at: epoch_end_at,
                commission_max_observed: row
                    .get::<_, Option<i32>>("commission_max_observed")
                    .map(|n| n.try_into().unwrap()),
                commission_min_observed: row
                    .get::<_, Option<i32>>("commission_min_observed")
                    .map(|n| n.try_into().unwrap()),
                commission_advertised: row
                    .get::<_, Option<i32>>("commission_advertised")
                    .map(|n| n.try_into().unwrap()),
                commission_effective: row
                    .get::<_, Option<i32>>("commission_effective")
                    .map(|n| n.try_into().unwrap()),
                version: row.get("version"),
                activated_stake: row.get::<_, Decimal>("activated_stake").try_into().unwrap(),
                marinade_stake: row.get::<_, Decimal>("marinade_stake").try_into().unwrap(),
                decentralizer_stake: row
                    .get::<_, Decimal>("decentralizer_stake")
                    .try_into()
                    .unwrap(),
                superminority: row.get("superminority"),
                stake_to_become_superminority: row
                    .get::<_, Decimal>("stake_to_become_superminority")
                    .try_into()
                    .unwrap(),
                credits: row.get::<_, Decimal>("credits").try_into().unwrap(),
                leader_slots: row.get::<_, Decimal>("leader_slots").try_into().unwrap(),
                blocks_produced: row.get::<_, Decimal>("blocks_produced").try_into().unwrap(),
                skip_rate: row.get("skip_rate"),
                uptime_pct: row.get("uptime_pct"),
                uptime: row
                    .get::<_, Option<Decimal>>("uptime")
                    .map(|n| n.try_into().unwrap()),
                downtime: row
                    .get::<_, Option<Decimal>>("downtime")
                    .map(|n| n.try_into().unwrap()),
                apr,
                apy,
                score: None,
                rank_apy: None,
                rank_score: None,
                rank_activated_stake: None,
            });
        }

        records
    })
    .await?;

    let last_epoch = get_last_epoch(psql_client).await?.unwrap_or(0);
    let mut first_epoch = last_epoch - display_epochs.min(last_epoch) + 1;
    let mut epochs_range = first_epoch..=last_epoch;

    log::info!("Updating with scores...");
    update_validators_with_scores(psql_client, &mut records, epochs_range.clone()).await?;

    first_epoch = last_epoch - computing_epochs.min(last_epoch) + 1;
    epochs_range = first_epoch..=last_epoch;

    log::info!("Updating averages...");
    update_validators_with_avgs(&mut records, epochs_range.clone());
    log::info!("Updating ranks...");
    update_validators_ranks(
        &mut records,
        |a: &ValidatorEpochStats| a.activated_stake,
        |a: &mut ValidatorEpochStats, rank: usize| a.rank_activated_stake = Some(rank),
    );
    update_validators_ranks(
        &mut records,
        |a: &ValidatorEpochStats| to_fixed_for_sort(a.score.unwrap_or(0.0)),
        |a: &mut ValidatorEpochStats, rank: usize| a.rank_score = Some(rank),
    );
    update_validators_ranks(
        &mut records,
        |a: &ValidatorEpochStats| to_fixed_for_sort(a.apy.unwrap_or(0.0)),
        |a: &mut ValidatorEpochStats, rank: usize| a.rank_apy = Some(rank),
    );
    update_with_warnings(&mut records, epochs_range.clone()).await?;
    log::info!("Records prepared...");
    Ok(records)
}

pub async fn update_validators_with_scores(
    psql_client: &Client,
    validators: &mut HashMap<String, ValidatorRecord>,
    epochs_range: RangeInclusive<u64>,
) -> anyhow::Result<()> {
    log::info!(
        "Updating validator score with epochs range: {:?}",
        epochs_range
    );
    let scores_per_epoch = load_scores_in_epochs(psql_client, epochs_range).await?;

    let latest_epoch_with_score = match scores_per_epoch.keys().max() {
        Some(epoch) => epoch,
        _ => return Ok(()),
    };

    let latest_scores = scores_per_epoch.get(latest_epoch_with_score).unwrap();

    for (_, validator) in validators.iter_mut() {
        for epoch_record in validator.epoch_stats.iter_mut() {
            if let Some(scores) = scores_per_epoch.get(&epoch_record.epoch) {
                epoch_record.score = scores.get(&validator.vote_account).cloned();
            }
        }

        validator.score = latest_scores.get(&validator.vote_account).cloned();
    }

    Ok(())
}

pub async fn load_scores_in_epochs(
    psql_client: &Client,
    epochs: std::ops::RangeInclusive<u64>,
) -> anyhow::Result<HashMap<u64, HashMap<String, f64>>> {
    log::info!("Loading scores for epochs: {:?}", epochs);
    let rows = psql_client
        .query(
            "
            WITH last_runs_in_epoch AS (SELECT epoch, MAX(scoring_run_id) as id FROM scoring_runs WHERE epoch = ANY($1) GROUP BY epoch)
            SELECT
                epoch,
                vote_account,
                score
            FROM last_runs_in_epoch
                INNER JOIN scores ON last_runs_in_epoch.id = scores.scoring_run_id
            ",
            &[&epochs.clone().map(|epoch| epoch as i32).collect::<Vec<_>>()],
        )
        .await?;

    let mut result: HashMap<u64, HashMap<String, f64>> = Default::default();

    for row in rows {
        let epoch = row.get::<_, i32>("epoch") as u64;
        let epoch_scores = result.entry(epoch).or_insert(Default::default());
        epoch_scores.insert(row.get("vote_account"), row.get("score"));
    }

    let mut last_valid_scores: Option<HashMap<String, f64>> = None;
    for epoch in epochs {
        if let Some(last_valid_scores) = last_valid_scores {
            result.entry(epoch).or_insert(last_valid_scores);
        }
        last_valid_scores = result.get(&epoch).cloned();
    }

    Ok(result)
}

pub async fn load_last_scoring_run(
    psql_client: &Client,
) -> anyhow::Result<Option<ScoringRunRecord>> {
    log::info!("Querying scoring run...");
    let result = psql_client
        .query_opt(
            "
            SELECT
                scoring_run_id::numeric,
                created_at,
                epoch,
                components,
                component_weights,
                ui_id
            FROM scoring_runs
            WHERE scoring_run_id IN (SELECT MAX(scoring_run_id) FROM scoring_runs)",
            &[],
        )
        .await?;

    let scoring_run = match result {
        Some(scoring_run) => scoring_run,
        _ => {
            log::warn!("No scoring run was found!");
            return Ok(None);
        }
    };

    Ok(Some(ScoringRunRecord {
        scoring_run_id: scoring_run.get("scoring_run_id"),
        created_at: scoring_run.get("created_at"),
        epoch: scoring_run.get("epoch"),
        components: scoring_run.get("components"),
        component_weights: scoring_run.get("component_weights"),
        ui_id: scoring_run.get("ui_id"),
    }))
}

pub async fn load_scores(
    psql_client: &Client,
    scoring_run_id: Decimal,
) -> anyhow::Result<HashMap<String, ValidatorScoreRecord>> {
    log::info!("Querying scores...");
    let rows = psql_client
        .query(
            "
            SELECT vote_account,
                score,
                rank,
                vemnde_votes,
                msol_votes,
                ui_hints,
                component_scores,
                component_ranks,
                component_values,
                eligible_stake_algo,
                eligible_stake_vemnde,
                eligible_stake_msol,
                target_stake_algo,
                target_stake_vemnde,
                target_stake_msol,
                scores.scoring_run_id,
                scoring_runs.created_at as created_at
            FROM scores
            LEFT JOIN scoring_runs ON scoring_runs.scoring_run_id = scores.scoring_run_id
            WHERE scores.scoring_run_id::numeric = $1 ORDER BY rank",
            &[&scoring_run_id],
        )
        .await?;

    let records: HashMap<_, _> = {
        log::info!("Aggregating scores records...");
        let mut records: HashMap<_, _> = Default::default();
        for row in rows {
            let vote_account: String = row.get("vote_account");

            records
                .entry(vote_account.clone())
                .or_insert_with(|| ValidatorScoreRecord {
                    vote_account: vote_account.clone(),
                    score: row.get("score"),
                    rank: row.get("rank"),
                    vemnde_votes: row.get::<_, Decimal>("vemnde_votes").try_into().unwrap(),
                    msol_votes: row.get::<_, Decimal>("msol_votes").try_into().unwrap(),
                    ui_hints: row.get("ui_hints"),
                    component_scores: row.get("component_scores"),
                    component_ranks: row.get("component_ranks"),
                    component_values: row.get("component_values"),
                    eligible_stake_algo: row.get("eligible_stake_algo"),
                    eligible_stake_vemnde: row.get("eligible_stake_vemnde"),
                    eligible_stake_msol: row.get("eligible_stake_msol"),
                    target_stake_algo: row
                        .get::<_, Decimal>("target_stake_algo")
                        .try_into()
                        .unwrap(),
                    target_stake_vemnde: row
                        .get::<_, Decimal>("target_stake_vemnde")
                        .try_into()
                        .unwrap(),
                    target_stake_msol: row
                        .get::<_, Decimal>("target_stake_msol")
                        .try_into()
                        .unwrap(),
                    scoring_run_id: row.get("scoring_run_id"),
                    created_at: row
                        .get::<_, DateTime<Utc>>("created_at")
                        .try_into()
                        .unwrap(),
                });
        }

        records
    };
    log::info!("Records prepared...");
    Ok(records)
}

pub async fn get_last_epoch(psql_client: &Client) -> anyhow::Result<Option<u64>> {
    let row = psql_client
        .query_opt("SELECT MAX(epoch) as last_epoch FROM validators", &[])
        .await?;

    Ok(row.map(|row| row.get::<_, Decimal>("last_epoch").try_into().unwrap()))
}

pub async fn load_dc_concentration_stats(
    psql_client: &Client,
    epochs: u64,
) -> anyhow::Result<Vec<DCConcentrationStats>> {
    let last_epoch = match get_last_epoch(psql_client).await? {
        Some(last_epoch) => last_epoch,
        _ => return Ok(Default::default()),
    };
    let first_epoch = last_epoch - epochs.min(last_epoch) + 1;

    let mut stats: Vec<_> = Default::default();

    let map_stake_to_concentration =
        |stake: &HashMap<String, u64>, total_stake: u64| -> HashMap<_, _> {
            stake
                .iter()
                .map(|(key, stake)| (key.clone(), *stake as f64 / total_stake as f64))
                .collect()
        };

    for epoch in (first_epoch..=last_epoch).rev() {
        let mut dc_stake_by_aso: HashMap<_, _> = Default::default();
        let mut dc_stake_by_asn: HashMap<_, _> = Default::default();
        let mut dc_stake_by_city: HashMap<_, _> = Default::default();
        let mut total_active_stake = 0;

        let rows = psql_client
            .query(
                "SELECT
                    activated_stake,
                    dc_aso,
                    dc_asn,
                    CONCAT(dc_continent, '/', dc_country, '/', dc_city) dc_full_city
                FROM validators WHERE epoch = $1",
                &[&Decimal::from(epoch)],
            )
            .await?;

        for row in rows.iter() {
            let activated_stake: u64 = row.get::<_, Decimal>("activated_stake").try_into()?;
            let dc_aso = row
                .get::<_, Option<String>>("dc_aso")
                .unwrap_or("Unknown".to_string());
            let dc_asn: String = row
                .get::<_, Option<i32>>("dc_asn")
                .map_or("Unknown".to_string(), |dc_asn| dc_asn.to_string());
            let dc_city: String = row
                .get::<_, Option<String>>("dc_full_city")
                .unwrap_or("Unknown".to_string());

            total_active_stake += activated_stake;
            *(dc_stake_by_aso.entry(dc_aso).or_insert(Default::default())) += activated_stake;
            *(dc_stake_by_asn.entry(dc_asn).or_insert(Default::default())) += activated_stake;
            *(dc_stake_by_city
                .entry(dc_city)
                .or_insert(Default::default())) += activated_stake;
        }

        stats.push(DCConcentrationStats {
            epoch,
            total_activated_stake: Default::default(),
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
        })
    }

    Ok(stats)
}

pub async fn load_block_production_stats(
    psql_client: &Client,
    epochs: u64,
) -> anyhow::Result<Vec<BlockProductionStats>> {
    let last_epoch = match get_last_epoch(psql_client).await? {
        Some(last_epoch) => last_epoch,
        _ => return Ok(Default::default()),
    };
    let first_epoch = last_epoch - epochs.min(last_epoch) + 1;

    let mut stats: Vec<_> = Default::default();

    let rows = psql_client
            .query(
                "SELECT
	                epoch,
                    COALESCE(SUM(blocks_produced), 0) blocks_produced,
                    COALESCE(SUM(leader_slots), 0) leader_slots,
                    (1 - COALESCE(SUM(blocks_produced), 0)  / coalesce(SUM(leader_slots), 1))::DOUBLE PRECISION avg_skip_rate
                FROM validators
                WHERE epoch > $1
                GROUP BY epoch ORDER BY epoch DESC",
                &[&Decimal::from(first_epoch)],
            )
            .await?;

    for row in rows {
        stats.push(BlockProductionStats {
            epoch: row.get::<_, Decimal>("epoch").try_into()?,
            blocks_produced: row.get::<_, Decimal>("blocks_produced").try_into()?,
            leader_slots: row.get::<_, Decimal>("leader_slots").try_into()?,
            avg_skip_rate: row.get("avg_skip_rate"),
        })
    }

    Ok(stats)
}

pub async fn load_cluster_stats(psql_client: &Client, epochs: u64) -> anyhow::Result<ClusterStats> {
    Ok(ClusterStats {
        block_production_stats: load_block_production_stats(psql_client, epochs).await?,
        dc_concentration_stats: load_dc_concentration_stats(psql_client, epochs).await?,
    })
}

pub fn aggregate_validators(
    validators: &HashMap<String, ValidatorRecord>,
) -> Vec<ValidatorsAggregated> {
    let mut epochs: HashSet<_> = Default::default();
    let mut epochs_start_dates: HashMap<u64, DateTime<Utc>> = Default::default();
    let mut marinade_scores: HashMap<u64, Vec<f64>> = Default::default();
    let mut apys: HashMap<u64, Vec<f64>> = Default::default();

    for (_, validator) in validators.iter() {
        for epoch_stats in validator.epoch_stats.iter() {
            epochs.insert(epoch_stats.epoch);
            epochs_start_dates.insert(
                epoch_stats.epoch,
                epoch_stats.epoch_start_at.unwrap_or(Utc::now()),
            );
            if let Some(score) = epoch_stats.score {
                marinade_scores
                    .entry(epoch_stats.epoch)
                    .or_insert(Default::default())
                    .push(score);
            }
            if let Some(apy) = epoch_stats.apy {
                apys.entry(epoch_stats.epoch)
                    .or_insert(Default::default())
                    .push(apy);
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

    agg.sort_by(|a, b| b.epoch.cmp(&a.epoch));

    agg
}

pub async fn load_validators_aggregated_flat(
    psql_client: &Client,
    last_epoch: u64,
    epochs: u64,
) -> anyhow::Result<Vec<ValidatorAggregatedFlat>> {
    let rows = psql_client
            .query(
                "with
                cluster_stake as (select epoch, sum(activated_stake) as stake from validators group by epoch),
                cluster_skip_rate as (select epoch, sum(skip_rate * activated_stake) / sum(activated_stake) stake_weighted_skip_rate from validators group by epoch),
                dc as (select validators.epoch, sum(activated_stake) / cluster_stake.stake as dc_concentration, dc_aso from validators left join cluster_stake on validators.epoch = cluster_stake.epoch group by validators.epoch, dc_aso, cluster_stake.stake),
                agg_versions as (select vote_account, (array_agg(version order by created_at desc))[1] as last_version from versions where version is not null group by vote_account)
                select
                    validators.vote_account,
                    min(activated_stake / 1e9)::double precision as minimum_stake,
                    avg(activated_stake / 1e9)::double precision as avg_stake,
                    coalesce(avg(dc_concentration), 0)::double precision as avg_dc_concentration,
                    coalesce(avg(skip_rate), 1)::double precision as avg_skip_rate,
                    coalesce(avg(case when leader_slots < 200 then least(skip_rate, cluster_skip_rate.stake_weighted_skip_rate) else skip_rate end), 1)::double precision as avg_grace_skip_rate,		
                    max(coalesce(commission_effective, commission_advertised, 100)) as max_commission,
                    (coalesce(avg(credits * greatest(0, 100 - coalesce(commission_effective, commission_advertised, 100))), 0) / 100)::double precision as avg_adjusted_credits,
                    coalesce((array_agg(validators.dc_aso ORDER BY validators.epoch DESC))[1], 'Unknown') dc_aso,
                    coalesce((array_agg((marinade_stake / 1e9)::double precision ORDER BY validators.epoch DESC))[1], 0) as marinade_stake,
                    coalesce((array_agg(agg_versions.last_version))[1], '0.0.0') as last_version
                from
                    validators
                    left join dc on dc.dc_aso = validators.dc_aso and dc.epoch = validators.epoch
                    left join cluster_skip_rate on cluster_skip_rate.epoch = validators.epoch
                    left join agg_versions on validators.vote_account = agg_versions.vote_account
                where
                validators.epoch between $1 and $2
                group by validators.vote_account
                having count(*) = $3 and count(*) filter (where credits > 0) > 0
                order by avg_adjusted_credits desc;
            ",
                &[&Decimal::from(last_epoch - u64::min(last_epoch, epochs - 1)), &Decimal::from(last_epoch), &i64::try_from(epochs).unwrap()],
            )
            .await?; // and vote_account not in ('3Z1N2Fkfha4ThNiRwN8RnU6U8dkFJ92DH2TFyLWJf8cj','2Dwg3x37yN4q8SyrrwDaRPGQTp14atcwMPewe3Y8FDoL','GkBrxrDjmx2kfTMUZJgYWAbar9fEpYJW7TgLatrZSjhN','5fdEXhCBKC7FRRsH64asZCSiwgNXRozxmzb1cFzfrtWM','7y4wStv8XxUkuBgwNkidfxdy1V6TMYr4UjaTDwcS3MUr','C33g1CBgcc47XFcrYksA3CEkBKaitKuhs9yD7LLtW98K','964w4qykexipZ7aCur1BEeJtexTMa1ehMUc9tCcxm9J3','Gj63nResvnBrKLw4GyyfWFpTudwQWDe9bExkE9Z1LvB1','9uygnf8zm2A88bS4tjqiYUPKAUuSWkJGeHxKLTndrs6v','A465fkGZut4A7FncUvzbCzGD8QE98yn2Lm8grr93c9dV','13zyX9jfGy1RvM28LcdqfLwR4VSowXx6whAL6AcFERCk','AUgLtpPVz6zL4iVCXZwi3cifLERdvnHsuVhNKzqmW45i','4R2eqfCDqN3UesKPW4kSTZVd55955V4awbof4vBuWibY','97AgcJPr1KGkwhq7tSD2LDMADreeCpGoFcX6hWjEuQpi','dVcK6ZibNvBiiwqxadXEGheznJFsWY9SHiMb8afTQns','C9pfCHG1zx5fTtmbsFwLG6yFoztyUVXoCmirUcCe2dt7','5hVPfoTZcfZTcyondKxjuVczaFap9pBGYBSPKXg9Jrg5','DXv73X82WCjVMsqDszK3z764tTJMU3nPXyCU3UktudBG','FvoZJRfV8LWMbkMeiswKzMHSZ2qvU8KVsEkUaW6MdN8m','6EftrAURp1rwpmy7Jeqem4kwWYeSnKmgYWKbdX5gEBHQ','HCZJjvZbaKaPTE96jz64HnBZTnXHBFv3pugqsBE5Z1D9','9fDyXmKS8Qgf9TNsRoDw8q2FJJL5J8LN7Y52sddigqyi','BHGHrKBJ9z6oE4Rjd7rBTsy9GLiFcTeDbkTkC5YmT5JG','CQCvXh6fDejoKVeMKXWirksnwCnhrLzb6XkyrBoQJzX5','Agsu9fcnH3rKBix59mktDRqJhjR8aStgLDd9njaddcdr','AUYVsW5ZGwPMAiFJUuAYPtCB9Xp5CVA1osJyAasj8CLe','FKCcfoLt2pq7boiNqRGucVq5LE1K5Dt4HALCx2WbEQkv','DHasctf9Gs2hRY2QzSoRiLuJnuEkRcGHSrh2JUxthxwa','467Bg8FwFFq5jqebWPnMQtdDRjpmHUqvCWBW1zYVMzHg','JCHsvHwF6TgeM1fapxgAkhVKDU5QtPox3bfCR5sjWirP','72i2i4D7ehg7k3rKUNZLFiHqvhVFv1UvdCQbiNTFudJa','G4RU9qUt7tG8M8E4L4ZfXtdnwPTcPpaWwLEvSxtdRNHF','JB8zjnRE6FeT8N6Yq2182vj69kKHGdeKJ7kBAhHKuHRq','Amhxcj1nt4BhnmTfy3ncqaoLzVr94QEfGMYY9Lqkg9en','783PrbTTsMojSJWv64ZCFnQ7mYoDj4oqdsAGXf22XVQs','FrWFgD5vfjJkKiCY4WeKBk65X4C7sDhi2X1DVMFCPfJ5','BeNvYv2pd3MRJBBSGiMPSRVYcafKAXocNNp79GoaHfoP','ZBfLjZjz48oS3ArtnjmPn4Fc1bd2VbKeBnxeCSrKE9S','DzhGmMUzpyQ5ruk5rRCfekTZMyvPXBXHtnn6aNnt94x4','8cUmk4UHZXFBXZJBnWnXd48iTSRMYikQ1QYbJddBfAxu','85qJ2DWmav9YgKLLdo6mrVAVLLKRH3fDuWPyiViA362n','5xk3gjstftRwZRqQdme4vTuZonpkgs2wsm734M68Bq1Y','7iC1Uu6QBqNG6oaBimnPgLmtoantH7Nc3RD7SoLHgVET','8sqpHTT3B8kLto6Vb98bNP93MVuRuPKdcUAwZaP6xuYs','4QMtvpJ2cFLWAa363dZsr46aBeDAnEsF66jomv4eVqu4','4rxFGSzXiTXuF9GveXbMr4fJAPPnQVjHmpEZbWV8jz9m','HC1NSDR9cbBeQ8V1XJ62VNceUAbjGdnCcH7f5wVFVZw3','Bm3rPaD62YWXJxvpW5viF9jUVdMmd7Q2HYA6eTbDhxxW','DeNodee9LR1WPokmRqidmAQEq8UbBqNCv6QfFTvU6k69','9xMyJXgxBABzV5bmiCuw4xZ8acao2xgvhC1G1yknW1Zj','9DYSMwSwMbQcckH1Zi7EQ3E6ipJKkChqRVJCQjF5FCWp','DqxQuDD9BZERufL2gTCipHhAqj7Bb2zoAEKfvHuXWNUL','FfE7rncxyYJvsqFu3Kn323sJpjBXkfMNXwd4d8kdURk9','9HvcpT4vGkgDU3TUXuJFtC5DPjMt5jb8MXFFTNTg9Mim','CrZEDyNQfbxakxdFYzMc8dtrYq4XDoRZ51xBa12skDpJ','D7wuZ935mznAM6hRJJQBpWcBWyvVgUK96pPDZT2uZq5g','9c5bpzVRbfsYY2fannb4hyX5CJUPg3BfH2cL6sR7kJM4','CnYYmAhuFcyocBbXxoVzPnu37a5ctpLaSr8ja1NGKNZ7','ESF3vCij1t6K437j7tzDyKspPeuMnYoEtooFN9Suzico','GHRvDXj9BfACkJ9CoLWbpi2UkMVti9DwXJGsaFT9XDcD','5HMtU9ngrq7vhQn4qPxFHzaVJRjbnT2VQxTTPdfwvbUL','3HSgNsx9rQsAFZrL7k2BAUuL8HpCjhgfxXjrPBK9cnjD','5GKFk6ptwtYTUVXZwofK3tgCJXRiQBfY6yS9w8dgZaSS','CZw1sSfjZbCccsk2kTjbFeVSgfzEgV1JxHEsEW69Qce9','6XimUrvgbdoAuavV9WGgSYdYKSw6ghajLGeQgZMG9aZU','13fUogQP3K8jAWgSW5gji5NyqHFprwoW3xVRs9MpLqdp','5KF6gMG6f5GCr4V6BXKzdroHxeXK68oKrLQdiujGsj9m','EAZpeduar1WoSCyR8W4YhurN3FfVmuKwdPx4ruy58VU8','4GhsFrzqekca4FiZQdwqbstzcEXsredqpemF9FdRQBqZ','BCFLyTNSoxQbVrTogK8n7ft1oYAgHYEdzafVfGgqz9WN','DREVB8Ce8nLp9Ha5m66sduRcjJtHeQo8B9BkYxjC4Zx3','A8XYMkTzKNceJT7BKtRwGrg5KGgaXcjyoAYuthrjfKUi','2e6hcXeqPMwskDfQXKuwVuHiFByEwaiG9ohgapNBk6qU','53gnaHMxDzGTZ9A58S4jbc1qzhYT4X51thUD4MdSBiyo','2kQZfvm5tqcBhXnscT3xe5SbCDttkipxgy1wCqhzqL2a','6vJGsbs5jYKEdQGUfMEYN4Nenwscgza1dBXB3WJraFyH','CGWR68eEdSDoj5LUn2MGKBgxRC3By64DCBFHCoHdSV21','DDmp7zGUzKhXsZhnUynohWrrKyWFf9gSJcGacihRRHuU','7yvrrixKhYrxMJHjzsPDz8tSAajLL3oD2arAsgeMdK9N','9DsSqMHnrSXkyHtG8sN4zPhjrsRUgfP9vBQ6hFEpEwM','5HedSkUKfYmusiV7rAppuHbz7fp8JmUoLLJjJqCQLS5j','725My2yzg5ZUpQtpEtivLT7JmRes2gGxF3KeGCbYACDe','9Mi8M1JnRmtcYpB42DxYPVmYy2safgdYFmeHmMgkW8TG','2jevuBmk1TrXA36bRZ4bhdJGPzGqcCDoVzRcyYtxzKHY','CHUF69YeA3gZv484izYuhKk1EjaJYjv1pNoJJ6QeFDQc','8b3JPQtHbw8MBJQNwUDVXC6xfaL26UNx6WA3GShGy5Vw','HHFqy4NJteQJScyoAsjwYjS8wCuV1AjNv4veoeKVACRi','DMYn88X6PkHAc2y5zDWm5jGZ2Tk2CyBUe8K1U2obF8jc','C31ocJKVAi8wxCvyAMjXte2fY9zECV2fKrn786F4WZ4N','6g6jypXGeavZPVkWSu4Ny5bfhTMLFnuSepfGMQkQpWV1','D6AkdRCEAvE8Rjh4AKDSCXZ5BaiKpp79da6XtUJotGq5','3Le35iTn2KXRfomruXiDLcMd4BVLKYVgQ7yssmrFJZXx','9TTpcbiTDUQH9goeRvhAhk4X3ahtZ6XttCjRyH8Pu7MP','7yXM5mUSAtBuh2TcCABvSJa3LouZ8wcLps5zTEMiwxvj','LSV1yYBUsxwY8y7AgL2RcJDVJjnwxfeShxaXm7Edrwr','FQY5UU6THEhRNZRg7YXfYGQhJi45TLXrHg76EsXJmESc','9TUJdBxnHvAapYoq8cVFgh1bMbTh6GYfY4etDqWVKXAT','FSH9xke8FBpx6YxEEzNXVWgjmT3G5SN9HpipmCSVamV','CFtrZKxqGfXSuZrM5G64prTfNM8GqWQFQa3GXq4tdzx2','BifNttkf51HzsPgUf2keDdVBL64YvnAVQGF3fkNDfB56','4e1B3jra6oS7nK5wLn9mPMtX1skUJsEvmhV9MscA6UA4','DZJWKjtj1fCJDWTYL1HvF9rLrxRRKKp6GQgyujEzqc22','3YKcH4c8eoAKkghQeGavg9HZ13fSe77RWM3QoFTCV2Gv','7r4qcfiaWaZ8i9zW2YjqgRLEpGGE7L5hfW8ncpF1HdTs','8aGU18Nxn99AEWEQNrBYy1ZsJBhiHVFrcqYqHQPNhmEv','5TFdzjKE6LnkhQArxWjt26yVCXskPo3fUXE8F351Cfn7','DpodNd2DWRLbNJVuV1R33xW2PkBJyRTFU5aZGmQrVtMi','Dpy1qt9MhoRD5YpmzfM9iBw2LuRvP1nZAa9La77nAHW5','8kcrp8M2c5LGYThHQxVgsp7BGfGjHZ9fLHa6YN3YpFNa','BgjQPDdsHeD9XXs7pYyHsmvKdkLR1A4SBNYs3mLmPUCD','9EzbogBnGi8hVeLXEyFu2xUo6qi5JdEELs4y3cQXQW33','9uASGafRPWpvpfXeuwcA3TzMUuP5BoHfQWtcdGMyYR9x','7aE66BtyfPELpp6hnb6qb3PjQzmzMqXRMKx9EU7tHak6','6JjWRGRM94G2cpnsqDD3KL8p4ravnFSpJP778V6M9LUS','AeHBkLDeWtMHqrM4uwuewwWKtyKd5aBZAygxZJ3MCjRp','6c2FJC1NfzNvivapAzPW8vj9TW63dpHCVh7zzehwnNLH','D7ZCDE1PHe8duMjNpxwHrYbrRzcnsS7p4nD2daLzWwtr','DaNexGpPeQChZTPZAn1BGmd4ASQpHE4hLDv7V4iAe2oA','8HciLEx6hGdb8mxaCx7ExFBxkcgdkpt43FhiJdvPA6XZ','ouLzBTp7vqzT1mhjtg1TpYHwACJpeB5akRC1zDVdg1N','AQB1eoovP55TyjkecjCPTvfXBEzS1JH1sxWguBo1gu9d','J9Go27V87fCdJtjMxmFJu48ctrHzFoe6xQpA6Ecq4Wkw','Fu4wz4US6dV6GzZrv9NnF18KeT47tdbDKRd7pA6DiyS4','CwEsA6kkUZHnuCK2HC1WVpriBpZWFJSKW9xxrdednm6J','5bjKPhoQDcpPVeMhu83SEtXqXA9vw62k7KhL9zpsK31b','2ikGwX24ATJQHPtWpHupEAJvAyp63niaFL5R2sGXwfnd','GAerry2FZncXgLJXohjgGmC3W4JKLDFxwhGz4beTgDeP','8sNLx7RinHfPWeoYE1N4dixtNACvgiUrj9Ty81p7iMhb','9EMmPd6zKqTnpj74rgmkTjkYAsZSZ42jBWcqu6iaoGTR','3v6FfdWMT2bcoQQ9hN4F2syu7qhRHzNuCPPQqV12hsw2','BYNXBFkB89FoRCJ4VxFE9Tfde3anECjZjasTP8qSYQUi','3iD36QhXqWzx5b4HHhkRAyUcbEgCaC42hi1GcBePNsp2','8hpaVczvUK24kogYWxV6s3hajDAbaHb6KGZsVRLDoksi','J61sYWwTT3Kfkjy3gJ1ViRwtfXVp7Bi89DLqvCp5WDgC','A9hwhEeQ7hNm8rPbRX7ZDAZRjTVrUCjgDEDD4Tt8rmT7','9b9F4xYHMenZfbD8pSLm45oJfoFYPQ9RVWPXSEmJQzVn','DawVi7TKkWS81ZKyGTmxLAabL1w5gcw8FhGgbHeGJGnj','5ycsa9WVK6zFcUR13m3yijxbevZehZeCbuSocSawsweW','4qS6unxhpNh6fp2rRU3nnyMZEYyZ4hUbjnP7iEN7Jx1w','BxTUwfMiokzimVDLDupGfVPmWXfLSGVpkGr9TUmetn6b','8J4xNmyAQskmPuyywPf1arig4X8hfza2xKBkKwz8E2gU','6zDrZWRXQ7GWi1W2fBTzSs59PSa2uj2k8w2qkc451rqG','74ibS6YRDBF3jMf3bxiLY1i3ohFhJySQwyeeMWaRsAk2','HTpinijYNYPe2UhfwoX7fHKC9j44QEJoVmStCmfvYZxA','76sb4FZPwewvxtST5tJMp9N43jj4hDC5DQ7bv8kBi1rA','FKyoehgzXD6KVSQoHJuTteXGCrChYe65k98wMckr9MN8','4qSZsB9QjXr97HzhzPd1zuvB8z7tqqDuM1xbxB5PcPFh','1M5USfamd1N4i1z6UZeECrWeu2VfrxjYMBSXThu6TqB','7LCnWqQGpNCiUvBLznYG9Q6Zo7mcLkhAHA7YBjbg8SET','AU4yDLbrnLzcjk2pnxvXwNeKJsj9CiUDRXWQbeSbk6Y9','AeSLUUNmADEM2xzfmWbRhfwomvJW3f3Rd1AdicXf27Gb','4RyNsFHDccFEEnbYJAFt2cNufFduh8Se8eKTqXDVr82h','5enTTfG63W4JUzCpwioeLte7827NrYXUgGr6z7Rm7xf5','8usnMxy6YunbfrjHDHPfRcpLWXigcSvrpVohv3F2v24H','J4ooR8AV8o5Ez2qN8ghQhR7YKhqRY5WEHfE8dTR2Yo6a','HFY5f6PF6cRyVAvVG1xV9X15q87qoZ1o6GDcyBzHSEnX')

    let mut validators: Vec<ValidatorAggregatedFlat> = Default::default();
    for row in rows.iter() {
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
    mut psql_client: &mut Client,
    epoch: i32,
    ui_id: String,
    components: Vec<&str>,
    component_weights: Vec<f64>,
    scores: Vec<crate::dto::ValidatorScoringCsvRow>,
) -> anyhow::Result<()> {
    let scoring_run_result = psql_client
        .query_one(
            "INSERT INTO scoring_runs (created_at, epoch, components, component_weights, ui_id)
            VALUES (now(), $1, $2, $3, $4) RETURNING scoring_run_id;",
            &[&epoch, &components, &component_weights, &ui_id],
        )
        .await?;

    let scoring_run_id: i64 = scoring_run_result.get("scoring_run_id");

    log::info!("Stored scoring run: {}", scoring_run_id);

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
                if row.ui_hints.len() == 0 {
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
        query.execute(&mut psql_client).await?;
    }

    Ok(())
}
