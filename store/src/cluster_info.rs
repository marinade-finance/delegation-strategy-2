use crate::CommonParams;
use chrono::{DateTime, Utc};
use collect::cluster_info::ClusterInfo;
use log::info;
use postgres::Client;
use rust_decimal::prelude::*;
use serde::Deserialize;
use structopt::StructOpt;

#[derive(Debug, StructOpt)]
pub struct StoreVersionsOptions {}

pub fn store_versions(common_params: CommonParams, mut psql_client: Client) -> anyhow::Result<()> {
    info!("Storing cluster info...");

    let snapshot_file = std::fs::File::open(common_params.snapshot_path)?;
    let snapshot: ClusterInfo = serde_yaml::from_reader(snapshot_file)?;

    info!("Loaded the cluster info");

    psql_client.execute(
        "
        INSERT INTO cluster_info (epoch, epoch_slot, transaction_count, created_at)
        VALUES ($1, $2, $3, $4)
    ",
        &[
            &(Decimal::from(snapshot.epoch)),
            &(Decimal::from(snapshot.epoch_slot)),
            &(Decimal::from(snapshot.transaction_count)),
            &snapshot.created_at.parse::<DateTime<Utc>>().unwrap(),
        ],
    )?;

    info!("Stored cluster info");

    Ok(())
}
