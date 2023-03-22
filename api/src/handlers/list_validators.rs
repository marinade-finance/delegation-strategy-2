use crate::context::WrappedContext;
use crate::metrics;
use crate::utils::response_error_500;
use log::error;
use rust_decimal::prelude::*;
use serde::{Deserialize, Serialize};
use solana_program::vote;
use store::{
    dto::{ValidatorRecord, ValidatorsAggregated},
    utils::to_fixed_for_sort,
};
use warp::{http::StatusCode, reply::json, Reply};

const DEFAULT_EPOCHS: usize = 15;
const MAX_LIMIT: usize = 5000;
const DEFAULT_LIMIT: usize = 100;
const DEFAULT_ORDER_FIELD: OrderField = OrderField::Stake;
const DEFAULT_ORDER_DIRECTION: OrderDirection = OrderDirection::DESC;

#[derive(Serialize, Debug)]
pub struct Response {
    validators: Vec<ValidatorRecord>,
    validators_aggregated: Vec<ValidatorsAggregated>,
}

#[derive(Deserialize, Serialize, Debug)]
pub struct QueryParams {
    epochs: Option<usize>,
    query: Option<String>,
    query_vote_accounts: Option<String>,
    order_field: Option<OrderField>,
    order_direction: Option<OrderDirection>,
    query_superminority: Option<bool>,
    query_score: Option<bool>,
    query_marinade_stake: Option<bool>,
    query_with_names: Option<bool>,
    offset: Option<usize>,
    limit: Option<usize>,
}

#[derive(Deserialize, Serialize, Debug)]
pub enum OrderField {
    Stake,
    MndeVotes,
    Credits,
    MarinadeScore,
    Apy,
    Commission,
    Uptime,
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
    pub query_vote_accounts: Option<Vec<String>>,
    pub query_superminority: Option<bool>,
    pub query_score: Option<bool>,
    pub query_marinade_stake: Option<bool>,
    pub query_with_names: Option<bool>,
    pub epochs: usize,
}

pub async fn get_validators(
    context: WrappedContext,
    config: GetValidatorsConfig,
) -> anyhow::Result<Vec<ValidatorRecord>> {
    let validators = context.read().await.cache.get_validators();

    let validators: Vec<_> = if let Some(vote_accounts) = config.query_vote_accounts {
        vote_accounts
            .iter()
            .filter_map(|i| validators.get(i))
            .collect()
    } else {
        validators.values().collect()
    };

    let validators: Vec<_> = if let Some(query) = config.query {
        let query = query.to_lowercase();
        validators
            .into_iter()
            .filter(|v| {
                v.vote_account.to_lowercase().find(&query).is_some()
                    || v.vote_account.to_lowercase().find(&query).is_some()
                    || v.info_name.clone().map_or(false, |info_name| {
                        info_name.to_lowercase().find(&query).is_some()
                    })
            })
            .collect()
    } else {
        validators
    };

    let validators: Vec<_> = if let Some(query_superminority) = config.query_superminority {
        validators
            .into_iter()
            .filter(|v| v.superminority == query_superminority)
            .collect()
    } else {
        validators
    };

    let validators: Vec<_> = if let Some(query_marinade_stake) = config.query_marinade_stake {
        validators
            .into_iter()
            .filter(|v| (v.marinade_stake > Decimal::from(0)) == query_marinade_stake)
            .collect()
    } else {
        validators
    };

    let validators: Vec<_> = if let Some(query_with_names) = config.query_with_names {
        validators
            .into_iter()
            .filter(|v| query_with_names == v.info_name.is_some())
            .collect()
    } else {
        validators
    };

    let mut validators: Vec<_> = if let Some(query_score) = config.query_score {
        validators
            .into_iter()
            .filter(|v| (v.score.unwrap_or(0.0) > 0.0) == query_score)
            .collect()
    } else {
        validators
    };

    let field_extractor = match config.order_field {
        OrderField::Stake => |a: &&ValidatorRecord| a.activated_stake,
        OrderField::MndeVotes => |a: &&ValidatorRecord| a.mnde_votes.unwrap_or(0.into()),
        OrderField::Credits => |a: &&ValidatorRecord| Decimal::from(a.credits),
        OrderField::MarinadeScore => {
            |a: &&ValidatorRecord| Decimal::from(to_fixed_for_sort(a.score.unwrap_or(0.0)))
        }
        OrderField::Apy => {
            |a: &&ValidatorRecord| Decimal::from(to_fixed_for_sort(a.avg_apy.unwrap_or(0.0)))
        }
        OrderField::Commission => {
            |a: &&ValidatorRecord| Decimal::from(a.commission_max_observed.unwrap_or(100))
        }
        OrderField::Uptime => {
            |a: &&ValidatorRecord| Decimal::from(to_fixed_for_sort(a.avg_uptime_pct.unwrap_or(0.0)))
        }
    };

    validators.sort_by(
        |a: &&ValidatorRecord, b: &&ValidatorRecord| match config.order_direction {
            OrderDirection::ASC => field_extractor(a).cmp(&field_extractor(b)),
            OrderDirection::DESC => field_extractor(b).cmp(&field_extractor(a)),
        },
    );

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
        query_vote_accounts: query_params.query_vote_accounts.map(|i| {
            i.split(",")
                .map(|vote_account| vote_account.to_string())
                .collect()
        }),
        query_superminority: query_params.query_superminority,
        query_score: query_params.query_score,
        query_marinade_stake: query_params.query_marinade_stake,
        query_with_names: query_params.query_with_names,
        epochs: query_params.epochs.unwrap_or(DEFAULT_EPOCHS),
    };

    log::info!("Query validators {:?}", config);

    let validators = get_validators(context.clone(), config).await;

    let validators_aggregated = context
        .read()
        .await
        .cache
        .get_validators_aggregated()
        .iter()
        .take(query_params.epochs.unwrap_or(DEFAULT_EPOCHS))
        .cloned()
        .collect();

    Ok(match validators {
        Ok(validators) => warp::reply::with_status(
            json(&Response {
                validators,
                validators_aggregated,
            }),
            StatusCode::OK,
        ),
        Err(err) => {
            error!("Failed to fetch validator records: {}", err);
            response_error_500("Failed to fetch records!".into())
        }
    })
}
