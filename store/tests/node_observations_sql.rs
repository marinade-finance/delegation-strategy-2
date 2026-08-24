mod common;

use chrono::{DateTime, Utc};
use collect::slot_params::baseline_slots_per_year;
use collect::solana_service::NodeContact;
use collect::validators_performance::ValidatorsPerformanceSnapshot;
use common::{migrated_client, skip_without_database, write_yaml};
use store::node_observations::{store_node_observations, StoreNodeObservationsParams};
use structopt::StructOpt;
use tokio_postgres::{Client, Row};

const EPOCH: u64 = 1000;

fn node(ip: Option<&str>) -> NodeContact {
    NodeContact {
        ip: ip.map(Into::into),
        gossip_port: Some(8001),
        version: Some("2.0.0".into()),
        client_id: Some(3),
        client_id_raw: Some("Agave".into()),
        feature_set: Some(123),
        shred_version: Some(456),
        rpc_public: false,
        pubsub_public: false,
    }
}

fn snapshot(
    epoch: u64,
    epoch_slot: u64,
    created_at: &str,
    nodes: Vec<(&str, NodeContact)>,
) -> ValidatorsPerformanceSnapshot {
    ValidatorsPerformanceSnapshot {
        epoch,
        epoch_slot,
        transaction_count: 1,
        created_at: created_at.into(),
        slots_per_year: baseline_slots_per_year(),
        cluster_inflation: None,
        validators: Default::default(),
        nodes: nodes
            .into_iter()
            .map(|(identity, node)| (identity.to_string(), node))
            .collect(),
        rewards: None,
    }
}

async fn run(client: &mut Client, name: &str, snapshot: &ValidatorsPerformanceSnapshot) {
    let path = write_yaml(name, &serde_yaml::to_string(snapshot).unwrap());
    store_node_observations(
        StoreNodeObservationsParams::from_iter(["store", "--snapshot-file", &path]),
        client,
    )
    .await
    .unwrap();
    std::fs::remove_file(path).unwrap();
}

async fn count(client: &Client) -> i64 {
    client
        .query_one("SELECT count(*) FROM node_observations", &[])
        .await
        .unwrap()
        .get(0)
}

async fn rows_for(client: &Client, identity: &str) -> Vec<Row> {
    client
        .query(
            "SELECT * FROM node_observations WHERE identity = $1 ORDER BY created_at, id",
            &[&identity],
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn a_first_run_records_every_node_field() {
    let schema = "ds_test_node_observations_first_run";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    run(
        &mut client,
        schema,
        &snapshot(
            EPOCH,
            10,
            "2026-07-31T00:00:00Z",
            vec![
                ("identityA", node(Some("1.2.3.4"))),
                ("identityB", node(Some("5.6.7.8"))),
                ("identityC", node(None)),
            ],
        ),
    )
    .await;

    assert_eq!(count(&client).await, 3);

    let rows = rows_for(&client, "identityA").await;
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(
        row.get::<_, Option<String>>("ip").as_deref(),
        Some("1.2.3.4")
    );
    assert_eq!(row.get::<_, Option<i32>>("gossip_port"), Some(8001));
    assert_eq!(
        row.get::<_, Option<String>>("version").as_deref(),
        Some("2.0.0")
    );
    assert_eq!(row.get::<_, Option<i32>>("client_id"), Some(3));
    assert_eq!(
        row.get::<_, Option<String>>("client_id_raw").as_deref(),
        Some("Agave")
    );
    assert_eq!(row.get::<_, Option<i64>>("feature_set"), Some(123));
    assert_eq!(row.get::<_, Option<i32>>("shred_version"), Some(456));
    assert_eq!(row.get::<_, Option<bool>>("rpc_public"), Some(false));
    assert_eq!(row.get::<_, Option<bool>>("pubsub_public"), Some(false));

    let unaddressed = rows_for(&client, "identityC").await;
    assert_eq!(unaddressed.len(), 1);
    assert_eq!(unaddressed[0].get::<_, Option<String>>("ip"), None);
}

#[tokio::test]
async fn an_identical_second_run_records_nothing() {
    let schema = "ds_test_node_observations_identical";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    let nodes = vec![
        ("identityA", node(Some("1.2.3.4"))),
        ("identityB", node(None)),
    ];
    run(
        &mut client,
        schema,
        &snapshot(EPOCH, 10, "2026-07-31T00:00:00Z", nodes.clone()),
    )
    .await;
    assert_eq!(count(&client).await, 2);

    run(
        &mut client,
        schema,
        &snapshot(EPOCH, 20, "2026-07-31T00:01:00Z", nodes),
    )
    .await;
    assert_eq!(count(&client).await, 2);
}

#[tokio::test]
async fn an_ip_change_appends_exactly_one_row_and_keeps_the_old_one() {
    let schema = "ds_test_node_observations_ip_change";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    run(
        &mut client,
        schema,
        &snapshot(
            EPOCH,
            10,
            "2026-07-31T00:00:00Z",
            vec![("identityA", node(Some("1.2.3.4")))],
        ),
    )
    .await;
    run(
        &mut client,
        schema,
        &snapshot(
            EPOCH,
            20,
            "2026-07-31T00:01:00Z",
            vec![("identityA", node(Some("9.9.9.9")))],
        ),
    )
    .await;

    let rows = rows_for(&client, "identityA").await;
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows[0].get::<_, Option<String>>("ip").as_deref(),
        Some("1.2.3.4")
    );
    assert_eq!(
        rows[1].get::<_, Option<String>>("ip").as_deref(),
        Some("9.9.9.9")
    );
}

// The versions table keys on epoch too, so every validator re-inserts at each rollover. This table must not.
#[tokio::test]
async fn an_epoch_rollover_alone_records_nothing() {
    let schema = "ds_test_node_observations_rollover";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    let nodes = vec![("identityA", node(Some("1.2.3.4")))];
    run(
        &mut client,
        schema,
        &snapshot(EPOCH, 431_000, "2026-07-31T00:00:00Z", nodes.clone()),
    )
    .await;
    run(
        &mut client,
        schema,
        &snapshot(EPOCH + 1, 5, "2026-07-31T00:01:00Z", nodes),
    )
    .await;

    assert_eq!(count(&client).await, 1);
}

#[tokio::test]
async fn a_client_id_raw_only_change_records_nothing() {
    let schema = "ds_test_node_observations_client_id_raw";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    let mut renamed = node(Some("1.2.3.4"));
    renamed.client_id_raw = Some("Unknown(3)".into());

    run(
        &mut client,
        schema,
        &snapshot(
            EPOCH,
            10,
            "2026-07-31T00:00:00Z",
            vec![("identityA", node(Some("1.2.3.4")))],
        ),
    )
    .await;
    run(
        &mut client,
        schema,
        &snapshot(
            EPOCH,
            20,
            "2026-07-31T00:01:00Z",
            vec![("identityA", renamed)],
        ),
    )
    .await;

    assert_eq!(count(&client).await, 1);
    assert_eq!(
        rows_for(&client, "identityA").await[0]
            .get::<_, Option<String>>("client_id_raw")
            .as_deref(),
        Some("Agave")
    );
}

#[tokio::test]
async fn gaining_and_losing_an_address_both_record() {
    let schema = "ds_test_node_observations_null_transitions";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    run(
        &mut client,
        schema,
        &snapshot(
            EPOCH,
            10,
            "2026-07-31T00:00:00Z",
            vec![("identityA", node(None))],
        ),
    )
    .await;
    run(
        &mut client,
        schema,
        &snapshot(
            EPOCH,
            20,
            "2026-07-31T00:01:00Z",
            vec![("identityA", node(Some("1.2.3.4")))],
        ),
    )
    .await;
    run(
        &mut client,
        schema,
        &snapshot(
            EPOCH,
            30,
            "2026-07-31T00:02:00Z",
            vec![("identityA", node(None))],
        ),
    )
    .await;

    let ips: Vec<Option<String>> = rows_for(&client, "identityA")
        .await
        .iter()
        .map(|row| row.get("ip"))
        .collect();
    assert_eq!(ips, vec![None, Some("1.2.3.4".to_string()), None]);
}

// The coverage promise: this table is fed from gossip, so a node that votes for nothing still lands.
#[tokio::test]
async fn a_node_with_no_vote_account_is_recorded() {
    let schema = "ds_test_node_observations_non_voting";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    let mut snapshot = snapshot(
        EPOCH,
        10,
        "2026-07-31T00:00:00Z",
        vec![("identityRpcOnly", node(Some("1.2.3.4")))],
    );
    snapshot.validators = Default::default();
    run(&mut client, schema, &snapshot).await;

    assert_eq!(rows_for(&client, "identityRpcOnly").await.len(), 1);
}

// 1200 nodes cross the 500-row chunk boundary; unchunked this would head for the 65535-parameter cap.
#[tokio::test]
async fn more_nodes_than_one_chunk_are_all_recorded() {
    let schema = "ds_test_node_observations_chunking";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    let identities: Vec<String> = (0..1200).map(|i| format!("identityChunk{i}")).collect();
    let nodes: Vec<(&str, NodeContact)> = identities
        .iter()
        .map(|identity| (identity.as_str(), node(Some("1.2.3.4"))))
        .collect();

    run(
        &mut client,
        schema,
        &snapshot(EPOCH, 10, "2026-07-31T00:00:00Z", nodes.clone()),
    )
    .await;
    assert_eq!(count(&client).await, 1200);

    run(
        &mut client,
        schema,
        &snapshot(EPOCH, 20, "2026-07-31T00:01:00Z", nodes),
    )
    .await;
    assert_eq!(count(&client).await, 1200);
}

#[tokio::test]
async fn an_rpc_public_flip_records_a_row() {
    let schema = "ds_test_node_observations_rpc_public";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    let mut serving = node(Some("1.2.3.4"));
    serving.rpc_public = true;

    run(
        &mut client,
        schema,
        &snapshot(
            EPOCH,
            10,
            "2026-07-31T00:00:00Z",
            vec![("identityA", node(Some("1.2.3.4")))],
        ),
    )
    .await;
    run(
        &mut client,
        schema,
        &snapshot(
            EPOCH,
            20,
            "2026-07-31T00:01:00Z",
            vec![("identityA", serving)],
        ),
    )
    .await;

    let flags: Vec<Option<bool>> = rows_for(&client, "identityA")
        .await
        .iter()
        .map(|row| row.get("rpc_public"))
        .collect();
    assert_eq!(flags, vec![Some(false), Some(true)]);
}

// The gap P1 of the review turned on: an unchanged node appends nothing, so only last_seen_at can
// still prove it is in the cluster.
#[tokio::test]
async fn an_unchanged_node_keeps_one_row_but_advances_last_seen_at() {
    let schema = "ds_test_node_observations_last_seen";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    let nodes = vec![("identityA", node(Some("1.2.3.4")))];
    run(
        &mut client,
        schema,
        &snapshot(EPOCH, 10, "2026-07-31T00:00:00Z", nodes.clone()),
    )
    .await;
    run(
        &mut client,
        schema,
        &snapshot(EPOCH, 20, "2026-08-30T00:00:00Z", nodes),
    )
    .await;

    let rows = rows_for(&client, "identityA").await;
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].get::<_, DateTime<Utc>>("created_at"),
        "2026-07-31T00:00:00Z".parse::<DateTime<Utc>>().unwrap()
    );
    assert_eq!(
        rows[0].get::<_, DateTime<Utc>>("last_seen_at"),
        "2026-08-30T00:00:00Z".parse::<DateTime<Utc>>().unwrap()
    );
}

// Backfilling an old snapshot must not shorten the observed interval, or the address drops out of the in-use window ip-info selects on.
#[tokio::test]
async fn a_replayed_older_snapshot_does_not_move_last_seen_at_backward() {
    let schema = "ds_test_node_observations_replay";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    let nodes = vec![("identityA", node(Some("1.2.3.4")))];
    run(
        &mut client,
        schema,
        &snapshot(EPOCH, 20, "2026-08-30T00:00:00Z", nodes.clone()),
    )
    .await;
    run(
        &mut client,
        schema,
        &snapshot(EPOCH, 10, "2026-07-31T00:00:00Z", nodes),
    )
    .await;

    let rows = rows_for(&client, "identityA").await;
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].get::<_, DateTime<Utc>>("created_at"),
        "2026-08-30T00:00:00Z".parse::<DateTime<Utc>>().unwrap()
    );
    assert_eq!(
        rows[0].get::<_, DateTime<Utc>>("last_seen_at"),
        "2026-08-30T00:00:00Z".parse::<DateTime<Utc>>().unwrap()
    );
}

// A node that stops gossiping must stop advancing, or the departure is invisible.
#[tokio::test]
async fn a_node_absent_from_the_snapshot_keeps_its_old_last_seen_at() {
    let schema = "ds_test_node_observations_departed";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    run(
        &mut client,
        schema,
        &snapshot(
            EPOCH,
            10,
            "2026-07-31T00:00:00Z",
            vec![
                ("identityStays", node(Some("1.2.3.4"))),
                ("identityLeaves", node(Some("5.6.7.8"))),
            ],
        ),
    )
    .await;
    run(
        &mut client,
        schema,
        &snapshot(
            EPOCH,
            20,
            "2026-08-30T00:00:00Z",
            vec![("identityStays", node(Some("1.2.3.4")))],
        ),
    )
    .await;

    assert_eq!(
        rows_for(&client, "identityLeaves").await[0].get::<_, DateTime<Utc>>("last_seen_at"),
        "2026-07-31T00:00:00Z".parse::<DateTime<Utc>>().unwrap()
    );
    assert_eq!(
        rows_for(&client, "identityStays").await[0].get::<_, DateTime<Utc>>("last_seen_at"),
        "2026-08-30T00:00:00Z".parse::<DateTime<Utc>>().unwrap()
    );
}

// A changed node gets a fresh row; the superseded one must keep the last_seen_at it actually had.
#[tokio::test]
async fn a_changed_node_does_not_advance_the_superseded_row() {
    let schema = "ds_test_node_observations_superseded";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    run(
        &mut client,
        schema,
        &snapshot(
            EPOCH,
            10,
            "2026-07-31T00:00:00Z",
            vec![("identityA", node(Some("1.2.3.4")))],
        ),
    )
    .await;
    run(
        &mut client,
        schema,
        &snapshot(
            EPOCH,
            20,
            "2026-08-30T00:00:00Z",
            vec![("identityA", node(Some("9.9.9.9")))],
        ),
    )
    .await;

    let rows = rows_for(&client, "identityA").await;
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows[0].get::<_, DateTime<Utc>>("last_seen_at"),
        "2026-07-31T00:00:00Z".parse::<DateTime<Utc>>().unwrap()
    );
    assert_eq!(
        rows[1].get::<_, DateTime<Utc>>("last_seen_at"),
        "2026-08-30T00:00:00Z".parse::<DateTime<Utc>>().unwrap()
    );
}
