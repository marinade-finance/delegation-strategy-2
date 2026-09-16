use crate::context::WrappedContext;
use crate::utils::response_error;
use chrono::{DateTime, Utc};
use log::{error, info};
use serde::{Deserialize, Serialize};
use store::dto::EventEpochRecord;
use store::validators_events::{get_events_with_context, resolve_epoch_for_date};
use store::warehouse::Warehouse;
use warp::{http::StatusCode, reply::json, Reply};

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct ResponseEvents {
    events: Vec<EventEpochRecord>,
}

#[derive(Deserialize, Serialize, Debug, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct QueryParams {
    /// Lower-bound epoch (inclusive). Mutually exclusive with `query_from_date`. Defaults to the whole served range, which is the last 80 epochs; that range is a hard floor, so a lower value adds no epochs to the answer.
    query_from_epoch: Option<u64>,
    /// Lower-bound date (RFC3339), resolved to the first epoch ending on/after it. Mutually exclusive with `query_from_epoch`.
    query_from_date: Option<DateTime<Utc>>,
}

impl QueryParams {
    /// Resolves the lower-bound epoch. `query_from_epoch` and `query_from_date` are mutually
    /// exclusive; on failure returns the HTTP status + message to respond with.
    fn resolve_from_epoch(
        &self,
        warehouse: &Warehouse,
    ) -> Result<Option<u64>, (StatusCode, String)> {
        match (self.query_from_epoch, self.query_from_date) {
            (Some(_), Some(_)) => Err((
                StatusCode::BAD_REQUEST,
                "Specify only one of query_from_epoch / query_from_date".into(),
            )),
            (Some(epoch), None) => Ok(Some(epoch)),
            (None, Some(date)) => match resolve_epoch_for_date(warehouse, date, true) {
                Some(epoch) => Ok(Some(epoch)),
                None => Err((
                    StatusCode::BAD_REQUEST,
                    "query_from_date is outside the recorded epoch range".into(),
                )),
            },
            (None, None) => Ok(None),
        }
    }
}

#[utoipa::path(
    get,
    tag = "Validators",
    operation_id = "List per-epoch validator events",
    description = "Per-epoch performance, downtime and PSR settlement events for a validator. `settlements[].reason` / `.meta` are raw upstream JSON.",
    path = "/validators/{vote_account}/events",
    params(
        ("vote_account" = String, Path, description = "Vote account or identity of the validator"),
        QueryParams
    ),
    responses(
        (status = 200, body = ResponseEvents),
        (status = 400, description = "Invalid query params (query_from_epoch / query_from_date are mutually exclusive, or query_from_date is outside the recorded epoch range)"),
        (status = 404, description = "No validator found for the given vote account or identity"),
        (status = 500, description = "Failed to fetch records")
    )
)]
pub async fn handler(
    vote_account: String,
    query_params: QueryParams,
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

    let warehouse = context.read().await.warehouse.clone();
    let warehouse = warehouse.read().await;

    let from_epoch = match query_params.resolve_from_epoch(&warehouse) {
        Ok(from_epoch) => from_epoch,
        Err((status, message)) => return Ok(response_error(status, message)),
    };

    let events = match get_events_with_context(&warehouse, &vote_key, from_epoch) {
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
