use crate::dto::{
    CommissionRecord, UptimeRecord, ValidatorEpochStats, ValidatorRecord, VersionRecord,
    WarningRecord,
};
use rust_decimal::prelude::*;
use std::collections::HashMap;
use tokio_postgres::{types::ToSql, Client};

pub struct InsertQueryCombiner<'a> {
    pub insertions: u64,
    statement: String,
    params: Vec<&'a (dyn ToSql + Sync)>,
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

struct InflationApyCalculator {
    supply: u64,
    duration: u64,
    inflation: f64,
    inflation_taper: f64,
    total_weighted_credits: u128,
}
impl InflationApyCalculator {
    fn estimate_yields(&self, credits: u64, stake: u64, commission: u8) -> (f64, f64) {
        let epochs_per_year = 365.25 * 24f64 * 3600f64 / self.duration as f64;
        let rewards_share = credits as f64 * stake as f64 / self.total_weighted_credits as f64;
        let inflation_change_per_epoch = (1.0 - self.inflation_taper).powf(1.0 / epochs_per_year);
        let generated_rewards =
            self.inflation * self.supply as f64 * rewards_share * self.inflation_taper
                / epochs_per_year
                / (1.0 - inflation_change_per_epoch);
        let staker_rewards = generated_rewards * (1.0 - commission as f64 / 100.0);
        let apr = staker_rewards / stake as f64;
        let apy = (1.0 + apr / epochs_per_year).powf(epochs_per_year - 1.0) - 1.0;

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
                    inflation_taper,
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
                inflation_taper: row.get("inflation_taper"),
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
    epochs: u8,
) -> anyhow::Result<HashMap<String, Vec<UptimeRecord>>> {
    let rows = psql_client
        .query(
            "
            WITH cluster AS (SELECT MAX(epoch) as last_epoch FROM cluster_info)
            SELECT
                identity, status, epoch, start_at, end_at
            FROM uptimes, cluster WHERE epoch > cluster.last_epoch - $1::NUMERIC",
            &[&Decimal::from(epochs)],
        )
        .await?;

    let mut records: HashMap<_, Vec<_>> = Default::default();
    for row in rows {
        let identity: String = row.get("identity");
        let commissions = records
            .entry(identity.clone())
            .or_insert(Default::default());
        commissions.push(UptimeRecord {
            epoch: row.get::<_, Decimal>("epoch").try_into()?,
            status: row.get("status"),
            start_at: row.get("start_at"),
            end_at: row.get("end_at"),
        })
    }

    Ok(records)
}

pub async fn load_versions(
    psql_client: &Client,
    epochs: u8,
) -> anyhow::Result<HashMap<String, Vec<VersionRecord>>> {
    let rows = psql_client
        .query(
            "
            WITH cluster AS (SELECT MAX(epoch) as last_epoch FROM cluster_info)
            SELECT
                identity, version, epoch, created_at
            FROM versions, cluster WHERE epoch > cluster.last_epoch - $1::NUMERIC",
            &[&Decimal::from(epochs)],
        )
        .await?;

    let mut records: HashMap<_, Vec<_>> = Default::default();
    for row in rows {
        let identity: String = row.get("identity");
        let commissions = records
            .entry(identity.clone())
            .or_insert(Default::default());
        commissions.push(VersionRecord {
            epoch: row.get::<_, Decimal>("epoch").try_into()?,
            version: row.get("version"),
            created_at: row.get("created_at"),
        })
    }

    Ok(records)
}

pub async fn load_commissions(
    psql_client: &Client,
    epochs: u8,
) -> anyhow::Result<HashMap<String, Vec<CommissionRecord>>> {
    let rows = psql_client
        .query(
            "
            WITH cluster AS (SELECT MAX(epoch) as last_epoch FROM cluster_info)
            SELECT
                identity, commission, epoch, epoch_slot, created_at
            FROM commissions, cluster
            WHERE epoch > cluster.last_epoch - $1::NUMERIC
            UNION
            SELECT
                identity, commission_effective, epoch, 432000, updated_at
            FROM validators, cluster
            WHERE epoch > cluster.last_epoch - $1::NUMERIC AND commission_effective IS NOT NULL
            ",
            &[&Decimal::from(epochs)],
        )
        .await?;

    let mut records: HashMap<_, Vec<_>> = Default::default();
    for row in rows {
        let identity: String = row.get("identity");
        let commissions = records
            .entry(identity.clone())
            .or_insert(Default::default());
        commissions.push(CommissionRecord {
            epoch: row.get::<_, Decimal>("epoch").try_into()?,
            epoch_slot: row.get::<_, Decimal>("epoch_slot").try_into()?,
            commission: row.get::<_, i32>("commission").try_into()?,
            created_at: row.get("created_at"),
        })
    }

    Ok(records)
}

pub async fn load_warnings(
    psql_client: &Client,
) -> anyhow::Result<HashMap<String, Vec<WarningRecord>>> {
    let rows = psql_client
        .query(
            "SELECT identity, code, message, details, created_at FROM warnings",
            &[],
        )
        .await?;

    let mut records: HashMap<_, Vec<_>> = Default::default();
    for row in rows {
        let identity: String = row.get("identity");
        let warnings = records
            .entry(identity.clone())
            .or_insert(Default::default());
        warnings.push(WarningRecord {
            code: row.get("code"),
            message: row.get("message"),
            details: row.get("details"),
            created_at: row.get("created_at"),
        })
    }

    Ok(records)
}

pub async fn load_validators(
    psql_client: &Client,
    epochs: u8,
) -> anyhow::Result<HashMap<String, ValidatorRecord>> {
    let apy_calculators = get_apy_calculators(psql_client).await?;
    let warnings = load_warnings(psql_client).await?;

    let rows = psql_client
        .query(
            "
            WITH cluster AS (SELECT MAX(epoch) as last_epoch FROM cluster_info)
            SELECT
                identity, vote_account, epoch,

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

                commission_max_observed,
                commission_min_observed,
                commission_advertised,
                commission_effective,
                version,
                mnde_votes,
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
                downtime
            FROM validators, cluster
            WHERE epoch > cluster.last_epoch - $1::NUMERIC
            ORDER BY epoch DESC",
            &[&Decimal::from(epochs)],
        )
        .await?;

    let mut records: HashMap<_, _> = Default::default();
    for row in rows {
        let identity: String = row.get("identity");
        let epoch: u64 = row.get::<_, Decimal>("epoch").try_into()?;
        let (apr, apy) = if let Some(c) = apy_calculators.get(&epoch) {
            let (apr, apy) = c.estimate_yields(
                row.get::<_, Decimal>("credits").try_into()?,
                row.get::<_, Decimal>("activated_stake").try_into()?,
                row.get::<_, Option<i32>>("commission_effective")
                    .map(|n| n.try_into().unwrap())
                    .unwrap_or(100),
            );
            (Some(apr), Some(apy))
        } else {
            (None, None)
        };
        let record = records
            .entry(identity.clone())
            .or_insert_with(|| ValidatorRecord {
                identity: identity.clone(),
                vote_account: row.get("vote_account"),
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
                dc_asn: row.get("dc_asn"),
                dc_aso: row.get("dc_aso"),
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
                mnde_votes: row
                    .get::<_, Option<Decimal>>("mnde_votes")
                    .map(|n| n.try_into().unwrap()),
                activated_stake: row.get::<_, Decimal>("activated_stake").try_into().unwrap(),
                marinade_stake: row.get::<_, Decimal>("marinade_stake").try_into().unwrap(),
                decentralizer_stake: row
                    .get::<_, Decimal>("decentralizer_stake")
                    .try_into()
                    .unwrap(),
                superminority: row.get("superminority"),
                credits: row.get::<_, Decimal>("credits").try_into().unwrap(),
                marinade_score: 0,

                epoch_stats: Default::default(),

                warnings: warnings.get(&identity).cloned().unwrap_or(vec![]),
            });
        record.epoch_stats.push(ValidatorEpochStats {
            epoch,
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
            mnde_votes: row
                .get::<_, Option<Decimal>>("mnde_votes")
                .map(|n| n.try_into().unwrap()),
            activated_stake: row.get::<_, Decimal>("activated_stake").try_into()?,
            marinade_stake: row.get::<_, Decimal>("marinade_stake").try_into()?,
            decentralizer_stake: row.get::<_, Decimal>("decentralizer_stake").try_into()?,
            superminority: row.get("superminority"),
            stake_to_become_superminority: row
                .get::<_, Decimal>("stake_to_become_superminority")
                .try_into()?,
            credits: row.get::<_, Decimal>("credits").try_into()?,
            leader_slots: row.get::<_, Decimal>("leader_slots").try_into()?,
            blocks_produced: row.get::<_, Decimal>("blocks_produced").try_into()?,
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
        });
    }

    Ok(records)
}
