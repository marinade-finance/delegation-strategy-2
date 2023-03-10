use crate::metrics;
use crate::{context::WrappedContext, utils::response_error};
use serde::{Deserialize, Serialize};
use store::dto::ValidatorScoreRecord;
use warp::{http::StatusCode, reply::json, Reply};

#[derive(Serialize, Debug)]
pub struct Response {
    score_breakdown: ScoreBreakdown,
}

#[derive(Deserialize, Serialize, Debug)]
pub struct QueryParams {
    query_vote_account: String,
}

#[derive(Deserialize, Serialize, Debug)]
pub struct ScoreBreakdown {
    pub vote_account: String,
    pub score: f64,
    pub rank: i32,
    pub ui_hints: Vec<String>,
    pub component_scores: Vec<f64>,
    pub eligible_stake_algo: bool,
    pub eligible_stake_mnde: bool,
    pub eligible_stake_msol: bool,
    pub target_stake_algo: u64,
    pub target_stake_mnde: u64,
    pub target_stake_msol: u64,
    pub scoring_run_id: i64,
}

pub async fn handler(
    query_params: QueryParams,
    context: WrappedContext,
) -> Result<impl Reply, warp::Rejection> {
    metrics::REQUEST_COUNT_VALIDATOR_SCORE_BREAKDOWN.inc();

    log::info!("Query validator score breakdown {:?}", query_params);

    let all_scores = context.read().await.cache.get_validators_scores();
    let ValidatorScoreRecord {
        vote_account,
        score,
        rank,
        ui_hints,
        component_scores,
        eligible_stake_algo,
        eligible_stake_mnde,
        eligible_stake_msol,
        target_stake_algo,
        target_stake_mnde,
        target_stake_msol,
        scoring_run_id,
    } = match all_scores.get(&query_params.query_vote_account).cloned() {
        Some(score) => score,
        None => {
            log::warn!("No score found for the validator!");
            return Ok(response_error(
                StatusCode::OK,
                "No score found for the validator!".into(),
            ));
        }
    };

    Ok(warp::reply::with_status(
        json(&Response {
            score_breakdown: ScoreBreakdown {
                vote_account,
                score,
                rank,
                ui_hints,
                component_scores,
                eligible_stake_algo,
                eligible_stake_mnde,
                eligible_stake_msol,
                target_stake_algo,
                target_stake_mnde,
                target_stake_msol,
                scoring_run_id,
            },
        }),
        StatusCode::OK,
    ))
}
