use warp::{http::StatusCode, Reply};

#[utoipa::path(
    get,
    tag = "General",
    operation_id = "Liveness",
    path = "/healthz",
    responses(
        (status = 200, description = "Process is serving requests")
    )
)]
pub async fn handler() -> Result<impl Reply, warp::Rejection> {
    Ok(warp::reply::with_status("alive", StatusCode::OK))
}
