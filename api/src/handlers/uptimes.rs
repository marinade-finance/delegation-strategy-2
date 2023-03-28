use crate::context::WrappedContext;
use crate::metrics;
use crate::utils::response_error;
use log::{error, info};
use serde::{Deserialize, Serialize};
use store::dto::UptimeRecord;
use warp::{http::StatusCode, reply::json, Reply};

#[derive(Serialize, Debug)]
pub struct Response {
    uptimes: Vec<UptimeRecord>,
}

#[derive(Deserialize, Serialize, Debug)]
enum OrderField {
    Stake,
    OtherField,
}

#[derive(Deserialize, Serialize, Debug)]
enum OrderDirection {
    ASC,
    DESC,
}

#[derive(Deserialize, Serialize, Debug)]
pub struct QueryParams {}

pub async fn handler(
    vote_account: String,
    _query_params: QueryParams,
    context: WrappedContext,
) -> Result<impl Reply, warp::Rejection> {
    info!("Fetching uptimes {:?}", &vote_account);
    metrics::REQUEST_COUNT_UPTIMES.inc();

    let validators = context.read().await.cache.get_validators();
    let validator = validators.iter().find(|(_vote_key, record)| {
        record.identity == vote_account || record.vote_account == vote_account
    });

    match validator {
        Some((vote_key, _validator)) => {
            let uptimes = context.read().await.cache.get_uptimes(&vote_key);

            Ok(match uptimes {
                Some(uptimes) => {
                    warp::reply::with_status(json(&Response { uptimes }), StatusCode::OK)
                }
                _ => {
                    error!("No uptimes found for {}", &vote_account);
                    response_error(StatusCode::NOT_FOUND, "Failed to fetch records!".into())
                }
            })
        }
        None => {
            error!("No validator found for {}", &vote_account);
            Ok(response_error(
                StatusCode::NOT_FOUND,
                "Failed to fetch records!".into(),
            ))
        }
    }
}
