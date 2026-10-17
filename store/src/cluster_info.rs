use crate::directory::{Directory, Doc, Precondition};
use crate::docs::{ClusterInfoDoc, ClusterInfoSample, LIVE_CLUSTER_INFO};
use chrono::{DateTime, Utc};
use clap::Parser;
use collect::validators_performance::ValidatorsPerformanceSnapshot;
use log::info;
use serde_yaml;

#[derive(Debug, Parser)]
pub struct StoreClusterInfoParams {
    #[arg(long = "snapshot-file")]
    snapshot_path: String,
}

pub async fn store_cluster_info(
    params: StoreClusterInfoParams,
    directory: &Directory,
) -> anyhow::Result<()> {
    info!("Storing cluster info...");

    let snapshot_file = std::fs::File::open(params.snapshot_path)?;
    let snapshot: ValidatorsPerformanceSnapshot = serde_yaml::from_reader(snapshot_file)?;

    info!("Loaded the cluster info");

    let stored = directory.get::<ClusterInfoDoc>(LIVE_CLUSTER_INFO).await?;
    write_cluster_info(directory, stored, &snapshot).await
}

pub async fn write_cluster_info(
    directory: &Directory,
    stored: Option<Doc<ClusterInfoDoc>>,
    snapshot: &ValidatorsPerformanceSnapshot,
) -> anyhow::Result<()> {
    let created_at: DateTime<Utc> = snapshot.created_at.parse()?;
    let (mut cluster_info, precondition) = match stored {
        Some(stored) => (stored.body, Precondition::IfMatch(stored.etag)),
        None => (
            ClusterInfoDoc {
                epoch: snapshot.epoch,
                samples: Vec::new(),
            },
            Precondition::Create,
        ),
    };
    if snapshot.epoch < cluster_info.epoch {
        anyhow::bail!(
            "Sample of epoch {} is older than the stored epoch {}",
            snapshot.epoch,
            cluster_info.epoch
        );
    }

    cluster_info.epoch = snapshot.epoch;
    cluster_info.samples.push(ClusterInfoSample {
        epoch: snapshot.epoch,
        epoch_slot: snapshot.epoch_slot,
        transaction_count: snapshot.transaction_count,
        created_at,
        slots_per_year: snapshot.slots_per_year,
    });

    // No retry on a conflict: the cron is the retry.
    directory
        .put(LIVE_CLUSTER_INFO, &cluster_info, precondition)
        .await?;

    info!("Stored cluster info");

    Ok(())
}
