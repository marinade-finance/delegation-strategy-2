use log::info;
use postgres::Client;
use rust_decimal::prelude::*;
use structopt::StructOpt;

#[derive(Debug, StructOpt)]
pub struct LsOpenEpochsOptions {}

pub fn list_open_epochs(mut psql_client: Client) -> anyhow::Result<()> {
    info!("Finding open epochs...");

    let rows = psql_client.query(
        "
        SELECT DISTINCT epoch
        FROM validators
        WHERE epoch NOT IN (SELECT DISTINCT epoch FROM epochs)
    ",
        &[],
    )?;

    for row in rows.iter() {
        let epoch: Decimal = row.get("epoch");
        println!("{}", epoch);
        info!("Open epoch: {}", epoch);
    }

    info!("Found open epochs: {}", rows.len());

    Ok(())
}
