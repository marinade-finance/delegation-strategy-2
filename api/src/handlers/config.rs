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
                        "name": "Marinade Decentralizer"
                    },
                    {
                        "delegation_authority": "mpa4abUkjQoAvPzREkh5Mo75hZhPFQ2FSH6w7dWKuQ5",
                        "name": "Solana Foundation"
                    },
                    {
                        "delegation_authority": "6iQKfEyhr3bZMotVkW6beNZz5CPAkiwvgV2CTje9pVSS",
                        "name": "Jito"
                    },
                    {
                        "delegation_authority": "W1ZQRwUfSkDKy2oefRBUWph82Vr2zg9txWMA8RQazN5",
                        "name": "Lido"
                    }
                ]
            }
        })),
        StatusCode::OK,
    ))
}
