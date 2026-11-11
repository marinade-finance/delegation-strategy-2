use crate::{
    context::WrappedContext,
    metrics,
    utils::response::{response_error, response_error_500},
};
use log::{error, info};
use store::blacklist::db::{load_blacklist, Blacklist};
use warp::{http::StatusCode, reply::json, Reply};

pub const NOT_BOOTSTRAPPED: &str = "The blacklist is not bootstrapped yet";

#[utoipa::path(
    get,
    tag = "Scoring",
    operation_id = "Blacklist",
    path = "/blacklist",
    responses(
        (status = 200, body = Blacklist),
        (status = 503, description = "The legacy list is not imported yet; serving part of the list would unlist the rest")
    )
)]
pub async fn handler(context: WrappedContext) -> Result<impl Reply, warp::Rejection> {
    info!("Fetching blacklist");
    metrics::REQUEST_BLACKLIST.inc();

    Ok(
        match load_blacklist(&context.read().await.psql_client).await {
            Ok(Some(blacklist)) => warp::reply::with_status(json(&blacklist), StatusCode::OK),
            Ok(None) => response_error(StatusCode::SERVICE_UNAVAILABLE, NOT_BOOTSTRAPPED.into()),
            Err(err) => {
                error!("Failed to load the blacklist: {err}");
                response_error_500("Failed to load the blacklist!".into())
            }
        },
    )
}
