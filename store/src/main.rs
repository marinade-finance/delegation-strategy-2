use commissions::*;
use env_logger::Env;
use log::{debug, error, info};
use postgres::{Client, NoTls};
use structopt::StructOpt;
use uptime::*;
use versions::*;

#[derive(Debug, StructOpt)]
pub struct CommonParams {
    #[structopt(long = "postgres-url")]
    postgres_url: String,

    #[structopt(long = "snapshot-file")]
    snapshot_path: String,
}

#[derive(Debug, StructOpt)]
struct Params {
    #[structopt(flatten)]
    common: CommonParams,

    #[structopt(subcommand)]
    command: StoreCommand,
}

#[derive(Debug, StructOpt)]
enum StoreCommand {
    Uptime(StoreUptimeOptions),
    Commissions(StoreCommissionsOptions),
    Versions(StoreVersionsOptions),
}

pub mod commissions;
pub mod uptime;
pub mod utils;
pub mod versions;

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(Env::default().default_filter_or("info")).init();

    let mut params = Params::from_args();

    let mut psql_client = Client::connect(&params.common.postgres_url, NoTls)?;

    Ok(match params.command {
        StoreCommand::Uptime(options) => store_uptime(params.common, psql_client),
        StoreCommand::Commissions(options) => store_commissions(params.common, psql_client),
        StoreCommand::Versions(options) => store_versions(params.common, psql_client),
    }?)
}
