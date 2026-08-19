use std::cmp::Ordering;
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
    /// When validator-bonds last answered for the `verified`/`protected` flags. Older than a few minutes means the flags are being reused because that API is failing.
    bond_flags_updated_at: Option<DateTime<Utc>>,
    /// When apy-api last answered for `net_apy`. Older than a few minutes means those values are being reused because that API is failing.
    net_apy_updated_at: Option<DateTime<Utc>>,
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
    /// Evaluated over the last 90 epochs of incidents, regardless of `epochs` and `query_from_date`.
    query_incident_free: Option<bool>,
    /// Minimum downtime in seconds for a `DOWN` interval to count as an incident for `query_incident_free`. Shorter intervals are restart noise. Ignored unless `query_incident_free` is set, and never filters the returned `incidents` array.
    min_incident_downtime_seconds: Option<u64>,
    query_verified: Option<bool>,
    query_protected: Option<bool>,
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
    /// Orders by the MEV-inclusive `net_apy`, which is what the validators list renders; `Apy` orders by the inflation-only `avg_apy`.
    NetApy,
    Commission,
    Uptime,
    TakeRate,
    /// Orders by the commission-derived `expected_take_rate`; `TakeRate` orders by the measured `avg_take_rate`.
    ExpectedTakeRate,
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
    pub min_incident_downtime_seconds: Option<u64>,
    pub query_verified: Option<bool>,
    pub query_protected: Option<bool>,
    pub query_flagged: Option<bool>,
    pub search_properties: Option<bool>,
    pub query_from_date: Option<DateTime<Utc>>,
    pub epochs: usize,
}

// One guard for the records and the timestamps: read apart, a warm landing in between makes the timestamps describe values this response does not carry.
pub async fn get_validators(
    context: WrappedContext,
    config: GetValidatorsConfig,
) -> anyhow::Result<(
    Vec<ValidatorRecord>,
    usize,
    Option<DateTime<Utc>>,
    Option<DateTime<Utc>>,
)> {
    let (validators, bond_flags_updated_at, net_apy_updated_at) = {
        let cache = &context.read().await.cache;
        (
            cache.get_validators(),
            cache.bond_flags_updated_at().map(DateTime::<Utc>::from),
            cache.net_apy_updated_at().map(DateTime::<Utc>::from),
        )
    };

    let validators = filter_validators(validators, &config);
    let total_count = validators.len();

    let validators = sort_validators(validators, config.order_field, &config.order_direction);
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

    Ok((page, total_count, bond_flags_updated_at, net_apy_updated_at))
}

// Tiebreak on vote_account: ties inherit HashMap iteration order otherwise, which changes
// on every cache refresh and makes offset pages overlap or skip rows.
fn sort_validators(
    validators: Vec<ValidatorRecord>,
    order_field: OrderField,
    order_direction: &OrderDirection,
) -> Vec<ValidatorRecord> {
    let field_extractor = get_field_extractor(order_field);
    // Keyed up front: sort_by would otherwise re-extract on both sides of every one of n·log n comparisons.
    let mut keyed: Vec<(Option<Decimal>, ValidatorRecord)> = validators
        .into_iter()
        .map(|validator| (field_extractor(&validator), validator))
        .collect();
    keyed.sort_by(|(a_key, a), (b_key, b)| {
        // A missing value is not a zero one, so it stays last whichever way the present ones go.
        let ord = match (a_key, b_key) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
            (Some(x), Some(y)) => match order_direction {
                OrderDirection::ASC => x.cmp(y),
                OrderDirection::DESC => y.cmp(x),
            },
        };
        ord.then_with(|| a.vote_account.cmp(&b.vote_account))
    });
    keyed.into_iter().map(|(_, validator)| validator).collect()
}

// None means the record has no value for the field, not that it has a zero one.
type FieldExtractor = fn(&ValidatorRecord) -> Option<Decimal>;

// Commission and Uptime keep worst-case sentinels: for those two unknown means risk, not no-data.
fn get_field_extractor(order_field: OrderField) -> FieldExtractor {
    match order_field {
        OrderField::Stake => |a: &ValidatorRecord| Some(a.activated_stake),
        OrderField::Credits => |a: &ValidatorRecord| Some(Decimal::from(a.credits)),
        OrderField::MarinadeScore => {
            |a: &ValidatorRecord| a.score.and_then(to_fixed_for_sort).map(Decimal::from)
        }
        // Shares NetApy's `ratio^n - 1` derivation but is computed here instead of served by apy-api, so an unrepresentable value sinks rather than being crowned.
        OrderField::Apy => |a: &ValidatorRecord| {
            a.avg_apy
                .filter(|apy| *apy >= 0.0)
                .and_then(Decimal::from_f64_retain)
        },
        // Deliberately not to_fixed_for_sort: rounding a fraction-valued APY to 4 decimals is what
        // collapses hundreds of validators into one bucket and makes the column look unsorted.
        // Saturating up is safe because apy-api derives this as `ratio^n - 1` from a positive ratio, so an unrepresentable value is always the high end.
        OrderField::NetApy => |a: &ValidatorRecord| {
            a.net_apy
                .map(|net_apy| Decimal::from_f64_retain(net_apy).unwrap_or(Decimal::MAX))
        },
        OrderField::Commission => {
            |a: &ValidatorRecord| Some(Decimal::from(a.commission_max_observed.unwrap_or(100)))
        }
        OrderField::Uptime => |a: &ValidatorRecord| {
            Some(Decimal::from(
                a.avg_uptime_pct.and_then(to_fixed_for_sort).unwrap_or(0),
            ))
        },
        // Same fraction-rounding trap as NetApy, but a degenerate value sinks here rather than saturating up: a take rate has no natural high end to saturate towards.
        OrderField::TakeRate => |a: &ValidatorRecord| {
            a.avg_take_rate
                .filter(|rate| *rate >= 0.0)
                .and_then(Decimal::from_f64_retain)
        },
        OrderField::ExpectedTakeRate => |a: &ValidatorRecord| {
            a.expected_take_rate
                .filter(|rate| *rate >= 0.0)
                .and_then(Decimal::from_f64_retain)
        },
    }
}

pub fn filter_validators(
    mut validators: HashMap<String, ValidatorRecord>,
    config: &GetValidatorsConfig,
) -> Vec<ValidatorRecord> {
    // Shared with the client and provider aggregates, so both describe the same population.
    let last_epoch = store::utils::last_reported_epoch(validators.values()).unwrap_or(0);
    validators.retain(|_, validator| store::utils::is_eligible_validator(validator, last_epoch));

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
        let min_incident_downtime = config.min_incident_downtime_seconds.unwrap_or(0);
        validators.retain(|_, v| {
            let has_incident = v
                .incidents
                .iter()
                .any(|i| i.downtime_seconds >= min_incident_downtime);
            has_incident != query_incident_free
        });
    }

    if let Some(query_verified) = config.query_verified {
        validators.retain(|_, v| v.verified == query_verified);
    }

    if let Some(query_protected) = config.query_protected {
        validators.retain(|_, v| v.protected == query_protected);
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
        min_incident_downtime_seconds: query_params.min_incident_downtime_seconds,
        query_verified: query_params.query_verified,
        query_protected: query_params.query_protected,
        query_flagged: query_params.query_flagged,
        search_properties: query_params.search_properties,
        query_from_date: query_params.query_from_date,
        epochs: query_params.epochs.unwrap_or(DEFAULT_EPOCHS),
    };

    log::info!("Query validators {config:?}");

    let validators = get_validators(context.clone(), config).await;

    Ok(match validators {
        Ok((validators, total_count, bond_flags_updated_at, net_apy_updated_at)) => {
            let validators_aggregated = store::utils::aggregate_validators(&validators);
            warp::reply::with_status(
                json(&ResponseValidators {
                    validators,
                    validators_aggregated,
                    total_count,
                    bond_flags_updated_at,
                    net_apy_updated_at,
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
    use store::dto::{IncidentRecord, ValidatorEpochStats, ValidatorWarning, UNKNOWN_CLIENT_NAME};

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
            client_name: UNKNOWN_CLIENT_NAME.to_string(),
            client_label: UNKNOWN_CLIENT_NAME.to_string(),
            client_vendor: None,
            client_lineage: None,
            client_id_raw: None,
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
            client_name: UNKNOWN_CLIENT_NAME.to_string(),
            client_label: UNKNOWN_CLIENT_NAME.to_string(),
            client_vendor: None,
            client_lineage: None,
            client_id_raw: None,
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
            expected_take_rate: None,
            net_apy: None,
            incidents: Vec::new(),
            verified: false,
            protected: false,
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
            min_incident_downtime_seconds: None,
            query_verified: None,
            query_protected: None,
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

    fn protected_validator(vote_account: &str) -> ValidatorRecord {
        ValidatorRecord {
            protected: true,
            ..validator(vote_account, 100, vec![])
        }
    }

    #[test]
    fn query_protected_true_keeps_only_protected_validators() {
        let validators = map(vec![
            protected_validator("bonded"),
            validator("unbonded", 100, vec![]),
        ]);
        let config = GetValidatorsConfig {
            query_protected: Some(true),
            ..config()
        };
        assert_eq!(
            vote_accounts(filter_validators(validators, &config)),
            vec!["bonded".to_string()]
        );
    }

    #[test]
    fn query_protected_false_keeps_only_unprotected_validators() {
        let validators = map(vec![
            protected_validator("bonded"),
            validator("unbonded", 100, vec![]),
        ]);
        let config = GetValidatorsConfig {
            query_protected: Some(false),
            ..config()
        };
        assert_eq!(
            vote_accounts(filter_validators(validators, &config)),
            vec!["unbonded".to_string()]
        );
    }

    #[test]
    fn query_protected_none_keeps_all() {
        let validators = map(vec![
            protected_validator("bonded"),
            validator("unbonded", 100, vec![]),
        ]);
        assert_eq!(filter_validators(validators, &config()).len(), 2);
    }

    // load_incidents derives downtime_seconds as EXTRACT(epoch FROM end_at - start_at).
    fn incident(downtime_seconds: u64) -> IncidentRecord {
        let end_at = Utc::now();
        IncidentRecord {
            epoch: 100,
            start_at: end_at - chrono::Duration::seconds(downtime_seconds as i64),
            end_at,
            downtime_seconds,
        }
    }

    fn validator_with_incidents(vote_account: &str, downtimes: &[u64]) -> ValidatorRecord {
        ValidatorRecord {
            incidents: downtimes.iter().copied().map(incident).collect(),
            ..validator(vote_account, 100, vec![])
        }
    }

    #[test]
    fn incident_free_without_floor_counts_every_incident() {
        let validators = map(vec![
            validator_with_incidents("blip", &[1]),
            validator_with_incidents("clean", &[]),
        ]);
        let config = GetValidatorsConfig {
            query_incident_free: Some(true),
            ..config()
        };
        assert_eq!(
            vote_accounts(filter_validators(validators, &config)),
            vec!["clean".to_string()]
        );
    }

    #[test]
    fn incident_free_ignores_downtime_below_the_floor() {
        let validators = map(vec![
            validator_with_incidents("blip", &[179]),
            validator_with_incidents("outage", &[180]),
        ]);
        let config = GetValidatorsConfig {
            query_incident_free: Some(true),
            min_incident_downtime_seconds: Some(180),
            ..config()
        };
        assert_eq!(
            vote_accounts(filter_validators(validators, &config)),
            vec!["blip".to_string()]
        );
    }

    #[test]
    fn incident_free_looks_at_every_incident_not_just_the_first() {
        let validators = map(vec![validator_with_incidents("mixed", &[10, 600])]);
        let config = GetValidatorsConfig {
            query_incident_free: Some(true),
            min_incident_downtime_seconds: Some(180),
            ..config()
        };
        assert!(filter_validators(validators, &config).is_empty());
    }

    #[test]
    fn incident_free_false_keeps_only_validators_over_the_floor() {
        let validators = map(vec![
            validator_with_incidents("blip", &[179]),
            validator_with_incidents("outage", &[180]),
            validator_with_incidents("clean", &[]),
        ]);
        let config = GetValidatorsConfig {
            query_incident_free: Some(false),
            min_incident_downtime_seconds: Some(180),
            ..config()
        };
        assert_eq!(
            vote_accounts(filter_validators(validators, &config)),
            vec!["outage".to_string()]
        );
    }

    #[test]
    fn min_incident_downtime_alone_filters_nothing() {
        let validators = map(vec![
            validator_with_incidents("blip", &[179]),
            validator_with_incidents("outage", &[180]),
        ]);
        let config = GetValidatorsConfig {
            min_incident_downtime_seconds: Some(180),
            ..config()
        };
        assert_eq!(filter_validators(validators, &config).len(), 2);
    }

    #[test]
    fn sort_tiebreaks_on_vote_account_ascending() {
        // Equal stake: order must fall back to vote_account ascending regardless of direction.
        for direction in [OrderDirection::ASC, OrderDirection::DESC] {
            let validators = sort_validators(
                vec![
                    validator("ccc", 100, vec![]),
                    validator("aaa", 100, vec![]),
                    validator("bbb", 100, vec![]),
                ],
                OrderField::Stake,
                &direction,
            );
            let order: Vec<_> = validators.iter().map(|v| v.vote_account.clone()).collect();
            assert_eq!(order, vec!["aaa", "bbb", "ccc"], "direction {direction:?}");
        }
    }

    #[test]
    fn sort_orders_by_field_before_tiebreak() {
        let validators = sort_validators(
            vec![validator("aaa", 50, vec![]), validator("bbb", 100, vec![])],
            OrderField::Stake,
            &OrderDirection::DESC,
        );
        let order: Vec<_> = validators.iter().map(|v| v.vote_account.clone()).collect();
        assert_eq!(order, vec!["bbb", "aaa"]);
    }

    type FieldSetter = fn(&mut ValidatorRecord, Option<f64>);

    fn with_field(vote_account: &str, set: FieldSetter, value: Option<f64>) -> ValidatorRecord {
        let mut record = validator(vote_account, 100, vec![]);
        set(&mut record, value);
        record
    }

    fn sinking_fields() -> Vec<(&'static str, OrderField, FieldSetter)> {
        vec![
            ("score", OrderField::MarinadeScore, |r, v| r.score = v),
            ("apy", OrderField::Apy, |r, v| r.avg_apy = v),
            ("take_rate", OrderField::TakeRate, |r, v| {
                r.avg_take_rate = v
            }),
            (
                "expected_take_rate",
                OrderField::ExpectedTakeRate,
                |r, v| r.expected_take_rate = v,
            ),
        ]
    }

    // Fields keyed through Decimal::from_f64_retain rather than to_fixed_for_sort, so they share a degenerate-value and a sub-bucket contract.
    fn precise_fraction_fields() -> Vec<(&'static str, OrderField, FieldSetter)> {
        vec![
            ("apy", OrderField::Apy, |r, v| r.avg_apy = v),
            ("take_rate", OrderField::TakeRate, |r, v| {
                r.avg_take_rate = v
            }),
            (
                "expected_take_rate",
                OrderField::ExpectedTakeRate,
                |r, v| r.expected_take_rate = v,
            ),
        ]
    }

    #[test]
    fn sort_orders_the_two_take_rates_independently() {
        // Measured and expected disagree by design, so one must not be sorting by the other.
        let validators = sort_validators(
            vec![
                ValidatorRecord {
                    avg_take_rate: Some(0.0),
                    expected_take_rate: Some(0.06),
                    ..validator("aaa_idle", 100, vec![])
                },
                ValidatorRecord {
                    avg_take_rate: Some(0.05),
                    expected_take_rate: Some(0.05),
                    ..validator("bbb_busy", 100, vec![])
                },
            ],
            OrderField::ExpectedTakeRate,
            &OrderDirection::ASC,
        );
        let order: Vec<_> = validators.iter().map(|v| v.vote_account.clone()).collect();
        assert_eq!(order, vec!["bbb_busy", "aaa_idle"]);
    }

    fn validator_with_net_apy(vote_account: &str, net_apy: Option<f64>) -> ValidatorRecord {
        ValidatorRecord {
            net_apy,
            ..validator(vote_account, 100, vec![])
        }
    }

    #[test]
    fn sort_sinks_missing_values_in_both_directions() {
        for direction in [OrderDirection::ASC, OrderDirection::DESC] {
            for (label, field, set) in sinking_fields() {
                let validators = sort_validators(
                    vec![
                        with_field("aaa_missing", set, None),
                        with_field("bbb_zero", set, Some(0.0)),
                        with_field("ccc_high", set, Some(0.05)),
                    ],
                    field,
                    &direction,
                );
                let order: Vec<_> = validators.iter().map(|v| v.vote_account.clone()).collect();
                let expected = match direction {
                    OrderDirection::ASC => vec!["bbb_zero", "ccc_high", "aaa_missing"],
                    OrderDirection::DESC => vec!["ccc_high", "bbb_zero", "aaa_missing"],
                };
                assert_eq!(order, expected, "{label} / {direction:?}");
            }
        }
    }

    #[test]
    fn sort_keeps_missing_distinct_from_zero() {
        // Missing used to fold onto 0.0, so an ascending sort opened with rows that have no value.
        for (label, field, set) in sinking_fields() {
            let validators = sort_validators(
                vec![
                    with_field("aaa_missing", set, None),
                    with_field("zzz_zero", set, Some(0.0)),
                ],
                field,
                &OrderDirection::ASC,
            );
            let order: Vec<_> = validators.iter().map(|v| v.vote_account.clone()).collect();
            assert_eq!(order, vec!["zzz_zero", "aaa_missing"], "{label}");
        }
    }

    #[test]
    fn sort_ranks_unknown_commission_as_max() {
        let mut validators = vec![
            validator("aaa_unknown", 100, vec![]),
            validator("bbb_low", 100, vec![]),
            validator("ccc_max", 100, vec![]),
        ];
        validators[1].commission_max_observed = Some(5);
        validators[2].commission_max_observed = Some(100);
        let validators = sort_validators(validators, OrderField::Commission, &OrderDirection::DESC);
        let order: Vec<_> = validators.iter().map(|v| v.vote_account.clone()).collect();
        assert_eq!(order, vec!["aaa_unknown", "ccc_max", "bbb_low"]);
    }

    #[test]
    fn sort_ranks_unknown_uptime_as_zero() {
        let set: FieldSetter = |r, v| r.avg_uptime_pct = v;
        let validators = sort_validators(
            vec![
                with_field("aaa_unknown", set, None),
                with_field("bbb_zero", set, Some(0.0)),
                with_field("ccc_high", set, Some(0.99)),
            ],
            OrderField::Uptime,
            &OrderDirection::ASC,
        );
        let order: Vec<_> = validators.iter().map(|v| v.vote_account.clone()).collect();
        assert_eq!(order, vec!["aaa_unknown", "bbb_zero", "ccc_high"]);
    }

    #[test]
    fn sort_sinks_negative_and_non_finite_values() {
        for degenerate in [-0.01, f64::NAN, f64::INFINITY] {
            for (label, field, set) in precise_fraction_fields() {
                let validators = sort_validators(
                    vec![
                        with_field("aaa_degenerate", set, Some(degenerate)),
                        with_field("zzz_zero", set, Some(0.0)),
                    ],
                    field,
                    &OrderDirection::ASC,
                );
                let order: Vec<_> = validators.iter().map(|v| v.vote_account.clone()).collect();
                assert_eq!(
                    order,
                    vec!["zzz_zero", "aaa_degenerate"],
                    "{label} / {degenerate}"
                );
            }
        }
    }

    #[test]
    fn sort_separates_fraction_fields_differing_below_four_decimals() {
        // Mainnet's actual take-rate collision: 5% inflation commission with 0 bps MEV against the same with 10 bps.
        const LOWER: f64 = 0.13635962633635962;
        const HIGHER: f64 = 0.13638048552938048;
        assert_eq!(
            to_fixed_for_sort(LOWER),
            to_fixed_for_sort(HIGHER),
            "the values have to share a rounding bucket for this test to be about rounding"
        );
        for (label, field, set) in precise_fraction_fields() {
            let validators = sort_validators(
                vec![
                    with_field("aaa_lower", set, Some(LOWER)),
                    with_field("zzz_higher", set, Some(HIGHER)),
                ],
                field,
                &OrderDirection::DESC,
            );
            let order: Vec<_> = validators.iter().map(|v| v.vote_account.clone()).collect();
            assert_eq!(order, vec!["zzz_higher", "aaa_lower"], "{label}");
        }
    }

    #[test]
    fn sort_by_net_apy_separates_values_differing_below_four_decimals() {
        // Both values round to the same 4-decimal bucket, and the higher one is alphabetically last,
        // so rounding would hand back the reverse order instead of ordering by the real value.
        assert_eq!(
            to_fixed_for_sort(0.0712389),
            to_fixed_for_sort(0.0712312),
            "the values have to share a rounding bucket for this test to be about rounding"
        );
        let validators = sort_validators(
            vec![
                validator_with_net_apy("aaa", Some(0.0712312)),
                validator_with_net_apy("zzz", Some(0.0712389)),
            ],
            OrderField::NetApy,
            &OrderDirection::DESC,
        );
        let order: Vec<_> = validators.iter().map(|v| v.vote_account.clone()).collect();
        assert_eq!(order, vec!["zzz", "aaa"]);
    }

    #[test]
    fn sort_by_net_apy_orders_ascending_and_descending() {
        let ordered = |direction| {
            sort_validators(
                vec![
                    validator_with_net_apy("mid", Some(0.07)),
                    validator_with_net_apy("high", Some(0.09)),
                    validator_with_net_apy("low", Some(0.05)),
                ],
                OrderField::NetApy,
                &direction,
            )
            .iter()
            .map(|v| v.vote_account.clone())
            .collect::<Vec<_>>()
        };
        assert_eq!(ordered(OrderDirection::DESC), vec!["high", "mid", "low"]);
        assert_eq!(ordered(OrderDirection::ASC), vec!["low", "mid", "high"]);
    }

    #[test]
    fn sort_by_net_apy_puts_validators_without_a_value_last_when_descending() {
        let validators = sort_validators(
            vec![
                validator_with_net_apy("unknown", None),
                validator_with_net_apy("known", Some(0.07)),
            ],
            OrderField::NetApy,
            &OrderDirection::DESC,
        );
        let order: Vec<_> = validators.iter().map(|v| v.vote_account.clone()).collect();
        assert_eq!(order, vec!["known", "unknown"]);
    }

    #[test]
    fn sort_by_net_apy_puts_validators_without_a_value_last_when_ascending() {
        let validators = sort_validators(
            vec![
                validator_with_net_apy("unknown", None),
                validator_with_net_apy("known", Some(0.07)),
            ],
            OrderField::NetApy,
            &OrderDirection::ASC,
        );
        let order: Vec<_> = validators.iter().map(|v| v.vote_account.clone()).collect();
        assert_eq!(
            order,
            vec!["known", "unknown"],
            "no value must not read as the lowest APY"
        );
    }

    #[test]
    fn sort_by_net_apy_ranks_a_genuine_zero_above_no_value_when_ascending() {
        // The no-value one is alphabetically first, so a collision would hand back the tiebreak order and the assert would not be about the collision at all.
        let validators = sort_validators(
            vec![
                validator_with_net_apy("aaaUnknown", None),
                validator_with_net_apy("zzzZero", Some(0.0)),
            ],
            OrderField::NetApy,
            &OrderDirection::ASC,
        );
        let order: Vec<_> = validators.iter().map(|v| v.vote_account.clone()).collect();
        assert_eq!(
            order,
            vec!["zzzZero", "aaaUnknown"],
            "apy-api serves 0.0 for a full-commission validator and omits one with no comparable rate"
        );
    }

    #[test]
    fn sort_by_net_apy_ranks_a_value_too_large_for_decimal_above_the_ordinary_ones() {
        // apy-api drops only non-finite points, so an extreme bump can still serve a value that
        // Decimal cannot hold; it must not land in the same bucket as a validator with no value.
        assert!(
            Decimal::from_f64_retain(1e30).is_none(),
            "the value has to exceed Decimal's range for this test to be about saturation"
        );
        let validators = sort_validators(
            vec![
                validator_with_net_apy("ordinary", Some(0.07)),
                validator_with_net_apy("absurd", Some(1e30)),
                validator_with_net_apy("unknown", None),
            ],
            OrderField::NetApy,
            &OrderDirection::DESC,
        );
        let order: Vec<_> = validators.iter().map(|v| v.vote_account.clone()).collect();
        assert_eq!(order, vec!["absurd", "ordinary", "unknown"]);
    }

    #[test]
    fn sort_by_net_apy_is_independent_of_the_inflation_only_apy() {
        // avg_apy is inflation-only and orders differently; ordering by NetApy must ignore it.
        let validators = sort_validators(
            vec![
                ValidatorRecord {
                    avg_apy: Some(0.08),
                    ..validator_with_net_apy("lowNet", Some(0.05))
                },
                ValidatorRecord {
                    avg_apy: Some(0.04),
                    ..validator_with_net_apy("highNet", Some(0.11))
                },
            ],
            OrderField::NetApy,
            &OrderDirection::DESC,
        );
        let order: Vec<_> = validators.iter().map(|v| v.vote_account.clone()).collect();
        assert_eq!(order, vec!["highNet", "lowNet"]);
    }

    fn validator_with_epoch_apy(vote_account: &str, apy: Option<f64>) -> ValidatorRecord {
        ValidatorRecord {
            epoch_stats: vec![ValidatorEpochStats {
                apy,
                ..epoch_stat(100, 100)
            }],
            ..validator(vote_account, 100, vec![])
        }
    }

    fn apy_ranks(validators: Vec<ValidatorRecord>) -> Vec<(String, Option<usize>)> {
        let mut by_vote_account: HashMap<_, _> = validators
            .into_iter()
            .map(|v| (v.vote_account.clone(), v))
            .collect();
        store::utils::update_validators_ranks(
            &mut by_vote_account,
            |a: &ValidatorEpochStats| {
                a.apy
                    .filter(|apy| *apy >= 0.0)
                    .and_then(Decimal::from_f64_retain)
            },
            |a: &mut ValidatorEpochStats, rank: usize| a.rank_apy = Some(rank),
        );
        let mut ranks: Vec<_> = by_vote_account
            .into_iter()
            .map(|(vote_account, v)| (vote_account, v.epoch_stats[0].rank_apy))
            .collect();
        ranks.sort();
        ranks
    }

    #[test]
    fn ranks_leave_a_validator_without_a_value_unranked() {
        assert_eq!(
            apy_ranks(vec![
                validator_with_epoch_apy("aTop", Some(0.09)),
                validator_with_epoch_apy("bTie", Some(0.07)),
                validator_with_epoch_apy("cTie", Some(0.07)),
                validator_with_epoch_apy("dMissing", None),
            ]),
            vec![
                ("aTop".to_string(), Some(1)),
                ("bTie".to_string(), Some(3)),
                ("cTie".to_string(), Some(3)),
                ("dMissing".to_string(), None),
            ],
            "a rank states where a validator placed, so having no value must read as no rank"
        );
    }

    #[test]
    fn ranks_separate_apys_differing_below_four_decimals() {
        assert_eq!(
            to_fixed_for_sort(0.0712312),
            to_fixed_for_sort(0.0712389),
            "the values have to share a rounding bucket for this test to be about rounding"
        );
        assert_eq!(
            apy_ranks(vec![
                validator_with_epoch_apy("aLower", Some(0.0712312)),
                validator_with_epoch_apy("bHigher", Some(0.0712389)),
            ]),
            vec![
                ("aLower".to_string(), Some(2)),
                ("bHigher".to_string(), Some(1)),
            ],
            "rounding used to hand both validators the same rank"
        );
    }

    #[test]
    fn ranks_do_not_tie_a_genuine_zero_with_a_validator_without_a_value() {
        // Excluding never-measured rows is what frees the rank a genuine zero deserves.
        assert_eq!(
            apy_ranks(vec![
                validator_with_epoch_apy("aTop", Some(0.09)),
                validator_with_epoch_apy("bZero", Some(0.0)),
                validator_with_epoch_apy("cMissing", None),
            ]),
            vec![
                ("aTop".to_string(), Some(1)),
                ("bZero".to_string(), Some(2)),
                ("cMissing".to_string(), None),
            ]
        );
    }
}
