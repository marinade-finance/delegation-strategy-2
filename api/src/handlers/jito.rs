use crate::context::WrappedContext;
use crate::utils::response_error;
use log::{error, info};
use serde::{Deserialize, Serialize};
use store::dto::JitoRecord;
use store::validators_jito::get_last_jito_info;
use warp::{http::StatusCode, reply::json, Reply};

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct ResponseJito {
    validators: Vec<JitoRecord>,
}

#[derive(Deserialize, Serialize, Debug)]
pub struct QueryParams {}
const DEFAULT_EPOCHS: u64 = 10;

#[utoipa::path(
    get,
    tag = "Last Jito Rewards Info",
    operation_id = "List last Jito Rewards Info",
    path = "/jito",
    responses(
        (status = 200, body = ResponseJito)
    )
)]
pub async fn handler(
    _: QueryParams,
    context: WrappedContext,
) -> Result<impl Reply, warp::Rejection> {
    info!("Fetching Jito Priority Fee Info");

    let validators =
        match get_last_jito_info(&context.read().await.psql_client, DEFAULT_EPOCHS).await {
            Ok(r) => r,
            Err(err) => {
                error!("Failed to fetch Jito info: {}", err);
                return Ok(response_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Failed to fetch Jito records!".into(),
                ));
            }
        };

    Ok(warp::reply::with_status(
        json(&ResponseJito { validators }),
        StatusCode::OK,
    ))
}
