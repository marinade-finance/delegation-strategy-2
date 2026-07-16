use crate::context::WrappedContext;
use crate::utils::response_error;
use chrono::{DateTime, Utc};
use log::{error, info};
use serde::{Deserialize, Serialize};
use store::dto::StakersRecord;
use warp::{http::StatusCode, reply::json, Reply};

const DEFAULT_EPOCHS: u64 = 80;

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct ResponseStakers {
    stakers: Vec<StakersRecord>,
}

#[derive(Deserialize, Serialize, Debug, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct QueryParams {
    query_from_date: Option<DateTime<Utc>>,
}

#[utoipa::path(
    get,
    tag = "Validators",
    operation_id = "List unique stakers per epoch",
    path = "/validators/{vote_account}/stakers",
    params(
        ("vote_account" = String, Path, description = "Vote account or identity of the validator"),
        QueryParams
    ),
    responses(
        (status = 200, body = ResponseStakers)
    )
)]
pub async fn handler(
    vote_account: String,
    query_params: QueryParams,
    context: WrappedContext,
) -> Result<impl Reply, warp::Rejection> {
    info!("Fetching stakers {:?}", &vote_account);

    let validators = context.read().await.cache.get_validators();
    let validator = validators.iter().find(|(_vote_key, record)| {
        record.identity == vote_account || record.vote_account == vote_account
    });

    match validator {
        Some((vote_key, _validator)) => {
            let stakers = context.read().await.cache.get_stakers(vote_key);

            Ok(match stakers {
                Some(mut stakers) => {
                    if let Some(query_from_date) = query_params.query_from_date {
                        stakers = stakers
                            .iter()
                            .filter(|v| v.epoch_end_at.is_none_or(|d| d > query_from_date))
                            .cloned()
                            .collect();
                    } else if let Some(max_epoch) =
                        stakers.iter().map(|v| v.epoch).max()
                    {
                        stakers = stakers
                            .iter()
                            .filter(|v| v.epoch > max_epoch.saturating_sub(DEFAULT_EPOCHS))
                            .cloned()
                            .collect();
                    }
                    warp::reply::with_status(json(&ResponseStakers { stakers }), StatusCode::OK)
                }
                _ => {
                    error!("No stakers found for {}", &vote_account);
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
