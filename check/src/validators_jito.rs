use log::info;
use rust_decimal::prelude::*;
use solana_client::rpc_client::RpcClient;
use structopt::StructOpt;
use tokio_postgres::Client;

const MILLISECONDS_PER_SLOT: u64 = 400;

#[derive(Debug, StructOpt)]
pub struct ValidatorsJitoCheckOptions {
    #[structopt(
        long = "execution-interval",
        help = "What should be number of slots between executions",
        default_value = "120000" // 13 hours
    )]
    execution_interval_slots: Decimal,
}

/// Verification if we should proceed with saving more JITO accounts data to the database.
/// Currently, we index two tables: `mev` and `jito_priority_fee`.
pub async fn check_jito(
    options: ValidatorsJitoCheckOptions,
    psql_client: &Client,
    rpc_client: &RpcClient,
    db_table: &str,
) -> anyhow::Result<()> {
    info!("Checking epoch data about epoch in DB table {db_table}");

    let rows = psql_client
        .query(
            format!(
                "SELECT epoch, MAX(epoch_slot) as epoch_slot
                    FROM {db_table}
                    WHERE epoch = (SELECT MAX(epoch) FROM {db_table})
                    GROUP BY epoch;"
            )
            .as_str(),
            &[],
        )
        .await?;

    // rust error code is 101, Err code is 1
    assert!(rows.len() <= 1);

    match rows.iter().next() {
        Some(row) => {
            // PostgreSQL type 'INTEGER'
            // the value saved within the `epoch` is the epoch of data record was created
            // it is the epoch prior to the epoch when the data collection was executed
            let sql_epoch: i32 = row.get("epoch");
            let sql_epoch: Decimal = Decimal::from(sql_epoch);
            // PostgreSQL type 'NUMERIC'
            // the value saved within the `epoch_slot` is the slot index when the data collection was executed (see collect/store)
            let sql_slot_index: Decimal = row.get("epoch_slot");

            let epoch_data = rpc_client.get_epoch_info()?;
            let current_epoch = Decimal::from(epoch_data.epoch);
            let current_slot_index = Decimal::from(epoch_data.slot_index);

            info!(
                "DB {db_table} stores last epoch: {sql_epoch}. Epoch {} slot index: {sql_slot_index}, on-chain epoch {current_epoch} slot index: {current_slot_index}",
                sql_epoch + Decimal::one()
            );

            // The lastly stored epoch saved in DB is delayed by 1 epoch compared to the current epoch.
            if current_epoch - Decimal::one() > sql_epoch {
                info!(
                    "The previous epoch ({}) has surpassed the last recorded table {db_table} epoch ({sql_epoch}). Initiating data collection for {db_table} analysis.",
                    current_epoch - Decimal::one()
                );
                return Ok(());
            }

            // If the stored slot index in SQL elapses the expected interval timing, we will proceed with the data collection.
            let slots_diff = current_slot_index.saturating_sub(sql_slot_index);
            if slots_diff >= options.execution_interval_slots {
                info!(
                    "With the current slot index {current_slot_index} of epoch {current_epoch}, the time elapsed since the execution interval is {} slots, compared to the saved slot index {sql_slot_index}",
                    options.execution_interval_slots,
                );
                return Ok(());
            }

            if sql_slot_index + options.execution_interval_slots
                < Decimal::from(epoch_data.slots_in_epoch)
            {
                info!(
                    "To execute required to wait at epoch {current_epoch} for slot index {}, approximately {} seconds",
                    sql_slot_index + options.execution_interval_slots,
                    (sql_slot_index + options.execution_interval_slots - current_slot_index) * Decimal::from(MILLISECONDS_PER_SLOT) / Decimal::from(1000)
                );
            }

            Err(anyhow::anyhow!(
                "{db_table} data collection for the epoch prior to {current_epoch} and current slot index {current_slot_index} has already been processed",
            ))
        }
        None => {
            info!("No {db_table} data found in DB. Proceed with data collection.");
            Ok(())
        }
    }
}
