use crate::context::WrappedContext;
use crate::utils::response_error;
use log::{error, info};
use serde::{Deserialize, Serialize};
use store::dto::JitoPriorityFeeRecord;
use store::validators_jito::get_last_priority_fee_info;
use warp::{http::StatusCode, reply::json, Reply};

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct ResponseJitoPriorityFee {
    validators: Vec<JitoPriorityFeeRecord>,
}

#[derive(Deserialize, Serialize, Debug)]
pub struct QueryParams {}
const DEFAULT_EPOCHS: u64 = 10;

#[utoipa::path(
    get,
    tag = "Last Jito Priority Fee Info",
    operation_id = "List last Jito Priority Fee Info",
    path = "/jito-priority-fee",
    responses(
        (status = 200, body = ResponseJitoPriorityFee)
    )
)]
pub async fn handler(
    _: QueryParams,
    context: WrappedContext,
) -> Result<impl Reply, warp::Rejection> {
    info!("Fetching Jito Priority Fee Info");

    let validators =
        match get_last_priority_fee_info(&context.read().await.psql_client, DEFAULT_EPOCHS).await {
            Ok(r) => r,
            Err(err) => {
                error!("Failed to fetch Jito Priority Fee info: {}", err);
                return Ok(response_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Failed to fetch Jito Priority Fee records!".into(),
                ));
            }
        };

    Ok(warp::reply::with_status(
        json(&ResponseJitoPriorityFee { validators }),
        StatusCode::OK,
    ))
}
