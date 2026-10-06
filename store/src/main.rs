use clap::Parser;
use collect::validators_jito::JitoAccountType;
use env_logger::Env;
use store::close_epoch::{close_epoch, CloseEpochParams};
use store::cluster_info::{store_cluster_info, StoreClusterInfoParams};
use store::commissions::{store_commissions, StoreCommissionsParams};
use store::directory::Directory;
use store::ls_open_epochs::{list_open_epochs, LsOpenEpochsParams};
use store::releases::{store_releases, StoreReleasesParams};
use store::take_rates::{store_take_rates, StoreTakeRatesParams};
use store::uptime::{store_uptime, StoreUptimeParams};
use store::validators::{store_validators, StoreValidatorsParams};
use store::validators_block_rewards::{store_block_rewards, StoreBlockRewardsParams};
use store::validators_events::{store_events, StoreEventsParams};
use store::validators_jito::{store_jito, StoreJitoParams};
use store::validators_sandwiches::{store_sandwiches, StoreSandwichesParams};
use store::versions::{store_versions, StoreVersionsParams};

#[derive(Debug, Parser)]
pub struct CommonParams {
    #[arg(long = "directory-url", env = "DIRECTORY_URL")]
    pub directory_url: String,

    #[arg(long = "directory-token", env = "DIRECTORY_TOKEN")]
    pub directory_token: String,
}

#[derive(Debug, Parser)]
struct Params {
    #[command(flatten)]
    common: CommonParams,

    #[command(subcommand)]
    command: StoreCommand,
}

#[derive(Debug, Parser)]
enum StoreCommand {
    Uptime(StoreUptimeParams),
    Commissions(StoreCommissionsParams),
    Versions(StoreVersionsParams),
    ClusterInfo(StoreClusterInfoParams),
    Validators(StoreValidatorsParams),
    ValidatorsBlockRewards(StoreBlockRewardsParams),
    ValidatorsEvents(StoreEventsParams),
    ValidatorsSandwiches(StoreSandwichesParams),
    TakeRates(StoreTakeRatesParams),
    Releases(StoreReleasesParams),
    JitoMev(StoreJitoParams),
    JitoPriority(StoreJitoParams),
    CloseEpoch(CloseEpochParams),
    LsOpenEpochs(LsOpenEpochsParams),
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(Env::default().default_filter_or("info")).init();

    let params = Params::parse();
    let directory = Directory::new(params.common.directory_url, params.common.directory_token)?;

    match params.command {
        StoreCommand::Uptime(store_params) => store_uptime(store_params, &directory).await,
        StoreCommand::Commissions(store_params) => {
            store_commissions(store_params, &directory).await
        }
        StoreCommand::Versions(store_params) => store_versions(store_params, &directory).await,
        StoreCommand::ClusterInfo(store_params) => {
            store_cluster_info(store_params, &directory).await
        }
        StoreCommand::Validators(store_params) => store_validators(store_params, &directory).await,
        StoreCommand::JitoMev(store_params) => {
            store_jito(
                store_params,
                &directory,
                JitoAccountType::MevTipDistribution,
            )
            .await
        }
        StoreCommand::JitoPriority(store_params) => {
            store_jito(
                store_params,
                &directory,
                JitoAccountType::PriorityFeeDistribution,
            )
            .await
        }
        StoreCommand::ValidatorsBlockRewards(store_params) => {
            store_block_rewards(store_params, &directory).await
        }
        StoreCommand::ValidatorsEvents(store_params) => {
            store_events(store_params, &directory).await
        }
        StoreCommand::ValidatorsSandwiches(store_params) => {
            store_sandwiches(store_params, &directory).await
        }
        StoreCommand::TakeRates(store_params) => store_take_rates(store_params, &directory).await,
        StoreCommand::Releases(store_params) => store_releases(store_params, &directory).await,
        StoreCommand::CloseEpoch(close_params) => close_epoch(close_params, &directory).await,
        StoreCommand::LsOpenEpochs(_ls_params) => list_open_epochs(&directory).await,
    }
}
