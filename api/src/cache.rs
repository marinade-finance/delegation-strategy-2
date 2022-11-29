use log::{error, info};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::{collections::HashMap, sync::Arc};
use store::dto::ValidatorRecord;
use tokio::{
    sync::RwLock,
    time::{sleep, Duration},
};
use tokio_postgres::Client;

pub struct Cache {
    pub validators: HashMap<String, ValidatorRecord>,
}

impl Cache {
    pub fn new() -> Self {
        Self {
            validators: Default::default(),
        }
    }

    pub async fn get_validators(
        &self,
        config: GetValidatorsConfig,
    ) -> anyhow::Result<Vec<ValidatorRecord>> {
        let validators: Vec<_> = if let Some(identities) = config.query_identities {
            identities
                .iter()
                .filter_map(|i| self.validators.get(i))
                .collect()
        } else {
            self.validators.values().collect()
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
                |a: &&ValidatorRecord, b: &&ValidatorRecord| {
                    a.activated_stake.cmp(&b.activated_stake)
                }
            }
            (OrderField::Stake, OrderDirection::DESC) => {
                |a: &&ValidatorRecord, b: &&ValidatorRecord| {
                    b.activated_stake.cmp(&a.activated_stake)
                }
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
