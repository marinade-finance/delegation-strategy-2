use crate::cache::CachedSingleRunScores;
use crate::metrics;
use crate::{context::WrappedContext, utils::response_error};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use store::dto::{ScoringRunRecord, ValidatorScoreRecord};
use store::utils::to_fixed_for_sort;
use utoipa::IntoParams;
use warp::{http::StatusCode, reply::json, Reply};

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct ResponseScoreBreakdown {
    score_breakdown: ScoreBreakdown,
}

#[derive(Deserialize, Serialize, Debug, IntoParams)]
pub struct QueryParams {
    query_vote_account: String,
}

#[derive(Deserialize, Serialize, Debug, utoipa::ToSchema)]
pub struct ScoreBreakdown {
    pub vote_account: String,
    pub score: f64,
    pub min_score_eligible_algo: Option<f64>,
    pub rank: i32,
    pub ui_hints: Vec<String>,
    pub vemnde_votes: u64,
    pub msol_votes: u64,
    pub component_scores: Vec<f64>,
    pub component_ranks: Vec<i32>,
    pub component_values: Vec<Option<String>>,
    pub component_weights: Vec<f64>,
    pub components: Vec<String>,
    pub eligible_stake_algo: bool,
    pub eligible_stake_vemnde: bool,
    #[deprecated  = "Use `eligible_stake_vemnde` instead"]
    pub eligible_stake_mnde: bool,
    pub eligible_stake_msol: bool,
    pub target_stake_algo: u64,
    pub target_stake_vemnde: u64,
    #[deprecated  = "Use `target_stake_vemnde` instead"]
    pub target_stake_mnde: u64,
    pub target_stake_msol: u64,
    pub scoring_run_id: i64,
    pub created_at: DateTime<Utc>,
    pub epoch: i32,
    pub ui_id: String,
}

#[utoipa::path(
    get,
    tag = "Scoring",
    operation_id = "Show last score breakdown for a validator",
    path = "/validators/score-breakdown",
    params(QueryParams),
    responses(
        (status = 200, body = ResponseScoreBreakdown)
    )
)]
pub async fn handler(
    query_params: QueryParams,
    context: WrappedContext,
) -> Result<impl Reply, warp::Rejection> {
    metrics::REQUEST_COUNT_VALIDATOR_SCORE_BREAKDOWN.inc();

    log::info!("Query validator score breakdown {:?}", query_params);

    let CachedSingleRunScores {
        scores,
        scoring_run,
    } = context
        .read()
        .await
        .cache
        .get_validators_single_run_scores();

    let ScoringRunRecord {
        epoch,
        components,
        component_weights,
        ui_id,
        ..
    } = match scoring_run {
        Some(scoring_run) => scoring_run,
        None => {
            log::warn!("No scoring run is present in the cache!");
            return Ok(response_error(
                StatusCode::OK,
                "No scoring run available!".into(),
            ));
        }
    };

    let ValidatorScoreRecord {
        vote_account,
        score,
        rank,
        ui_hints,
        vemnde_votes,
        msol_votes,
        component_scores,
        component_ranks,
        component_values,
        eligible_stake_algo,
        eligible_stake_vemnde,
        eligible_stake_msol,
        target_stake_algo,
        target_stake_vemnde,
        target_stake_msol,
        scoring_run_id,
        created_at,
    } = match scores.get(&query_params.query_vote_account).cloned() {
        Some(score) => score,
        None => {
            log::warn!("No score found for the validator!");
            return Ok(response_error(
                StatusCode::OK,
                "No score found for the validator!".into(),
            ));
        }
    };

    let min_score_eligible_algo = scores
        .iter()
        .filter(|(_, score)| score.target_stake_algo > 0)
        .map(|(_, ValidatorScoreRecord { score, .. })| *score)
        .min_by(|a, b| to_fixed_for_sort(*a).cmp(&to_fixed_for_sort(*b)));

    Ok(warp::reply::with_status(
        json(&ResponseScoreBreakdown {
            score_breakdown: ScoreBreakdown {
                vote_account,
                score,
                min_score_eligible_algo,
                rank,
                ui_hints,
                vemnde_votes,
                msol_votes,
                component_scores,
                component_ranks,
                component_values,
                component_weights,
                components,
                eligible_stake_algo,
                eligible_stake_vemnde,
                eligible_stake_mnde: eligible_stake_vemnde,
                eligible_stake_msol,
                target_stake_algo,
                target_stake_vemnde,
                target_stake_mnde: target_stake_vemnde,
                target_stake_msol,
                scoring_run_id,
                created_at,
                epoch,
                ui_id,
            },
        }),
        StatusCode::OK,
    ))
}
