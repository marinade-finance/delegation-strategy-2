use crate::cache::ReadyFlag;
use warp::{http::StatusCode, Reply};

#[utoipa::path(
    get,
    tag = "General",
    operation_id = "Readiness",
    path = "/readyz",
    responses(
        (status = 200, description = "All caches loaded"),
        (status = 503, description = "Cache warmup has not finished")
    )
)]
pub async fn handler(ready: ReadyFlag) -> Result<impl Reply, warp::Rejection> {
    Ok(if ready.is_ready() {
        warp::reply::with_status("ready", StatusCode::OK)
    } else {
        warp::reply::with_status("warming up", StatusCode::SERVICE_UNAVAILABLE)
    })
}
