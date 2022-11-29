use close_epoch::{close_epoch, CloseEpochOptions};
use cluster_info::{store_cluster_info, StoreClusterInfoOptions};
use commissions::{store_commissions, StoreCommissionsOptions};
use env_logger::Env;
use ls_open_epochs::{list_open_epochs, LsOpenEpochsOptions};
use structopt::StructOpt;
use tokio_postgres::NoTls;
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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(Env::default().default_filter_or("info")).init();

    let params = Params::from_args();
    let (mut psql_client, psql_conn) =
        tokio_postgres::connect(&params.common.postgres_url, NoTls).await?;
    tokio::spawn(async move {
        if let Err(err) = psql_conn.await {
            log::error!("Connection error: {}", err);
            std::process::exit(1);
        }
    });

    Ok(match params.command {
        StoreCommand::Uptime(options) => store_uptime(options, &mut psql_client).await,
        StoreCommand::Commissions(options) => store_commissions(options, &mut psql_client).await,
        StoreCommand::Versions(options) => store_versions(options, &mut psql_client).await,
        StoreCommand::ClusterInfo(options) => store_cluster_info(options, &mut psql_client).await,
        StoreCommand::Validators(options) => store_validators(options, &mut psql_client).await,
        StoreCommand::CloseEpoch(options) => close_epoch(options, &mut psql_client).await,
        StoreCommand::LsOpenEpochs(_options) => list_open_epochs(&psql_client).await,
    }?)
}
