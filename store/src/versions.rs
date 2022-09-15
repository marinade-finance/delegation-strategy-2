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
pub struct StoreVersionsOptions {}

pub fn store_versions(common_params: CommonParams, mut psql_client: Client) -> anyhow::Result<()> {
    info!("Storing versions...");
    let now = Utc::now();

    let snapshot_file = std::fs::File::open(common_params.snapshot_path)?;
    let validators: Vec<ValidatorSnapshot> = serde_yaml::from_reader(snapshot_file)?;
    let validators: HashMap<_, _> = validators
        .iter()
        .map(|v| (v.identity.clone(), v.clone()))
        .collect();
    let mut skip_validators = HashSet::new();

    info!("Loaded the snapshot");

    for row in psql_client.query(
        "
        SELECT DISTINCT ON (identity)
            id,
            identity,
            version,
            epoch_slot,
            epoch,
            created_at
        FROM versions
        ORDER BY identity, created_at DESC
    ",
        &[],
    )? {
        let id: i64 = row.get(0);
        let identity: &str = row.get(1);
        let version: &str = row.get(2);
        let epoch_slot: i32 = row.get(3);
        let epoch: i32 = row.get(4);
        let created_at: DateTime<Utc> = row.get(5);

        if let Some(snapshot) = validators.get(identity) {
            if
            /*epoch == snapshot.epoch &&*/
            version == snapshot.version {
                skip_validators.insert(identity.to_string());
            }
        }
    }
    info!(
        "Found {} validators with no changes since last run",
        skip_validators.len()
    );

    let mut query = InsertQueryCombiner::new(
        "versions".to_string(),
        "identity, version, epoch_slot, epoch, created_at".to_string(),
    );

    for (identity, snapshot) in validators.iter() {
        if !skip_validators.contains(identity) {
            let mut params: Vec<&(dyn ToSql + Sync)> =
                vec![&snapshot.identity, &snapshot.version, &0, &0, &now];
            query.add(&mut params);
        }
    }
    let insertions = query.execute(&mut psql_client)?;

    info!("Stored {} version changes", insertions.unwrap_or(0));

    Ok(())
}
