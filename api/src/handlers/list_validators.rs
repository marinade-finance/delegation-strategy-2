use std::collections::HashMap;

use crate::context::WrappedContext;
use crate::metrics;
use crate::utils::response_error_500;
use chrono::{DateTime, Utc};
use log::error;
use rust_decimal::prelude::*;
use serde::{Deserialize, Serialize};
use store::{
    dto::{ValidatorRecord, ValidatorsAggregated},
    utils::to_fixed_for_sort,
};
use warp::{http::StatusCode, reply::json, Reply};

const MIN_REQUIRED_EPOCHS_IN_THE_PAST: u64 = 1;
const MIN_REQUIRED_EPOCHS_WITH_CREDITS_OR_STAKE: u64 = 1;
const DEFAULT_EPOCHS: usize = 15;
const DEFAULT_LIMIT: usize = 100;
const DEFAULT_ORDER_FIELD: OrderField = OrderField::Stake;
const DEFAULT_ORDER_DIRECTION: OrderDirection = OrderDirection::DESC;

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct ResponseValidators {
    validators: Vec<ValidatorRecord>,
    validators_aggregated: Vec<ValidatorsAggregated>,
}

#[derive(Deserialize, Serialize, Debug, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct QueryParams {
    epochs: Option<usize>,
    query: Option<String>,
    query_from_date: Option<DateTime<Utc>>,
    query_vote_accounts: Option<String>,
    query_identities: Option<String>,
    order_field: Option<OrderField>,
    order_direction: Option<OrderDirection>,
    query_superminority: Option<bool>,
    query_score: Option<bool>,
    query_marinade_stake: Option<bool>,
    query_with_names: Option<bool>,
    query_sfdp: Option<bool>,
    offset: Option<usize>,
    limit: Option<usize>,
}

#[derive(Deserialize, Serialize, Debug, utoipa::ToSchema)]
pub enum OrderField {
    Stake,
    Credits,
    MarinadeScore,
    Apy,
    Commission,
    Uptime,
}

#[derive(Deserialize, Serialize, Debug, utoipa::ToSchema)]
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
    pub query_vote_accounts: Option<Vec<String>>,
    pub query_superminority: Option<bool>,
    pub query_score: Option<bool>,
    pub query_marinade_stake: Option<bool>,
    pub query_with_names: Option<bool>,
    pub query_sfdp: Option<bool>,
    pub query_from_date: Option<DateTime<Utc>>,
    pub epochs: usize,
}

pub async fn get_validators(
    context: WrappedContext,
    config: GetValidatorsConfig,
) -> anyhow::Result<Vec<ValidatorRecord>> {
    let validators = context.read().await.cache.get_validators();

    let mut validators = filter_validators(validators, &config);

    let field_extractor = get_field_extractor(config.order_field);

    validators.sort_by(
        |a: &ValidatorRecord, b: &ValidatorRecord| match config.order_direction {
            OrderDirection::ASC => field_extractor(a).cmp(&field_extractor(b)),
            OrderDirection::DESC => field_extractor(b).cmp(&field_extractor(a)),
        },
    );
    let max_epoch = validators
        .iter()
        .flat_map(|validator| &validator.epoch_stats)
        .map(|epoch_stat| epoch_stat.epoch)
        .max()
        .unwrap_or(0);
    let min_epoch = (max_epoch + 1).saturating_sub(config.epochs as u64);

    Ok(validators
        .into_iter()
        .skip(config.offset)
        .take(config.limit)
        .map(|mut v| {
            v.epoch_stats = match config.query_from_date {
                Some(from_date) => v
                    .epoch_stats
                    .into_iter()
                    .filter(|es| es.epoch_start_at.is_some())
                    .filter(|es| es.epoch_start_at.unwrap() > from_date)
                    .collect(),
                None => v
                    .epoch_stats
                    .into_iter()
                    .filter(|es| es.epoch >= min_epoch)
                    .collect(),
            };

            v
        })
        .collect())
}

fn get_field_extractor(order_field: OrderField) -> Box<dyn Fn(&ValidatorRecord) -> Decimal> {
    match order_field {
        OrderField::Stake => Box::new(|a: &ValidatorRecord| a.activated_stake),
        OrderField::Credits => Box::new(|a: &ValidatorRecord| Decimal::from(a.credits)),
        OrderField::MarinadeScore => {
            Box::new(|a: &ValidatorRecord| Decimal::from(to_fixed_for_sort(a.score.unwrap_or(0.0))))
        }
        OrderField::Apy => Box::new(|a: &ValidatorRecord| {
            Decimal::from(to_fixed_for_sort(a.avg_apy.unwrap_or(0.0)))
        }),
        OrderField::Commission => {
            Box::new(|a: &ValidatorRecord| Decimal::from(a.commission_max_observed.unwrap_or(100)))
        }
        OrderField::Uptime => Box::new(|a: &ValidatorRecord| {
            Decimal::from(to_fixed_for_sort(a.avg_uptime_pct.unwrap_or(0.0)))
        }),
    }
}

pub fn filter_validators(
    mut validators: HashMap<String, ValidatorRecord>,
    config: &GetValidatorsConfig,
) -> Vec<ValidatorRecord> {
    let last_epoch = validators
        .values()
        .flat_map(|validator| &validator.epoch_stats)
        .map(|epoch_stat| epoch_stat.epoch)
        .max()
        .unwrap_or(0);

    let min_required_epoch = last_epoch.saturating_sub(MIN_REQUIRED_EPOCHS_IN_THE_PAST);
    let last_epochs_with_credits_or_stake_start =
        last_epoch.saturating_sub(MIN_REQUIRED_EPOCHS_WITH_CREDITS_OR_STAKE);

    validators.retain(|_, validator| {
        // Check that validator has stats for the last 2 epochs including last
        if !(min_required_epoch..=last_epoch).all(|epoch| {
            validator
                .epoch_stats
                .iter()
                .any(|epoch_stat| epoch_stat.epoch == epoch)
        }) {
            return false;
        }
        // Check that validator has credits or has active stake in the last 2 epochs including last
        (last_epochs_with_credits_or_stake_start..=last_epoch).all(|epoch| {
            validator
                .epoch_stats
                .iter()
                .find(|&epoch_stat| epoch_stat.epoch == epoch)
                .is_some_and(|epoch_stat| {
                    epoch_stat.activated_stake > Decimal::from(0) || epoch_stat.credits > 0
                })
        })
    });

    if config.query_sfdp.is_some() {
        validators.retain(|_, validator| validator.foundation_stake.gt(&Decimal::ZERO))
    }

    if let Some(vote_accounts) = &config.query_vote_accounts {
        validators.retain(|key, _| vote_accounts.contains(key));
    }

    if let Some(identities) = &config.query_identities {
        validators.retain(|_, v| identities.contains(&v.identity));
    }

    if let Some(query) = &config.query {
        let query = query.to_lowercase();
        validators.retain(|_, v| {
            v.vote_account.to_lowercase().contains(&query)
                || v.identity.to_lowercase().contains(&query)
                || v.info_name
                    .clone()
                    .is_some_and(|info_name| info_name.to_lowercase().contains(&query))
        });
    }

    if let Some(query_superminority) = config.query_superminority {
        validators.retain(|_, v| v.superminority == query_superminority);
    }

    if let Some(query_marinade_stake) = config.query_marinade_stake {
        validators.retain(|_, v| (v.marinade_stake > Decimal::from(0)) == query_marinade_stake);
    }

    if let Some(query_with_names) = config.query_with_names {
        validators.retain(|_, v| query_with_names == v.info_name.is_some());
    }

    if let Some(query_score) = config.query_score {
        validators.retain(|_, v| (v.score.unwrap_or(0.0) > 0.0) == query_score);
    }

    validators.into_values().collect()
}

#[utoipa::path(
    get,
    tag = "Validators",
    operation_id = "List validators",
    path = "/validators",
    params(QueryParams),
    responses(
        (status = 200, body = ResponseValidators)
    )
)]
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
        query_identities: query_params
            .query_identities
            .map(|i| i.split(",").map(|identity| identity.to_string()).collect()),
        query_superminority: query_params.query_superminority,
        query_score: query_params.query_score,
        query_marinade_stake: query_params.query_marinade_stake,
        query_with_names: query_params.query_with_names,
        query_sfdp: query_params.query_sfdp,
        query_from_date: query_params.query_from_date,
        epochs: query_params.epochs.unwrap_or(DEFAULT_EPOCHS),
    };

    log::info!("Query validators {config:?}");

    let validators = get_validators(context.clone(), config).await;

    let mut validators_aggregated = context.read().await.cache.get_validators_aggregated();

    if let Some(from_date) = query_params.query_from_date {
        validators_aggregated = validators_aggregated
            .iter()
            .filter(|v| v.epoch_start_date.is_some())
            .filter(|v| v.epoch_start_date.unwrap() > from_date)
            .cloned()
            .collect();
    } else {
        validators_aggregated = validators_aggregated
            .iter()
            .take(query_params.epochs.unwrap_or(DEFAULT_EPOCHS))
            .cloned()
            .collect();
    }

    Ok(match validators {
        Ok(validators) => warp::reply::with_status(
            json(&ResponseValidators {
                validators,
                validators_aggregated,
            }),
            StatusCode::OK,
        ),
        Err(err) => {
            error!("Failed to fetch validator records: {err}");
            response_error_500("Failed to fetch records!".into())
        }
    })
}
