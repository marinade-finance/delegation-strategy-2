use crate::utils::*;
use chrono::{DateTime, Duration, Utc};
use collect::validators_performance::ValidatorsPerformanceSnapshot;
use log::{debug, info, warn};
use rust_decimal::prelude::*;
use serde_yaml;
use std::collections::{HashMap, HashSet};
use structopt::StructOpt;
use tokio_postgres::types::ToSql;
use tokio_postgres::Client;

#[derive(Debug, StructOpt)]
pub struct StoreUptimeOptions {
    #[structopt(long = "snapshot-file")]
    snapshot_path: String,
}

static UP: &str = "UP";
static DOWN: &str = "DOWN";

fn status_from_delinquency(delinquent: bool) -> &'static str {
    if delinquent {
        DOWN
    } else {
        UP
    }
}

pub async fn store_uptime(
    options: StoreUptimeOptions,
    mut psql_client: &mut Client,
) -> anyhow::Result<()> {
    info!("Storing uptime...");

    let snapshot_file = std::fs::File::open(options.snapshot_path)?;
    let snapshot: ValidatorsPerformanceSnapshot = serde_yaml::from_reader(snapshot_file)?;
    let mut validators_with_extended_status: HashSet<String> = HashSet::new();
    let snapshot_epoch: Decimal = snapshot.epoch.into();
    let snapshot_created_at = snapshot.created_at.parse::<DateTime<Utc>>().unwrap();
    let default_status_end_at = snapshot_created_at
        .checked_add_signed(Duration::minutes(1))
        .unwrap();
    let status_max_delay_to_extend = Duration::minutes(5);
    let mut records_extensions: HashMap<i64, DateTime<Utc>> = Default::default();

    info!("Loaded the snapshot");

    for row in psql_client
        .query(
            "
        SELECT DISTINCT ON (identity)
            id,
            identity,
            status,
            epoch,
            start_at,
            end_at
        FROM uptimes
        ORDER BY identity, end_at DESC
    ",
            &[],
        )
        .await?
    {
        let id: i64 = row.get("id");
        let identity: &str = row.get("identity");
        let status: &str = row.get("status");
        let epoch: Decimal = row.get("epoch");
        let start_at: DateTime<Utc> = row.get("start_at");
        let end_at: DateTime<Utc> = row.get("end_at");
        let latest_end_extension_at = end_at
            .checked_add_signed(status_max_delay_to_extend.clone())
            .unwrap();

        if let Some(validator_snapshot) = snapshot.validators.get(identity) {
            let status_from_snapshot = status_from_delinquency(validator_snapshot.delinquent);
            if latest_end_extension_at > snapshot_created_at {
                if status == status_from_snapshot && epoch == snapshot_epoch {
                    validators_with_extended_status.insert(identity.to_string());
                    records_extensions.insert(id, default_status_end_at.clone());
                } else {
                    records_extensions.insert(id, snapshot_created_at.clone());
                }
            }
        }

        debug!(
            "found uptime record: {} {} {} {} {}",
            id, identity, status, start_at, end_at
        );
    }

    let mut query = UpdateQueryCombiner::new(
        "uptimes".to_string(),
        "end_at = u.end_at".to_string(),
        "u(id, end_at)".to_string(),
        "uptimes.id = u.id".to_string(),
    );

    for (id, status_end_at) in records_extensions.iter() {
        let mut params: Vec<&(dyn ToSql + Sync)> = vec![id, status_end_at];
        query.add(
            &mut params,
            HashMap::from_iter([(0, "BIGINT".into()), (1, "TIMESTAMP WITH TIME ZONE".into())]),
        );
    }
    query.execute(&mut psql_client).await?;
    info!("Extended previous {} uptimes", records_extensions.len());

    let mut query = InsertQueryCombiner::new(
        "uptimes".to_string(),
        "identity, status, epoch, start_at, end_at".to_string(),
    );

    for (identity, snapshot) in snapshot.validators.iter() {
        if !validators_with_extended_status.contains(identity) {
            if snapshot.delinquent {
                let mut params: Vec<&(dyn ToSql + Sync)> = vec![
                    identity,
                    &DOWN,
                    &snapshot_epoch,
                    &snapshot_created_at,
                    &default_status_end_at,
                ];
                query.add(&mut params);
                warn!("Validator {} is now DOWN", identity);
            } else {
                let mut params: Vec<&(dyn ToSql + Sync)> = vec![
                    identity,
                    &UP,
                    &snapshot_epoch,
                    &snapshot_created_at,
                    &default_status_end_at,
                ];
                query.add(&mut params);
                info!("Validator {} is now UP", identity);
            }
        }
    }
    let insertions = query.execute(&mut psql_client).await?;
    info!("Stored {} changed uptimes", insertions.unwrap_or(0));

    Ok(())
}
