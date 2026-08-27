use chrono::{DateTime, Utc};
use log::error;
use serde::Serialize;
use store::validators_events::resolve_epoch_for_date;
use tokio_postgres::Client;
use warp::{
    http::StatusCode,
    reply::{json, Json, WithStatus},
};

#[derive(Serialize)]
struct ErrorResponse {
    message: String,
}

pub fn response_error_500(message: String) -> WithStatus<Json> {
    response_error(StatusCode::INTERNAL_SERVER_ERROR, message)
}

pub fn response_error(status: StatusCode, message: String) -> WithStatus<Json> {
    warp::reply::with_status(json(&ErrorResponse { message }), status)
}

/// Resolves the lower-bound epoch of a time series query. `query_from_epoch` and
/// `query_from_date` are mutually exclusive; on failure returns the HTTP status + message to
/// respond with.
pub async fn resolve_from_epoch(
    psql_client: &Client,
    query_from_epoch: Option<u64>,
    query_from_date: Option<DateTime<Utc>>,
) -> Result<Option<u64>, (StatusCode, String)> {
    match (query_from_epoch, query_from_date) {
        (Some(_), Some(_)) => Err((
            StatusCode::BAD_REQUEST,
            "Specify only one of query_from_epoch / query_from_date".into(),
        )),
        (Some(epoch), None) => Ok(Some(epoch)),
        (None, Some(date)) => match resolve_epoch_for_date(psql_client, date, true).await {
            Ok(Some(epoch)) => Ok(Some(epoch)),
            Ok(None) => Err((
                StatusCode::BAD_REQUEST,
                "query_from_date is outside the recorded epoch range".into(),
            )),
            Err(err) => {
                error!("Failed to resolve query_from_date: {err}");
                Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Failed to fetch records!".into(),
                ))
            }
        },
        (None, None) => Ok(None),
    }
}
