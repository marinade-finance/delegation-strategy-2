use crate::{context::WrappedContext, metrics};
use log::info;
use serde::Serialize;
use store::utils::get_last_epoch;
use warp::{http::StatusCode, reply, Reply};

#[derive(Serialize, Debug)]
pub struct Response {
    planned: Vec<Stake>,
}

#[derive(Serialize, Debug)]
pub struct Stake {
    identity: String,
    current_stake: u128,
    next_stake: u128,
}

#[derive(Serialize, Debug)]
pub struct StakingChange {
    identity: String,
    score: f64,
    current_stake: u128,
    next_stake: u128,
}

pub async fn get_planned_stakes(
    context: WrappedContext,
) -> anyhow::Result<Vec<StakingChange>> {
    let psql_client = &context.read().await.psql_client;
    let cache = &context.read().await.cache;
    
    let mut records = Vec::new();

    let validators = cache.get_validators();
    let scores  = cache.get_validators_scores();
    let last_epoch = match get_last_epoch(psql_client).await? {
        Some(last_epoch) => last_epoch,
        _ => return Ok(Default::default()),
    };

    for current_validator in validators.values().into_iter() {
        if let Some(current_epoch_stats) = current_validator.epoch_stats.iter().filter(|validator| validator.epoch == last_epoch).last() {
            let marinade_stake = current_epoch_stats.marinade_stake as u128;
            if let Some(validator_score) = scores.get(&current_validator.vote_account) {
                let should_have = (validator_score.target_stake_algo + validator_score.target_stake_mnde + validator_score.target_stake_msol) as u128;
                if validator_score.eligible_stake_algo && (should_have as f64 - marinade_stake as f64 != 0.0) {
                    records.push(StakingChange {
                        identity: current_validator.identity.clone(),
                        score: validator_score.score,
                        current_stake: marinade_stake,
                        next_stake: should_have * 1_000_000_000,
                    })
                }
            }
        }
    }
    records.sort_by_key(|a| {
        if a.next_stake > 0 && a.next_stake > a.current_stake {
            -(a.score as i64)
        } else {
            (a.current_stake - a.next_stake) as i64
        }
    });
    Ok(records)
}


pub async fn handler(context: WrappedContext) -> Result<impl Reply, warp::Rejection> {
    info!("Serving the staking report");
    metrics::REQUEST_COUNT_REPORT_STAKING.inc();
    let mut stakes: Vec<Stake> = Vec::new();
    if let Ok(planned_stakes) = get_planned_stakes(context).await {
        for planned_stake in planned_stakes {
            if planned_stake.score > 0.0 || planned_stake.current_stake > 0 {
                stakes.push(Stake {
                    identity: planned_stake.identity,
                    current_stake: planned_stake.current_stake,
                    next_stake: planned_stake.next_stake
                });
            }
        }
    }

    Ok(warp::reply::with_status(
        reply::json( &Response {
            planned: stakes
        }),
        StatusCode::OK,
    ))
}
