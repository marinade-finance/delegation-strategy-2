use crate::{context::WrappedContext, metrics, utils::response_error_500};
use log::{error, info};
use serde::{Deserialize, Serialize};
use warp::{http::StatusCode, reply::json, Reply};

#[derive(Serialize)]
pub struct Response {
    unstake_hints: Vec<store::dto::UnstakeHintRecord>,
}

#[derive(Deserialize, Serialize, Debug)]
pub struct QueryParams {
    epoch: u64,
}

pub async fn handler(
    query_params: QueryParams,
    context: WrappedContext,
) -> Result<impl Reply, warp::Rejection> {
    info!("Fetching unstake hints {:?}", query_params.epoch);
    metrics::REQUEST_UNSTAKE_HINTS.inc();

    let unstake_hints = store::scoring::load_unstake_hints(
        &context.read().await.psql_client,
        &context.read().await.blacklist_path,
        query_params.epoch,
    )
    .await;

    Ok(match unstake_hints {
        Ok(unstake_hints) => {
            warp::reply::with_status(json(&Response { unstake_hints }), StatusCode::OK)
        }
        Err(err) => {
            error!("Failed to load unstake hints: {}", err);
            response_error_500("Failed to load unstake hints!".into())
        }
    })
}
