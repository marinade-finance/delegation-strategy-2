use crate::{
    cache::CachedSingleRunScores, context::WrappedContext, metrics, utils::response_error,
};
use log::{error, info, warn};
use serde::Serialize;
use solana_program::native_token::LAMPORTS_PER_SOL;
use store::utils::get_last_epoch;
use warp::{http::StatusCode, reply, Reply};

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct ResponseReportStaking {
    planned: Vec<Stake>,
}

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct Stake {
    vote_account: String,
    identity: String,
    current_stake: u64,
    next_stake: u64,
}

#[derive(Serialize, Debug)]
pub struct StakingChange {
    vote_account: String,
    identity: String,
    score: f64,
    current_stake: u64,
    next_stake: u64,
}

fn filter_and_sort_stakes(records: &mut Vec<StakingChange>) {
    records.retain(|stake| stake.next_stake as f64 - stake.current_stake as f64 != 0.0);
    records.sort_by_key(|a| {
        if a.next_stake > 0 && a.next_stake > a.current_stake {
            -(a.next_stake as i64)
        } else {
            a.current_stake as i64 - a.next_stake as i64
        }
    });
}

async fn get_planned_stakes(context: WrappedContext) -> anyhow::Result<Vec<StakingChange>> {
    let mut records = Vec::new();
    let last_epoch = match get_last_epoch(&context.read().await.psql_client).await? {
        Some(last_epoch) => last_epoch,
        _ => return Ok(Default::default()),
    };

    let CachedSingleRunScores { scores, .. } = &context
        .read()
        .await
        .cache
        .get_validators_single_run_scores();
    let validators = &context.read().await.cache.get_validators();

    for (vote_account, score_record) in scores.iter() {
        let validator = validators
            .get(vote_account)
            .filter(|v| v.has_last_epoch_stats);
        let should_have = score_record.target_stake_algo
            + score_record.target_stake_vemnde
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
                        identity: validator.identity.clone(),
                        vote_account: validator.vote_account.clone(),
                        score: score_record.score,
                        current_stake: current_epoch_stats.marinade_stake.try_into().unwrap(),
                        next_stake: should_have * LAMPORTS_PER_SOL,
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
                warn!("Couldn't find info for {} in current epoch", vote_account);
                continue;
            }
        }
    }

    Ok(records)
}

#[utoipa::path(
    get,
    tag = "Scoring",
    operation_id = "Show planned stakes",
    path = "/reports/staking",
    responses(
        (status = 200, body = ResponseReportStaking)
    )
)]
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
                        identity: planned_stake.identity,
                        current_stake: planned_stake.current_stake,
                        next_stake: planned_stake.next_stake,
                    });
                }
            }
            Ok(warp::reply::with_status(
                reply::json(&ResponseReportStaking { planned: stakes }),
                StatusCode::OK,
            ))
        }
        Err(err) => {
            error!("Failed to fetch scores records: {}", err);
            Ok(response_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to fetch records".into(),
            ))
        }
    }
}
