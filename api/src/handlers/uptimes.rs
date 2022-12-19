use crate::context::WrappedContext;
use crate::metrics;
use crate::utils::response_error;
use log::{error, info};
use serde::{Deserialize, Serialize};
use store::dto::UptimeRecord;
use warp::{http::StatusCode, reply::json, Reply};

#[derive(Serialize, Debug)]
pub struct Response {
    uptimes: Vec<UptimeRecord>,
}

#[derive(Deserialize, Serialize, Debug)]
enum OrderField {
    Stake,
    OtherField,
}

#[derive(Deserialize, Serialize, Debug)]
enum OrderDirection {
    ASC,
    DESC,
}

#[derive(Deserialize, Serialize, Debug)]
pub struct QueryParams {}

pub async fn handler(
    identity: String,
    _query_params: QueryParams,
    context: WrappedContext,
) -> Result<impl Reply, warp::Rejection> {
    info!("Fetching uptimes {:?}", &identity);
    metrics::REQUEST_COUNT_UPTIMES.inc();

    let uptimes = context.read().await.cache.get_uptimes(&identity);

    Ok(match uptimes {
        Some(uptimes) => warp::reply::with_status(json(&Response { uptimes }), StatusCode::OK),
        _ => {
            error!("No uptimes found for {}", &identity);
            response_error(StatusCode::NOT_FOUND, "Failed to fetch records!".into())
        }
    })
}
