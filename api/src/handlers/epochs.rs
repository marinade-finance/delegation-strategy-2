use crate::context::WrappedContext;
use crate::utils::response_error;
use log::{error, info};
use serde::{Deserialize, Serialize};
use store::epochs::get_epochs;
use warp::{http::StatusCode, reply::json, Reply};
use store::dto::EpochInfo;

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct ResponseEpochs {
    epochs: Vec<EpochInfo>,
}

#[derive(Deserialize, Serialize, Debug)]
pub struct QueryParams {
}

#[utoipa::path(
    get,
    tag = "Epochs",
    operation_id = "List epochs",
    path = "/epochs",
    responses(
        (status = 200, body = ResponseEpochs)
    )
)]
pub async fn handler(
    query_params: QueryParams,
    context: WrappedContext,
) -> Result<impl Reply, warp::Rejection> {
    let epochs =
        match get_epochs(&context.read().await.psql_client).await {
            Ok(r) => r,
            Err(err) => {
                error!("Failed to fetch epochs info: {}", err);
                return Ok(response_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Failed to fetch epoch infos!".into(),
                ));
            }
        };
    Ok(warp::reply::with_status(
        json(&ResponseEpochs {
            epochs,
        }),
        StatusCode::OK,
    ))
}
