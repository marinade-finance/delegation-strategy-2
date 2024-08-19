use crate::context::WrappedContext;
use crate::utils::response_error;
use log::{error, info};
use serde::{Deserialize, Serialize};
use store::{dto::MevRecord, validators_mev::get_last_mev_info};
use warp::{http::StatusCode, reply::json, Reply};

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct ResponseMev {
    validators: Vec<MevRecord>,
}

#[derive(Deserialize, Serialize, Debug)]
pub struct QueryParams {}
const DEFAULT_EPOCHS: u64 = 10;

#[utoipa::path(
    get,
    tag = "Last MEV Info",
    operation_id = "List last MEV Info",
    path = "/mev",
    responses(
        (status = 200, body = ResponseMev)
    )
)]
pub async fn handler(
    query_params: QueryParams,
    context: WrappedContext,
) -> Result<impl Reply, warp::Rejection> {
    info!("Fetching MEV Info");

    let validators =
        match get_last_mev_info(&context.read().await.psql_client, DEFAULT_EPOCHS).await {
            Ok(r) => r,
            Err(err) => {
                error!("Failed to fetch MEV info: {}", err);
                return Ok(response_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Failed to fetch MEV records!".into(),
                ));
            }
        };

    Ok(warp::reply::with_status(
        json(&ResponseMev { validators }),
        StatusCode::OK,
    ))
}
