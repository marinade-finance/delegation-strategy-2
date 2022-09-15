use crate::common::*;
use crate::solana_service::solana_client;
use log::{debug, info};
use serde::Serialize;
use serde_yaml::{self};
use structopt::StructOpt;

#[derive(Debug, StructOpt)]
pub struct ClusterInfoOptions {}

#[derive(Debug, Serialize)]
pub struct ClusterInfo {
    epoch: u64,
    epoch_slot: u64,
    epoch_elapsed_pct: f64,
    transaction_count: u64,
}

pub fn collect_cluster_info(common_params: CommonParams) -> anyhow::Result<()> {
    let client = solana_client(common_params.rpc_url, common_params.commitment);

    let epoch_info = client.get_epoch_info()?;

    let cluster_info = ClusterInfo {
        epoch: epoch_info.epoch,
        epoch_slot: epoch_info.slot_index,
        epoch_elapsed_pct: epoch_info.slot_index as f64 / epoch_info.slots_in_epoch as f64,
        transaction_count: epoch_info.transaction_count.unwrap(),
    };

    serde_yaml::to_writer(std::io::stdout(), &cluster_info)?;

    Ok(())
}
