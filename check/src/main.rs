use crate::validators_jito::{check_jito, ValidatorsJitoCheckParams};
use check::validators_block_rewards::{check_block_rewards, BlockRewardsCheckParams};
use collect::solana_service::solana_client;
use env_logger::Env;
use log::info;
use openssl::ssl::{SslConnector, SslMethod};
use postgres_openssl::MakeTlsConnector;
use structopt::StructOpt;

#[derive(Debug, StructOpt)]
pub struct CommonParams {
    #[structopt(long = "postgres-url")]
    postgres_url: String,

    #[structopt(long = "postgres-ssl-root-cert", env = "PG_SSLROOTCERT")]
    pub postgres_ssl_root_cert: String,

    #[structopt(short = "u", long = "rpc-url", env = "RPC_URL")]
    pub rpc_url: String,

    #[structopt(short = "c", long = "commitment", default_value = "finalized")]
    pub commitment: String,
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
    JitoMev(ValidatorsJitoCheckParams),
    JitoPriority(ValidatorsJitoCheckParams),
    BlockRewards(BlockRewardsCheckParams),
}

pub mod validators_jito;

#[tokio::main]
async fn main() {
    env_logger::Builder::from_env(Env::default().default_filter_or("info")).init();

    match run().await {
        Ok(true) => {}
        Ok(false) => {
            info!("Not a good time to collect, skipping");
            std::process::exit(1);
        }
        Err(err) => {
            log::error!("Check failed: {err:?}");
            std::process::exit(2);
        }
    }
}

async fn run() -> anyhow::Result<bool> {
    let params = Params::from_args();
    info!("params {params:?}");
    let mut builder = SslConnector::builder(SslMethod::tls())?;
    builder.set_ca_file(&params.common.postgres_ssl_root_cert)?;
    let connector = MakeTlsConnector::new(builder.build());

    let (psql_client, psql_conn) =
        tokio_postgres::connect(&params.common.postgres_url, connector).await?;
    tokio::spawn(async move {
        if let Err(err) = psql_conn.await {
            log::error!("Connection error: {err}");
            std::process::exit(2);
        }
    });

    let rpc_client = solana_client(params.common.rpc_url, params.common.commitment);

    match params.command {
        StoreCommand::JitoMev(mev_params) => {
            check_jito(
                mev_params,
                &psql_client,
                &rpc_client,
                collect::validators_jito::JitoAccountType::MevTipDistribution.db_table_name(),
            )
            .await
        }
        StoreCommand::JitoPriority(jito_params) => {
            check_jito(
                jito_params,
                &psql_client,
                &rpc_client,
                collect::validators_jito::JitoAccountType::PriorityFeeDistribution.db_table_name(),
            )
            .await
        }
        StoreCommand::BlockRewards(rewards_params) => {
            check_block_rewards(
                rewards_params,
                &psql_client,
                &rpc_client,
                store::validators_block_rewards::VALIDATORS_BLOCK_REWARDS_TABLE,
            )
            .await
        }
    }
}
