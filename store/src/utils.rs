use crate::dto::{
    CommissionRecord, UptimeRecord, ValidatorEpochStats, ValidatorRecord, VersionRecord,
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

pub async fn load_uptimes(
    psql_client: &Client,
    identity: String,
    epochs: u8,
) -> anyhow::Result<Vec<UptimeRecord>> {
    let rows = psql_client
        .query(
            "
            WITH cluster AS (SELECT MAX(epoch) as last_epoch FROM cluster_info)
            SELECT
                status, epoch, start_at, end_at
            FROM uptimes, cluster WHERE identity = $1 AND epoch > cluster.last_epoch - $2::NUMERIC",
            &[&identity, &Decimal::from(epochs)],
        )
        .await?;

    let mut records: Vec<_> = Default::default();
    for row in rows {
        records.push(UptimeRecord {
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
    identity: String,
    epochs: u8,
) -> anyhow::Result<Vec<VersionRecord>> {
    let rows = psql_client
        .query(
            "
            WITH cluster AS (SELECT MAX(epoch) as last_epoch FROM cluster_info)
            SELECT
                version, epoch, created_at
            FROM versions, cluster WHERE identity = $1 AND epoch > cluster.last_epoch - $2::NUMERIC",
            &[&identity, &Decimal::from(epochs)],
        )
        .await?;

    let mut records: Vec<_> = Default::default();
    for row in rows {
        records.push(VersionRecord {
            epoch: row.get::<_, Decimal>("epoch").try_into()?,
            version: row.get("version"),
            created_at: row.get("created_at"),
        })
    }

    Ok(records)
}

pub async fn load_commissions(
    psql_client: &Client,
    identity: String,
    epochs: u8,
) -> anyhow::Result<Vec<CommissionRecord>> {
    let rows = psql_client
        .query(
            "
            WITH cluster AS (SELECT MAX(epoch) as last_epoch FROM cluster_info)
            SELECT
                commission, epoch, created_at
            FROM commissions, cluster WHERE identity = $1 AND epoch > cluster.last_epoch - $2::NUMERIC",
            &[&identity, &Decimal::from(epochs)],
        )
        .await?;

    let mut records: Vec<_> = Default::default();
    for row in rows {
        records.push(CommissionRecord {
            epoch: row.get::<_, Decimal>("epoch").try_into()?,
            commission: row.get::<_, i32>("commission").try_into()?,
            created_at: row.get("created_at"),
        })
    }

    Ok(records)
}

pub async fn load_validators(
    psql_client: &Client,
    epochs: u8,
) -> anyhow::Result<HashMap<String, ValidatorRecord>> {
    let rows = psql_client
        .query(
            "
            WITH cluster AS (SELECT MAX(epoch) as last_epoch FROM cluster_info)
            SELECT
                identity, vote_account, epoch,

                info_name,
                info_url,
                info_keybase,
                dc_ip,
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

        let record = records
            .entry(identity.clone())
            .or_insert_with(|| ValidatorRecord {
                identity,
                vote_account: row.get("vote_account"),
                info_name: row.get("info_name"),
                info_url: row.get("info_url"),
                info_keybase: row.get("info_keybase"),
                dc_ip: row.get("dc_ip"),
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
                superminority: false,
                credits: row.get::<_, Decimal>("credits").try_into().unwrap(),
                marinade_score: 0,

                epoch_stats: Default::default(),
            });
        record.epoch_stats.push(ValidatorEpochStats {
            epoch: row.get::<_, Decimal>("epoch").try_into()?,
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
            superminority: false,
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
        });
    }

    Ok(records)
}
