mod common;

use common::{migrated_client, skip_without_database};
use store::dto::IncidentRecord;
use store::utils::load_incidents;
use tokio_postgres::Client;

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

// The handler measures its own window off the same head this bound is derived from, so an epoch
// exactly on the bound has to be served or the two disagree by one epoch.
#[tokio::test]
async fn the_window_starts_on_from_epoch_itself() {
    let schema = "ds_test_incidents_window";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    down(
        &client,
        "voteA",
        99,
        "2026-01-01T00:00:00Z",
        "2026-01-01T00:05:00Z",
    )
    .await;
    down(
        &client,
        "voteA",
        100,
        "2026-02-01T00:00:00Z",
        "2026-02-01T00:05:00Z",
    )
    .await;
    down(
        &client,
        "voteA",
        101,
        "2026-03-01T00:00:00Z",
        "2026-03-01T00:05:00Z",
    )
    .await;

    let incidents = load_incidents(&client, 100).await.unwrap();

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

    assert!(load_incidents(&client, 100).await.unwrap().is_empty());
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

    let incidents = load_incidents(&client, 100).await.unwrap();

    assert_eq!(incidents["voteA"][0].downtime_seconds, 200);
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

    let incidents = load_incidents(&client, 100).await.unwrap();

    assert_eq!(epochs(&incidents["voteA"]), vec![100, 101, 102]);
}

#[tokio::test]
async fn incidents_are_keyed_by_the_validator_that_was_down() {
    let schema = "ds_test_incidents_grouping";
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
    down(
        &client,
        "voteB",
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

    let incidents = load_incidents(&client, 100).await.unwrap();

    assert_eq!(incidents.len(), 2);
    assert_eq!(epochs(&incidents["voteA"]), vec![100, 101]);
    assert_eq!(epochs(&incidents["voteB"]), vec![100]);
}
