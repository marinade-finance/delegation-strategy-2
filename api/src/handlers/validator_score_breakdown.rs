use crate::cache::CachedSingleRunScores;
use crate::metrics;
use crate::{context::WrappedContext, utils::response_error};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use store::dto::{ScoringRunRecord, ValidatorScoreRecord};
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
    #[deprecated = "Use `eligible_stake_vemnde` instead"]
    pub eligible_stake_mnde: bool,
    pub eligible_stake_msol: bool,
    pub target_stake_algo: u64,
    pub target_stake_vemnde: u64,
    #[deprecated = "Use `target_stake_vemnde` instead"]
    pub target_stake_mnde: u64,
    pub target_stake_msol: u64,
    pub scoring_run_id: i64,
    pub created_at: DateTime<Utc>,
    pub epoch: i32,
    pub ui_id: String,
}

// total_cmp, not to_fixed_for_sort: the latter's None for a non-finite score sorts ahead of every Some and would be handed back as the minimum.
pub fn min_eligible_algo_score(scores: &HashMap<String, ValidatorScoreRecord>) -> Option<f64> {
    scores
        .values()
        .filter(|score| score.target_stake_algo > 0)
        .map(|score| score.score)
        .min_by(f64::total_cmp)
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

    log::info!("Query validator score breakdown {query_params:?}");

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

    let min_score_eligible_algo = min_eligible_algo_score(&scores);

    #[allow(deprecated)]
    let score_breakdown = ScoreBreakdown {
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
    };
    Ok(warp::reply::with_status(
        json(&ResponseScoreBreakdown { score_breakdown }),
        StatusCode::OK,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scores(records: Vec<(&str, f64, u64)>) -> HashMap<String, ValidatorScoreRecord> {
        records
            .into_iter()
            .map(|(vote_account, score, target_stake_algo)| {
                (
                    vote_account.to_string(),
                    ValidatorScoreRecord {
                        vote_account: vote_account.to_string(),
                        score,
                        target_stake_algo,
                        rank: 1,
                        vemnde_votes: 0,
                        msol_votes: 0,
                        ui_hints: vec![],
                        component_scores: vec![],
                        component_ranks: vec![],
                        component_values: vec![],
                        eligible_stake_algo: true,
                        eligible_stake_vemnde: true,
                        eligible_stake_msol: true,
                        target_stake_vemnde: 0,
                        target_stake_msol: 0,
                        scoring_run_id: 1,
                        created_at: Utc::now(),
                    },
                )
            })
            .collect()
    }

    #[test]
    fn min_eligible_algo_score_skips_validators_without_algo_stake() {
        assert_eq!(
            min_eligible_algo_score(&scores(vec![("aaa", 1.0, 0), ("bbb", 5.0, 100)])),
            Some(5.0)
        );
        assert_eq!(
            min_eligible_algo_score(&scores(vec![("aaa", 1.0, 0)])),
            None
        );
    }

    #[test]
    fn min_eligible_algo_score_is_not_won_by_a_non_finite_score() {
        // The scoring CSV parses "1e400" to infinity, which used to saturate to u64::MAX and now yields None.
        assert_eq!(
            min_eligible_algo_score(&scores(vec![
                ("aaa", 5.0, 100),
                ("bbb", f64::INFINITY, 100),
                ("ccc", 9.0, 100),
            ])),
            Some(5.0)
        );
    }
}
