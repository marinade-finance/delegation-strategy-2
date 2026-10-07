use crate::stored::last_epoch;
use clap::Parser;
use collect::common::measure_milliseconds_per_slot;
use log::{debug, info};
use solana_rpc_client::rpc_client::RpcClient;
use store::directory::Directory;
use store::docs::BLOCK_REWARDS_DIR;
use validator::Validate;

#[derive(Debug, Parser, Validate)]
pub struct BlockRewardsCheckParams {
    #[arg(
        long = "slot-offset-wait",
        help = "How many slots to wait after epoch has just started before collecting block rewards. Max slots per epoch is 432000.",
        default_value = "10000"
    )]
    #[validate(range(min = 0, max = 432000))]
    slot_offset_wait: u64,
}

/// Verification if block rewards data collection is possible.
/// Returns `Ok(true)` to proceed with collection, `Ok(false)` to skip
/// (data already stored or too early in the epoch to collect).
pub async fn check_block_rewards(
    params: BlockRewardsCheckParams,
    directory: &Directory,
    rpc_client: &RpcClient,
) -> anyhow::Result<bool> {
    let dir = BLOCK_REWARDS_DIR;
    info!("Checking epoch data about epoch in {dir}");

    match last_epoch(directory, dir).await? {
        Some(sql_epoch) => {
            let current_epoch_data = rpc_client.get_epoch_info()?;
            let current_epoch = current_epoch_data.epoch;
            let current_slot_index = current_epoch_data.slot_index;

            info!(
                "{dir} stores last epoch: {sql_epoch}. On-chain epoch {current_epoch} slot index: {current_slot_index}",
            );

            if current_epoch - 1 > sql_epoch {
                info!(
                    "The previous epoch ({}) has surpassed the last recorded {dir} epoch ({sql_epoch}). Initiating data collection for {dir} analysis.",
                    current_epoch - 1
                );

                return if current_slot_index > params.slot_offset_wait {
                    // this is a preliminary check as the real collection may happen only when Google stakes-etl job loaded data to BQ
                    Ok(true)
                } else {
                    let slots_to_wait = params.slot_offset_wait - current_slot_index;
                    // An unavailable ETA must not turn "not yet" into a failed check.
                    match measure_milliseconds_per_slot(rpc_client, &current_epoch_data)
                        .unwrap_or_else(|err| {
                            debug!("Cannot measure the slot time for the ETA: {err}");
                            None
                        })
                    {
                        Some(ms_per_slot) => info!(
                            "To execute required to wait at epoch {current_epoch} for {slots_to_wait} slots, approximately {} seconds",
                            slots_to_wait * ms_per_slot / 1000u64
                        ),
                        None => info!(
                            "To execute required to wait at epoch {current_epoch} for {slots_to_wait} slots"
                        ),
                    }
                    Ok(false)
                };
            }

            info!(
                "{dir} data collection for the epoch prior {} has already been processed",
                current_epoch - 1
            );
            Ok(false)
        }
        None => {
            info!("No {dir} data found in the store. Proceed with data collection.");
            Ok(true)
        }
    }
}
