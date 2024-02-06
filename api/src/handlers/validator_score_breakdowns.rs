use std::collections::HashMap;
use std::convert::Infallible;

use crate::metrics;
use crate::utils::response_error;
use crate::{cache::CachedMultiRunScores, context::WrappedContext};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use store::dto::{ScoringRunRecord, ValidatorScoreRecord};
use store::utils::to_fixed_for_sort;
use utoipa::IntoParams;
use warp::reply::{Json, WithStatus};
use warp::{http::StatusCode, reply::json, Reply};

use super::validator_score_breakdown::ScoreBreakdown;

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct ResponseScoreBreakdowns {
    score_breakdowns: Vec<ScoreBreakdown>,
}

#[derive(Deserialize, Serialize, Debug, IntoParams)]
pub struct QueryParams {
    query_from_date: Option<DateTime<Utc>>,
    query_vote_account: Option<String>,
}

#[utoipa::path(
    get,
    tag = "Scoring",
    operation_id = "Show score breakdowns for a validator for a certain period of time",
    path = "/validators/score-breakdowns",
    params(QueryParams),
    responses(
        (status = 200, body = ResponseScoreBreakdowns)
    )
)]

pub async fn handler(
    query_params: QueryParams,
    context: WrappedContext,
) -> Result<impl Reply, Infallible> {
    log::info!("Query validator score breakdown for {:?}", query_params);
    metrics::REQUEST_COUNT_VALIDATOR_SCORE_BREAKDOWNS.inc();

    match get_and_validate_scores(context).await {
        Ok((scoring_runs, mut validator_scores)) => {
            if let Some(from_date) = query_params.query_from_date {
                validator_scores = filter_scores_by_date(from_date, validator_scores);
            }

            let runs_min_elig_scores =
                compute_runs_min_elig_scores(&scoring_runs, &validator_scores);

            if let Some(from_date) = query_params.query_vote_account {
                validator_scores = filter_scores_by_vote_account(from_date, validator_scores);
            }

            let filtered_validator_scores = validator_scores.values()
                    .flat_map(|v| v.clone())
                    .collect();
            let score_breakdowns = compute_score_breakdowns(
                &scoring_runs,
                &filtered_validator_scores,
                &runs_min_elig_scores,
            );

            Ok(warp::reply::with_status(
                json(&ResponseScoreBreakdowns { score_breakdowns }),
                StatusCode::OK,
            ))
        }
        Err(error_response) => Ok(error_response),
    }
}

async fn get_and_validate_scores(
    context: WrappedContext,
) -> Result<
    (
        Vec<ScoringRunRecord>,
        HashMap<Decimal, Vec<ValidatorScoreRecord>>,
    ),
    WithStatus<Json>,
> {
    let CachedMultiRunScores {
        scoring_runs,
        scores,
    } = context.read().await.cache.get_validators_multi_run_scores();

    let scoring_runs = scoring_runs.ok_or_else(|| {
        log::warn!("No scoring runs found!");
        response_error(StatusCode::NOT_FOUND, "No scoring runs found!".to_string())
    })?;

    Ok((scoring_runs, scores))
}

fn filter_scores_by_date(
    from_date: DateTime<Utc>,
    validator_scores: HashMap<Decimal, Vec<ValidatorScoreRecord>>,
) -> HashMap<Decimal, Vec<ValidatorScoreRecord>> {
    validator_scores
        .into_iter()
        .filter_map(|(key, v)| {
            let filtered_records: Vec<ValidatorScoreRecord> =
                v.into_iter().filter(|v| v.created_at > from_date).collect();

            if filtered_records.is_empty() {
                None
            } else {
                Some((key, filtered_records))
            }
        })
        .collect()
}

fn filter_scores_by_vote_account(
    vote_account: String,
    validator_scores: HashMap<Decimal, Vec<ValidatorScoreRecord>>,
) -> HashMap<Decimal, Vec<ValidatorScoreRecord>> {
    validator_scores
        .into_iter()
        .filter_map(|(key, v)| {
            let filtered_records: Vec<ValidatorScoreRecord> = v
                .into_iter()
                .filter(|v| v.vote_account == vote_account)
                .collect();

            if filtered_records.is_empty() {
                None
            } else {
                Some((key, filtered_records))
            }
        })
        .collect()
}

fn compute_runs_min_elig_scores(
    scoring_runs: &Vec<ScoringRunRecord>,
    validator_scores: &HashMap<Decimal, Vec<ValidatorScoreRecord>>,
) -> HashMap<Decimal, Option<f64>> {
    let mut runs_min_elig_scores: HashMap<Decimal, Option<f64>> = Default::default();

    for scoring_run in scoring_runs {
        let mut scoring_run_scores: HashMap<String, ValidatorScoreRecord> = HashMap::new();

        if let Some(records) = validator_scores.get(&scoring_run.scoring_run_id) {
            for record in records {
                scoring_run_scores.insert(record.vote_account.clone(), record.clone());
            }
        }

        let min_score_eligible_algo = scoring_run_scores
            .iter()
            .filter(|(_, score)| score.target_stake_algo > 0)
            .map(|(_, score)| score.score)
            .min_by(|a, b| to_fixed_for_sort(*a).cmp(&to_fixed_for_sort(*b)));

        runs_min_elig_scores
            .entry(scoring_run.scoring_run_id)
            .or_insert_with(|| min_score_eligible_algo);
    }

    runs_min_elig_scores
}

fn compute_score_breakdowns(
    scoring_runs: &Vec<ScoringRunRecord>,
    validator_scores: &Vec<ValidatorScoreRecord>,
    runs_min_elig_scores: &HashMap<Decimal, Option<f64>>,
) -> Vec<ScoreBreakdown> {
    let mut score_breakdowns: Vec<ScoreBreakdown> = Vec::new();

    for score in validator_scores {
        if let Some(scoring_run) = scoring_runs
            .iter()
            .find(|s| s.scoring_run_id == score.scoring_run_id.into())
        {
            score_breakdowns.push(ScoreBreakdown {
                vote_account: score.vote_account.clone(),
                score: score.score,
                rank: score.rank,
                min_score_eligible_algo: *runs_min_elig_scores
                    .get(&scoring_run.scoring_run_id)
                    .unwrap(),
                ui_hints: score.ui_hints.clone(),
                vemnde_votes: score.vemnde_votes,
                msol_votes: score.msol_votes,
                component_scores: score.component_scores.clone(),
                component_ranks: score.component_ranks.clone(),
                component_values: score.component_values.clone(),
                component_weights: scoring_run.component_weights.clone(),
                components: scoring_run.components.clone(),
                eligible_stake_algo: score.eligible_stake_algo,
                eligible_stake_vemnde: score.eligible_stake_vemnde,
                eligible_stake_mnde: score.eligible_stake_vemnde,
                eligible_stake_msol: score.eligible_stake_msol,
                target_stake_algo: score.target_stake_algo,
                target_stake_vemnde: score.target_stake_vemnde,
                target_stake_mnde: score.target_stake_vemnde,
                target_stake_msol: score.target_stake_msol,
                scoring_run_id: score.scoring_run_id,
                created_at: score.created_at,
                epoch: scoring_run.epoch,
                ui_id: scoring_run.ui_id.clone(),
            });
        }
    }

    score_breakdowns
}
