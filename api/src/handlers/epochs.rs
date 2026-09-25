use crate::context::WrappedContext;
use crate::metrics;
use crate::utils::response::response_error;
use log::{error, info};
use serde::{Deserialize, Serialize};
use store::dto::EpochRecord;
use store::epochs::load_epochs;
use warp::{http::StatusCode, reply::json, Reply};

const DEFAULT_EPOCHS: u32 = 15;

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct ResponseEpochs {
    epochs: Vec<EpochRecord>,
}

#[derive(Deserialize, Serialize, Debug, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct QueryParams {
    /// How many of the newest closed epochs to return. Default 15.
    epochs: Option<u32>,
}

#[utoipa::path(
    get,
    tag = "General",
    operation_id = "List epochs",
    description = "Start and end time of the newest closed epochs, newest first. The running epoch has no row until it closes.",
    path = "/epochs",
    params(QueryParams),
    responses(
        (status = 200, body = ResponseEpochs),
        (status = 500, description = "Failed to fetch records")
    )
)]
pub async fn handler(
    query_params: QueryParams,
    context: WrappedContext,
) -> Result<impl Reply, warp::Rejection> {
    metrics::REQUEST_COUNT_EPOCHS.inc();
    info!("Fetching epochs {query_params:?}");

    let epochs = query_params.epochs.unwrap_or(DEFAULT_EPOCHS);
    match load_epochs(&context.read().await.psql_client, epochs.into()).await {
        Ok(epochs) => Ok(warp::reply::with_status(
            json(&ResponseEpochs { epochs }),
            StatusCode::OK,
        )),
        Err(err) => {
            error!("Failed to fetch epochs: {err}");
            Ok(response_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to fetch records!".into(),
            ))
        }
    }
}
