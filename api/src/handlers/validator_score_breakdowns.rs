use std::collections::HashMap;

use crate::metrics;
use crate::utils::response_error;
use crate::{cache::CachedAllScores, context::WrappedContext};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use store::dto::ValidatorScoreRecord;
use store::utils::to_fixed_for_sort;
use utoipa::IntoParams;
use warp::{http::StatusCode, reply::json, Reply};

use super::validator_score_breakdown::ScoreBreakdown;

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct ResponseScoreBreakdowns {
    score_breakdowns: Vec<ScoreBreakdown>,
}

#[derive(Deserialize, Serialize, Debug, IntoParams)]
pub struct QueryParams {
    query_from_date: Option<DateTime<Utc>>,
}

#[utoipa::path(
    get,
    tag = "Scoring",
    operation_id = "Show score breakdowns for a validator for a certain period of time",
    path = "/validators/<vote_account>/score-breakdowns",
    params(QueryParams),
    responses(
        (status = 200, body = ResponseScoreBreakdowns)
    )
)]
pub async fn handler(
    vote_account: String,
    query_params: QueryParams,
    context: WrappedContext,
) -> Result<impl Reply, warp::Rejection> {
    log::info!("Query validator score breakdown for {:?}", query_params);
    metrics::REQUEST_COUNT_VALIDATOR_SCORE_BREAKDOWNS.inc();
    let CachedAllScores {
        scoring_runs,
        scores,
    } = context.read().await.cache.get_validators_all_scores();

    let mut all_scores = match scores.get(&vote_account).cloned() {
        Some(all_scores) => all_scores,
        None => {
            log::warn!("No scores found for the validators!");
            return Ok(response_error(
                StatusCode::OK,
                "No scores found for the validators!".into(),
            ));
        }
    };

    if let Some(from_date) = query_params.query_from_date {
        all_scores = all_scores
            .iter()
            .filter(|s| s.created_at > from_date)
            .cloned()
            .collect();
    }

    let mut score_breakdowns: Vec<ScoreBreakdown> = Vec::new();

    let all_scoring_runs = match scoring_runs {
        Some(all_scoring_runs) => all_scoring_runs,
        None => {
            log::warn!("No scoring runs found!");
            return Ok(response_error(
                StatusCode::OK,
                "No scoring runs found!".into(),
            ));
        }
    };

    let mut runs_min_elig_scores: HashMap<Decimal, Option<f64>> = Default::default();
    for scoring_run in &all_scoring_runs {
        let scoring_run_scores: HashMap<String, ValidatorScoreRecord> = scores
            .iter()
            .filter_map(|(_k, v)| {
                v.iter()
                    .find(|i| scoring_run.scoring_run_id == i.scoring_run_id.into())
                    .map(|item| (item.clone().vote_account, item.clone()))
            })
            .collect();
        let min_score_eligible_algo = scoring_run_scores
            .iter()
            .filter(|(_, score)| score.target_stake_algo > 0)
            .map(|(_, ValidatorScoreRecord { score, .. })| *score)
            .min_by(|a, b| to_fixed_for_sort(*a).cmp(&to_fixed_for_sort(*b)));
        runs_min_elig_scores
            .entry(scoring_run.scoring_run_id)
            .or_insert_with(|| min_score_eligible_algo);
    }

    for score in all_scores {
        let scoring_run = all_scoring_runs
            .iter()
            .find(|s| s.scoring_run_id == score.scoring_run_id.into());
        if let Some(scoring_run) = scoring_run {
            score_breakdowns.push(ScoreBreakdown {
                vote_account: score.vote_account,
                score: score.score,
                rank: score.rank,
                min_score_eligible_algo: *runs_min_elig_scores
                    .get(&scoring_run.scoring_run_id)
                    .unwrap(),
                ui_hints: score.ui_hints,
                mnde_votes: score.mnde_votes,
                component_scores: score.component_scores,
                component_ranks: score.component_ranks,
                component_values: score.component_values,
                component_weights: scoring_run.clone().component_weights,
                components: scoring_run.clone().components,
                eligible_stake_algo: score.eligible_stake_algo,
                eligible_stake_mnde: score.eligible_stake_mnde,
                eligible_stake_msol: score.eligible_stake_msol,
                target_stake_algo: score.target_stake_algo,
                target_stake_mnde: score.target_stake_mnde,
                target_stake_msol: score.target_stake_msol,
                scoring_run_id: score.scoring_run_id,
                created_at: score.created_at,
                epoch: scoring_run.epoch,
                ui_id: scoring_run.clone().ui_id,
            });
        }
    }

    Ok(warp::reply::with_status(
        json(&ResponseScoreBreakdowns { score_breakdowns }),
        StatusCode::OK,
    ))
}
