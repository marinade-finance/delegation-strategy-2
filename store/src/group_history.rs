use crate::dto::{client_lineage, effective_client_id};
use crate::groups::is_unknown_placeholder;
use chrono::{DateTime, Utc};
use log::info;
use rust_decimal::prelude::*;
use std::collections::HashMap;
use tokio_postgres::Client;

/// When a group was first observed, read over the whole history the DB holds rather than the epochs
/// the cache carries.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FirstSeen {
    pub epoch: u64,
    /// Null while the epoch is still open, since it has no `epochs` row to take a date from.
    pub at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default)]
pub struct GroupFirstSeen {
    /// Keyed by the lowercased hosting organisation, the way the groups fold their keys.
    pub providers: HashMap<String, FirstSeen>,
    /// Keyed by the lowercased client lineage.
    pub client_lineages: HashMap<String, FirstSeen>,
    /// Earliest epoch any row names a client at all. The columns behind it landed in migration 0017
    /// and 0019 discarded what had been collected before the rename, so a client first seen in this
    /// epoch was most likely running before it.
    pub client_floor_epoch: Option<u64>,
}

impl GroupFirstSeen {
    fn keep_earliest(seen: &mut HashMap<String, FirstSeen>, key: String, candidate: FirstSeen) {
        seen.entry(key)
            .and_modify(|held| {
                if candidate.epoch < held.epoch {
                    *held = candidate;
                }
            })
            .or_insert(candidate);
    }
}

/// One row per group key, with the epoch it was first seen in.
struct FirstSeenRow {
    key: String,
    first_seen: FirstSeen,
}

fn first_seen_rows(rows: Vec<tokio_postgres::Row>) -> anyhow::Result<Vec<FirstSeenRow>> {
    rows.into_iter()
        .map(|row| {
            Ok(FirstSeenRow {
                key: row.get("key"),
                first_seen: FirstSeen {
                    epoch: u64::try_from(row.get::<_, Decimal>("first_epoch"))?,
                    at: row.get("first_seen_at"),
                },
            })
        })
        .collect()
}

pub async fn load_group_first_seen(psql_client: &Client) -> anyhow::Result<GroupFirstSeen> {
    info!("Loading group first seen epochs");

    let providers = first_seen_rows(
        psql_client
            .query(
                "
        SELECT
            LOWER(dc_aso) AS key,
            MIN(validators.epoch) AS first_epoch,
            MIN(epochs.start_at) AS first_seen_at
        FROM validators
        LEFT JOIN epochs ON epochs.epoch = validators.epoch
        WHERE dc_aso IS NOT NULL AND TRIM(dc_aso) <> ''
        GROUP BY 1
    ",
                &[],
            )
            .await?,
    )?;

    // The numeric id and the rendering the answering RPC returned are folded apart, the way
    // `effective_client_id` does it for the rows the cache holds.
    let clients = first_seen_rows(
        psql_client
            .query(
                "
        SELECT
            COALESCE(client_id::TEXT, client_id_raw) AS key,
            MIN(validators.epoch) AS first_epoch,
            MIN(epochs.start_at) AS first_seen_at
        FROM validators
        LEFT JOIN epochs ON epochs.epoch = validators.epoch
        WHERE client_id IS NOT NULL OR client_id_raw IS NOT NULL
        GROUP BY 1
    ",
                &[],
            )
            .await?,
    )?;

    let mut first_seen = GroupFirstSeen {
        client_floor_epoch: clients.iter().map(|row| row.first_seen.epoch).min(),
        ..Default::default()
    };

    for row in providers {
        if is_unknown_placeholder(&row.key) {
            continue;
        }
        GroupFirstSeen::keep_earliest(&mut first_seen.providers, row.key, row.first_seen);
    }

    for row in clients {
        let client_id = match row.key.parse::<u16>() {
            Ok(client_id) => Some(client_id),
            Err(_) => effective_client_id(None, Some(&row.key)),
        };
        if let Some(lineage) = client_lineage(client_id) {
            GroupFirstSeen::keep_earliest(
                &mut first_seen.client_lineages,
                lineage.to_lowercase(),
                row.first_seen,
            );
        }
    }

    Ok(first_seen)
}
