use crate::context::WrappedContext;
use crate::metrics;
use crate::utils::reponse_error_500;
use log::error;
use serde::{Deserialize, Serialize};
use store::dto::ValidatorRecord;
use warp::{http::StatusCode, reply::json, Reply};

const DEFAULT_EPOCHS: usize = 15;
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
    epochs: Option<usize>,
    query: Option<String>,
    query_identities: Option<String>,
    order_field: Option<OrderField>,
    order_direction: Option<OrderDirection>,
    offset: Option<usize>,
    limit: Option<usize>,
}

#[derive(Deserialize, Serialize, Debug)]
pub enum OrderField {
    Stake,
    MndeVotes,
    Credits,
}

#[derive(Deserialize, Serialize, Debug)]
pub enum OrderDirection {
    ASC,
    DESC,
}

#[derive(Debug)]
pub struct GetValidatorsConfig {
    pub order_direction: OrderDirection,
    pub order_field: OrderField,
    pub offset: usize,
    pub limit: usize,
    pub query: Option<String>,
    pub query_identities: Option<Vec<String>>,
    pub epochs: usize,
}

pub async fn get_validators(
    context: WrappedContext,
    config: GetValidatorsConfig,
) -> anyhow::Result<Vec<ValidatorRecord>> {
    let validators = context.read().await.cache.get_validators();

    let validators: Vec<_> = if let Some(identities) = config.query_identities {
        identities
            .iter()
            .filter_map(|i| validators.get(i))
            .collect()
    } else {
        validators.values().collect()
    };

    let mut validators: Vec<_> = if let Some(query) = config.query {
        let query = query.to_lowercase();
        validators
            .into_iter()
            .filter(|v| {
                v.identity.to_lowercase().find(&query).is_some()
                    || v.vote_account.to_lowercase().find(&query).is_some()
                    || v.info_name.clone().map_or(false, |info_name| {
                        info_name.to_lowercase().find(&query).is_some()
                    })
            })
            .collect()
    } else {
        validators
    };

    let order_fn = match (config.order_field, config.order_direction) {
        (OrderField::Stake, OrderDirection::ASC) => {
            |a: &&ValidatorRecord, b: &&ValidatorRecord| a.activated_stake.cmp(&b.activated_stake)
        }
        (OrderField::Stake, OrderDirection::DESC) => {
            |a: &&ValidatorRecord, b: &&ValidatorRecord| b.activated_stake.cmp(&a.activated_stake)
        }
        (OrderField::MndeVotes, OrderDirection::ASC) => {
            |a: &&ValidatorRecord, b: &&ValidatorRecord| a.mnde_votes.cmp(&b.mnde_votes)
        }
        (OrderField::MndeVotes, OrderDirection::DESC) => {
            |a: &&ValidatorRecord, b: &&ValidatorRecord| b.mnde_votes.cmp(&a.mnde_votes)
        }
        (OrderField::Credits, OrderDirection::ASC) => {
            |a: &&ValidatorRecord, b: &&ValidatorRecord| a.credits.cmp(&b.credits)
        }
        (OrderField::Credits, OrderDirection::DESC) => {
            |a: &&ValidatorRecord, b: &&ValidatorRecord| b.credits.cmp(&a.credits)
        }
    };

    validators.sort_by(order_fn);

    Ok(validators
        .into_iter()
        .skip(config.offset)
        .take(config.limit)
        .cloned()
        .map(|v| ValidatorRecord {
            epoch_stats: v.epoch_stats.into_iter().take(config.epochs).collect(),
            ..v
        })
        .collect())
}

pub async fn handler(
    query_params: QueryParams,
    context: WrappedContext,
) -> Result<impl Reply, warp::Rejection> {
    metrics::REQUEST_COUNT_VALIDATORS.inc();
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
        epochs: query_params.epochs.unwrap_or(DEFAULT_EPOCHS),
    };

    log::info!("Query validators {:?}", config);

    let validators = get_validators(context, config).await;

    Ok(match validators {
        Ok(validators) => warp::reply::with_status(json(&Response { validators }), StatusCode::OK),
        Err(err) => {
            error!("Failed to fetch validator records: {}", err);
            reponse_error_500("Failed to fetch records!".into())
        }
    })
}
