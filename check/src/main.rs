use crate::validators_jito::{check_jito, ValidatorsJitoCheckOptions};
use collect::solana_service::solana_client;
use env_logger::Env;
use structopt::StructOpt;
use tokio_postgres::NoTls;

#[derive(Debug, StructOpt)]
pub struct CommonParams {
    #[structopt(long = "postgres-url")]
    postgres_url: String,

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
    JitoMev(ValidatorsJitoCheckOptions),
    JitoPriority(ValidatorsJitoCheckOptions),
}

pub mod validators_jito;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(Env::default().default_filter_or("info")).init();

    let params = Params::from_args();
    let (psql_client, psql_conn) =
        tokio_postgres::connect(&params.common.postgres_url, NoTls).await?;
    tokio::spawn(async move {
        if let Err(err) = psql_conn.await {
            log::error!("Connection error: {}", err);
            std::process::exit(1);
        }
    });

    let rpc_client = solana_client(params.common.rpc_url, params.common.commitment);

    match params.command {
        StoreCommand::JitoMev(options) => {
            check_jito(
                options,
                &psql_client,
                &rpc_client,
                collect::validators_jito::JitoAccountType::MevTipDistribution.db_table_name(),
            )
            .await
        }
        StoreCommand::JitoPriority(options) => {
            check_jito(
                options,
                &psql_client,
                &rpc_client,
                collect::validators_jito::JitoAccountType::PriorityFeeDistribution.db_table_name(),
            )
            .await
        }
    }
}
