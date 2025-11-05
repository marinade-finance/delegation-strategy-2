use crate::context::WrappedContext;
use crate::utils::response_error;
use log::{error, info};
use serde::{Deserialize, Serialize};
use store::{dto::JitoMevRecord, validators_jito::get_last_mev_info};
use warp::{http::StatusCode, reply::json, Reply};

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct ResponseJitoMev {
    validators: Vec<JitoMevRecord>,
}

#[derive(Deserialize, Serialize, Debug)]
pub struct QueryParams {}
const DEFAULT_EPOCHS: u64 = 10;

#[utoipa::path(
    get,
    tag = "Last Jito MEV Info (DEPRECATED)",
    operation_id = "List last Jito MEV Info",
    path = "/mev",
    responses(
        (status = 200, body = ResponseJitoMev, description = "DEPRECATED: use /jito endpoint instead")
    )
)]
pub async fn handler(
    _: QueryParams,
    context: WrappedContext,
) -> Result<impl Reply, warp::Rejection> {
    info!("Fetching Jito MEV Info");

    let response = match get_last_mev_info(&context.read().await.psql_client, DEFAULT_EPOCHS).await
    {
        Ok(validators) => {
            warp::reply::with_status(json(&ResponseJitoMev { validators }), StatusCode::OK)
        }
        Err(err) => {
            error!("Failed to fetch Jito MEV info: {err}");
            response_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to fetch Jito MEV records!".into(),
            )
        }
    };

    Ok(warp::reply::with_header(response, "Deprecation", "true"))
}
