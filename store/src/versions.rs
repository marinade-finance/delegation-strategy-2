use crate::utils::*;
use chrono::{DateTime, Utc};
use collect::validators_performance::ValidatorsPerformanceSnapshot;
use log::info;
use postgres::types::ToSql;
use postgres::Client;
use rust_decimal::prelude::*;
use serde_yaml;
use std::collections::HashMap;
use structopt::StructOpt;

#[derive(Debug, StructOpt)]
pub struct StoreVersionsOptions {
    #[structopt(long = "snapshot-file")]
    snapshot_path: String,
}

pub fn store_versions(
    options: StoreVersionsOptions,
    mut psql_client: Client,
) -> anyhow::Result<()> {
    info!("Storing versions...");

    let snapshot_file = std::fs::File::open(options.snapshot_path)?;
    let snapshot: ValidatorsPerformanceSnapshot = serde_yaml::from_reader(snapshot_file)?;
    let snapshot_epoch_slot: Decimal = snapshot.epoch_slot.into();
    let snapshot_epoch: Decimal = snapshot.epoch.into();
    let snapshot_created_at = snapshot.created_at.parse::<DateTime<Utc>>().unwrap();

    info!("Loaded the snapshot");

    let mut versions: HashMap<String, Option<String>> = Default::default();

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
        let version: Option<String> = row.get("version");
        let epoch: Decimal = row.get("epoch");

        if let Some(validator_snapshot) = snapshot.validators.get(identity) {
            if epoch != snapshot_epoch && version != validator_snapshot.version {
                versions.insert(identity.to_string(), version);
            }
        }
    }

    let mut query = InsertQueryCombiner::new(
        "versions".to_string(),
        "identity, version, epoch_slot, epoch, created_at".to_string(),
    );

    for (identity, version) in versions.iter() {
        let mut params: Vec<&(dyn ToSql + Sync)> = vec![
            identity,
            version,
            &snapshot_epoch_slot,
            &snapshot_epoch,
            &snapshot_created_at,
        ];
        query.add(&mut params);
    }
    let insertions = query.execute(&mut psql_client)?;

    info!("Stored {} version changes", insertions.unwrap_or(0));

    Ok(())
}
