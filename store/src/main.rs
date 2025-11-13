use close_epoch::{close_epoch, CloseEpochParams};
use cluster_info::{store_cluster_info, StoreClusterInfoParams};
use collect::validators_jito::JitoAccountType;
use commissions::{store_commissions, StoreCommissionsParams};
use env_logger::Env;
use ls_open_epochs::{list_open_epochs, LsOpenEpochsParams};
use openssl::ssl::{SslConnector, SslMethod};
use postgres_openssl::MakeTlsConnector;
use store::validators_block_rewards::{store_block_rewards, StoreBlockRewardsParams};
use structopt::StructOpt;
use uptime::{store_uptime, StoreUptimeParams};
use validators::{store_validators, StoreValidatorsParams};
use validators_jito::{store_jito, StoreJitoParams};
use versions::{store_versions, StoreVersionsParams};

#[derive(Debug, StructOpt)]
pub struct CommonParams {
    #[structopt(long = "postgres-url")]
    postgres_url: String,

    #[structopt(long = "postgres-ssl-root-cert", env = "PG_SSLROOTCERT")]
    pub postgres_ssl_root_cert: String,
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
    Uptime(StoreUptimeParams),
    Commissions(StoreCommissionsParams),
    Versions(StoreVersionsParams),
    ClusterInfo(StoreClusterInfoParams),
    Validators(StoreValidatorsParams),
    ValidatorsBlockRewards(StoreBlockRewardsParams),
    JitoMev(StoreJitoParams),
    JitoPriority(StoreJitoParams),
    CloseEpoch(CloseEpochParams),
    LsOpenEpochs(LsOpenEpochsParams),
}

pub mod close_epoch;
pub mod cluster_info;
pub mod commissions;
pub mod dto;
pub mod ls_open_epochs;
pub mod uptime;
pub mod utils;
pub mod validators;
pub mod validators_jito;
pub mod versions;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(Env::default().default_filter_or("info")).init();

    let params = Params::from_args();

    let mut builder = SslConnector::builder(SslMethod::tls())?;
    builder.set_ca_file(&params.common.postgres_ssl_root_cert)?;
    let connector = MakeTlsConnector::new(builder.build());

    let (mut psql_client, psql_conn) =
        tokio_postgres::connect(&params.common.postgres_url, connector).await?;
    tokio::spawn(async move {
        if let Err(err) = psql_conn.await {
            log::error!("Connection error: {err}");
            std::process::exit(1);
        }
    });

    match params.command {
        StoreCommand::Uptime(store_params) => store_uptime(store_params, &mut psql_client).await,
        StoreCommand::Commissions(store_params) => {
            store_commissions(store_params, &mut psql_client).await
        }
        StoreCommand::Versions(store_params) => {
            store_versions(store_params, &mut psql_client).await
        }
        StoreCommand::ClusterInfo(store_params) => {
            store_cluster_info(store_params, &mut psql_client).await
        }
        StoreCommand::Validators(store_params) => {
            store_validators(store_params, &mut psql_client).await
        }
        StoreCommand::JitoMev(store_params) => {
            store_jito(
                store_params,
                &mut psql_client,
                JitoAccountType::MevTipDistribution,
            )
            .await
        }
        StoreCommand::JitoPriority(store_params) => {
            store_jito(
                store_params,
                &mut psql_client,
                JitoAccountType::PriorityFeeDistribution,
            )
            .await
        }
        StoreCommand::ValidatorsBlockRewards(store_params) => {
            store_block_rewards(store_params, &mut psql_client).await
        }
        StoreCommand::CloseEpoch(close_params) => close_epoch(close_params, &mut psql_client).await,
        StoreCommand::LsOpenEpochs(_ls_params) => list_open_epochs(&psql_client).await,
    }
}
