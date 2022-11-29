use crate::context::WrappedContext;
use log::info;
use serde_json::json;
use warp::{http::StatusCode, reply, Reply};

pub async fn handler(_context: WrappedContext) -> Result<impl Reply, warp::Rejection> {
    info!("Serving the scoring report");
    Ok(warp::reply::with_status(
        reply::json(&json!({
            "planned": [
                { "identity": "XkCriyrNwS3G4rzAXtG5B1nnvb5Ka1JtCku93VqeKAr", "stake": 1_000_000_000_000_000u64, "change": 50_000_000_000_000i64 },
                { "identity": "Awes4Tr6TX8JDzEhCZY2QVNimT6iD1zWHzf1vNyGvpLM", "stake": 50_000_000_000_000u64, "change": -50_000_000_000_000i64 },
            ]
        })),
        StatusCode::OK,
    ))
}
