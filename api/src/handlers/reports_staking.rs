use crate::{cache::CachedScores, context::WrappedContext, metrics, utils::response_error};
use log::{error, info};
use serde::Serialize;
use solana_program::native_token::LAMPORTS_PER_SOL;
use warp::{http::StatusCode, reply, Reply};

#[derive(Serialize, Debug)]
pub struct Response {
    planned: Vec<Stake>,
}

#[derive(Serialize, Debug)]
pub struct Stake {
    identity: String,
    current_stake: u64,
    next_stake_sol: u64,
}

#[derive(Serialize, Debug)]
pub struct StakingChange {
    identity: String,
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
    let cache = &context.read().await.cache;
    let mut records = Vec::new();

    let CachedScores { scores, .. } = cache.get_validators_scores();
    let stakes = cache.get_validators_current_stakes();

    for validator_score in scores.values().into_iter() {
        let should_have = validator_score.target_stake_algo
            + validator_score.target_stake_mnde
            + validator_score.target_stake_msol;
        let stake_info = stakes.get(&validator_score.vote_account);

        match stake_info {
            Some(stake_info) => records.push(StakingChange {
                identity: stake_info.identity.clone(),
                score: validator_score.score,
                current_stake: stake_info.marinade_stake,
                next_stake_sol: should_have,
            }),
            None => {
                error!(
                    "Couldn't find current stake info for {}",
                    validator_score.vote_account
                );
                continue;
            }
        };
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
                        identity: planned_stake.identity,
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
