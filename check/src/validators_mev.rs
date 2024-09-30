use log::info;
use rust_decimal::prelude::*;
use solana_client::rpc_client::RpcClient;
use tokio_postgres::Client;

pub async fn check_mev(psql_client: &Client, rpc_client: &RpcClient) -> anyhow::Result<()> {
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
            let sql_epoch: i32 = row.get("epoch");
            let sql_epoch: Decimal = Decimal::from(sql_epoch);
            // PostgreSQL type 'NUMERIC'
            let sql_slot: Decimal = row.get("epoch_slot");

            let epoch_data = rpc_client.get_epoch_info()?;
            let current_epoch = Decimal::from(epoch_data.epoch);
            let current_slot = Decimal::from(epoch_data.absolute_slot);

            info!(
                "DB stores last MEV epoch: {sql_epoch}, slot: {sql_slot}, on-chain epoch: {current_epoch}, slot: {current_slot}"
            );

            // The lastly stored MEV epoch saved in DB is delayed by 1 epoch compared to the current epoch,
            if current_epoch - Decimal::from(1) > sql_epoch {
                info!(
                    "The previous epoch ({}) has surpassed the last recorded MEV epoch ({}). Initiating data collection for MEV analysis.",
                    current_epoch - Decimal::from(1),
                    sql_epoch
                );
                return Ok(());
            }

            Err(anyhow::anyhow!("MEV data collection for the epoch prior to {current_epoch} has already been processed."))
        }
        None => {
            info!("No MEV data found in DB. Proceed with MEV data collection.");
            Ok(())
        }
    }
}
