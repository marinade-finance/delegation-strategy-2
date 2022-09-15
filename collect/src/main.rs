use collect::cluster_info::*;
use collect::common::*;
use collect::validators::*;
use env_logger::Env;
use log::{debug, error, info};
use structopt::StructOpt;

#[derive(Debug, StructOpt)]
struct Params {
    #[structopt(flatten)]
    common: CommonParams,

    #[structopt(subcommand)]
    command: CollectCommand,
}

#[derive(Debug, StructOpt)]
enum CollectCommand {
    ClusterInfo(ClusterInfoOptions),
    Validators(ValidatorsOptions),
}

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(Env::default().default_filter_or("info")).init();

    let mut params = Params::from_args();

    Ok(match params.command {
        CollectCommand::ClusterInfo(options) => collect_cluster_info(params.common),
        CollectCommand::Validators(options) => collect_validators_info(params.common, options),
    }?)
}
