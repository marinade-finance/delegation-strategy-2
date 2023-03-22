use crate::context::WrappedContext;
use crate::metrics;
use crate::utils::response_error;
use log::{error, info};
use serde::{Deserialize, Serialize};
use store::dto::VersionRecord;
use warp::{http::StatusCode, reply::json, Reply};

#[derive(Serialize, Debug)]
pub struct Response {
    versions: Vec<VersionRecord>,
}

#[derive(Deserialize, Serialize, Debug)]
pub struct QueryParams {}

pub async fn handler(
    vote_account: String,
    _query_params: QueryParams,
    context: WrappedContext,
) -> Result<impl Reply, warp::Rejection> {
    info!("Fetching versions {:?}", &vote_account);
    metrics::REQUEST_COUNT_VERSIONS.inc();

    let versions = context.read().await.cache.get_versions(&vote_account);

    Ok(match versions {
        Some(versions) => warp::reply::with_status(json(&Response { versions }), StatusCode::OK),
        _ => {
            error!("No versions found for {}", &vote_account);
            response_error(StatusCode::NOT_FOUND, "Failed to fetch records!".into())
        }
    })
}
