mod common;

use common::{migrated_client, skip_without_database};
use std::collections::HashMap;
use store::dto::{IncidentDetail, IncidentRecord, ValidatorEpochStats, ValidatorRecord};
use store::utils::load_incidents;
use tokio_postgres::Client;

/// No records, so no block production incident can be derived and only the query is under test.
fn no_records() -> HashMap<String, ValidatorRecord> {
    HashMap::new()
}

/// One validator whose epoch produced too few of its leader slots to pass: 8 of 64 missed is 12.5%,
/// over any bar the rule can set.
fn skipped_epoch(vote_account: &str, epoch: u64) -> HashMap<String, ValidatorRecord> {
    HashMap::from([(
        vote_account.to_string(),
        ValidatorRecord {
            epoch_stats: vec![ValidatorEpochStats {
                epoch,
                leader_slots: 64,
                blocks_produced: 56,
                ..Default::default()
            }],
            ..Default::default()
        },
    )])
}

// `identity` and `vote_account` are the same string here; nothing the query reads distinguishes them.
async fn interval(
    client: &Client,
    vote_account: &str,
    status: &str,
    epoch: u64,
    start_at: &str,
    end_at: &str,
) {
    client
        .execute(
            "INSERT INTO uptimes (identity, vote_account, status, epoch, start_at, end_at)
             VALUES ($1, $1, $2, $3::TEXT::NUMERIC, $4::TEXT::TIMESTAMPTZ, $5::TEXT::TIMESTAMPTZ)",
            &[
                &vote_account,
                &status,
                &epoch.to_string(),
                &start_at,
                &end_at,
            ],
        )
        .await
        .unwrap();
}

async fn down(client: &Client, vote_account: &str, epoch: u64, start_at: &str, end_at: &str) {
    interval(client, vote_account, "DOWN", epoch, start_at, end_at).await
}

fn epochs(incidents: &[IncidentRecord]) -> Vec<u64> {
    incidents.iter().map(|incident| incident.epoch).collect()
}

// `uptimes` is written every minute and `validators` hourly, so epoch 102 is a live case: a DOWN row
// above the head for the hour the validator write takes to catch up.
#[tokio::test]
async fn the_window_is_closed_on_both_ends() {
    let schema = "ds_test_incidents_window";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    for (epoch, start_at, end_at) in [
        (99, "2026-01-01T00:00:00Z", "2026-01-01T00:05:00Z"),
        (100, "2026-02-01T00:00:00Z", "2026-02-01T00:05:00Z"),
        (101, "2026-03-01T00:00:00Z", "2026-03-01T00:05:00Z"),
        (102, "2026-04-01T00:00:00Z", "2026-04-01T00:05:00Z"),
    ] {
        down(&client, "voteA", epoch, start_at, end_at).await;
    }

    let incidents = load_incidents(&client, 100, 101, &no_records())
        .await
        .unwrap();

    assert_eq!(epochs(&incidents["voteA"]), vec![100, 101]);
}

#[tokio::test]
async fn an_up_interval_is_not_an_incident() {
    let schema = "ds_test_incidents_status";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    interval(
        &client,
        "voteA",
        "UP",
        100,
        "2026-01-01T00:00:00Z",
        "2026-01-02T00:00:00Z",
    )
    .await;

    assert!(load_incidents(&client, 100, 100, &no_records())
        .await
        .unwrap()
        .is_empty());
}

// Every filter the API applies afterwards reads `downtime_seconds`, including the restart-noise floor.
#[tokio::test]
async fn downtime_seconds_is_the_length_of_the_interval() {
    let schema = "ds_test_incidents_downtime";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    down(
        &client,
        "voteA",
        100,
        "2026-01-01T00:00:00Z",
        "2026-01-01T00:03:20Z",
    )
    .await;

    let incidents = load_incidents(&client, 100, 100, &no_records())
        .await
        .unwrap();

    let IncidentDetail::Downtime {
        downtime_seconds, ..
    } = incidents["voteA"][0].detail
    else {
        panic!("a DOWN row is a downtime incident");
    };
    assert_eq!(downtime_seconds, 200);
}

#[tokio::test]
async fn each_down_row_is_its_own_incident_oldest_first() {
    let schema = "ds_test_incidents_order";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    down(
        &client,
        "voteA",
        102,
        "2026-03-01T00:00:00Z",
        "2026-03-01T00:05:00Z",
    )
    .await;
    down(
        &client,
        "voteA",
        100,
        "2026-01-01T00:00:00Z",
        "2026-01-01T00:05:00Z",
    )
    .await;
    down(
        &client,
        "voteA",
        101,
        "2026-02-01T00:00:00Z",
        "2026-02-01T00:05:00Z",
    )
    .await;

    let incidents = load_incidents(&client, 100, 102, &no_records())
        .await
        .unwrap();

    assert_eq!(epochs(&incidents["voteA"]), vec![100, 101, 102]);
}

// Both symptoms of one epoch belong to one incident, so the epoch is served once with the block
// production numbers on the downtime row.
#[tokio::test]
async fn an_epoch_that_went_down_and_skipped_is_one_incident() {
    let schema = "ds_test_incidents_dedup";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    down(
        &client,
        "voteA",
        100,
        "2026-01-01T00:00:00Z",
        "2026-01-01T00:05:00Z",
    )
    .await;

    let incidents = load_incidents(&client, 100, 100, &skipped_epoch("voteA", 100))
        .await
        .unwrap();

    assert_eq!(epochs(&incidents["voteA"]), vec![100]);
    let IncidentDetail::Downtime {
        block_production, ..
    } = &incidents["voteA"][0].detail
    else {
        panic!("the downtime row is the one served");
    };
    assert_eq!(
        block_production.as_ref().map(|detail| detail.missed_slots),
        Some(8)
    );
}

#[tokio::test]
async fn an_epoch_that_only_skipped_is_its_own_incident() {
    let schema = "ds_test_incidents_block_production";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    let incidents = load_incidents(&client, 100, 100, &skipped_epoch("voteA", 100))
        .await
        .unwrap();

    assert_eq!(epochs(&incidents["voteA"]), vec![100]);
    assert!(matches!(
        incidents["voteA"][0].detail,
        IncidentDetail::BlockProduction { .. }
    ));
}

// The window bound is the query's, so a skipped epoch older than it is not served either.
#[tokio::test]
async fn a_skipped_epoch_before_from_epoch_is_left_out() {
    let schema = "ds_test_incidents_block_production_window";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    let incidents = load_incidents(&client, 100, 100, &skipped_epoch("voteA", 99))
        .await
        .unwrap();

    assert!(incidents.is_empty());
}
