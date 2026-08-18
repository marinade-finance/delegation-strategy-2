use crate::context::WrappedContext;
use crate::handlers::list_validators::OrderDirection;
use crate::handlers::validator_groups::{
    page_tree, GetGroupsConfig, GroupOrderField, DEFAULT_LIMIT, DEFAULT_ORDER_DIRECTION,
    DEFAULT_ORDER_FIELD,
};
use crate::metrics;
use chrono::{DateTime, Utc};
use rust_decimal::prelude::*;
use serde::{Deserialize, Serialize};
use store::dto::ValidatorGroupNode;
use warp::{http::StatusCode, reply::json, Reply};

/// Clients as a two-level tree: one row per client, its block engines underneath.
#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct ResponseClients {
    clients: Vec<ValidatorGroupNode>,
    total_count: usize,
    total_activated_stake: Decimal,
    current_epoch: Option<u64>,
    net_apy_updated_at: Option<DateTime<Utc>>,
}

#[derive(Deserialize, Serialize, Debug, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct QueryParams {
    /// Case-insensitive text search. A client is kept when its own name matches or one of its
    /// block engines does, and its block engines are then served whole.
    query: Option<String>,
    /// Orders the clients, and the block engines inside each one by the same column.
    order_field: Option<GroupOrderField>,
    order_direction: Option<OrderDirection>,
    /// Applies to the clients only; a client always arrives with all of its block engines.
    offset: Option<usize>,
    limit: Option<usize>,
}

#[utoipa::path(
    get,
    tag = "Validators",
    operation_id = "List clients",
    path = "/clients",
    params(QueryParams),
    responses(
        (status = 200, body = ResponseClients)
    )
)]
pub async fn handler(
    query_params: QueryParams,
    context: WrappedContext,
) -> Result<impl Reply, warp::Rejection> {
    metrics::REQUEST_COUNT_CLIENTS.inc();

    let config = GetGroupsConfig {
        order_field: query_params.order_field.unwrap_or(DEFAULT_ORDER_FIELD),
        order_direction: query_params
            .order_direction
            .unwrap_or(DEFAULT_ORDER_DIRECTION),
        offset: query_params.offset.unwrap_or(0),
        limit: query_params.limit.unwrap_or(DEFAULT_LIMIT),
        query: query_params.query,
    };

    log::info!("Query clients {config:?}");

    let (tree, net_apy_updated_at) = {
        let cache = &context.read().await.cache;
        (
            cache.get_client_groups(),
            cache.net_apy_updated_at().map(DateTime::<Utc>::from),
        )
    };
    let page = page_tree(tree, &config);

    Ok(warp::reply::with_status(
        json(&ResponseClients {
            clients: page.nodes,
            total_count: page.total_count,
            total_activated_stake: page.total_activated_stake,
            current_epoch: page.current_epoch,
            net_apy_updated_at,
        }),
        StatusCode::OK,
    ))
}
