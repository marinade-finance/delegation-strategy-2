use chrono::{DateTime, Utc};
use collect::validators_performance::ValidatorsPerformanceSnapshot;
use log::info;
use rust_decimal::prelude::*;
use serde_yaml;
use structopt::StructOpt;
use tokio_postgres::Client;

#[derive(Debug, StructOpt)]
pub struct StoreClusterInfoOptions {
    #[structopt(long = "snapshot-file")]
    snapshot_path: String,
}

pub async fn store_cluster_info(
    options: StoreClusterInfoOptions,
    psql_client: &mut Client,
) -> anyhow::Result<()> {
    info!("Storing cluster info...");

    let snapshot_file = std::fs::File::open(options.snapshot_path)?;
    let snapshot: ValidatorsPerformanceSnapshot = serde_yaml::from_reader(snapshot_file)?;

    info!("Loaded the cluster info");

    psql_client
        .execute(
            // todo add supply, inflation and active stake
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
        )
        .await?;

    info!("Stored cluster info");

    Ok(())
}
