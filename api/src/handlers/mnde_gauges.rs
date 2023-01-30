use crate::context::WrappedContext;
use crate::metrics;
use serde::Serialize;
use std::collections::HashMap;
use warp::{http::StatusCode, reply::json, Reply};

#[derive(Serialize, Debug)]
pub struct Response {
    mnde_gauges: HashMap<String, u64>,
}

pub async fn handler(context: WrappedContext) -> Result<impl Reply, warp::Rejection> {
    metrics::REQUEST_COUNT_MNDE_GAUGES.inc();
    log::info!("Query MNDE gauges");

    let validators = context.read().await.cache.get_validators();

    let mnde_gauges = validators
        .iter()
        .flat_map(|(_, validator)| {
            if let Some(votes) = validator.mnde_votes {
                let votes: u64 = votes.try_into().unwrap();
                if votes > 0 {
                    Some((validator.vote_account.to_string(), votes))
                } else {
                    None
                }
            } else {
                None
            }
        })
        .collect();

    Ok(warp::reply::with_status(
        json(&Response { mnde_gauges }),
        StatusCode::OK,
    ))
}
