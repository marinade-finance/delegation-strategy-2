use log::info;
use rust_decimal::prelude::*;
use structopt::StructOpt;
use tokio_postgres::Client;

#[derive(Debug, StructOpt)]
pub struct LsOpenEpochsParams {}

pub async fn list_open_epochs(psql_client: &Client) -> anyhow::Result<()> {
    info!("Finding open epochs...");

    let rows = psql_client
        .query(
            "
        SELECT DISTINCT epoch
        FROM validators
        WHERE epoch NOT IN (SELECT DISTINCT epoch FROM epochs)
    ",
            &[],
        )
        .await?;

    for row in rows.iter() {
        let epoch: Decimal = row.get("epoch");
        println!("{epoch}");
        info!("Open epoch: {epoch}");
    }

    info!("Found open epochs: {}", rows.len());

    Ok(())
}
