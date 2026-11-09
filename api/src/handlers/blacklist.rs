use crate::{context::WrappedContext, metrics, utils::response::response_error_500};
use log::{error, info};
use serde::Serialize;
use store::blacklist::db::{load_blacklist, BlacklistRow};
use warp::{http::StatusCode, reply::json, Reply};

#[derive(Serialize, utoipa::ToSchema)]
pub struct ResponseBlacklist {
    /// The last closed epoch the rules judged or skipped; `null` before the rules first ran.
    evaluated_epoch: Option<u64>,
    rows: Vec<BlacklistRow>,
}

#[utoipa::path(
    get,
    tag = "Scoring",
    operation_id = "Blacklist",
    path = "/blacklist",
    responses(
        (status = 200, body = ResponseBlacklist)
    )
)]
pub async fn handler(context: WrappedContext) -> Result<impl Reply, warp::Rejection> {
    info!("Fetching blacklist");
    metrics::REQUEST_BLACKLIST.inc();

    Ok(
        match load_blacklist(&context.read().await.psql_client).await {
            Ok((evaluated_epoch, rows)) => warp::reply::with_status(
                json(&ResponseBlacklist {
                    evaluated_epoch,
                    rows,
                }),
                StatusCode::OK,
            ),
            Err(err) => {
                error!("Failed to load the blacklist: {err}");
                response_error_500("Failed to load the blacklist!".into())
            }
        },
    )
}
