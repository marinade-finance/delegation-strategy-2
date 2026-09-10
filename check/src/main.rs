use check::validators_block_rewards::{check_block_rewards, BlockRewardsCheckParams};
use check::validators_jito::{check_jito, ValidatorsJitoCheckParams};
use collect::solana_service::solana_client;
use env_logger::Env;
use log::info;
use store::directory::Directory;
use store::docs::{MEV_DIR, PRIORITY_FEE_DIR};
use structopt::StructOpt;

#[derive(Debug, StructOpt)]
pub struct CommonParams {
    #[structopt(long = "directory-url", env = "DIRECTORY_URL")]
    pub directory_url: String,

    #[structopt(long = "directory-token", env = "DIRECTORY_TOKEN")]
    pub directory_token: String,

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
    info!(
        "Running check command {:?} with commitment {}",
        params.command, params.common.commitment
    );
    let directory = Directory::new(params.common.directory_url, params.common.directory_token)?;

    let rpc_client = solana_client(params.common.rpc_url, params.common.commitment);

    match params.command {
        StoreCommand::JitoMev(mev_params) => {
            check_jito(mev_params, &directory, &rpc_client, MEV_DIR).await
        }
        StoreCommand::JitoPriority(jito_params) => {
            check_jito(jito_params, &directory, &rpc_client, PRIORITY_FEE_DIR).await
        }
        StoreCommand::BlockRewards(rewards_params) => {
            check_block_rewards(rewards_params, &directory, &rpc_client).await
        }
    }
}
