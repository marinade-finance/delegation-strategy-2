use crate::context::WrappedContext;
use crate::metrics;
use crate::utils::response_error;
use chrono::{DateTime, Utc};
use log::{error, info};
use serde::{Deserialize, Serialize};
use store::dto::CommissionRecord;
use warp::{http::StatusCode, reply::json, Reply};

const DEFAULT_EPOCHS: u64 = 20;

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct ResponseCommissions {
    commissions: Vec<CommissionRecord>,
}

#[derive(Deserialize, Serialize, Debug, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct QueryParams {
    query_from_date: Option<DateTime<Utc>>,
}

#[utoipa::path(
    get,
    tag = "Validators",
    operation_id = "List commission changes",
    path = "/validators/{vote_account}/commissions",
    params(
        ("vote_account" = String, Path, description = "Vote account or identity of the validator"),
        QueryParams
    ),
    responses(
        (status = 200, body = ResponseCommissions)
    )
)]
pub async fn handler(
    vote_account: String,
    query_params: QueryParams,
    context: WrappedContext,
) -> Result<impl Reply, warp::Rejection> {
    info!("Fetching commissions {:?}", &vote_account);
    metrics::REQUEST_COUNT_COMMISSIONS.inc();

    let validators = context.read().await.cache.get_validators();
    let validator = validators.iter().find(|(_vote_key, record)| {
        record.identity == vote_account || record.vote_account == vote_account
    });

    match validator {
        Some((vote_key, _validator)) => {
            let commissions = context.read().await.cache.get_commissions(vote_key);

            Ok(match commissions {
                Some(mut commissions) => {
                    if let Some(query_from_date) = query_params.query_from_date {
                        commissions = commissions
                            .iter()
                            .filter(|v| v.epoch_start_at > query_from_date)
                            .cloned()
                            .collect();
                    } else {
                        let max_epoch = commissions
                            .iter()
                            .max_by(|x, y| x.epoch.cmp(&y.epoch))
                            .unwrap()
                            .epoch;
                        commissions = commissions
                            .iter()
                            .filter(|v| v.epoch > max_epoch - DEFAULT_EPOCHS)
                            .cloned()
                            .collect();
                    }
                    warp::reply::with_status(
                        json(&ResponseCommissions { commissions }),
                        StatusCode::OK,
                    )
                }
                _ => {
                    error!("No commissions found for {}", &vote_account);
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
