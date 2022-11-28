use crate::utils::*;
use chrono::{DateTime, Utc};
use collect::validators_performance::ValidatorsPerformanceSnapshot;
use log::info;
use postgres::types::ToSql;
use postgres::Client;
use rust_decimal::prelude::*;
use serde_yaml;
use std::collections::{HashMap, HashSet};
use structopt::StructOpt;

#[derive(Debug, StructOpt)]
pub struct StoreCommissionsOptions {
    #[structopt(long = "snapshot-file")]
    snapshot_path: String,
}

pub fn store_commissions(
    options: StoreCommissionsOptions,
    mut psql_client: Client,
) -> anyhow::Result<()> {
    info!("Storing commission...");

    let snapshot_file = std::fs::File::open(options.snapshot_path)?;
    let snapshot: ValidatorsPerformanceSnapshot = serde_yaml::from_reader(snapshot_file)?;
    let snapshot_epoch_slot: Decimal = snapshot.epoch_slot.into();
    let snapshot_epoch: Decimal = snapshot.epoch.into();
    let snapshot_created_at = snapshot.created_at.parse::<DateTime<Utc>>().unwrap();

    info!("Loaded the snapshot");

    let mut skipped_identities: HashSet<String> = Default::default();

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

        if let Some(validator_snapshot) = snapshot.validators.get(identity) {
            if epoch == snapshot_epoch && commission == validator_snapshot.commission as i32 {
                skipped_identities.insert(identity.to_string());
            }
        }
    }

    let mut query = InsertQueryCombiner::new(
        "commissions".to_string(),
        "identity, commission, epoch_slot, epoch, created_at".to_string(),
    );

    let commissions: HashMap<_, _> = snapshot
        .validators
        .iter()
        .map(|(i, v)| (i.clone(), v.commission as i32))
        .collect();

    for (identity, commission) in commissions.iter() {
        if !skipped_identities.contains(identity) {
            let mut params: Vec<&(dyn ToSql + Sync)> = vec![
                identity,
                commission,
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
