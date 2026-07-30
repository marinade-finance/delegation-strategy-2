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
    /// Number of validators matching the query and filters, before `offset`/`limit`.
    total_count: usize,
}

#[derive(Deserialize, Serialize, Debug, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct QueryParams {
    epochs: Option<usize>,
    /// Text search over validator name, vote account and identity. To also search other
    /// properties (datacenter location), set `search_properties=true`.
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
    query_incident_free: Option<bool>,
    query_verified: Option<bool>,
    query_flagged: Option<bool>,
    /// When true, `query` also matches datacenter location fields (country, city) in addition to
    /// validator name, vote account and identity.
    search_properties: Option<bool>,
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
    TakeRate,
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
    pub query_incident_free: Option<bool>,
    pub query_verified: Option<bool>,
    pub query_flagged: Option<bool>,
    pub search_properties: Option<bool>,
    pub query_from_date: Option<DateTime<Utc>>,
    pub epochs: usize,
}

pub async fn get_validators(
    context: WrappedContext,
    config: GetValidatorsConfig,
) -> anyhow::Result<(Vec<ValidatorRecord>, usize)> {
    let validators = context.read().await.cache.get_validators();

    let mut validators = filter_validators(validators, &config);
    let total_count = validators.len();

    sort_validators(&mut validators, config.order_field, &config.order_direction);
    let max_epoch = validators
        .iter()
        .flat_map(|validator| &validator.epoch_stats)
        .map(|epoch_stat| epoch_stat.epoch)
        .max()
        .unwrap_or(0);
    let min_epoch = (max_epoch + 1).saturating_sub(config.epochs as u64);

    let page = validators
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
        .collect();

    Ok((page, total_count))
}

// Tiebreak on vote_account: ties inherit HashMap iteration order otherwise, which changes
// on every cache refresh and makes offset pages overlap or skip rows.
fn sort_validators(
    validators: &mut [ValidatorRecord],
    order_field: OrderField,
    order_direction: &OrderDirection,
) {
    let field_extractor = get_field_extractor(order_field);
    validators.sort_by(|a: &ValidatorRecord, b: &ValidatorRecord| {
        let ord = match order_direction {
            OrderDirection::ASC => field_extractor(a).cmp(&field_extractor(b)),
            OrderDirection::DESC => field_extractor(b).cmp(&field_extractor(a)),
        };
        ord.then_with(|| a.vote_account.cmp(&b.vote_account))
    });
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
        OrderField::TakeRate => Box::new(|a: &ValidatorRecord| {
            Decimal::from(to_fixed_for_sort(a.avg_take_rate.unwrap_or(0.0)))
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
        let search_properties = config.search_properties.unwrap_or(false);
        validators.retain(|_, v| {
            let matches = |field: &Option<String>| {
                field
                    .as_ref()
                    .is_some_and(|s| s.to_lowercase().contains(&query))
            };
            v.vote_account.to_lowercase().contains(&query)
                || v.identity.to_lowercase().contains(&query)
                || matches(&v.info_name)
                || (search_properties
                    && (matches(&v.dc_country) || matches(&v.dc_city) || matches(&v.dc_full_city)))
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

    if let Some(query_incident_free) = config.query_incident_free {
        validators.retain(|_, v| v.incidents.is_empty() == query_incident_free);
    }

    if let Some(query_verified) = config.query_verified {
        validators.retain(|_, v| v.verified == query_verified);
    }

    if let Some(query_flagged) = config.query_flagged {
        validators.retain(|_, v| v.warnings.is_empty() != query_flagged);
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
        query_incident_free: query_params.query_incident_free,
        query_verified: query_params.query_verified,
        query_flagged: query_params.query_flagged,
        search_properties: query_params.search_properties,
        query_from_date: query_params.query_from_date,
        epochs: query_params.epochs.unwrap_or(DEFAULT_EPOCHS),
    };

    log::info!("Query validators {config:?}");

    let validators = get_validators(context.clone(), config).await;

    Ok(match validators {
        Ok((validators, total_count)) => {
            let validators_aggregated = store::utils::aggregate_validators(&validators);
            warp::reply::with_status(
                json(&ResponseValidators {
                    validators,
                    validators_aggregated,
                    total_count,
                }),
                StatusCode::OK,
            )
        }
        Err(err) => {
            error!("Failed to fetch validator records: {err}");
            response_error_500("Failed to fetch records!".into())
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use store::dto::{ValidatorEpochStats, ValidatorWarning};

    fn epoch_stat(epoch: u64, stake: i64) -> ValidatorEpochStats {
        ValidatorEpochStats {
            epoch,
            epoch_start_at: None,
            epoch_end_at: None,
            commission_max_observed: None,
            commission_min_observed: None,
            commission_advertised: None,
            commission_effective: None,
            version: None,
            mev_commission_bps: None,
            priority_commission_bps: None,
            dc_asn: None,
            dc_aso: None,
            dc_city: None,
            dc_country: None,
            client_id: None,
            client_vendor: None,
            client_lineage: None,
            feature_set: None,
            shred_version: None,
            gossip_port: None,
            rpc_public: None,
            pubsub_public: None,
            activated_stake: Decimal::from(stake),
            marinade_stake: Decimal::ZERO,
            foundation_stake: Decimal::ZERO,
            marinade_native_stake: Decimal::ZERO,
            institutional_stake: Decimal::ZERO,
            self_stake: Decimal::ZERO,
            superminority: false,
            stake_to_become_superminority: Decimal::ZERO,
            credits: 1,
            leader_slots: 0,
            blocks_produced: 0,
            skip_rate: 0.0,
            uptime_pct: None,
            uptime: None,
            downtime: None,
            apr: None,
            apy: None,
            score: None,
            rank_score: None,
            rank_activated_stake: None,
            rank_apy: None,
        }
    }

    fn validator(
        vote_account: &str,
        stake: i64,
        warnings: Vec<ValidatorWarning>,
    ) -> ValidatorRecord {
        ValidatorRecord {
            identity: format!("id-{vote_account}"),
            vote_account: vote_account.to_string(),
            start_epoch: 99,
            start_date: None,
            info_name: None,
            info_url: None,
            info_keybase: None,
            info_icon_url: None,
            node_ip: None,
            dc_coordinates_lat: None,
            dc_coordinates_lon: None,
            dc_continent: None,
            dc_country_iso: None,
            dc_country: None,
            dc_city: None,
            dc_full_city: None,
            dc_asn: None,
            dc_aso: None,
            dcc_full_city: None,
            dcc_asn: None,
            dcc_aso: None,
            dcc_country: None,
            commission_max_observed: None,
            commission_min_observed: None,
            commission_advertised: None,
            commission_effective: None,
            commission_aggregated: None,
            rugged_commission_occurrences: 0,
            rugged_commission: false,
            rugged_commission_info: Vec::new(),
            version: None,
            client_id: None,
            client_vendor: None,
            client_lineage: None,
            feature_set: None,
            shred_version: None,
            gossip_port: None,
            rpc_public: None,
            pubsub_public: None,
            activated_stake: Decimal::from(stake),
            marinade_stake: Decimal::ZERO,
            foundation_stake: Decimal::ZERO,
            marinade_native_stake: Decimal::ZERO,
            institutional_stake: Decimal::ZERO,
            self_stake: Decimal::ZERO,
            superminority: false,
            credits: 1,
            score: None,
            warnings,
            epoch_stats: vec![epoch_stat(99, stake), epoch_stat(100, stake)],
            epochs_count: 2,
            has_last_epoch_stats: true,
            avg_uptime_pct: None,
            avg_apy: None,
            unique_delegators: None,
            avg_take_rate: None,
            incidents: Vec::new(),
            verified: false,
        }
    }

    fn config() -> GetValidatorsConfig {
        GetValidatorsConfig {
            order_direction: OrderDirection::DESC,
            order_field: OrderField::Stake,
            offset: 0,
            limit: 100,
            query: None,
            query_identities: None,
            query_vote_accounts: None,
            query_superminority: None,
            query_score: None,
            query_marinade_stake: None,
            query_with_names: None,
            query_sfdp: None,
            query_incident_free: None,
            query_verified: None,
            query_flagged: None,
            search_properties: None,
            query_from_date: None,
            epochs: 15,
        }
    }

    fn map(validators: Vec<ValidatorRecord>) -> HashMap<String, ValidatorRecord> {
        validators
            .into_iter()
            .map(|v| (v.vote_account.clone(), v))
            .collect()
    }

    fn vote_accounts(mut validators: Vec<ValidatorRecord>) -> Vec<String> {
        validators.sort_by(|a, b| a.vote_account.cmp(&b.vote_account));
        validators.into_iter().map(|v| v.vote_account).collect()
    }

    #[test]
    fn query_flagged_true_keeps_only_validators_with_warnings() {
        let validators = map(vec![
            validator("flagged", 100, vec![ValidatorWarning::HighCommission]),
            validator("clean", 100, vec![]),
        ]);
        let config = GetValidatorsConfig {
            query_flagged: Some(true),
            ..config()
        };
        assert_eq!(
            vote_accounts(filter_validators(validators, &config)),
            vec!["flagged".to_string()]
        );
    }

    #[test]
    fn query_flagged_false_keeps_only_validators_without_warnings() {
        let validators = map(vec![
            validator("flagged", 100, vec![ValidatorWarning::LowUptime]),
            validator("clean", 100, vec![]),
        ]);
        let config = GetValidatorsConfig {
            query_flagged: Some(false),
            ..config()
        };
        assert_eq!(
            vote_accounts(filter_validators(validators, &config)),
            vec!["clean".to_string()]
        );
    }

    #[test]
    fn query_flagged_none_keeps_all() {
        let validators = map(vec![
            validator("flagged", 100, vec![ValidatorWarning::Superminority]),
            validator("clean", 100, vec![]),
        ]);
        assert_eq!(filter_validators(validators, &config()).len(), 2);
    }

    #[test]
    fn sort_tiebreaks_on_vote_account_ascending() {
        // Equal stake: order must fall back to vote_account ascending regardless of direction.
        for direction in [OrderDirection::ASC, OrderDirection::DESC] {
            let mut validators = vec![
                validator("ccc", 100, vec![]),
                validator("aaa", 100, vec![]),
                validator("bbb", 100, vec![]),
            ];
            sort_validators(&mut validators, OrderField::Stake, &direction);
            let order: Vec<_> = validators.iter().map(|v| v.vote_account.clone()).collect();
            assert_eq!(order, vec!["aaa", "bbb", "ccc"], "direction {direction:?}");
        }
    }

    #[test]
    fn sort_orders_by_field_before_tiebreak() {
        let mut validators = vec![validator("aaa", 50, vec![]), validator("bbb", 100, vec![])];
        sort_validators(&mut validators, OrderField::Stake, &OrderDirection::DESC);
        let order: Vec<_> = validators.iter().map(|v| v.vote_account.clone()).collect();
        assert_eq!(order, vec!["bbb", "aaa"]);
    }
}
