use crate::context::WrappedContext;
use crate::handlers::order::{
    OrderDirection, OrderField, DEFAULT_ORDER_DIRECTION, DEFAULT_ORDER_FIELD,
};
use crate::handlers::validator_groups::{page_groups, GetGroupsConfig, DEFAULT_LIMIT};
use crate::metrics;
use chrono::{DateTime, Utc};
use rust_decimal::prelude::*;
use serde::{Deserialize, Serialize};
use store::dto::ValidatorGroupRecord;
use warp::{http::StatusCode, reply::json, Reply};

/// Groups by hosting organisation (`dc_aso`); the data never resolves the building.
#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct ResponseProviders {
    providers: Vec<ValidatorGroupRecord>,
    /// Number of providers matching `query`, before `offset`/`limit`.
    total_count: usize,
    /// Activated stake of every validator counted, in lamports — the denominator behind `stake_share`.
    total_activated_stake: Decimal,
    /// Epoch the rows describe.
    current_epoch: Option<u64>,
    /// When apy-api last answered for the net APY the rates are derived from. Older than a few
    /// minutes means those values are being reused because that API is failing.
    net_apy_updated_at: Option<DateTime<Utc>>,
}

#[derive(Deserialize, Serialize, Debug, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct QueryParams {
    /// Case-insensitive text search over the provider name.
    query: Option<String>,
    order_field: Option<OrderField>,
    order_direction: Option<OrderDirection>,
    offset: Option<usize>,
    limit: Option<usize>,
}

#[utoipa::path(
    get,
    tag = "Validators",
    operation_id = "List providers",
    path = "/providers",
    params(QueryParams),
    responses(
        (status = 200, body = ResponseProviders)
    )
)]
pub async fn handler(
    query_params: QueryParams,
    context: WrappedContext,
) -> Result<impl Reply, warp::Rejection> {
    metrics::REQUEST_COUNT_PROVIDERS.inc();

    let config = GetGroupsConfig {
        order_field: query_params.order_field.unwrap_or(DEFAULT_ORDER_FIELD),
        order_direction: query_params
            .order_direction
            .unwrap_or(DEFAULT_ORDER_DIRECTION),
        offset: query_params.offset.unwrap_or(0),
        limit: query_params.limit.unwrap_or(DEFAULT_LIMIT),
        query: query_params.query,
    };

    log::info!("Query providers {config:?}");

    let (groups, net_apy_updated_at) = {
        let cache = &context.read().await.cache;
        (
            cache.get_provider_groups(),
            cache.net_apy_updated_at().map(DateTime::<Utc>::from),
        )
    };
    let page = page_groups(groups, &config);

    Ok(warp::reply::with_status(
        json(&ResponseProviders {
            providers: page.groups,
            total_count: page.total_count,
            total_activated_stake: page.total_activated_stake,
            current_epoch: page.current_epoch,
            net_apy_updated_at,
        }),
        StatusCode::OK,
    ))
}
