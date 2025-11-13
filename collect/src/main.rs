use collect::common::*;
use collect::validators::*;
use collect::validators_block_rewards::{collect_validator_block_rewards_info, BlockRewardsParams};
use collect::validators_jito::{collect_jito_info, JitoAccountType, JitoParams};
use collect::validators_performance::{
    collect_validators_performance_info, ValidatorsPerformanceParams,
};
use env_logger::Env;
use log::info;
use std::fmt::Display;
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
    Validators(ValidatorsParams),
    ValidatorsPerformance(ValidatorsPerformanceParams),
    JitoMev(JitoParams),
    JitoPriority(JitoParams),
    ValidatorsBlockRewards(BlockRewardsParams),
}

impl Display for CollectCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CollectCommand::Validators(_) => write!(f, "validators"),
            CollectCommand::ValidatorsPerformance(_) => write!(f, "validators-performance"),
            CollectCommand::JitoMev(_) => write!(f, "jito-mev"),
            CollectCommand::JitoPriority(_) => write!(f, "jito-priority"),
            CollectCommand::ValidatorsBlockRewards(_) => write!(f, "validators-block-rewards"),
        }
    }
}

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(Env::default().default_filter_or("info")).init();

    let params = Params::from_args();

    let command_name = params.command.to_string();
    let result = match params.command {
        CollectCommand::Validators(validators_params) => {
            collect_validators_info(params.common, validators_params)
        }
        CollectCommand::ValidatorsPerformance(performance_params) => {
            collect_validators_performance_info(params.common, performance_params)
        }
        CollectCommand::JitoMev(jito_params) => collect_jito_info(
            params.common,
            jito_params,
            JitoAccountType::MevTipDistribution,
        ),
        CollectCommand::JitoPriority(jito_params) => collect_jito_info(
            params.common,
            jito_params,
            JitoAccountType::PriorityFeeDistribution,
        ),
        CollectCommand::ValidatorsBlockRewards(rewards_params) => {
            collect_validator_block_rewards_info(params.common, rewards_params)
        }
    };

    match result {
        Ok(_) => info!("Collect {command_name} finished successfully."),
        Err(err) => {
            anyhow::bail!("Collect {command_name} processing finished with an error: {err}")
        }
    }

    Ok(())
}
