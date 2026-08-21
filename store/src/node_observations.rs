use crate::utils::InsertQueryCombiner;
use chrono::{DateTime, Utc};
use collect::solana_service::NodeContact;
use collect::validators_performance::ValidatorsPerformanceSnapshot;
use log::info;
use rust_decimal::prelude::*;
use serde_yaml;
use std::collections::HashMap;
use structopt::StructOpt;
use tokio_postgres::{types::ToSql, Client};

#[derive(Debug, StructOpt)]
pub struct StoreNodeObservationsParams {
    #[structopt(long = "snapshot-file")]
    snapshot_path: String,
}

const DEFAULT_CHUNK_SIZE: usize = 500;

// Only what a new row is worth recording for. client_id_raw renders per answering RPC rather than
// per node, and an epoch in here would re-insert every node at each rollover the way versions does.
#[derive(PartialEq)]
struct NodeKey {
    ip: Option<String>,
    gossip_port: Option<i32>,
    version: Option<String>,
    client_id: Option<i32>,
    feature_set: Option<i64>,
    shred_version: Option<i32>,
    rpc_public: Option<bool>,
    pubsub_public: Option<bool>,
}

impl From<&NodeContact> for NodeKey {
    fn from(node: &NodeContact) -> Self {
        Self {
            ip: node.ip.clone(),
            gossip_port: node.gossip_port.map(|p| p as i32),
            version: node.version.clone(),
            client_id: node.client_id.map(|id| id as i32),
            feature_set: node.feature_set.map(|f| f as i64),
            shred_version: node.shred_version.map(|s| s as i32),
            rpc_public: Some(node.rpc_public),
            pubsub_public: Some(node.pubsub_public),
        }
    }
}

struct ObservationRow {
    identity: String,
    key: NodeKey,
    client_id_raw: Option<String>,
}

// Re-stamps the node's newest row instead of appending one, so an unchanged node stays a single row
// while still proving it was in gossip at this instant.
async fn touch_last_seen(
    psql_client: &Client,
    identities: &[String],
    last_seen_at: DateTime<Utc>,
) -> anyhow::Result<u64> {
    if identities.is_empty() {
        return Ok(0);
    }

    Ok(psql_client
        .execute(
            "
        UPDATE node_observations o
        SET last_seen_at = $2
        FROM (
            SELECT DISTINCT ON (identity) id
            FROM node_observations
            WHERE identity = ANY($1)
            ORDER BY identity, created_at DESC, id DESC
        ) newest
        WHERE o.id = newest.id
    ",
            &[&identities, &last_seen_at],
        )
        .await?)
}

pub async fn store_node_observations(
    params: StoreNodeObservationsParams,
    psql_client: &mut Client,
) -> anyhow::Result<()> {
    info!("Storing node observations...");

    let snapshot_file = std::fs::File::open(params.snapshot_path)?;
    let snapshot: ValidatorsPerformanceSnapshot = serde_yaml::from_reader(snapshot_file)?;
    let snapshot_epoch_slot: Decimal = snapshot.epoch_slot.into();
    let snapshot_epoch: Decimal = snapshot.epoch.into();
    let snapshot_created_at: DateTime<Utc> = snapshot.created_at.parse()?;

    info!("Loaded the snapshot");

    let mut previous: HashMap<String, NodeKey> = Default::default();
    for row in psql_client
        .query(
            "
        SELECT DISTINCT ON (identity)
            identity,
            ip,
            gossip_port,
            version,
            client_id,
            feature_set,
            shred_version,
            rpc_public,
            pubsub_public
        FROM node_observations
        ORDER BY identity, created_at DESC, id DESC
    ",
            &[],
        )
        .await?
    {
        previous.insert(
            row.get("identity"),
            NodeKey {
                ip: row.get("ip"),
                gossip_port: row.get("gossip_port"),
                version: row.get("version"),
                client_id: row.get("client_id"),
                feature_set: row.get("feature_set"),
                shred_version: row.get("shred_version"),
                rpc_public: row.get("rpc_public"),
                pubsub_public: row.get("pubsub_public"),
            },
        );
    }

    // Comparing in Rust, not SQL: a node that gained or lost an address must count as changed, and
    // NULL = NULL in SQL is UNKNOWN.
    let mut unchanged_identities: Vec<String> = Vec::new();
    let mut rows_to_insert: Vec<ObservationRow> = Vec::new();
    for (identity, node) in snapshot.nodes {
        let row = ObservationRow {
            key: NodeKey::from(&node),
            client_id_raw: node.client_id_raw,
            identity,
        };
        if previous.get(&row.identity) == Some(&row.key) {
            unchanged_identities.push(row.identity);
        } else {
            rows_to_insert.push(row);
        }
    }

    let touched = touch_last_seen(psql_client, &unchanged_identities, snapshot_created_at).await?;

    let mut insertions = 0;
    for chunk in rows_to_insert.chunks(DEFAULT_CHUNK_SIZE) {
        let mut query = InsertQueryCombiner::new(
            "node_observations".to_string(),
            "identity, ip, gossip_port, version, client_id, client_id_raw, feature_set, shred_version, rpc_public, pubsub_public, epoch_slot, epoch, created_at, last_seen_at".to_string(),
        );
        for row in chunk {
            let mut params: Vec<&(dyn ToSql + Sync)> = vec![
                &row.identity,
                &row.key.ip,
                &row.key.gossip_port,
                &row.key.version,
                &row.key.client_id,
                &row.client_id_raw,
                &row.key.feature_set,
                &row.key.shred_version,
                &row.key.rpc_public,
                &row.key.pubsub_public,
                &snapshot_epoch_slot,
                &snapshot_epoch,
                &snapshot_created_at,
                &snapshot_created_at,
            ];
            query.add(&mut params);
        }
        insertions += query.execute(psql_client).await?.unwrap_or(0);
    }

    info!("Stored {insertions} node observation changes, {touched} nodes unchanged");

    Ok(())
}
