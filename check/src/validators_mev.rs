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

    assert!(rows.len() <= 1);

    match rows.iter().next() {
        Some(row) => {
            let sql_epoch: Decimal = row.get("epoch");
            let sql_slot: Decimal = row.get("epoch_slot");

            let epoch_data = rpc_client.get_epoch_info()?;
            let current_epoch = Decimal::from(epoch_data.epoch);
            let current_slot = Decimal::from(epoch_data.absolute_slot);

            info!(
                "DB stores last MEV epoch: {sql_epoch}, slot: {sql_slot}, on-chain epoch: {current_epoch}, slot: {current_slot}"
            );

            // If the current epoch is bigger than the lastly stored MEV epoch, we need to proceed
            if current_epoch > sql_epoch {
                info!(
                    "Current epoch {current_epoch} is bigger than the last observed MEV epoch {sql_epoch} in DB. Proceed with MEV data collection."
                );
                return Ok(());
            }

            Err(anyhow::anyhow!("The current epoch {current_epoch} has already been processed for MEV data collection."))
        }
        None => {
            info!("No MEV data found in DB. Proceed with MEV data collection.");
            Ok(())
        }
    }
}
