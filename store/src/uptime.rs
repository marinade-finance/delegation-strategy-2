use crate::utils::*;
use crate::CommonParams;
use chrono::{DateTime, Duration, Utc};
use collect::validators::ValidatorSnapshot;
use core::marker::Send;
use log::{debug, info};
use postgres::types::ToSql;
use postgres::{Client, NoTls};
use serde::Serialize;
use serde_yaml::{self};
use std::collections::{HashMap, HashSet};
use structopt::StructOpt;

#[derive(Debug, StructOpt)]
pub struct StoreUptimeOptions {}

static UP: &str = "UP";
static DOWN: &str = "DOWN";

fn status_from_delinquency(delinquent: bool) -> &'static str {
    if delinquent {
        DOWN
    } else {
        UP
    }
}

pub fn store_uptime(common_params: CommonParams, mut psql_client: Client) -> anyhow::Result<()> {
    info!("Storing uptime...");
    let now = Utc::now();
    let status_end_at = now.checked_add_signed(Duration::minutes(1)).unwrap();
    let status_max_delay_to_extend = Duration::minutes(2);

    let snapshot_file = std::fs::File::open(common_params.snapshot_path)?;
    let validators: Vec<ValidatorSnapshot> = serde_yaml::from_reader(snapshot_file)?;
    let validators: HashMap<_, _> = validators
        .iter()
        .map(|v| (v.identity.clone(), v.clone()))
        .collect();
    let mut validators_with_extended_status: HashSet<String> = HashSet::new();
    let mut records_to_extend = HashSet::new();
    info!("Loaded the snapshot");

    for row in psql_client.query(
        "
        SELECT DISTINCT ON (identity)
            id,
            identity,
            status,
            start_at,
            end_at
        FROM uptimes
        ORDER BY identity, end_at DESC
    ",
        &[],
    )? {
        let id: i64 = row.get(0);
        let identity: &str = row.get(1);
        let status: &str = row.get(2);
        let start_at: DateTime<Utc> = row.get(3);
        let end_at: DateTime<Utc> = row.get(4);
        let latest_end_extension_at = end_at
            .checked_add_signed(status_max_delay_to_extend.clone())
            .unwrap();

        if let Some(snapshot) = validators.get(identity) {
            let status_from_snapshot = status_from_delinquency(snapshot.performance.delinquent);
            if latest_end_extension_at > now && status == status_from_snapshot {
                validators_with_extended_status.insert(identity.to_string());
                records_to_extend.insert(id);
            }
        }

        println!(
            "found uptime record: {} {} {} {} {}",
            id, identity, status, start_at, end_at
        );
    }

    let mut query = UpdateQueryCombiner::new(
        "uptimes".to_string(),
        "uptimes.end_at = update.end_at".to_string(),
        "updates(id, end_at)".to_string(),
        "uptimes.id = updates.id".to_string(),
    );

    for record_to_extend in records_to_extend.iter() {
        let mut params: Vec<&(dyn ToSql + Sync)> = vec![record_to_extend, &status_end_at];
        query.add(&mut params);
    }
    query.execute(&mut psql_client)?;
    info!("Extended previous uptimes");

    let mut query = InsertQueryCombiner::new(
        "uptimes".to_string(),
        "identity, status, start_at, end_at".to_string(),
    );

    for (identity, snapshot) in validators.iter() {
        if !validators_with_extended_status.contains(identity) {
            if snapshot.performance.delinquent {
                let mut params: Vec<&(dyn ToSql + Sync)> =
                    vec![&snapshot.identity, &DOWN, &now, &now];
                query.add(&mut params);
            } else {
                let mut params: Vec<&(dyn ToSql + Sync)> =
                    vec![&snapshot.identity, &UP, &now, &now];
                query.add(&mut params);
            }
        }
    }
    let insertions = query.execute(&mut psql_client)?;

    info!("Stored {} changed uptimes", insertions.unwrap_or(0));

    Ok(())
}
