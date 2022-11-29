use serde::Serialize;
use warp::{
    http::StatusCode,
    reply::{json, Json, WithStatus},
};

#[derive(Serialize)]
struct ErrorResponse {
    message: String,
}

pub fn reponse_error_500(message: String) -> WithStatus<Json> {
    warp::reply::with_status(
        json(&ErrorResponse { message }),
        StatusCode::INTERNAL_SERVER_ERROR,
    )
}
