use crate::stored::{last_epoch, last_epoch_slot};
use clap::Parser;
use collect::common::measure_milliseconds_per_slot;
use log::{debug, info};
use rust_decimal::prelude::*;
use solana_rpc_client::rpc_client::RpcClient;
use store::directory::Directory;

#[derive(Debug, Parser)]
pub struct ValidatorsJitoCheckParams {
    #[arg(
        long = "execution-interval",
        help = "What should be number of slots between executions",
        default_value = "120000" // 13 hours
    )]
    execution_interval_slots: Decimal,
}

/// Verification if we should proceed with saving more JITO accounts data to the
/// store. Currently, we index two document kinds: mev and priority-fee.
pub async fn check_jito(
    params: ValidatorsJitoCheckParams,
    directory: &Directory,
    rpc_client: &RpcClient,
    dir: &str,
) -> anyhow::Result<bool> {
    info!("Checking epoch data about epoch in {dir}");

    let stored = match last_epoch(directory, dir).await? {
        Some(epoch) => last_epoch_slot(directory, dir, epoch)
            .await?
            .map(|epoch_slot| (epoch, epoch_slot)),
        None => None,
    };

    match stored {
        Some((stored_epoch, stored_slot_index)) => {
            // the epoch of the data the record was created for: the epoch prior
            // to the epoch when the data collection was executed
            let sql_epoch = Decimal::from(stored_epoch);
            let sql_slot_index = Decimal::from(stored_slot_index);

            let epoch_data = rpc_client.get_epoch_info()?;
            let current_epoch = Decimal::from(epoch_data.epoch);
            let current_slot_index = Decimal::from(epoch_data.slot_index);

            info!(
                "{dir} stores last epoch: {sql_epoch}. Epoch {} slot index: {sql_slot_index}, on-chain epoch {current_epoch} slot index: {current_slot_index}",
                sql_epoch + Decimal::one()
            );

            if current_epoch - Decimal::one() > sql_epoch {
                info!(
                    "The previous epoch ({}) has surpassed the last recorded {dir} epoch ({sql_epoch}). Initiating data collection for {dir} analysis.",
                    current_epoch - Decimal::one()
                );
                return Ok(true);
            }

            let slots_diff = current_slot_index.saturating_sub(sql_slot_index);
            if slots_diff >= params.execution_interval_slots {
                info!(
                    "With the current slot index {current_slot_index} of epoch {current_epoch}, the time elapsed since the execution interval is {} slots, compared to the saved slot index {sql_slot_index}",
                    params.execution_interval_slots,
                );
                return Ok(true);
            }

            if sql_slot_index + params.execution_interval_slots
                < Decimal::from(epoch_data.slots_in_epoch)
            {
                let target_slot_index = sql_slot_index + params.execution_interval_slots;
                let slots_to_wait = target_slot_index - current_slot_index;
                // An unavailable ETA must not turn "not yet" into a failed check.
                match measure_milliseconds_per_slot(rpc_client, &epoch_data).unwrap_or_else(|err| {
                    debug!("Cannot measure the slot time for the ETA: {err}");
                    None
                }) {
                    Some(ms_per_slot) => info!(
                        "To execute required to wait at epoch {current_epoch} for slot index {target_slot_index}, approximately {} seconds",
                        slots_to_wait * Decimal::from(ms_per_slot) / Decimal::from(1000)
                    ),
                    None => info!(
                        "To execute required to wait at epoch {current_epoch} for slot index {target_slot_index} ({slots_to_wait} slots)"
                    ),
                }
            }

            info!(
                "{dir} data collection for the epoch prior to {current_epoch} and current slot index {current_slot_index} has already been processed"
            );
            Ok(false)
        }
        None => {
            info!("No {dir} data found in the store. Proceed with data collection.");
            Ok(true)
        }
    }
}
