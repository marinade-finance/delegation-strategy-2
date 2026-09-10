use crate::context::WrappedContext;
use crate::handlers::list_validators::{
    count_validators, FilterParams, GetValidatorsConfig, ValidatorPageParams,
};
use crate::metrics;
use crate::utils::response::response_error;
use serde::Serialize;
use warp::{http::StatusCode, reply::json, Reply};

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct ResponseValidatorsCount {
    /// Rows matching the query and filters: validators, or operator blocks under
    /// `with_operator_groups`. The same number `/validators` serves as `total_count`.
    count: usize,
}

#[utoipa::path(
    get,
    tag = "Validators",
    operation_id = "Count validators",
    path = "/validators/count",
    params(FilterParams),
    responses(
        (status = 200, body = ResponseValidatorsCount),
        (status = 400, description = "Invalid incident query params")
    )
)]
pub async fn handler(
    filters: FilterParams,
    context: WrappedContext,
) -> Result<impl Reply, warp::Rejection> {
    metrics::REQUEST_COUNT_VALIDATORS_COUNT.inc();
    let config = match GetValidatorsConfig::from_params(filters, ValidatorPageParams::default()) {
        Ok(config) => config,
        Err((status, message)) => return Ok(response_error(status, message)),
    };

    log::info!("Count validators {config:?}");

    let count = count_validators(context, config).await;

    Ok(warp::reply::with_status(
        json(&ResponseValidatorsCount { count }),
        StatusCode::OK,
    ))
}
