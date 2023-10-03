use crate::{context::WrappedContext, metrics, utils::response_error_500};
use log::{error, info};
use serde::{Deserialize, Serialize};
use store::dto::GlobalUnstakeHintRecord;
use warp::{http::StatusCode, reply::json, Reply};

#[derive(Serialize, utoipa::ToSchema)]
pub struct ResponseGlobalUnstakeHints {
    unstake_hints: Vec<GlobalUnstakeHintRecord>,
}

#[derive(Deserialize, Serialize, Debug, utoipa::IntoParams)]
pub struct QueryParams {
    epoch: u64,
}

#[utoipa::path(
    get,
    tag = "Scoring",
    operation_id = "List global unstake hints",
    path = "/global-unstake-hints",
    params(QueryParams),
    responses(
        (status = 200, body = ResponseGlobalUnstakeHints)
    )
)]
pub async fn handler(
    query_params: QueryParams,
    context: WrappedContext,
) -> Result<impl Reply, warp::Rejection> {
    info!("Fetching unstake hints {:?}", query_params.epoch);
    metrics::REQUEST_UNSTAKE_HINTS.inc();

    let unstake_hints = store::scoring::load_global_unstake_hint_records(
        &context.read().await.psql_client,
        &context.read().await.blacklist_path,
        query_params.epoch,
    )
    .await;

    Ok(match unstake_hints {
        Ok(unstake_hints) => warp::reply::with_status(
            json(&ResponseGlobalUnstakeHints { unstake_hints }),
            StatusCode::OK,
        ),
        Err(err) => {
            error!("Failed to load global unstake hints: {}", err);
            response_error_500("Failed to load global unstake hints!".into())
        }
    })
}
