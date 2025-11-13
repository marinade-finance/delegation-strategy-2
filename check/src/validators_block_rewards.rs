use crate::validators_jito::MILLISECONDS_PER_SLOT;
use log::info;
use rust_decimal::prelude::*;
use solana_client::rpc_client::RpcClient;
use structopt::StructOpt;
use tokio_postgres::Client;

#[derive(Debug, StructOpt)]
pub struct BlockRewardsCheckParams {
    #[structopt(
        long = "slot-offset-wait",
        help = "How many slots to wait after epoch has just started before collecting block rewards. Max slots per epoch is 432000.",
        default_value = "10000"
    )]
    slot_offset_wait: Decimal,
}

/// Verification if data is to save block rewards to DB
pub async fn check_block_rewards(
    params: BlockRewardsCheckParams,
    psql_client: &Client,
    rpc_client: &RpcClient,
    db_table: &str,
) -> anyhow::Result<()> {
    info!("Checking epoch data about epoch in DB table {db_table}");

    if params.slot_offset_wait > 432_000.into() {
        return Err(anyhow::anyhow!(
            "slot-offset-wait value {} is too high. Max slots per epoch is 432000",
            params.slot_offset_wait
        ));
    }

    // in block rewards, we only care about epoch
    let row_epoch = psql_client
        .query_opt(
            format!(
                "SELECT epoch
                    FROM {db_table}
                    WHERE epoch = (SELECT MAX(epoch) FROM {db_table})
                    GROUP BY epoch;"
            )
            .as_str(),
            &[],
        )
        .await?;

    match row_epoch {
        Some(row) => {
            // PostgreSQL type 'NUMERIC'
            // the value saved within the `epoch` is the epoch of data record was created for
            let sql_epoch: Decimal = row.get("epoch");

            let current_epoch_data = rpc_client.get_epoch_info()?;
            let current_epoch = Decimal::from(current_epoch_data.epoch);
            let current_slot_index = Decimal::from(current_epoch_data.slot_index);

            info!(
                "DB {db_table} stores last epoch: {sql_epoch}. On-chain epoch {current_epoch} slot index: {current_slot_index}",
            );

            // The lastly stored epoch saved in DB is delayed by 1 epoch compared to the current epoch
            if current_epoch - Decimal::one() > sql_epoch {
                info!(
                    "The previous epoch ({}) has surpassed the last recorded table {db_table} epoch ({sql_epoch}). Initiating data collection for {db_table} analysis.",
                    current_epoch - Decimal::one()
                );

                if current_slot_index > params.slot_offset_wait {
                    // the slot offset wait is overpassed, we can proceed
                    // this is a preliminary check as the real collection may happen only when Google stakes-etl job loaded data to BQ
                    return Ok(());
                } else {
                    return Err(anyhow::anyhow!(
                        "To execute required to wait at epoch {current_epoch} for slot index {}, approximately {} seconds",
                        params.slot_offset_wait - current_slot_index,
                        (params.slot_offset_wait - current_slot_index) * Decimal::from(MILLISECONDS_PER_SLOT) / Decimal::from(1000)
                    ));
                }
            }

            Err(anyhow::anyhow!(
                "{db_table} data collection for the epoch prior {} has already been processed",
                current_epoch - Decimal::one()
            ))
        }
        None => {
            info!("No {db_table} data found in DB. Proceed with data collection.");
            Ok(())
        }
    }
}
