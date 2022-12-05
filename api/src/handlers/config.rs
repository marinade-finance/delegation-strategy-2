use crate::context::WrappedContext;
use log::info;
use serde_json::json;
use warp::{http::StatusCode, reply, Reply};

pub async fn handler(_context: WrappedContext) -> Result<impl Reply, warp::Rejection> {
    info!("Serving the configuration data");
    Ok(warp::reply::with_status(
        reply::json(&json!({
            "stakes": {
                "delegation_authorities": [
                    {
                        "delegation_authority": "4bZ6o3eUUNXhKuqjdCnCoPAoLgWiuLYixKaxoa8PpiKk",
                        "name": "Marinade"
                    },
                    {
                        "delegation_authority": "noMa7dN4cHQLV4ZonXrC29HTKFpxrpFbDLK5Gub8W8t",
                        "name": "Marinade's Decentralizer"
                    }
                ]
            }
        })),
        StatusCode::OK,
    ))
}
