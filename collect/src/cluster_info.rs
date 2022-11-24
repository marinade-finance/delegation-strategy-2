use crate::common::*;
use crate::solana_service::solana_client;
use log::info;
use serde::{Deserialize, Serialize};
use structopt::StructOpt;

#[derive(Debug, StructOpt)]
pub struct ClusterInfoOptions {}

#[derive(Debug, Serialize, Deserialize)]
pub struct ClusterInfo {
    pub created_at: String,
    pub epoch: u64,
    pub epoch_slot: u64,
    pub transaction_count: u64,
}

pub fn collect_cluster_info(common_params: CommonParams) -> anyhow::Result<()> {
    let client = solana_client(common_params.rpc_url, common_params.commitment);

    let created_at = chrono::Utc::now();
    let epoch_info = client.get_epoch_info()?;

    let cluster_info = ClusterInfo {
        created_at: created_at.to_string(),
        epoch: epoch_info.epoch,
        epoch_slot: epoch_info.slot_index,
        transaction_count: epoch_info.transaction_count.unwrap(),
    };

    info!("Cluster info fetched: {:?}", cluster_info);

    serde_yaml::to_writer(std::io::stdout(), &cluster_info)?;

    Ok(())
}
