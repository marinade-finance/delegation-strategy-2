use chrono::{DateTime, Utc};
use clap::Parser;
use collect::slot_params::baseline_slots_per_year;
use collect::solana_service::NodeContact;
use collect::validators_performance::{ClusterInflation, ValidatorsPerformanceSnapshot};
use std::collections::HashMap;
use store::close_epoch::{close_epoch, CloseEpochParams};
use store::directory::{Directory, Precondition};
use store::docs::{
    epoch_doc_path, ClusterInfoDoc, ClusterInfoSample, CommissionsDoc, NodeObservationsDoc,
    SealedNodeObservationsDoc, SnapshotDoc, UptimesDoc, VersionsDoc, LIVE_CLUSTER_INFO,
    LIVE_COMMISSIONS, LIVE_NODE_OBSERVATIONS, LIVE_UPTIMES, LIVE_VERSIONS, NODE_OBSERVATIONS_DIR,
    SNAPSHOT_DIR,
};
use store::node_observations::{store_node_observations, StoreNodeObservationsParams};

mod common;

const EPOCH: u64 = 1000;

fn at(moment: &str) -> DateTime<Utc> {
    moment.parse().expect("timestamp")
}

fn node(ip: Option<&str>, version: &str) -> NodeContact {
    NodeContact {
        ip: ip.map(Into::into),
        gossip_port: Some(8001),
        version: Some(version.into()),
        client_id: Some(3),
        client_id_raw: Some("Agave".into()),
        feature_set: Some(123),
        shred_version: Some(456),
        rpc_public: false,
        pubsub_public: false,
    }
}

fn nodes_snapshot(
    epoch: u64,
    created_at: &str,
    nodes: Vec<(&str, NodeContact)>,
) -> ValidatorsPerformanceSnapshot {
    ValidatorsPerformanceSnapshot {
        epoch,
        epoch_slot: 10,
        transaction_count: 0,
        created_at: created_at.into(),
        slots_per_year: baseline_slots_per_year(),
        cluster_inflation: Some(ClusterInflation {
            sol_total_supply: 0,
            inflation: 0f64,
            inflation_taper: 0f64,
        }),
        validators: HashMap::new(),
        nodes: nodes
            .into_iter()
            .map(|(identity, node)| (identity.to_string(), node))
            .collect(),
        rewards: None,
    }
}

async fn run_store_node_observations(
    directory: &Directory,
    name: &str,
    snapshot: &ValidatorsPerformanceSnapshot,
) {
    let path = common::write_yaml(name, snapshot);
    store_node_observations(
        StoreNodeObservationsParams::parse_from(["store", "--snapshot-file", &path]),
        directory,
    )
    .await
    .expect("store node observations");
    std::fs::remove_file(path).expect("remove snapshot");
}

async fn observations(directory: &Directory) -> NodeObservationsDoc {
    directory
        .get(LIVE_NODE_OBSERVATIONS)
        .await
        .expect("get node observations")
        .expect("node observations document")
        .body
}

#[tokio::test]
async fn node_observations_record_a_change_and_re_stamp_an_unchanged_node() {
    let Some(store) = common::directory_store("node-observations").await else {
        return;
    };
    let directory = store.client();

    run_store_node_observations(
        &directory,
        "nodes-first",
        &nodes_snapshot(
            EPOCH,
            "2026-01-01T00:00:00Z",
            vec![
                ("node", node(Some("1.1.1.1"), "2.3.0")),
                ("unstaked", node(None, "2.3.0")),
            ],
        ),
    )
    .await;
    let first = observations(&directory).await;
    assert_eq!(
        first.len(),
        2,
        "a node with no vote account is recorded too"
    );
    assert_eq!(first["node"].changes.len(), 1);
    assert_eq!(first["node"].last.ip.as_deref(), Some("1.1.1.1"));
    assert_eq!(first["node"].last.gossip_port, Some(8001));
    assert_eq!(first["node"].last.client_id_raw.as_deref(), Some("Agave"));
    assert_eq!(first["node"].last.rpc_public, Some(false));

    // The same node a minute on, rendered by another RPC, in the next epoch: nothing changed.
    let mut same = node(Some("1.1.1.1"), "2.3.0");
    same.client_id_raw = Some("Unknown(3)".into());
    run_store_node_observations(
        &directory,
        "nodes-unchanged",
        &nodes_snapshot(EPOCH + 1, "2026-01-01T00:01:00Z", vec![("node", same)]),
    )
    .await;
    let unchanged = observations(&directory).await;
    assert_eq!(
        unchanged["node"].changes.len(),
        1,
        "an epoch rollover and a per-RPC rendering record nothing"
    );
    assert_eq!(
        unchanged["node"].last.last_seen_at,
        at("2026-01-01T00:01:00Z"),
        "the observation is an interval that now reaches this run"
    );
    assert_eq!(
        unchanged["node"].changes[0].last_seen_at,
        at("2026-01-01T00:01:00Z"),
        "the recorded observation carries the same end"
    );
    assert_eq!(
        unchanged["unstaked"].last.last_seen_at,
        at("2026-01-01T00:00:00Z"),
        "a node absent from the snapshot keeps when it was last seen"
    );

    run_store_node_observations(
        &directory,
        "nodes-replayed",
        &nodes_snapshot(
            EPOCH,
            "2025-12-31T00:00:00Z",
            vec![("node", node(Some("1.1.1.1"), "2.3.0"))],
        ),
    )
    .await;
    assert_eq!(
        observations(&directory).await["node"].last.last_seen_at,
        at("2026-01-01T00:01:00Z"),
        "a replayed older snapshot does not move last_seen_at backward"
    );

    run_store_node_observations(
        &directory,
        "nodes-moved",
        &nodes_snapshot(
            EPOCH + 1,
            "2026-01-01T00:02:00Z",
            vec![("node", node(Some("2.2.2.2"), "2.3.0"))],
        ),
    )
    .await;
    let moved = observations(&directory).await;
    assert_eq!(
        moved["node"].changes.len(),
        2,
        "an address change is recorded"
    );
    assert_eq!(
        moved["node"].changes[0].last_seen_at,
        at("2026-01-01T00:01:00Z"),
        "the superseded observation keeps the end it had"
    );
    assert_eq!(moved["node"].last.ip.as_deref(), Some("2.2.2.2"));
    assert_eq!(moved["node"].last.epoch, EPOCH + 1);
}

#[tokio::test]
async fn close_epoch_seals_the_node_observations_of_the_epoch() {
    let Some(store) = common::directory_store("node-observations-seal").await else {
        return;
    };
    let directory = store.client();

    run_store_node_observations(
        &directory,
        "nodes-e",
        &nodes_snapshot(
            EPOCH,
            "2026-01-01T00:00:00Z",
            vec![("node", node(Some("1.1.1.1"), "2.3.0"))],
        ),
    )
    .await;
    run_store_node_observations(
        &directory,
        "nodes-e1",
        &nodes_snapshot(
            EPOCH + 1,
            "2026-01-03T00:00:00Z",
            vec![("node", node(Some("1.1.1.1"), "2.4.0"))],
        ),
    )
    .await;

    let sample = |created_at: &str| ClusterInfoSample {
        epoch: EPOCH,
        epoch_slot: 1,
        transaction_count: 0,
        created_at: at(created_at),
        slots_per_year: baseline_slots_per_year(),
    };
    directory
        .put(
            LIVE_CLUSTER_INFO,
            &ClusterInfoDoc {
                epoch: EPOCH,
                samples: vec![
                    sample("2026-01-01T00:00:00Z"),
                    sample("2026-01-02T23:00:00Z"),
                ],
            },
            Precondition::Create,
        )
        .await
        .expect("put cluster info");
    for (path, doc) in [
        (LIVE_UPTIMES, serde_json::to_value(UptimesDoc::new())),
        (
            LIVE_COMMISSIONS,
            serde_json::to_value(CommissionsDoc::new()),
        ),
        (LIVE_VERSIONS, serde_json::to_value(VersionsDoc::new())),
    ] {
        directory
            .put(path, &doc.expect("json"), Precondition::Create)
            .await
            .expect("put live stream");
    }
    directory
        .put(
            &epoch_doc_path(SNAPSHOT_DIR, EPOCH),
            &SnapshotDoc::new(),
            Precondition::Create,
        )
        .await
        .expect("put snapshot");

    let path = common::write_yaml(
        "nodes-close",
        &nodes_snapshot(EPOCH, "2026-01-02T23:00:00Z", vec![]),
    );
    close_epoch(
        CloseEpochParams::parse_from(["store", "--snapshot-file", &path]),
        &directory,
    )
    .await
    .expect("close epoch");
    std::fs::remove_file(path).expect("remove snapshot");

    let sealed: SealedNodeObservationsDoc = directory
        .get(&epoch_doc_path(NODE_OBSERVATIONS_DIR, EPOCH))
        .await
        .expect("get sealed")
        .expect("sealed node observations")
        .body;
    assert_eq!(sealed["node"].len(), 1);
    assert_eq!(sealed["node"][0].version.as_deref(), Some("2.3.0"));

    let live = observations(&directory).await;
    assert_eq!(
        live["node"].changes.len(),
        1,
        "the sealed epoch's change leaves the accumulator"
    );
    assert_eq!(live["node"].changes[0].epoch, EPOCH + 1);
    assert_eq!(
        live["node"].last.version.as_deref(),
        Some("2.4.0"),
        "the last observation outlives the seal"
    );
}
