use crate::context::WrappedContext;
use crate::metrics;
use crate::utils::response_error;
use chrono::{DateTime, Utc};
use log::{error, info};
use serde::{Deserialize, Serialize};
use store::dto::UptimeRecord;
use warp::{http::StatusCode, reply::json, Reply};

const DEFAULT_EPOCHS: u64 = 20;

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct ResponseUptimes {
    uptimes: Vec<UptimeRecord>,
}

#[derive(Deserialize, Serialize, Debug)]
pub struct QueryParams {
    query_from_date: Option<DateTime<Utc>>,
}

#[utoipa::path(
    get,
    tag = "Validators",
    operation_id = "List uptimes",
    path = "/validators/<vote_account>/uptimes",
    responses(
        (status = 200, body = ResponseUptimes)
    )
)]
pub async fn handler(
    vote_account: String,
    query_params: QueryParams,
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
            let uptimes = context.read().await.cache.get_uptimes(vote_key);

            Ok(match uptimes {
                Some(mut uptimes) => {
                    if let Some(query_from_date) = query_params.query_from_date {
                        uptimes = uptimes
                            .iter()
                            .filter(|v| v.epoch_start_at > query_from_date)
                            .cloned()
                            .collect();
                    } else {
                        let max_epoch = uptimes
                            .iter()
                            .max_by(|x, y| x.epoch.cmp(&y.epoch))
                            .unwrap()
                            .epoch;
                        uptimes = uptimes
                            .iter()
                            .filter(|v| v.epoch > max_epoch - DEFAULT_EPOCHS)
                            .cloned()
                            .collect();
                    }
                    warp::reply::with_status(json(&ResponseUptimes { uptimes }), StatusCode::OK)
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
