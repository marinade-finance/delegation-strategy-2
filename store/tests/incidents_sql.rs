mod common;

use chrono::{DateTime, Utc};
use common::{migrated_client, skip_without_database};
use std::collections::HashMap;
use store::dto::{ValidatorEpochStats, ValidatorRecord};
use store::incidents::ValidatorIncidents;
use store::utils::load_validator_incidents;
use tokio_postgres::Client;

/// No records, so no block production can be derived and only the query is under test.
fn no_records() -> HashMap<String, ValidatorRecord> {
    HashMap::new()
}

/// One validator that missed 8 of its 64 leader slots in the given epoch.
fn epoch_stats(vote_account: &str, epoch: u64) -> HashMap<String, ValidatorRecord> {
    // Block production is only recorded for a closed epoch, so the boundaries have to be set as
    // the `epochs` join would set them.
    let epoch_start_at: DateTime<Utc> = "2026-01-01T00:00:00Z".parse().unwrap();
    HashMap::from([(
        vote_account.to_string(),
        ValidatorRecord {
            epoch_stats: vec![ValidatorEpochStats {
                epoch,
                leader_slots: 64,
                blocks_produced: 56,
                epoch_start_at: Some(epoch_start_at),
                epoch_end_at: Some(epoch_start_at + chrono::Duration::days(2)),
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

fn downtime_epochs(incidents: &ValidatorIncidents, vote_account: &str) -> Vec<u64> {
    incidents
        .get(vote_account)
        .expect("the validator has incident material")
        .downtimes
        .iter()
        .map(|downtime| downtime.epoch)
        .collect()
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

    let incidents = load_validator_incidents(&client, 100, 101, &no_records())
        .await
        .unwrap();

    assert_eq!(downtime_epochs(&incidents, "voteA"), vec![100, 101]);
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

    assert!(load_validator_incidents(&client, 100, 100, &no_records())
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

    let incidents = load_validator_incidents(&client, 100, 100, &no_records())
        .await
        .unwrap();

    assert_eq!(
        incidents.get("voteA").unwrap().downtimes[0].downtime_seconds,
        200
    );
}

#[tokio::test]
async fn every_down_row_is_loaded_oldest_first() {
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

    let incidents = load_validator_incidents(&client, 100, 102, &no_records())
        .await
        .unwrap();

    assert_eq!(downtime_epochs(&incidents, "voteA"), vec![100, 101, 102]);
}

#[tokio::test]
async fn a_closed_epoch_reports_its_block_production() {
    let schema = "ds_test_incidents_block_production";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    let incidents = load_validator_incidents(&client, 100, 100, &epoch_stats("voteA", 100))
        .await
        .unwrap();

    let production = &incidents.get("voteA").unwrap().block_production;
    assert_eq!(production.len(), 1);
    assert_eq!(production[0].leader_slots, 64);
    assert_eq!(production[0].blocks_produced, 56);
}

// The epoch in flight has no `epochs` row yet, and its counters cover only the slots so far.
#[tokio::test]
async fn the_running_epoch_reports_no_block_production() {
    let schema = "ds_test_incidents_running_epoch";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    let mut records = epoch_stats("voteA", 100);
    for record in records.values_mut() {
        record.epoch_stats[0].epoch_end_at = None;
    }

    let incidents = load_validator_incidents(&client, 100, 100, &records)
        .await
        .unwrap();

    assert!(incidents.is_empty());
}

// The window bound is the query's, so an epoch older than it is not read either.
#[tokio::test]
async fn an_epoch_before_from_epoch_is_left_out() {
    let schema = "ds_test_incidents_block_production_window";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    let incidents = load_validator_incidents(&client, 100, 100, &epoch_stats("voteA", 99))
        .await
        .unwrap();

    assert!(incidents.is_empty());
}
