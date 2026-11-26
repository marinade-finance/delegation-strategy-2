use crate::context::WrappedContext;
use crate::metrics;
use serde::Serialize;
use store::dto::ValidatorScoreRecord;
use warp::{http::StatusCode, reply::json, Reply};

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct ResponseScores {
    scores: Vec<ValidatorScoreRecord>,
}

#[utoipa::path(
    get,
    tag = "Scoring",
    operation_id = "List last scores for all validators",
    path = "/validators/scores",
    responses(
        (status = 200, body = ResponseScores)
    )
)]
pub async fn handler(context: WrappedContext) -> Result<impl Reply, warp::Rejection> {
    metrics::REQUEST_COUNT_VALIDATOR_SCORES.inc();

    log::info!("Query validator scores");

    Ok(warp::reply::with_status(
        json(&ResponseScores {
            scores: context
                .read()
                .await
                .cache
                .get_validators_single_run_scores()
                .scores
                .values()
                .cloned()
                .collect(),
        }),
        StatusCode::OK,
    ))
}
