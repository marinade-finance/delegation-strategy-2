use crate::context::WrappedContext;
use crate::metrics;
use serde::Serialize;
use store::dto::ValidatorScoreRecord;
use warp::{http::StatusCode, reply::json, Reply};

#[derive(Serialize, Debug)]
pub struct Response {
    scores: Vec<ValidatorScoreRecord>,
}

pub async fn handler(context: WrappedContext) -> Result<impl Reply, warp::Rejection> {
    metrics::REQUEST_COUNT_VALIDATOR_SCORES.inc();

    log::info!("Query validator scores");

    Ok(warp::reply::with_status(
        json(&Response {
            scores: context
                .read()
                .await
                .cache
                .get_validators_scores()
                .scores
                .values()
                .cloned()
                .collect(),
        }),
        StatusCode::OK,
    ))
}
