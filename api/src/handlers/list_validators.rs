use std::collections::{HashMap, HashSet};

use crate::context::WrappedContext;
use crate::metrics;
use crate::utils::order::{
    compare_keys, OrderDirection, OrderField, SortKey, DEFAULT_ORDER_DIRECTION, DEFAULT_ORDER_FIELD,
};
use crate::utils::response::{response_error, response_error_500};
use crate::utils::validator_groups::{compare_group_rows, group_column, sort_groups};
use chrono::{DateTime, Utc};
use log::error;
use rust_decimal::prelude::*;
use serde::{Deserialize, Serialize};
use store::{
    dto::{
        IncidentDetail, ValidatorGroupRecord, ValidatorGroups, ValidatorRecord,
        ValidatorsAggregated,
    },
    groups::{aggregate_operators, singleton_group},
    incidents::{MIN_LEADER_SLOTS, MIN_MISSED_SLOTS},
    utils::{to_fixed_for_sort, worst_known_commission, DEFAULT_CACHE_EPOCHS},
};
use warp::{http::StatusCode, reply::json, Reply};

const DEFAULT_EPOCHS: usize = 15;
const DEFAULT_MIN_INCIDENT_DOWNTIME_SECONDS: u64 = 180;
const DEFAULT_INCIDENTS_WINDOW_EPOCHS: u64 = 90;
const DEFAULT_LIMIT: usize = 100;

/// Which kind of incident a caller wants served.
#[derive(Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum IncidentType {
    Downtime,
    BlockProduction,
}

impl IncidentType {
    /// Reads the names the response emits under `incident_type`, and their snake_case spellings.
    fn parse_list(types: &str) -> Result<Vec<Self>, String> {
        types
            .split(',')
            .map(|name| match name.trim().to_lowercase().as_str() {
                "downtime" => Ok(Self::Downtime),
                "block_production" | "blockproduction" => Ok(Self::BlockProduction),
                other => Err(other.to_string()),
            })
            .collect()
    }
}

// Incidents older than the cache reaches were never loaded, so a wider window would serve less than it says.
const _: () = assert!(DEFAULT_INCIDENTS_WINDOW_EPOCHS <= DEFAULT_CACHE_EPOCHS);

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct ResponseValidators {
    validators: Vec<ValidatorRecord>,
    validators_aggregated: Vec<ValidatorsAggregated>,
    /// Operator rows, ordered by `order_field`, present only under `with_operator_groups`. Aggregated
    /// over the validators matching the query and filters, so a row describes the validators served
    /// under it.
    #[serde(skip_serializing_if = "Option::is_none")]
    operators: Option<Vec<ValidatorGroupRecord>>,
    /// Activated stake of every validator matching the query, in lamports — the denominator behind the
    /// operator rows' `stake_share`. Summing the rows does not recover it: a validator belonging to no
    /// operator counts here and has no row. Present only under `with_operator_groups`.
    #[serde(skip_serializing_if = "Option::is_none")]
    total_activated_stake: Option<Decimal>,
    /// Epoch the operator rows describe.
    #[serde(skip_serializing_if = "Option::is_none")]
    current_epoch: Option<u64>,
    /// Number of rows matching the query and filters, before `offset`/`limit`: validators, or
    /// top-level rows under `with_operator_groups`.
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
    /// `true` keeps the validators whose `incidents` array comes back empty, `false` the rest. It reads that array, so `query_incident_types`, `min_incident_downtime_seconds`, `min_incident_missed_slots` and `incident_window_epochs` shape it too, where `epochs` and `query_from_date` do not.
    query_incident_free: Option<bool>,
    /// Comma-separated incident types to serve: `downtime`, `block_production`. Defaults to all of them. An epoch with more than one symptom is served under any of them.
    query_incident_types: Option<String>,
    /// Minimum downtime in seconds for a `DOWN` interval to read as an incident. Shorter intervals are restart noise, and reach neither the `incidents` array nor `order_field=incidents` nor `query_incident_free`. Only applies to the downtime incident type.
    min_incident_downtime_seconds: Option<u64>,
    /// Minimum missed leader slots for a skipped epoch to read as an incident. Defaults to 4, minimum 4.
    min_incident_missed_slots: Option<u64>,
    /// Minimum leader slots an epoch needs before its block production is judged. Defaults to 64, minimum 64.
    min_incident_leader_slots: Option<u64>,
    /// Epochs back the `incidents` array reaches, counting the newest reported epoch itself. Defaults to 90; above 90 — the whole window the cache holds — answers 400. Unrelated to `epochs`, which sizes `epoch_stats`.
    incident_window_epochs: Option<u64>,
    query_verified: Option<bool>,
    query_protected: Option<bool>,
    query_flagged: Option<bool>,
    /// When true, `query` also matches datacenter location fields (country, city) in addition to
    /// validator name, vote account and identity.
    search_properties: Option<bool>,
    /// `true` groups the validators into operator blocks, returns the `operators` aggregates beside them, and pages over those top-level rows rather than validators.
    with_operator_groups: Option<bool>,
    offset: Option<usize>,
    limit: Option<usize>,
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
    pub query_incident_types: Option<Vec<IncidentType>>,
    pub min_incident_downtime_seconds: Option<u64>,
    pub min_incident_missed_slots: Option<u64>,
    pub min_incident_leader_slots: Option<u64>,
    pub incident_window_epochs: Option<u64>,
    pub query_verified: Option<bool>,
    pub query_protected: Option<bool>,
    pub query_flagged: Option<bool>,
    pub search_properties: Option<bool>,
    pub query_from_date: Option<DateTime<Utc>>,
    pub epochs: usize,
    pub with_operator_groups: bool,
}

#[derive(Debug)]
pub struct ValidatorsPage {
    pub validators: Vec<ValidatorRecord>,
    pub operators: Option<ValidatorGroups>,
    /// Number of rows matching the query and filters, before `offset`/`limit`: validators, or
    /// top-level rows under `with_operator_groups`.
    pub total_count: usize,
    pub bond_flags_updated_at: Option<DateTime<Utc>>,
    pub net_apy_updated_at: Option<DateTime<Utc>>,
}

pub async fn get_validators(
    context: WrappedContext,
    config: GetValidatorsConfig,
) -> anyhow::Result<ValidatorsPage> {
    let (validators, bond_flags_updated_at, net_apy_updated_at) = {
        let cache = &context.read().await.cache;
        (
            cache.get_validators(),
            cache.bond_flags_updated_at().map(DateTime::<Utc>::from),
            cache.net_apy_updated_at().map(DateTime::<Utc>::from),
        )
    };

    let validators = filter_validators(validators, &config);
    // Measured over the whole match rather than the page, so every page reads the same window.
    let newest_epoch = validators
        .iter()
        .flat_map(|validator| &validator.epoch_stats)
        .map(|epoch_stat| epoch_stat.epoch)
        .max()
        .unwrap_or(0);
    // `epochs` counts the newest epoch itself.
    let min_epoch = (newest_epoch + 1).saturating_sub(config.epochs as u64);

    // Rows describe the validators this response serves, so they are aggregated per request.
    let mut operators = config.with_operator_groups.then(|| {
        let matching: Vec<&ValidatorRecord> = validators.iter().collect();
        let aggregated = aggregate_operators(&matching);

        ValidatorGroups {
            groups: sort_groups(
                aggregated.groups,
                config.order_field,
                &config.order_direction,
            ),
            ..aggregated
        }
    });
    let (validators, total_count) = page_validators(
        validators,
        operators.as_mut().map(|operators| &mut operators.groups),
        &config,
    );

    let page = validators
        .into_iter()
        .map(|mut v| {
            match config.query_from_date {
                Some(from_date) => v.epoch_stats.retain(|es| {
                    es.epoch_start_at
                        .is_some_and(|start_at| start_at > from_date)
                }),
                None => v.epoch_stats.retain(|stats| stats.epoch >= min_epoch),
            }

            v
        })
        .collect();

    Ok(ValidatorsPage {
        validators: page,
        operators,
        total_count,
        bond_flags_updated_at,
        net_apy_updated_at,
    })
}

#[derive(PartialEq, Eq, PartialOrd, Ord, Hash, Clone, Debug)]
enum TopLevelRow {
    /// Case-folded operator name.
    Operator(String),
    /// Vote account of a validator belonging to no operator.
    Standalone(String),
}

/// Position of each top-level row in the order asked for.
type TopLevelRanks = HashMap<TopLevelRow, usize>;

fn top_level_row(validator: &ValidatorRecord) -> TopLevelRow {
    match &validator.operator {
        Some(operator) => TopLevelRow::Operator(operator.to_lowercase()),
        None => TopLevelRow::Standalone(validator.vote_account.clone()),
    }
}

/// The rows `validators` occupies, ranked against each other on the same column. An operator no
/// validator here belongs to is not a row, so a page of rows never comes back empty.
fn top_level_ranks(
    validators: &[ValidatorRecord],
    operators: &[ValidatorGroupRecord],
    order_field: OrderField,
    order_direction: &OrderDirection,
) -> TopLevelRanks {
    let aggregated: HashMap<String, &ValidatorGroupRecord> = operators
        .iter()
        .map(|operator| (operator.key.to_lowercase(), operator))
        .collect();

    let mut rows: Vec<(TopLevelRow, SortKey, String)> = Vec::new();
    let mut placed: HashSet<TopLevelRow> = HashSet::new();

    for validator in validators {
        let row = top_level_row(validator);
        if !placed.insert(row.clone()) {
            continue;
        }
        match &validator.operator {
            Some(operator) => match aggregated.get(&operator.to_lowercase()) {
                Some(aggregate) => rows.push((
                    row,
                    group_column(aggregate, order_field),
                    aggregate.key.clone(),
                )),
                // No row for this operator, so no column value: the block sorts at the tail.
                None => rows.push((row, SortKey::Missing, operator.clone())),
            },
            None => {
                let standalone = singleton_group(validator);
                rows.push((row, group_column(&standalone, order_field), standalone.key));
            }
        }
    }

    rows.sort_by(|(a_row, a_column, a_name), (b_row, b_column, b_name)| {
        compare_group_rows((a_column, a_name), (b_column, b_name), order_direction)
            // Names collide across the two kinds of row, and the list is paged, so the order has to be total.
            .then_with(|| a_row.cmp(b_row))
    });

    rows.into_iter()
        .enumerate()
        .map(|(rank, (row, ..))| (row, rank))
        .collect()
}

/// The page and its total count. With `operators`, both are in top-level rows: an operator's
/// validators arrive whole or not at all, and the rows no validator on the page belongs to are
/// dropped from `operators`.
fn page_validators(
    validators: Vec<ValidatorRecord>,
    operators: Option<&mut Vec<ValidatorGroupRecord>>,
    config: &GetValidatorsConfig,
) -> (Vec<ValidatorRecord>, usize) {
    let Some(operators) = operators else {
        let validators = sort_validators(validators, config.order_field, &config.order_direction);
        let total_count = validators.len();

        return (
            validators
                .into_iter()
                .skip(config.offset)
                .take(config.limit)
                .collect(),
            total_count,
        );
    };

    let ranks = top_level_ranks(
        &validators,
        operators,
        config.order_field,
        &config.order_direction,
    );
    let rows = config.offset..config.offset.saturating_add(config.limit);
    let page: Vec<ValidatorRecord> = sort_validators_ranked(
        validators,
        Some(&ranks),
        config.order_field,
        &config.order_direction,
    )
    .into_iter()
    .filter(|validator| {
        ranks
            .get(&top_level_row(validator))
            .is_some_and(|rank| rows.contains(rank))
    })
    .collect();

    let on_page: HashSet<String> = page
        .iter()
        .filter_map(|validator| validator.operator.as_ref())
        .map(|operator| operator.to_lowercase())
        .collect();
    operators.retain(|operator| on_page.contains(&operator.key.to_lowercase()));

    (page, ranks.len())
}

fn sort_validators(
    validators: Vec<ValidatorRecord>,
    order_field: OrderField,
    order_direction: &OrderDirection,
) -> Vec<ValidatorRecord> {
    sort_validators_ranked(validators, None, order_field, order_direction)
}

fn sort_validators_ranked(
    validators: Vec<ValidatorRecord>,
    top_level_ranks: Option<&TopLevelRanks>,
    order_field: OrderField,
    order_direction: &OrderDirection,
) -> Vec<ValidatorRecord> {
    let field_extractor = get_field_extractor(order_field);
    // Ungrouped, every validator keys the same, so the rank drops out of the comparison.
    let rank = |validator: &ValidatorRecord| {
        top_level_ranks
            .and_then(|ranks| ranks.get(&top_level_row(validator)).copied())
            .unwrap_or(usize::MAX)
    };
    // Keyed up front: sort_by would otherwise re-extract on both sides of every one of n·log n comparisons.
    let mut keyed: Vec<(usize, SortKey, ValidatorRecord)> = validators
        .into_iter()
        .map(|validator| (rank(&validator), field_extractor(&validator), validator))
        .collect();
    keyed.sort_by(|(a_rank, a_key, a), (b_rank, b_key, b)| {
        // Ascending in both directions: the direction is already spent on the operator order.
        a_rank
            .cmp(b_rank)
            .then_with(|| compare_keys(a_key, b_key, order_direction))
            // Without this tiebreak ties inherit HashMap iteration order, which changes on every
            // cache refresh and makes offset pages overlap or skip rows.
            .then_with(|| a.vote_account.cmp(&b.vote_account))
    });
    keyed.into_iter().map(|(.., validator)| validator).collect()
}

type FieldExtractor = fn(&ValidatorRecord) -> SortKey;

// Commission and Uptime keep worst-case sentinels: for those two unknown means risk, not no-data.
fn get_field_extractor(order_field: OrderField) -> FieldExtractor {
    match order_field {
        OrderField::Stake => |a: &ValidatorRecord| SortKey::Number(a.activated_stake),
        OrderField::Credits => |a: &ValidatorRecord| SortKey::Number(Decimal::from(a.credits)),
        OrderField::MarinadeScore => |a: &ValidatorRecord| {
            a.score
                .and_then(to_fixed_for_sort)
                .map(Decimal::from)
                .into()
        },
        // Shares NetApy's `ratio^n - 1` derivation but is computed here instead of served by apy-api, so an unrepresentable value sinks rather than being crowned.
        OrderField::Apy => |a: &ValidatorRecord| {
            a.avg_apy
                .filter(|apy| *apy >= 0.0)
                .and_then(Decimal::from_f64_retain)
                .into()
        },
        // Deliberately not to_fixed_for_sort: rounding a fraction-valued APY to 4 decimals is what
        // collapses hundreds of validators into one bucket and makes the column look unsorted.
        // Saturating up is safe because apy-api derives this as `ratio^n - 1` from a positive ratio, so an unrepresentable value is always the high end.
        OrderField::NetApy => |a: &ValidatorRecord| {
            a.net_apy
                .map(|net_apy| Decimal::from_f64_retain(net_apy).unwrap_or(Decimal::MAX))
                .into()
        },
        // Same input as expected_take_rate: sorting on the closed-epoch ceiling alone would rank a validator that already declared a raise this epoch among the cheaper ones.
        OrderField::Commission => |a: &ValidatorRecord| {
            SortKey::Number(Decimal::from(
                worst_known_commission(a.commission_max_observed, a.commission_advertised)
                    .unwrap_or(100),
            ))
        },
        OrderField::Uptime => |a: &ValidatorRecord| {
            SortKey::Number(Decimal::from(
                a.avg_uptime_pct.and_then(to_fixed_for_sort).unwrap_or(0),
            ))
        },
        // Same fraction-rounding trap as NetApy, but a degenerate value sinks here rather than saturating up: a take rate has no natural high end to saturate towards.
        OrderField::TakeRate => |a: &ValidatorRecord| {
            a.avg_take_rate
                .filter(|rate| *rate >= 0.0)
                .and_then(Decimal::from_f64_retain)
                .into()
        },
        OrderField::ExpectedTakeRate => |a: &ValidatorRecord| {
            a.expected_take_rate
                .filter(|rate| *rate >= 0.0)
                .and_then(Decimal::from_f64_retain)
                .into()
        },
        OrderField::DelegationRelationships => {
            |a: &ValidatorRecord| a.unique_delegators.map(Decimal::from).into()
        }
        OrderField::Incidents => {
            |a: &ValidatorRecord| SortKey::Number(Decimal::from(a.incidents.len()))
        }
        OrderField::StakeDelta7d => |a: &ValidatorRecord| a.stake_delta_7d.into(),
        OrderField::StakeDelta30d => |a: &ValidatorRecord| a.stake_delta_30d.into(),
        // The name the list shows: what it reports for itself, or its vote account when it reports none.
        OrderField::Name => |a: &ValidatorRecord| {
            SortKey::Text(
                a.info_name
                    .as_deref()
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .unwrap_or(&a.vote_account)
                    .to_lowercase(),
            )
        },
        // Only relevant for grouping by operator and sorting by validator count (query param `with_operator_groups`).
        // This `Decimal::ONE` is here for completeness, individual validators tie-break on vote account.
        OrderField::Validators => |_: &ValidatorRecord| SortKey::Number(Decimal::ONE),
    }
}

pub fn filter_validators(
    mut validators: HashMap<String, ValidatorRecord>,
    config: &GetValidatorsConfig,
) -> Vec<ValidatorRecord> {
    // Shared with the client and provider aggregates, so both describe the same population.
    let last_epoch = store::utils::last_reported_epoch(validators.values()).unwrap_or(0);
    validators.retain(|_, validator| store::utils::is_eligible_validator(validator, last_epoch));

    // Everything downstream reads whatever survives here: the array served, the ordering,
    // `query_incident_free`, and the operator rows aggregated off these records.
    let min_incident_downtime = config
        .min_incident_downtime_seconds
        .unwrap_or(DEFAULT_MIN_INCIDENT_DOWNTIME_SECONDS);
    // `counts_as_incident` owns both defaults, so the caller's floors travel as they arrived.
    let (min_missed_slots, min_leader_slots) = (
        config.min_incident_missed_slots,
        config.min_incident_leader_slots,
    );
    let wants = |incident_type| {
        config
            .query_incident_types
            .as_ref()
            .is_none_or(|types| types.contains(&incident_type))
    };
    let (wants_downtime, wants_block_production) = (
        wants(IncidentType::Downtime),
        wants(IncidentType::BlockProduction),
    );
    // The window counts `last_epoch` itself.
    let from_epoch = (last_epoch + 1).saturating_sub(
        config
            .incident_window_epochs
            .unwrap_or(DEFAULT_INCIDENTS_WINDOW_EPOCHS),
    );
    for validator in validators.values_mut() {
        validator.incidents.retain_mut(|incident| {
            if incident.epoch < from_epoch {
                return false;
            }
            let (downtime_seconds, block_production) = match &mut incident.detail {
                IncidentDetail::Downtime {
                    downtime_seconds,
                    block_production,
                    ..
                } => (Some(*downtime_seconds), block_production.as_mut()),
                IncidentDetail::BlockProduction {
                    block_production, ..
                } => (None, Some(block_production)),
            };
            // Re-answered against the caller's floors, so what is served agrees with what is said.
            let counts_as_incident = block_production.is_some_and(|detail| {
                detail.counts_as_incident = detail.counts_as_incident(min_missed_slots, min_leader_slots);
                detail.counts_as_incident
            });
            // Served when any symptom is asked for and over its own floor.
            (wants_downtime
                && downtime_seconds.is_some_and(|seconds| seconds >= min_incident_downtime))
                || (wants_block_production && counts_as_incident)
        });
    }

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
    if let Some(window) = query_params.incident_window_epochs {
        if window == 0 || window > DEFAULT_CACHE_EPOCHS {
            return Ok(response_error(
                StatusCode::BAD_REQUEST,
                format!("incident_window_epochs must be between 1 and {DEFAULT_CACHE_EPOCHS}"),
            ));
        }
    }
    if let Some(missed_slots) = query_params.min_incident_missed_slots {
        if missed_slots < MIN_MISSED_SLOTS {
            return Ok(response_error(
                StatusCode::BAD_REQUEST,
                format!("min_incident_missed_slots must be at least {MIN_MISSED_SLOTS}"),
            ));
        }
    }
    if let Some(leader_slots) = query_params.min_incident_leader_slots {
        if leader_slots < MIN_LEADER_SLOTS {
            return Ok(response_error(
                StatusCode::BAD_REQUEST,
                format!("min_incident_leader_slots must be at least {MIN_LEADER_SLOTS}"),
            ));
        }
    }
    let query_incident_types = match query_params.query_incident_types.as_deref() {
        Some(types) => match IncidentType::parse_list(types) {
            Ok(types) => Some(types),
            Err(unknown) => {
                return Ok(response_error(
                    StatusCode::BAD_REQUEST,
                    format!(
                        "query_incident_types does not know {unknown:?}, expected downtime or block_production"
                    ),
                ))
            }
        },
        None => None,
    };
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
        query_incident_types,
        min_incident_downtime_seconds: query_params.min_incident_downtime_seconds,
        min_incident_missed_slots: query_params.min_incident_missed_slots,
        min_incident_leader_slots: query_params.min_incident_leader_slots,
        incident_window_epochs: query_params.incident_window_epochs,
        query_verified: query_params.query_verified,
        query_protected: query_params.query_protected,
        query_flagged: query_params.query_flagged,
        search_properties: query_params.search_properties,
        query_from_date: query_params.query_from_date,
        epochs: query_params.epochs.unwrap_or(DEFAULT_EPOCHS),
        with_operator_groups: query_params.with_operator_groups == Some(true),
    };

    log::info!("Query validators {config:?}");

    let validators = get_validators(context.clone(), config).await;

    Ok(match validators {
        Ok(page) => {
            let validators_aggregated = store::utils::aggregate_validators(&page.validators);
            let (operators, total_activated_stake, current_epoch) = match page.operators {
                Some(operators) => (
                    Some(operators.groups),
                    Some(operators.total_activated_stake),
                    operators.current_epoch,
                ),
                None => (None, None, None),
            };
            warp::reply::with_status(
                json(&ResponseValidators {
                    validators: page.validators,
                    validators_aggregated,
                    operators,
                    total_activated_stake,
                    current_epoch,
                    total_count: page.total_count,
                    bond_flags_updated_at: page.bond_flags_updated_at,
                    net_apy_updated_at: page.net_apy_updated_at,
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
    use store::dto::{
        BlockProductionDetail, IncidentDetail, IncidentRecord, ValidatorEpochStats,
        ValidatorWarning, UNKNOWN_CLIENT_NAME,
    };

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
            operator: None,
            stake_delta_7d: None,
            stake_delta_30d: None,
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
            query_incident_types: None,
            min_incident_downtime_seconds: None,
            min_incident_missed_slots: None,
            min_incident_leader_slots: None,
            incident_window_epochs: None,
            query_verified: None,
            query_protected: None,
            query_flagged: None,
            search_properties: None,
            query_from_date: None,
            epochs: 15,
            with_operator_groups: false,
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
    fn incident_in_epoch(epoch: u64, downtime_seconds: u64) -> IncidentRecord {
        let end_at = Utc::now();
        IncidentRecord {
            epoch,
            detail: IncidentDetail::Downtime {
                start_at: end_at - chrono::Duration::seconds(downtime_seconds as i64),
                end_at,
                downtime_seconds,
                block_production: None,
            },
        }
    }

    const FIXTURE_LEADER_SLOTS: u64 = 6392;
    const FIXTURE_MISSED_SLOTS: u64 = 548;

    fn skipped(missed_slots: u64) -> BlockProductionDetail {
        let mut detail = BlockProductionDetail {
            leader_slots: FIXTURE_LEADER_SLOTS,
            blocks_produced: FIXTURE_LEADER_SLOTS - missed_slots,
            missed_slots,
            skip_rate: missed_slots as f64 / FIXTURE_LEADER_SLOTS as f64,
            cluster_skip_rate: 0.001_57,
            threshold: 0.015_7,
            counts_as_incident: false,
        };
        detail.counts_as_incident = detail.counts_as_incident(None, None);
        detail
    }

    fn block_production_incident_of(epoch: u64, missed_slots: u64) -> IncidentRecord {
        let epoch_end_at = Utc::now();
        IncidentRecord {
            epoch,
            detail: IncidentDetail::BlockProduction {
                epoch_start_at: epoch_end_at - chrono::Duration::days(2),
                epoch_end_at: Some(epoch_end_at),
                block_production: skipped(missed_slots),
            },
        }
    }

    fn block_production_incident(epoch: u64) -> IncidentRecord {
        block_production_incident_of(epoch, FIXTURE_MISSED_SLOTS)
    }

    /// One epoch that went down and also skipped: a downtime row carrying a symptom of each kind.
    fn down_and_skipped(downtime_seconds: u64, missed_slots: u64) -> IncidentRecord {
        let end_at = Utc::now();
        IncidentRecord {
            epoch: 100,
            detail: IncidentDetail::Downtime {
                start_at: end_at - chrono::Duration::seconds(downtime_seconds as i64),
                end_at,
                downtime_seconds,
                block_production: Some(skipped(missed_slots)),
            },
        }
    }

    fn with_incidents(vote_account: &str, incidents: Vec<IncidentRecord>) -> ValidatorRecord {
        ValidatorRecord {
            incidents,
            ..validator(vote_account, 100, vec![])
        }
    }

    // The fixture validators report epoch_stats up to epoch 100.
    fn incident(downtime_seconds: u64) -> IncidentRecord {
        incident_in_epoch(100, downtime_seconds)
    }

    fn validator_with_incidents(vote_account: &str, downtimes: &[u64]) -> ValidatorRecord {
        ValidatorRecord {
            incidents: downtimes.iter().copied().map(incident).collect(),
            ..validator(vote_account, 100, vec![])
        }
    }

    #[test]
    fn incident_free_without_a_floor_reads_the_default_one() {
        let validators = map(vec![
            validator_with_incidents("blip", &[1]),
            validator_with_incidents("outage", &[180]),
            validator_with_incidents("clean", &[]),
        ]);
        let config = GetValidatorsConfig {
            query_incident_free: Some(true),
            ..config()
        };
        assert_eq!(
            vote_accounts(filter_validators(validators, &config)),
            vec!["blip".to_string(), "clean".to_string()],
            "a one-second blip is restart noise, not an incident"
        );
    }

    #[test]
    fn the_default_window_reaches_ninety_epochs_back() {
        // The fixtures report up to epoch 100, so the window opens at epoch 11.
        let stale = ValidatorRecord {
            incidents: vec![incident_in_epoch(10, 600)],
            ..validator("stale", 100, vec![])
        };
        let recent = ValidatorRecord {
            incidents: vec![incident_in_epoch(11, 600)],
            ..validator("recent", 100, vec![])
        };
        let config = GetValidatorsConfig {
            query_incident_free: Some(true),
            ..config()
        };
        assert_eq!(
            vote_accounts(filter_validators(map(vec![stale, recent]), &config)),
            vec!["stale".to_string()]
        );
    }

    #[test]
    fn incident_window_epochs_trims_the_array_and_the_filter_together() {
        let validators = || {
            map(vec![ValidatorRecord {
                incidents: vec![incident_in_epoch(98, 600)],
                ..validator("outage", 100, vec![])
            }])
        };
        let narrowed = GetValidatorsConfig {
            query_incident_free: Some(true),
            incident_window_epochs: Some(2),
            ..config()
        };
        let filtered = filter_validators(validators(), &narrowed);
        assert_eq!(
            vote_accounts(filtered.clone()),
            vec!["outage".to_string()],
            "epoch 98 is outside the last two epochs"
        );
        assert!(
            filtered[0].incidents.is_empty(),
            "what the filter cannot see is not served either"
        );

        let widened = GetValidatorsConfig {
            incident_window_epochs: Some(3),
            ..narrowed
        };
        assert!(filter_validators(validators(), &widened).is_empty());
    }

    #[test]
    fn the_window_trims_the_array_without_query_incident_free() {
        let validators = map(vec![ValidatorRecord {
            incidents: vec![incident_in_epoch(98, 600), incident_in_epoch(100, 600)],
            ..validator("outage", 100, vec![])
        }]);
        let config = GetValidatorsConfig {
            incident_window_epochs: Some(2),
            ..config()
        };
        let filtered = filter_validators(validators, &config);
        assert_eq!(
            filtered[0]
                .incidents
                .iter()
                .map(|incident| incident.epoch)
                .collect::<Vec<_>>(),
            vec![100]
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
    fn a_block_production_incident_is_not_incident_free() {
        let validators = map(vec![
            ValidatorRecord {
                incidents: vec![block_production_incident(100)],
                ..validator("skipper", 100, vec![])
            },
            validator("clean", 100, vec![]),
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
    fn the_downtime_floor_does_not_reach_block_production_incidents() {
        let validators = map(vec![ValidatorRecord {
            incidents: vec![block_production_incident(100)],
            ..validator("skipper", 100, vec![])
        }]);
        let config = GetValidatorsConfig {
            query_incident_free: Some(true),
            min_incident_downtime_seconds: Some(u64::MAX),
            ..config()
        };
        assert!(filter_validators(validators, &config).is_empty());
    }

    #[test]
    fn the_missed_slot_floor_trims_block_production_incidents() {
        let validators = map(vec![with_incidents(
            "skipper",
            vec![block_production_incident_of(100, 548)],
        )]);
        let config = GetValidatorsConfig {
            query_incident_free: Some(true),
            min_incident_missed_slots: Some(549),
            ..config()
        };
        assert_eq!(
            vote_accounts(filter_validators(validators, &config)),
            vec!["skipper".to_string()]
        );
    }

    #[test]
    fn the_missed_slot_floor_does_not_reach_downtime_incidents() {
        let validators = map(vec![validator_with_incidents("outage", &[600])]);
        let config = GetValidatorsConfig {
            query_incident_free: Some(true),
            min_incident_missed_slots: Some(u64::MAX),
            ..config()
        };
        assert!(filter_validators(validators, &config).is_empty());
    }

    #[test]
    fn each_incident_type_serves_only_its_own_kind() {
        for (incident_type, served) in [
            (IncidentType::Downtime, "outage"),
            (IncidentType::BlockProduction, "skipper"),
        ] {
            let validators = map(vec![
                validator_with_incidents("outage", &[600]),
                with_incidents("skipper", vec![block_production_incident(100)]),
            ]);
            let config = GetValidatorsConfig {
                query_incident_free: Some(false),
                query_incident_types: Some(vec![incident_type]),
                ..config()
            };
            assert_eq!(
                vote_accounts(filter_validators(validators, &config)),
                vec![served.to_string()],
                "{incident_type:?}"
            );
        }
    }

    #[test]
    fn an_epoch_with_both_symptoms_is_served_under_either_type() {
        for incident_type in [IncidentType::Downtime, IncidentType::BlockProduction] {
            let validators = map(vec![with_incidents(
                "both",
                vec![down_and_skipped(600, 548)],
            )]);
            let config = GetValidatorsConfig {
                query_incident_types: Some(vec![incident_type]),
                ..config()
            };
            assert_eq!(
                filter_validators(validators, &config)[0].incidents.len(),
                1,
                "{incident_type:?}"
            );
        }
    }

    #[test]
    fn a_downtime_carrying_numbers_under_the_rule_is_no_block_production_incident() {
        // 5 of 6392 is 0.08%, under the bar: the numbers are information, not a verdict.
        let validators = map(vec![with_incidents("blip", vec![down_and_skipped(600, 5)])]);
        let config = GetValidatorsConfig {
            query_incident_types: Some(vec![IncidentType::BlockProduction]),
            ..config()
        };
        assert!(filter_validators(validators, &config)[0]
            .incidents
            .is_empty());
    }

    #[test]
    fn a_downtime_carrying_numbers_over_the_rule_is_a_block_production_incident() {
        let validators = map(vec![with_incidents(
            "skipper",
            vec![down_and_skipped(600, FIXTURE_MISSED_SLOTS)],
        )]);
        let config = GetValidatorsConfig {
            query_incident_types: Some(vec![IncidentType::BlockProduction]),
            ..config()
        };
        assert_eq!(filter_validators(validators, &config)[0].incidents.len(), 1);
    }

    // What is served has to agree with what it says about itself.
    #[test]
    fn a_caller_floor_rewrites_the_counts_as_incident_flag_it_is_served_with() {
        let validators = map(vec![with_incidents(
            "skipper",
            vec![down_and_skipped(600, FIXTURE_MISSED_SLOTS)],
        )]);
        let config = GetValidatorsConfig {
            min_incident_missed_slots: Some(FIXTURE_MISSED_SLOTS + 1),
            ..config()
        };

        let served = filter_validators(validators, &config);
        let IncidentDetail::Downtime {
            block_production, ..
        } = &served[0].incidents[0].detail
        else {
            panic!("a downtime fixture is a downtime incident");
        };
        // Kept by its downtime, but no longer a block production incident under this floor.
        assert!(!block_production.as_ref().unwrap().counts_as_incident);
    }

    #[test]
    fn a_leader_slot_floor_above_the_default_drops_what_the_rule_admitted() {
        let validators = map(vec![with_incidents(
            "skipper",
            vec![down_and_skipped(600, FIXTURE_MISSED_SLOTS)],
        )]);
        let config = GetValidatorsConfig {
            query_incident_types: Some(vec![IncidentType::BlockProduction]),
            min_incident_leader_slots: Some(FIXTURE_LEADER_SLOTS + 1),
            ..config()
        };
        assert!(filter_validators(validators, &config)[0].incidents.is_empty());
    }

    #[test]
    fn an_epoch_with_both_symptoms_under_both_floors_is_dropped() {
        // Under the 180s floor and under one leader turn: no symptom carries it on its own.
        let validators = map(vec![with_incidents("both", vec![down_and_skipped(30, 2)])]);
        assert!(filter_validators(validators, &config())[0]
            .incidents
            .is_empty());
    }

    #[test]
    fn an_epoch_that_only_skipped_enough_survives_the_downtime_floor() {
        let validators = map(vec![with_incidents(
            "both",
            vec![down_and_skipped(30, 548)],
        )]);
        assert_eq!(
            filter_validators(validators, &config())[0].incidents.len(),
            1
        );
    }

    #[test]
    fn incident_types_read_the_names_the_response_emits() {
        assert_eq!(
            IncidentType::parse_list("Downtime,block_production").unwrap(),
            vec![IncidentType::Downtime, IncidentType::BlockProduction]
        );
        assert_eq!(
            IncidentType::parse_list("uptime"),
            Err("uptime".to_string())
        );
    }

    #[test]
    fn a_block_production_incident_outside_the_window_is_dropped_like_any_other() {
        // The fixtures report up to epoch 100, so the default window opens at epoch 11.
        let validators = map(vec![ValidatorRecord {
            incidents: vec![block_production_incident(10)],
            ..validator("skipper", 100, vec![])
        }]);
        let config = GetValidatorsConfig {
            query_incident_free: Some(true),
            ..config()
        };
        assert_eq!(
            vote_accounts(filter_validators(validators, &config)),
            vec!["skipper".to_string()]
        );
    }

    #[test]
    fn min_incident_downtime_alone_keeps_every_validator_but_trims_their_arrays() {
        let validators = map(vec![
            validator_with_incidents("blip", &[179]),
            validator_with_incidents("outage", &[180]),
        ]);
        let config = GetValidatorsConfig {
            min_incident_downtime_seconds: Some(180),
            ..config()
        };
        let filtered = filter_validators(validators, &config);
        assert_eq!(filtered.len(), 2);
        assert_eq!(
            filtered
                .iter()
                .map(|v| (v.vote_account.as_str(), v.incidents.len()))
                .collect::<HashMap<_, _>>(),
            HashMap::from([("blip", 0), ("outage", 1)])
        );
    }

    #[test]
    fn the_incidents_array_drops_restart_noise_by_default() {
        let validators = map(vec![validator_with_incidents("mixed", &[1, 180])]);
        let filtered = filter_validators(validators, &config());
        assert_eq!(
            filtered[0]
                .incidents
                .iter()
                .map(|incident| match incident.detail {
                    IncidentDetail::Downtime {
                        downtime_seconds, ..
                    } => downtime_seconds,
                    _ => panic!("a downtime fixture is a downtime incident"),
                })
                .collect::<Vec<_>>(),
            vec![180]
        );
    }

    #[test]
    fn a_zero_floor_serves_every_interval() {
        let validators = map(vec![validator_with_incidents("mixed", &[1, 600])]);
        let config = GetValidatorsConfig {
            min_incident_downtime_seconds: Some(0),
            ..config()
        };
        assert_eq!(filter_validators(validators, &config)[0].incidents.len(), 2);
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

    fn operated(vote_account: &str, stake: i64, operator: Option<&str>) -> ValidatorRecord {
        ValidatorRecord {
            operator: operator.map(str::to_string),
            ..validator(vote_account, stake, vec![])
        }
    }

    fn operator(key: &str, stake: i64) -> ValidatorGroupRecord {
        ValidatorGroupRecord {
            key: key.to_string(),
            total_stake: Decimal::from(stake),
            ..Default::default()
        }
    }

    /// `vote_accounts` re-sorts alphabetically; this keeps the order as sorted.
    fn order(validators: Vec<ValidatorRecord>) -> Vec<String> {
        validators
            .into_iter()
            .map(|validator| validator.vote_account)
            .collect()
    }

    fn by_operator(
        validators: Vec<ValidatorRecord>,
        operators: Vec<ValidatorGroupRecord>,
        order_field: OrderField,
        order_direction: &OrderDirection,
    ) -> Vec<String> {
        let operators = sort_groups(operators, order_field, order_direction);
        let ranks = top_level_ranks(&validators, &operators, order_field, order_direction);
        order(sort_validators_ranked(
            validators,
            Some(&ranks),
            order_field,
            order_direction,
        ))
    }

    /// The page and the `total_count` beside it.
    fn paged(
        validators: Vec<ValidatorRecord>,
        operators: Option<Vec<ValidatorGroupRecord>>,
        offset: usize,
        limit: usize,
    ) -> (Vec<String>, usize) {
        let config = GetValidatorsConfig {
            offset,
            limit,
            with_operator_groups: operators.is_some(),
            ..config()
        };
        let mut operators = operators
            .map(|operators| sort_groups(operators, config.order_field, &config.order_direction));
        let (page, total_count) = page_validators(validators, operators.as_mut(), &config);

        (order(page), total_count)
    }

    /// The operator rows served beside a page, in order.
    fn paged_operators(
        validators: Vec<ValidatorRecord>,
        operators: Vec<ValidatorGroupRecord>,
        offset: usize,
        limit: usize,
    ) -> Vec<String> {
        let config = GetValidatorsConfig {
            offset,
            limit,
            with_operator_groups: true,
            ..config()
        };
        let mut groups = sort_groups(operators, config.order_field, &config.order_direction);
        page_validators(validators, Some(&mut groups), &config);

        groups.into_iter().map(|group| group.key).collect()
    }

    fn two_operators() -> (Vec<ValidatorRecord>, Vec<ValidatorGroupRecord>) {
        (
            vec![
                operated("bigOfX", 400, Some("X")),
                operated("smallOfX", 200, Some("X")),
                operated("alone", 500, None),
                operated("bigOfY", 200, Some("Y")),
                operated("smallOfY", 100, Some("Y")),
            ],
            vec![operator("X", 600), operator("Y", 300)],
        )
    }

    #[test]
    fn paging_cuts_top_level_rows_and_never_an_operators_validators() {
        let (validators, operators) = two_operators();
        assert_eq!(
            paged(validators, Some(operators), 0, 1),
            (vec!["bigOfX".to_string(), "smallOfX".to_string()], 3),
            "one row is one operator with both of its validators, out of three rows"
        );

        let (validators, operators) = two_operators();
        assert_eq!(
            paged(validators, Some(operators), 1, 1),
            (vec!["alone".to_string()], 3),
            "the next row is the lone validator, and X does not repeat"
        );

        let (validators, operators) = two_operators();
        assert_eq!(
            paged(validators, Some(operators), 2, 1),
            (vec!["bigOfY".to_string(), "smallOfY".to_string()], 3)
        );
    }

    #[test]
    fn a_row_offset_past_the_last_row_serves_nothing() {
        let (validators, operators) = two_operators();
        assert_eq!(paged(validators, Some(operators), 3, 10), (Vec::new(), 3));
    }

    #[test]
    fn paging_without_the_operator_groups_cuts_validators() {
        let (validators, _) = two_operators();
        assert_eq!(
            paged(validators, None, 1, 2),
            (vec!["bigOfX".to_string(), "bigOfY".to_string()], 5),
            "by stake: alone, bigOfX, bigOfY, smallOfX, smallOfY"
        );

        let (validators, _) = two_operators();
        assert_eq!(
            paged(validators, None, 0, 1),
            (vec!["alone".to_string()], 5),
            "the count is validators, and a page can hold one"
        );
    }

    #[test]
    fn an_operator_no_validator_in_the_page_belongs_to_is_not_a_row() {
        assert_eq!(
            paged(
                vec![operated("onlyOne", 100, Some("X"))],
                Some(vec![operator("X", 600), operator("Y", 300)]),
                0,
                100,
            ),
            (vec!["onlyOne".to_string()], 1),
            "Y has no validator here, so it is not a row the pager can land on"
        );
    }

    #[test]
    fn the_operators_beside_a_page_are_the_ones_on_it() {
        let (validators, operators) = two_operators();
        assert_eq!(
            paged_operators(validators, operators, 0, 1),
            vec!["X".to_string()],
            "row 0 is X, so Y does not ride along"
        );

        let (validators, operators) = two_operators();
        assert_eq!(
            paged_operators(validators, operators, 1, 1),
            Vec::<String>::new(),
            "row 1 is the lone validator, which is no operator's row"
        );

        let (validators, operators) = two_operators();
        assert_eq!(
            paged_operators(validators, operators, 0, 3),
            vec!["X".to_string(), "Y".to_string()],
            "a page holding every row still serves both, in the ordered position"
        );
    }

    #[test]
    fn an_operator_is_kept_beside_its_page_whatever_the_case_it_is_spelled_in() {
        assert_eq!(
            paged_operators(
                vec![operated("onlyOne", 100, Some("acme ops"))],
                vec![operator("ACME Ops", 100)],
                0,
                100,
            ),
            vec!["ACME Ops".to_string()],
            "rows are bucketed case-folded, so the served key need not match the validator's"
        );
    }

    #[test]
    fn operator_order_groups_the_validators_before_the_column_does() {
        assert_eq!(
            by_operator(
                vec![
                    operated("smallOfBig", 1, Some("Big")),
                    operated("bigOfSmall", 100, Some("small")),
                    operated("bigOfBig", 50, Some("BIG")),
                ],
                vec![operator("Big", 900), operator("Small", 100)],
                OrderField::Stake,
                &OrderDirection::DESC,
            ),
            vec!["bigOfBig", "smallOfBig", "bigOfSmall"],
            "the largest validator of the smaller operator still sorts after both of the larger one's"
        );
    }

    #[test]
    fn the_direction_turns_the_operator_blocks_and_their_contents_together() {
        assert_eq!(
            by_operator(
                vec![
                    operated("bigOfBig", 50, Some("BIG")),
                    operated("smallOfBig", 1, Some("Big")),
                    operated("bigOfSmall", 100, Some("small")),
                ],
                vec![operator("Big", 900), operator("Small", 100)],
                OrderField::Stake,
                &OrderDirection::ASC,
            ),
            vec!["bigOfSmall", "smallOfBig", "bigOfBig"]
        );
    }

    #[test]
    fn a_validator_with_no_operator_ranks_between_the_operators_it_outweighs_and_the_rest() {
        assert_eq!(
            by_operator(
                vec![
                    operated("bigOfX", 400, Some("X")),
                    operated("smallOfX", 200, Some("X")),
                    operated("alone", 500, None),
                    operated("bigOfY", 200, Some("Y")),
                    operated("smallOfY", 100, Some("Y")),
                ],
                vec![operator("X", 600), operator("Y", 300)],
                OrderField::Stake,
                &OrderDirection::DESC,
            ),
            vec!["bigOfX", "smallOfX", "alone", "bigOfY", "smallOfY"],
            "500 on its own belongs between the 600 and the 300 operator, not behind both"
        );
    }

    #[test]
    fn the_direction_turns_the_operators_and_the_lone_validators_as_one_list() {
        assert_eq!(
            by_operator(
                vec![
                    operated("bigOfX", 400, Some("X")),
                    operated("smallOfX", 200, Some("X")),
                    operated("alone", 500, None),
                    operated("bigOfY", 200, Some("Y")),
                    operated("smallOfY", 100, Some("Y")),
                ],
                vec![operator("X", 600), operator("Y", 300)],
                OrderField::Stake,
                &OrderDirection::ASC,
            ),
            vec!["smallOfY", "bigOfY", "alone", "smallOfX", "bigOfX"]
        );
    }

    #[test]
    fn a_standalone_validator_ranks_on_the_column_the_operators_rank_on() {
        // Equal stake throughout, so only the rate can be placing the rows.
        assert_eq!(
            by_operator(
                vec![
                    ValidatorRecord {
                        net_apy: Some(0.05),
                        ..operated("ofHigh", 100, Some("High"))
                    },
                    ValidatorRecord {
                        net_apy: Some(0.07),
                        ..operated("alone", 100, None)
                    },
                    ValidatorRecord {
                        net_apy: Some(0.02),
                        ..operated("ofLow", 100, Some("Low"))
                    },
                ],
                vec![
                    ValidatorGroupRecord {
                        net_apy: Some(0.09),
                        ..operator("High", 100)
                    },
                    ValidatorGroupRecord {
                        net_apy: Some(0.02),
                        ..operator("Low", 100)
                    },
                ],
                OrderField::NetApy,
                &OrderDirection::DESC,
            ),
            vec!["ofHigh", "alone", "ofLow"]
        );
    }

    #[test]
    fn ordering_by_name_places_a_lone_validator_by_the_name_the_list_shows_for_it() {
        assert_eq!(
            by_operator(
                vec![
                    operated("ofAcme", 100, Some("Acme")),
                    ValidatorRecord {
                        info_name: Some("Bravo".to_string()),
                        ..operated("named", 900, None)
                    },
                    operated("ofCharlie", 100, Some("Charlie")),
                    operated("zzzUnnamed", 900, None),
                ],
                vec![operator("Acme", 100), operator("Charlie", 100)],
                OrderField::Name,
                &OrderDirection::ASC,
            ),
            vec!["ofAcme", "named", "ofCharlie", "zzzUnnamed"],
            "a validator with no name of its own is shown as its vote account, so it sorts as one"
        );
    }

    /// `Gone` has no row: unclassified validators are never aggregated.
    fn dropped_operator_rows(direction: OrderDirection) -> Vec<String> {
        by_operator(
            vec![
                operated("bigOfGone", 900, Some("Gone")),
                operated("smallOfGone", 800, Some("Gone")),
                operated("alone", 1, None),
                operated("mapped", 2, Some("Big")),
            ],
            vec![operator("Big", 2)],
            OrderField::Stake,
            &direction,
        )
    }

    #[test]
    fn an_operator_with_no_row_keeps_its_validators_together_at_the_tail() {
        assert_eq!(
            dropped_operator_rows(OrderDirection::DESC),
            vec!["mapped", "alone", "bigOfGone", "smallOfGone"]
        );
        assert_eq!(
            dropped_operator_rows(OrderDirection::ASC),
            vec!["alone", "mapped", "smallOfGone", "bigOfGone"],
            "the block stays last whichever way the sort runs, and stays contiguous"
        );
    }

    #[test]
    fn the_stake_delta_columns_order_the_validators_and_the_operators_alike() {
        let with_delta = |vote_account: &str, delta: i64, operator: Option<&str>| ValidatorRecord {
            stake_delta_7d: Some(Decimal::from(delta)),
            ..operated(vote_account, 100, operator)
        };
        assert_eq!(
            by_operator(
                vec![
                    with_delta("grewOfX", 400, Some("X")),
                    with_delta("shrankOfX", -100, Some("X")),
                    with_delta("alone", 200, None),
                    with_delta("ofY", 50, Some("Y")),
                ],
                vec![
                    ValidatorGroupRecord {
                        stake_delta_7d: Some(Decimal::from(300)),
                        ..operator("X", 100)
                    },
                    ValidatorGroupRecord {
                        stake_delta_7d: Some(Decimal::from(50)),
                        ..operator("Y", 100)
                    },
                ],
                OrderField::StakeDelta7d,
                &OrderDirection::DESC,
            ),
            vec!["grewOfX", "shrankOfX", "alone", "ofY"]
        );
    }

    #[test]
    fn every_validator_counts_as_one_so_a_count_leaves_them_on_the_vote_account_tiebreak() {
        assert_eq!(
            by_operator(
                vec![
                    operated("bbb", 1, Some("Big")),
                    operated("aaa", 900, Some("Big")),
                ],
                vec![operator("Big", 900)],
                OrderField::Validators,
                &OrderDirection::DESC,
            ),
            vec!["aaa", "bbb"]
        );
    }

    #[test]
    fn a_count_ranks_an_operator_above_the_validators_standing_alone() {
        let mut big = operator("Big", 900);
        big.validator_count = 2;

        assert_eq!(
            by_operator(
                vec![
                    operated("ofBig", 100, Some("Big")),
                    operated("alsoOfBig", 100, Some("Big")),
                    operated("alone", 900, None),
                ],
                vec![big],
                OrderField::Validators,
                &OrderDirection::DESC,
            ),
            vec!["alsoOfBig", "ofBig", "alone"],
            "two validators outrank one, however much stake the lone one holds"
        );
    }

    fn named(
        vote_account: &str,
        info_name: Option<&str>,
        operator: Option<&str>,
    ) -> ValidatorRecord {
        ValidatorRecord {
            info_name: info_name.map(str::to_string),
            ..operated(vote_account, 100, operator)
        }
    }

    #[test]
    fn ordering_by_name_orders_an_operators_validators_by_the_name_each_reports() {
        for (direction, expected) in [
            (
                OrderDirection::ASC,
                vec!["zzzAcme", "mmmUnnamed", "aaaZulu"],
            ),
            (
                OrderDirection::DESC,
                vec!["aaaZulu", "mmmUnnamed", "zzzAcme"],
            ),
        ] {
            assert_eq!(
                by_operator(
                    vec![
                        named("aaaZulu", Some("Zulu"), Some("Big")),
                        named("zzzAcme", Some("acme"), Some("Big")),
                        // No name of its own, so it sorts as its vote account.
                        named("mmmUnnamed", None, Some("Big")),
                    ],
                    vec![operator("Big", 900)],
                    OrderField::Name,
                    &direction,
                ),
                expected,
                "{direction:?}: the name folds case and beats the vote account"
            );
        }
    }

    #[test]
    fn ordering_by_name_treats_a_blank_name_as_none() {
        assert_eq!(
            by_operator(
                vec![
                    named("aaaBlank", Some("   "), Some("Big")),
                    named("zzzNamed", Some("bbb"), Some("Big")),
                ],
                vec![operator("Big", 900)],
                OrderField::Name,
                &OrderDirection::ASC,
            ),
            vec!["aaaBlank", "zzzNamed"],
            "a blank name is no name, so the vote account orders it"
        );
    }

    #[test]
    fn sorting_without_the_operator_groups_ignores_the_operator_a_validator_belongs_to() {
        let validators = sort_validators(
            vec![
                operated("small", 1, Some("Big")),
                operated("big", 900, None),
            ],
            OrderField::Stake,
            &OrderDirection::DESC,
        );
        assert_eq!(order(validators), vec!["big", "small"]);
    }

    #[test]
    fn incidents_and_delegation_relationships_order_validators_too() {
        let with_incidents = |vote_account: &str, count: usize| ValidatorRecord {
            incidents: vec![incident(600); count],
            ..validator(vote_account, 100, vec![])
        };
        assert_eq!(
            order(sort_validators(
                vec![with_incidents("quiet", 0), with_incidents("noisy", 3)],
                OrderField::Incidents,
                &OrderDirection::DESC
            )),
            vec!["noisy", "quiet"]
        );

        let with_delegators = |vote_account: &str, count: Option<u64>| ValidatorRecord {
            unique_delegators: count,
            ..validator(vote_account, 100, vec![])
        };
        assert_eq!(
            order(sort_validators(
                vec![
                    with_delegators("unmeasured", None),
                    with_delegators("few", Some(1)),
                    with_delegators("many", Some(900)),
                ],
                OrderField::DelegationRelationships,
                &OrderDirection::DESC
            )),
            vec!["many", "few", "unmeasured"],
            "a validator with no count is not one with none"
        );
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
    fn sort_ranks_a_declared_raise_above_a_validator_genuinely_charging_the_old_rate() {
        let mut validators = vec![
            validator("aaa_raised", 100, vec![]),
            validator("bbb_low", 100, vec![]),
        ];
        validators[0].commission_max_observed = Some(5);
        validators[0].commission_advertised = Some(10);
        validators[1].commission_max_observed = Some(5);
        validators[1].commission_advertised = Some(5);
        let validators = sort_validators(validators, OrderField::Commission, &OrderDirection::ASC);
        let order: Vec<_> = validators.iter().map(|v| v.vote_account.clone()).collect();
        assert_eq!(
            order,
            vec!["bbb_low", "aaa_raised"],
            "reading commission_max_observed alone is what used to tie a raise to the old rate"
        );
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
