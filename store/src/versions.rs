use crate::utils::*;
use crate::CommonParams;
use chrono::{DateTime, Utc};
use collect::validators::Snapshot;
use log::info;
use postgres::types::ToSql;
use postgres::Client;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use structopt::StructOpt;

#[derive(Debug, StructOpt)]
pub struct ClusterInfoOptions {}

pub fn store_cluster_info(
    common_params: CommonParams,
    mut psql_client: Client,
) -> anyhow::Result<()> {
    info!("Storing versions...");

    let snapshot_file = std::fs::File::open(common_params.snapshot_path)?;
    let snapshot: Snapshot = serde_yaml::from_reader(snapshot_file)?;
    let validators: HashMap<_, _> = snapshot
        .validators
        .iter()
        .map(|v| (v.identity.clone(), v.clone()))
        .collect();
    let snapshot_epoch_slot = snapshot.epoch_slot as i64;
    let snapshot_epoch = snapshot.epoch as i64;
    let snapshot_created_at = snapshot.created_at.parse::<DateTime<Utc>>().unwrap();
    let mut skip_validators = HashSet::new();

    info!("Loaded the snapshot");

    for row in psql_client.query(
        "
        SELECT DISTINCT ON (identity)
            identity,
            version,
            epoch
        FROM versions
        ORDER BY identity, created_at DESC
    ",
        &[],
    )? {
        let identity: &str = row.get("identity");
        let version: &str = row.get("version");
        let epoch: i64 = row.get("epoch");

        if let Some(validator_snapshot) = validators.get(identity) {
            if epoch == snapshot_epoch && version == validator_snapshot.version {
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

    for (identity, validator) in validators.iter() {
        if !skip_validators.contains(identity) {
            let mut params: Vec<&(dyn ToSql + Sync)> = vec![
                &validator.identity,
                &validator.version,
                &snapshot_epoch_slot,
                &snapshot_epoch,
                &snapshot_created_at,
            ];
            query.add(&mut params);
        }
    }
    let insertions = query.execute(&mut psql_client)?;

    info!("Stored {} version changes", insertions.unwrap_or(0));

    Ok(())
}
