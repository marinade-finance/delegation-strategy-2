use log::info;
use rust_decimal::prelude::*;
use solana_client::rpc_client::RpcClient;
use structopt::StructOpt;
use tokio_postgres::Client;

const MILLISECONDS_PER_SLOT: u64 = 400;

#[derive(Debug, StructOpt)]
pub struct ValidatorsMevOptions {
    #[structopt(
        long = "execution-interval",
        help = "What should be number of slots between executions",
        default_value = "120000" // 13 hours
    )]
    execution_interval_slots: Decimal,
}

pub async fn check_mev(
    options: ValidatorsMevOptions,
    psql_client: &Client,
    rpc_client: &RpcClient,
) -> anyhow::Result<()> {
    info!("Checking `mev` data table about epoch in DB");

    let rows = psql_client
        .query(
            "SELECT epoch, MAX(epoch_slot) as epoch_slot
                    FROM mev
                    WHERE epoch = (SELECT MAX(epoch) FROM mev)
                    GROUP BY epoch;",
            &[],
        )
        .await?;

    // rust error code is 101, Err code is 1
    assert!(rows.len() <= 1);

    match rows.iter().next() {
        Some(row) => {
            // PostgreSQL type 'INTEGER'
            // the value saved within the `epoch` is the epoch of the MEV data record was created
            // it is the epoch prior to the epoch when the data collection was executed
            let sql_epoch: i32 = row.get("epoch");
            let sql_epoch: Decimal = Decimal::from(sql_epoch);
            // PostgreSQL type 'NUMERIC'
            // the value saved within the `epoch_slot` is the slot index when the data collection was executed (see collect/store validator_mev)
            let sql_slot_index: Decimal = row.get("epoch_slot");

            let epoch_data = rpc_client.get_epoch_info()?;
            let current_epoch = Decimal::from(epoch_data.epoch);
            let current_slot_index = Decimal::from(epoch_data.slot_index);

            info!(
                "DB stores last MEV epoch: {sql_epoch}. Epoch {} slot index: {}, on-chain epoch {} slot index: {}",
                sql_epoch + Decimal::one(), sql_slot_index, current_epoch, current_slot_index
            );

            // The lastly stored MEV epoch saved in DB is delayed by 1 epoch compared to the current epoch.
            if current_epoch - Decimal::one() > sql_epoch {
                info!(
                    "The previous epoch ({}) has surpassed the last recorded MEV epoch ({}). Initiating data collection for MEV analysis.",
                    current_epoch - Decimal::one(),
                    sql_epoch
                );
                return Ok(());
            }

            // If the stored slot index in SQL elapses the expected interval timing, we will proceed with the MEV data collection.
            let slots_diff = current_slot_index.saturating_sub(sql_slot_index);
            if slots_diff >= options.execution_interval_slots {
                info!(
                    "With the current slot index {} of epoch {}, the time elapsed since the execution interval is {} slots, compared to the saved slot index {}",
                    current_slot_index,
                    current_epoch,
                    options.execution_interval_slots,
                    sql_slot_index
                );
                return Ok(());
            }

            if sql_slot_index + options.execution_interval_slots
                < Decimal::from(epoch_data.slots_in_epoch)
            {
                info!(
                    "To execute required to wait at epoch {} for slot index {}, approximately {} seconds",
                    current_epoch,
                    sql_slot_index + options.execution_interval_slots,
                    (sql_slot_index + options.execution_interval_slots - current_slot_index) * Decimal::from(MILLISECONDS_PER_SLOT) / Decimal::from(1000)
                );
            }

            Err(anyhow::anyhow!(
                "MEV data collection for the epoch prior to {} and current slot index {} has already been processed",
                current_epoch,
                current_slot_index
            ))
        }
        None => {
            info!("No MEV data found in DB. Proceed with MEV data collection.");
            Ok(())
        }
    }
}
