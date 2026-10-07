use crate::directory::{Directory, Doc, Precondition};
use crate::docs::{
    NodeObservation, NodeObservationState, NodeObservationsDoc, LIVE_NODE_OBSERVATIONS,
};
use chrono::{DateTime, Utc};
use clap::Parser;
use collect::solana_service::NodeContact;
use collect::validators_performance::ValidatorsPerformanceSnapshot;
use log::info;
use serde_yaml;

#[derive(Debug, Parser)]
pub struct StoreNodeObservationsParams {
    #[arg(long = "snapshot-file")]
    snapshot_path: String,
}

pub async fn store_node_observations(
    params: StoreNodeObservationsParams,
    directory: &Directory,
) -> anyhow::Result<()> {
    info!("Storing node observations...");

    let snapshot_file = std::fs::File::open(params.snapshot_path)?;
    let snapshot: ValidatorsPerformanceSnapshot = serde_yaml::from_reader(snapshot_file)?;

    info!("Loaded the snapshot");

    let stored = directory
        .get::<NodeObservationsDoc>(LIVE_NODE_OBSERVATIONS)
        .await?;
    write_node_observations(directory, stored, &snapshot).await
}

pub async fn write_node_observations(
    directory: &Directory,
    stored: Option<Doc<NodeObservationsDoc>>,
    snapshot: &ValidatorsPerformanceSnapshot,
) -> anyhow::Result<()> {
    let created_at: DateTime<Utc> = snapshot.created_at.parse()?;
    let (mut observations, precondition) = match stored {
        Some(stored) => (stored.body, Precondition::IfMatch(stored.etag)),
        None => (NodeObservationsDoc::new(), Precondition::Create),
    };

    let (changed, unchanged) = apply_node_observations(&mut observations, snapshot, created_at);

    // No retry on a conflict: the cron is the retry.
    directory
        .put(LIVE_NODE_OBSERVATIONS, &observations, precondition)
        .await?;

    info!("Stored {changed} node observation changes, {unchanged} nodes unchanged");

    Ok(())
}

/// Records a change when the node advertises something new, and otherwise
/// re-stamps when it was last seen, so an unchanged node stays one
/// observation while still proving it was in gossip at this instant. The
/// epoch is not part of what is compared: it would record every node again
/// at each rollover.
pub fn apply_node_observations(
    observations: &mut NodeObservationsDoc,
    snapshot: &ValidatorsPerformanceSnapshot,
    created_at: DateTime<Utc>,
) -> (usize, usize) {
    let mut changed = 0;
    let mut unchanged = 0;

    for (identity, node) in snapshot.nodes.iter() {
        let sample = observation(node, snapshot, created_at);
        match observations.get_mut(identity) {
            Some(state) if state.last.same_node(&sample) => {
                let last_seen_at = state.last.last_seen_at.max(created_at);
                state.last.last_seen_at = last_seen_at;
                if let Some(recorded) = state.changes.last_mut() {
                    if recorded.created_at == state.last.created_at {
                        recorded.last_seen_at = last_seen_at;
                    }
                }
                unchanged += 1;
            }
            Some(state) => {
                state.changes.push(sample.clone());
                state.last = sample;
                changed += 1;
            }
            None => {
                observations.insert(
                    identity.clone(),
                    NodeObservationState {
                        last: sample.clone(),
                        changes: vec![sample],
                    },
                );
                changed += 1;
            }
        }
    }

    (changed, unchanged)
}

fn observation(
    node: &NodeContact,
    snapshot: &ValidatorsPerformanceSnapshot,
    created_at: DateTime<Utc>,
) -> NodeObservation {
    NodeObservation {
        ip: node.ip.clone(),
        gossip_port: node.gossip_port.map(i32::from),
        version: node.version.clone(),
        client_id: node.client_id.map(i32::from),
        client_id_raw: node.client_id_raw.clone(),
        feature_set: node.feature_set.map(i64::from),
        shred_version: node.shred_version.map(i32::from),
        rpc_public: Some(node.rpc_public),
        pubsub_public: Some(node.pubsub_public),
        epoch: snapshot.epoch,
        epoch_slot: snapshot.epoch_slot,
        created_at,
        last_seen_at: created_at,
    }
}
