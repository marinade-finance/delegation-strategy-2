use crate::utils::*;
use crate::CommonParams;
use chrono::{DateTime, Duration, Utc};
use collect::validators::Snapshot;
use core::marker::Send;
use log::{debug, info};
use postgres::types::ToSql;
use postgres::{Client, NoTls};
use rust_decimal::prelude::*;
use serde::Serialize;
use serde_yaml::{self};
use std::collections::{HashMap, HashSet};
use structopt::StructOpt;

#[derive(Debug, StructOpt)]
pub struct StoreCommissionsOptions {}

pub fn store_commissions(
    common_params: CommonParams,
    mut psql_client: Client,
) -> anyhow::Result<()> {
    info!("Storing commission...");

    let snapshot_file = std::fs::File::open(common_params.snapshot_path)?;
    let snapshot: Snapshot = serde_yaml::from_reader(snapshot_file)?;
    let validators: HashMap<_, _> = snapshot
        .validators
        .iter()
        .map(|v| (v.identity.clone(), v.clone()))
        .collect();
    let snapshot_epoch_slot: Decimal = snapshot.epoch_slot.into();
    let snapshot_epoch: Decimal = snapshot.epoch.into();
    let snapshot_created_at = snapshot.created_at.parse::<DateTime<Utc>>().unwrap();
    let mut skip_validators = HashSet::new();

    info!("Loaded the snapshot");

    for row in psql_client.query(
        "
        SELECT DISTINCT ON (identity)
            identity,
            commission,
            epoch
        FROM commissions
        ORDER BY identity, created_at DESC
    ",
        &[],
    )? {
        let identity: &str = row.get("identity");
        let commission: i32 = row.get("commission");
        let epoch: Decimal = row.get("epoch");

        if let Some(validator_snapshot) = validators.get(identity) {
            if epoch == snapshot_epoch && commission == validator_snapshot.commission {
                skip_validators.insert(identity.to_string());
            }
        }
    }
    info!(
        "Found {} validators with no changes since last run",
        skip_validators.len()
    );

    let mut query = InsertQueryCombiner::new(
        "commissions".to_string(),
        "identity, commission, epoch_slot, epoch, created_at".to_string(),
    );

    for (identity, snapshot) in validators.iter() {
        if !skip_validators.contains(identity) {
            let mut params: Vec<&(dyn ToSql + Sync)> = vec![
                &snapshot.identity,
                &snapshot.commission,
                &snapshot_epoch_slot,
                &snapshot_epoch,
                &snapshot_created_at,
            ];
            query.add(&mut params);
        }
    }
    let insertions = query.execute(&mut psql_client)?;

    info!("Stored {} commission changes", insertions.unwrap_or(0));

    Ok(())
}
