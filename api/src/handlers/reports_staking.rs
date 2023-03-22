use crate::{cache::CachedScores, context::WrappedContext, metrics, utils::response_error};
use log::{error, info};
use serde::Serialize;
use solana_program::native_token::LAMPORTS_PER_SOL;
use store::utils::get_last_epoch;
use warp::{http::StatusCode, reply, Reply};

#[derive(Serialize, Debug)]
pub struct Response {
    planned: Vec<Stake>,
}

#[derive(Serialize, Debug)]
pub struct Stake {
    vote_account: String,
    current_stake: u64,
    next_stake_sol: u64,
}

#[derive(Serialize, Debug)]
pub struct StakingChange {
    vote_account: String,
    score: f64,
    current_stake: u64,
    next_stake_sol: u64,
}

fn filter_and_sort_stakes(records: &mut Vec<StakingChange>) {
    records.retain(|stake| {
        (stake.next_stake_sol * LAMPORTS_PER_SOL) as f64 - stake.current_stake as f64 != 0.0
    });
    records.sort_by_key(|a| {
        if a.next_stake_sol > 0 && a.next_stake_sol * LAMPORTS_PER_SOL > a.current_stake {
            -(a.score as i64)
        } else {
            (a.current_stake - a.next_stake_sol * LAMPORTS_PER_SOL) as i64
        }
    });
}

async fn get_planned_stakes(context: WrappedContext) -> anyhow::Result<Vec<StakingChange>> {
    let psql_client = &context.read().await.psql_client;
    let cache = &context.read().await.cache;
    let mut records = Vec::new();
    let last_epoch = match get_last_epoch(psql_client).await? {
        Some(last_epoch) => last_epoch,
        _ => return Ok(Default::default()),
    };

    let CachedScores { scores, .. } = cache.get_validators_scores();
    let validators = cache.get_validators();

    for (vote_account, score_record) in scores.iter() {
        let validator = validators
            .get(vote_account)
            .filter(|v| v.has_last_epoch_stats);
        let should_have = score_record.target_stake_algo
            + score_record.target_stake_mnde
            + score_record.target_stake_msol;
        match validator {
            Some(validator) => {
                let current_epoch_stats = validator
                    .epoch_stats
                    .iter()
                    .filter(|validator| validator.epoch == last_epoch)
                    .last();
                match current_epoch_stats {
                    Some(current_epoch_stats) => records.push(StakingChange {
                        vote_account: validator.vote_account.clone(),
                        score: score_record.score,
                        current_stake: current_epoch_stats.marinade_stake,
                        next_stake_sol: should_have,
                    }),
                    None => {
                        error!(
                            "Couldn't find current epoch stats for {}",
                            validator.vote_account
                        );
                        continue;
                    }
                }
            }
            None => {
                error!("Couldn't find info for {} in current epoch", vote_account);
                continue;
            }
        }
    }

    Ok(records)
}

pub async fn handler(context: WrappedContext) -> Result<impl Reply, warp::Rejection> {
    info!("Serving the staking report");
    metrics::REQUEST_COUNT_REPORT_STAKING.inc();
    let mut stakes: Vec<Stake> = Vec::new();
    match get_planned_stakes(context).await {
        Ok(mut planned_stakes) => {
            filter_and_sort_stakes(&mut planned_stakes);
            for planned_stake in planned_stakes {
                if planned_stake.score > 0.0 || planned_stake.current_stake > 0 {
                    stakes.push(Stake {
                        vote_account: planned_stake.vote_account,
                        current_stake: planned_stake.current_stake,
                        next_stake_sol: planned_stake.next_stake_sol,
                    });
                }
            }
            return Ok(warp::reply::with_status(
                reply::json(&Response { planned: stakes }),
                StatusCode::OK,
            ));
        }
        Err(err) => {
            error!("Failed to fetch scores records: {}", err);
            return Ok(response_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to fetch records".into(),
            ));
        }
    }
}
