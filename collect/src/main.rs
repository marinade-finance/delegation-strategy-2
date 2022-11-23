use collect::cluster_info::*;
use collect::common::*;
use collect::validators::*;
use env_logger::Env;
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

    let params = Params::from_args();

    Ok(match params.command {
        CollectCommand::ClusterInfo(_options) => collect_cluster_info(params.common),
        CollectCommand::Validators(options) => collect_validators_info(params.common, options),
    }?)
}
