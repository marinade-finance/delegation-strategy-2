use close_epoch::{close_epoch, CloseEpochOptions};
use cluster_info::{store_cluster_info, StoreClusterInfoOptions};
use commissions::{store_commissions, StoreCommissionsOptions};
use env_logger::Env;
use ls_open_epochs::{list_open_epochs, LsOpenEpochsOptions};
use postgres::{Client, NoTls};
use structopt::StructOpt;
use uptime::{store_uptime, StoreUptimeOptions};
use validators::{store_validators, StoreValidatorsOptions};
use versions::{store_versions, StoreVersionsOptions};

#[derive(Debug, StructOpt)]
pub struct CommonParams {
    #[structopt(long = "postgres-url")]
    postgres_url: String,
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
    Validators(StoreValidatorsOptions),
    CloseEpoch(CloseEpochOptions),
    LsOpenEpochs(LsOpenEpochsOptions),
}

pub mod close_epoch;
pub mod cluster_info;
pub mod commissions;
pub mod dto;
pub mod ls_open_epochs;
pub mod uptime;
pub mod utils;
pub mod validators;
pub mod versions;

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(Env::default().default_filter_or("info")).init();

    let params = Params::from_args();

    let psql_client = Client::connect(&params.common.postgres_url, NoTls)?;

    Ok(match params.command {
        StoreCommand::Uptime(options) => store_uptime(options, psql_client),
        StoreCommand::Commissions(options) => store_commissions(options, psql_client),
        StoreCommand::Versions(options) => store_versions(options, psql_client),
        StoreCommand::ClusterInfo(options) => store_cluster_info(options, psql_client),
        StoreCommand::Validators(options) => store_validators(options, psql_client),
        StoreCommand::CloseEpoch(options) => close_epoch(options, psql_client),
        StoreCommand::LsOpenEpochs(_options) => list_open_epochs(psql_client),
    }?)
}
