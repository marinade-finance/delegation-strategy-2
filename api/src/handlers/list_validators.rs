use crate::cache::{GetValidatorsConfig, OrderDirection, OrderField};
use crate::context::WrappedContext;
use crate::utils::reponse_error_500;
use log::error;
use serde::{Deserialize, Serialize};
use store::dto::ValidatorRecord;
use warp::{http::StatusCode, reply::json, Reply};

const MAX_LIMIT: usize = 5000;
const DEFAULT_LIMIT: usize = 100;
const DEFAULT_ORDER_FIELD: OrderField = OrderField::Stake;
const DEFAULT_ORDER_DIRECTION: OrderDirection = OrderDirection::DESC;

#[derive(Serialize, Debug)]
pub struct Response {
    validators: Vec<ValidatorRecord>,
}

#[derive(Deserialize, Serialize, Debug)]
pub struct QueryParams {
    query: Option<String>,
    query_identities: Option<String>,
    order_field: Option<OrderField>,
    order_direction: Option<OrderDirection>,
    offset: Option<usize>,
    limit: Option<usize>,
}

pub async fn handler(
    query_params: QueryParams,
    context: WrappedContext,
) -> Result<impl Reply, warp::Rejection> {
    let config = GetValidatorsConfig {
        order_direction: query_params
            .order_direction
            .unwrap_or(DEFAULT_ORDER_DIRECTION),
        order_field: query_params.order_field.unwrap_or(DEFAULT_ORDER_FIELD),
        offset: query_params.offset.unwrap_or(0),
        limit: query_params.limit.unwrap_or(DEFAULT_LIMIT),
        query: query_params.query,
        query_identities: query_params
            .query_identities
            .map(|i| i.split(",").map(|identity| identity.to_string()).collect()),
        epochs: 15,
    };

    log::info!("Query validators {:?}", config);

    let validators = context.read().await.cache.get_validators(config).await;

    Ok(match validators {
        Ok(validators) => warp::reply::with_status(json(&Response { validators }), StatusCode::OK),
        Err(err) => {
            error!("Failed to fetch validator records: {}", err);
            reponse_error_500("Failed to fetch records!".into())
        }
    })
}
