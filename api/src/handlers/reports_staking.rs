use crate::context::WrappedContext;
use log::info;
use serde_json::json;
use warp::{http::StatusCode, reply, Reply};

pub async fn handler(_context: WrappedContext) -> Result<impl Reply, warp::Rejection> {
    info!("Serving the scoring report");
    Ok(warp::reply::with_status(
        reply::json(&json!({
            "planned": [
                { "identity": "XkCriyrNwS3G4rzAXtG5B1nnvb5Ka1JtCku93VqeKAr", "current_stake": 1_000_000_000_000_000u64, "next_stake": 1_200_000_000_000_000u64, "immediate": true },
                { "identity": "Awes4Tr6TX8JDzEhCZY2QVNimT6iD1zWHzf1vNyGvpLM", "current_stake": 50_000_000_000_000u64, "next_stake": 0, "immediate": true },
                { "identity": "DRpbCBMxVnDK7maPM5tGv6MvB3v1sRMC86PZ8okm21hy", "current_stake": 20_000_000_000_000u64, "next_stake": 0, "immediate": false },
            ]
        })),
        StatusCode::OK,
    ))
}
