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
        help = "What should be the approximate time between two executions (seconds).",
        default_value = "3600"
    )]
    execution_interval: u64,
}

pub async fn check_mev(
    options: ValidatorsMevOptions,
    psql_client: &Client,
    rpc_client: &RpcClient,
) -> anyhow::Result<()> {
    info!("Checking MEV data table about epoch in DB");

    let rows = psql_client
        .query(
            "SELECT epoch, MAX(epoch_slot) as epoch_slot
                    FROM mev
                    WHERE epoch = (SELECT MAX(epoch) FROM mev)
                    GROUP BY epoch;",
            &[],
        )
        .await?;

    assert!(rows.len() <= 1);

    match rows.iter().next() {
        Some(row) => {
            let sql_epoch: Decimal = row.get("epoch");
            let sql_slot: Decimal = row.get("epoch_slot");
            info!("Last MEV SQL epoch: {}, slot: {}", sql_epoch, sql_slot);

            let epoch_data = rpc_client.get_epoch_info()?;
            let current_epoch = Decimal::from(epoch_data.epoch);
            let current_slot = Decimal::from(epoch_data.absolute_slot);

            // If the current epoch is bigger than the lastly stored MEV epoch, we need to proceed
            if current_epoch > sql_epoch {
                info!(
                    "Current epoch {current_epoch} is bigger than the last MEV epoch {sql_epoch}"
                );
                return Ok(());
            }

            // If the current epoch elapses the expected interval timing to the lastly stored MEV epoch, we need to proceed
            let slots_diff = current_slot.saturating_sub(sql_slot);
            let execution_interval_slots = Decimal::from(options.execution_interval * 1000)
                / Decimal::from(MILLISECONDS_PER_SLOT);
            if slots_diff >= execution_interval_slots {
                info!(
                    "With the current slot {} the time elapses of execution-interval {} in seconds ({} slots) to the last MEV slot {}",
                    current_slot,
                    options.execution_interval,
                    execution_interval_slots,
                    sql_slot
                );
                return Ok(());
            }

            Err(anyhow::anyhow!("The current epoch {current_epoch} and slot {current_slot} are not enough to proceed with MEV data collection"))
        }
        None => {
            info!("No MEV data found in DB");
            Ok(())
        }
    }
}
