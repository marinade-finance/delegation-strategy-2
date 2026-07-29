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
    pub search_properties: Option<bool>,
    pub query_from_date: Option<DateTime<Utc>>,
    pub epochs: usize,
}

pub async fn get_validators(
    context: WrappedContext,
    config: GetValidatorsConfig,
) -> anyhow::Result<Vec<ValidatorRecord>> {
    let ctx = context.read().await;

    let mut validators = filter_validators(&ctx.cache.validators, &config);

    let field_extractor = get_field_extractor(config.order_field);

    validators.sort_by(|a, b| match config.order_direction {
        OrderDirection::ASC => field_extractor(a).cmp(&field_extractor(b)),
        OrderDirection::DESC => field_extractor(b).cmp(&field_extractor(a)),
    });
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
        .map(|v| {
            let mut v = v.clone();
            match config.query_from_date {
                Some(from_date) => v
                    .epoch_stats
                    .retain(|es| es.epoch_start_at.is_some_and(|start| start > from_date)),
                None => v.epoch_stats.retain(|es| es.epoch >= min_epoch),
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

pub fn filter_validators<'a>(
    validators: &'a HashMap<String, ValidatorRecord>,
    config: &GetValidatorsConfig,
) -> Vec<&'a ValidatorRecord> {
    let last_epoch = validators
        .values()
        .flat_map(|validator| &validator.epoch_stats)
        .map(|epoch_stat| epoch_stat.epoch)
        .max()
        .unwrap_or(0);

    let min_required_epoch = last_epoch.saturating_sub(MIN_REQUIRED_EPOCHS_IN_THE_PAST);
    let last_epochs_with_credits_or_stake_start =
        last_epoch.saturating_sub(MIN_REQUIRED_EPOCHS_WITH_CREDITS_OR_STAKE);

    let mut validators: Vec<&ValidatorRecord> = validators.values().collect();

    validators.retain(|validator| {
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
        validators.retain(|validator| validator.foundation_stake.gt(&Decimal::ZERO))
    }

    if let Some(vote_accounts) = &config.query_vote_accounts {
        validators.retain(|v| vote_accounts.contains(&v.vote_account));
    }

    if let Some(identities) = &config.query_identities {
        validators.retain(|v| identities.contains(&v.identity));
    }

    if let Some(query) = &config.query {
        let query = query.to_lowercase();
        let search_properties = config.search_properties.unwrap_or(false);
        validators.retain(|v| {
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
        validators.retain(|v| v.superminority == query_superminority);
    }

    if let Some(query_marinade_stake) = config.query_marinade_stake {
        validators.retain(|v| (v.marinade_stake > Decimal::from(0)) == query_marinade_stake);
    }

    if let Some(query_with_names) = config.query_with_names {
        validators.retain(|v| query_with_names == v.info_name.is_some());
    }

    if let Some(query_score) = config.query_score {
        validators.retain(|v| (v.score.unwrap_or(0.0) > 0.0) == query_score);
    }

    if let Some(query_incident_free) = config.query_incident_free {
        validators.retain(|v| v.incidents.is_empty() == query_incident_free);
    }

    validators
}

#[cfg(test)]
mod tests {
    use super::*;
    use store::dto::ValidatorEpochStats;

    const LAST_EPOCH: u64 = 100;

    fn config() -> GetValidatorsConfig {
        GetValidatorsConfig {
            order_direction: DEFAULT_ORDER_DIRECTION,
            order_field: DEFAULT_ORDER_FIELD,
            offset: 0,
            limit: DEFAULT_LIMIT,
            query: None,
            query_identities: None,
            query_vote_accounts: None,
            query_superminority: None,
            query_score: None,
            query_marinade_stake: None,
            query_with_names: None,
            query_sfdp: None,
            query_incident_free: None,
            search_properties: None,
            query_from_date: None,
            epochs: DEFAULT_EPOCHS,
        }
    }

    fn record(vote_account: &str, identity: &str, epochs: &[u64]) -> ValidatorRecord {
        ValidatorRecord {
            vote_account: vote_account.into(),
            identity: identity.into(),
            epoch_stats: epochs
                .iter()
                .map(|&epoch| ValidatorEpochStats {
                    epoch,
                    credits: 1,
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }
    }

    // production keys this map by vote_account (store::utils::load_validators), and the
    // query_vote_accounts filter relies on that equivalence
    fn cache_of(records: Vec<ValidatorRecord>) -> HashMap<String, ValidatorRecord> {
        records
            .into_iter()
            .map(|record| (record.vote_account.clone(), record))
            .collect()
    }

    fn kept(
        validators: &HashMap<String, ValidatorRecord>,
        config: &GetValidatorsConfig,
    ) -> Vec<String> {
        let mut kept: Vec<String> = filter_validators(validators, config)
            .into_iter()
            .map(|v| v.vote_account.clone())
            .collect();
        kept.sort();
        kept
    }

    #[test]
    fn drops_validators_missing_either_of_the_last_two_epochs() {
        let validators = cache_of(vec![
            record("full", "id-full", &[LAST_EPOCH - 1, LAST_EPOCH]),
            record("gap", "id-gap", &[LAST_EPOCH]),
            record("stale", "id-stale", &[LAST_EPOCH - 2, LAST_EPOCH - 1]),
        ]);

        assert_eq!(kept(&validators, &config()), vec!["full"]);
    }

    #[test]
    fn drops_validators_without_credits_or_stake_in_the_last_two_epochs() {
        let mut idle = record("idle", "id-idle", &[LAST_EPOCH - 1, LAST_EPOCH]);
        for stats in idle.epoch_stats.iter_mut() {
            stats.credits = 0;
        }
        let mut staked = record("staked", "id-staked", &[LAST_EPOCH - 1, LAST_EPOCH]);
        for stats in staked.epoch_stats.iter_mut() {
            stats.credits = 0;
            stats.activated_stake = Decimal::from(1);
        }

        let validators = cache_of(vec![
            idle,
            staked,
            record("active", "id-active", &[LAST_EPOCH - 1, LAST_EPOCH]),
        ]);

        assert_eq!(kept(&validators, &config()), vec!["active", "staked"]);
    }

    #[test]
    fn filters_by_vote_account() {
        let validators = cache_of(vec![
            record("wanted", "id-wanted", &[LAST_EPOCH - 1, LAST_EPOCH]),
            record("other", "id-other", &[LAST_EPOCH - 1, LAST_EPOCH]),
        ]);

        let config = GetValidatorsConfig {
            query_vote_accounts: Some(vec!["wanted".to_string()]),
            ..config()
        };

        assert_eq!(kept(&validators, &config), vec!["wanted"]);
    }

    #[test]
    fn filters_by_identity() {
        let validators = cache_of(vec![
            record("wanted", "id-wanted", &[LAST_EPOCH - 1, LAST_EPOCH]),
            record("other", "id-other", &[LAST_EPOCH - 1, LAST_EPOCH]),
        ]);

        let config = GetValidatorsConfig {
            query_identities: Some(vec!["id-wanted".to_string()]),
            ..config()
        };

        assert_eq!(kept(&validators, &config), vec!["wanted"]);
    }

    #[test]
    fn text_query_matches_name_vote_account_and_identity_but_not_location_by_default() {
        let mut named = record("aaa", "id-aaa", &[LAST_EPOCH - 1, LAST_EPOCH]);
        named.info_name = Some("Alice Node".to_string());
        let mut located = record("bbb", "id-bbb", &[LAST_EPOCH - 1, LAST_EPOCH]);
        located.dc_city = Some("Alice Springs".to_string());

        let validators = cache_of(vec![
            named,
            located,
            record("alice-vote", "id-ccc", &[LAST_EPOCH - 1, LAST_EPOCH]),
            record("ddd", "identity-alice", &[LAST_EPOCH - 1, LAST_EPOCH]),
        ]);

        let config = GetValidatorsConfig {
            query: Some("ALICE".to_string()),
            ..config()
        };
        assert_eq!(kept(&validators, &config), vec!["aaa", "alice-vote", "ddd"]);

        let config = GetValidatorsConfig {
            search_properties: Some(true),
            ..config
        };
        assert_eq!(
            kept(&validators, &config),
            vec!["aaa", "alice-vote", "bbb", "ddd"]
        );
    }

    // Context owns a live Client, so get_validators cannot be exercised without a database
    // even though it never queries one
    async fn context_with(records: Vec<ValidatorRecord>) -> Option<WrappedContext> {
        let url = std::env::var("DS_TEST_POSTGRES_URL").ok()?;
        let (client, connection) = tokio_postgres::connect(&url, tokio_postgres::NoTls)
            .await
            .unwrap();
        tokio::spawn(async move {
            let _ = connection.await;
        });

        let mut context = crate::context::Context::new(
            client,
            Default::default(),
            Default::default(),
            Default::default(),
        )
        .unwrap();
        context.cache.validators = cache_of(records);
        Some(std::sync::Arc::new(tokio::sync::RwLock::new(context)))
    }

    fn stake_record(vote_account: &str, epochs: &[u64], stake: u64) -> ValidatorRecord {
        let mut record = record(vote_account, &format!("id-{vote_account}"), epochs);
        record.activated_stake = Decimal::from(stake);
        record
    }

    #[tokio::test]
    async fn get_validators_orders_by_stake_and_pages_without_touching_the_rest() {
        let records = vec![
            stake_record("small", &[LAST_EPOCH - 1, LAST_EPOCH], 10),
            stake_record("large", &[LAST_EPOCH - 1, LAST_EPOCH], 30),
            stake_record("medium", &[LAST_EPOCH - 1, LAST_EPOCH], 20),
        ];
        let Some(context) = context_with(records).await else {
            eprintln!("skipping: DS_TEST_POSTGRES_URL is not set");
            return;
        };

        let all = get_validators(context.clone(), config()).await.unwrap();
        assert_eq!(
            all.iter()
                .map(|v| v.vote_account.as_str())
                .collect::<Vec<_>>(),
            vec!["large", "medium", "small"],
            "default order is stake descending"
        );

        let page = get_validators(
            context.clone(),
            GetValidatorsConfig {
                offset: 1,
                limit: 1,
                ..config()
            },
        )
        .await
        .unwrap();
        assert_eq!(
            page.iter()
                .map(|v| v.vote_account.as_str())
                .collect::<Vec<_>>(),
            vec!["medium"],
            "offset and limit apply to the sorted order"
        );

        let ascending = get_validators(
            context,
            GetValidatorsConfig {
                order_direction: OrderDirection::ASC,
                ..config()
            },
        )
        .await
        .unwrap();
        assert_eq!(
            ascending
                .iter()
                .map(|v| v.vote_account.as_str())
                .collect::<Vec<_>>(),
            vec!["small", "medium", "large"]
        );
    }

    #[tokio::test]
    async fn get_validators_trims_epoch_stats_to_the_requested_window() {
        let epochs: Vec<u64> = (LAST_EPOCH - 4..=LAST_EPOCH).collect();
        let Some(context) = context_with(vec![stake_record("only", &epochs, 10)]).await else {
            eprintln!("skipping: DS_TEST_POSTGRES_URL is not set");
            return;
        };

        let two = get_validators(
            context.clone(),
            GetValidatorsConfig {
                epochs: 2,
                ..config()
            },
        )
        .await
        .unwrap();
        assert_eq!(
            two[0]
                .epoch_stats
                .iter()
                .map(|es| es.epoch)
                .collect::<Vec<_>>(),
            vec![LAST_EPOCH - 1, LAST_EPOCH],
            "epochs=2 keeps only the newest two, in their stored order"
        );

        let none = get_validators(
            context,
            GetValidatorsConfig {
                epochs: 0,
                ..config()
            },
        )
        .await
        .unwrap();
        assert_eq!(none.len(), 1, "the validator is still returned");
        assert!(
            none[0].epoch_stats.is_empty(),
            "epochs=0 is what marinade-web sends: the record stays, every epoch stat goes"
        );
    }

    #[test]
    fn filters_by_presence_of_a_name() {
        let mut named = record("named", "id-named", &[LAST_EPOCH - 1, LAST_EPOCH]);
        named.info_name = Some("Alice".to_string());
        let validators = cache_of(vec![
            named,
            record("nameless", "id-nameless", &[LAST_EPOCH - 1, LAST_EPOCH]),
        ]);

        let config = GetValidatorsConfig {
            query_with_names: Some(true),
            ..config()
        };
        assert_eq!(kept(&validators, &config), vec!["named"]);

        let config = GetValidatorsConfig {
            query_with_names: Some(false),
            ..config
        };
        assert_eq!(kept(&validators, &config), vec!["nameless"]);
    }
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
        search_properties: query_params.search_properties,
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
