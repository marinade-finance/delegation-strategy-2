// use close_epoch::*;
use cluster_info::{store_cluster_info, StoreClusterInfoOptions};
use commissions::{store_commissions, StoreCommissionsOptions};
use env_logger::Env;
use postgres::{Client, NoTls};
use structopt::StructOpt;
use uptime::{store_uptime, StoreUptimeOptions};
use versions::{store_versions, StoreVersionsOptions};

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
    ClusterInfo(StoreClusterInfoOptions),
    // CloseEpoch(CloseEpochOptions),
}

// pub mod close_epoch;
pub mod cluster_info;
pub mod commissions;
pub mod dto;
pub mod uptime;
pub mod utils;
pub mod versions;

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(Env::default().default_filter_or("info")).init();

    let params = Params::from_args();

    let psql_client = Client::connect(&params.common.postgres_url, NoTls)?;

    Ok(match params.command {
        StoreCommand::Uptime(_options) => store_uptime(params.common, psql_client),
        StoreCommand::Commissions(_options) => store_commissions(params.common, psql_client),
        StoreCommand::Versions(_options) => store_versions(params.common, psql_client),
        StoreCommand::ClusterInfo(_options) => store_cluster_info(params.common, psql_client),
        // StoreCommand::CloseEpoch(_options) => close_epoch(params.common, psql_client),
    }?)
}
