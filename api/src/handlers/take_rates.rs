use crate::context::WrappedContext;
use crate::metrics;
use crate::utils::{resolve_from_epoch, response_error};
use chrono::{DateTime, Utc};
use log::{error, info};
use serde::{Deserialize, Serialize};
use store::dto::TakeRateRecord;
use store::take_rates::get_take_rate_series;
use tokio_postgres::Client;
use warp::{http::StatusCode, reply::json, Reply};

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct ResponseTakeRates {
    take_rates: Vec<TakeRateRecord>,
}

#[derive(Deserialize, Serialize, Debug, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct QueryParams {
    /// Lower-bound epoch (inclusive). Mutually exclusive with `query_from_date`.
    query_from_epoch: Option<u64>,
    /// Lower-bound date (RFC3339), resolved to the first epoch ending on/after it. Mutually exclusive with `query_from_epoch`.
    query_from_date: Option<DateTime<Utc>>,
}

impl QueryParams {
    async fn resolve_from_epoch(&self, psql: &Client) -> Result<Option<u64>, (StatusCode, String)> {
        resolve_from_epoch(psql, self.query_from_epoch, self.query_from_date).await
    }
}

#[utoipa::path(
    get,
    tag = "Validators",
    operation_id = "List take rate history",
    description = "Take rate per epoch for a validator. Returns the whole stored history unless bounded by a query parameter. `epoch_start_at` and `epoch_end_at` are null for epochs whose boundaries are not recorded.",
    path = "/validators/{vote_account}/take-rates",
    params(
        ("vote_account" = String, Path, description = "Vote account or identity of the validator"),
        QueryParams
    ),
    responses(
        (status = 200, body = ResponseTakeRates),
        (status = 400, description = "Both query parameters given, or query_from_date outside the recorded epoch range"),
        (status = 404, description = "No validator found for the given vote account or identity"),
        (status = 500, description = "Failed to fetch records")
    )
)]
pub async fn handler(
    vote_account: String,
    query_params: QueryParams,
    context: WrappedContext,
) -> Result<impl Reply, warp::Rejection> {
    info!("Fetching take rates {:?}", &vote_account);
    metrics::REQUEST_COUNT_TAKE_RATES.inc();

    let validators = context.read().await.cache.get_validators();
    let validator = validators.iter().find(|(_vote_key, record)| {
        record.identity == vote_account || record.vote_account == vote_account
    });

    let Some((vote_key, _validator)) = validator else {
        error!("No validator found for {}", &vote_account);
        return Ok(response_error(
            StatusCode::NOT_FOUND,
            "Failed to fetch records!".into(),
        ));
    };

    let context_guard = context.read().await;
    let psql_client = &context_guard.psql_client;

    let from_epoch = match query_params.resolve_from_epoch(psql_client).await {
        Ok(from_epoch) => from_epoch,
        Err((status, message)) => return Ok(response_error(status, message)),
    };

    let reward_mix = context_guard.cache.get_epoch_reward_mix();

    Ok(
        match get_take_rate_series(psql_client, vote_key, from_epoch, reward_mix).await {
            Ok(take_rates) => {
                warp::reply::with_status(json(&ResponseTakeRates { take_rates }), StatusCode::OK)
            }
            Err(err) => {
                error!("Failed to fetch take rates for {vote_account}: {err}");
                response_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Failed to fetch records!".into(),
                )
            }
        },
    )
}
