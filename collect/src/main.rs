use collect::common::*;
use collect::validators::*;
use collect::validators_mev::collect_validators_mev_info;
use collect::validators_performance::{
    collect_validators_performance_info, ValidatorsPerformanceOptions,
};
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
    Validators(ValidatorsOptions),
    ValidatorsPerformance(ValidatorsPerformanceOptions),
    ValidatorsMEV,
}

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(Env::default().default_filter_or("info")).init();

    let params = Params::from_args();

    Ok(match params.command {
        CollectCommand::Validators(options) => collect_validators_info(params.common, options),
        CollectCommand::ValidatorsPerformance(options) => {
            collect_validators_performance_info(params.common, options)
        }
        CollectCommand::ValidatorsMEV => collect_validators_mev_info(params.common),
    }?)
}
