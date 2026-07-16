use crate::context::WrappedContext;
use crate::utils::response_error;
use log::{error, info};
use serde::{Deserialize, Serialize};
use store::dto::EventEpochRecord;
use store::validators_events::get_events_with_context;
use warp::{http::StatusCode, reply::json, Reply};

const DEFAULT_EPOCHS: u64 = 80;

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct ResponseEvents {
    events: Vec<EventEpochRecord>,
}

#[derive(Deserialize, Serialize, Debug, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct QueryParams {}

#[utoipa::path(
    get,
    tag = "Validators",
    operation_id = "List per-epoch validator events",
    description = "Per-epoch performance, downtime and PSR settlement events for a validator. `settlements[].reason` / `.meta` are raw upstream JSON",
    path = "/validators/{vote_account}/events",
    params(
        ("vote_account" = String, Path, description = "Vote account or identity of the validator")
    ),
    responses(
        (status = 200, body = ResponseEvents)
    )
)]
pub async fn handler(
    vote_account: String,
    _query_params: QueryParams,
    context: WrappedContext,
) -> Result<impl Reply, warp::Rejection> {
    info!("Fetching events {:?}", &vote_account);

    let validators = context.read().await.cache.get_validators();
    let validator = validators.iter().find(|(_vote_key, record)| {
        record.identity == vote_account || record.vote_account == vote_account
    });

    let vote_key = match validator {
        Some((vote_key, _validator)) => vote_key.clone(),
        None => {
            error!("No validator found for {}", &vote_account);
            return Ok(response_error(
                StatusCode::NOT_FOUND,
                "Failed to fetch records!".into(),
            ));
        }
    };

    let events = match get_events_with_context(
        &context.read().await.psql_client,
        &vote_key,
        DEFAULT_EPOCHS,
    )
    .await
    {
        Ok(events) => events,
        Err(err) => {
            error!("Failed to fetch events for {vote_account}: {err}");
            return Ok(response_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to fetch records!".into(),
            ));
        }
    };

    Ok(warp::reply::with_status(
        json(&ResponseEvents { events }),
        StatusCode::OK,
    ))
}
