mod common;

use clap::Parser;
use collect::validators_slot_times::{ValidatorSlotTime, ValidatorsSlotTimesSnapshot};
use common::{migrated_client, skip_without_database, write_yaml};
use store::validators_slot_times::{store_slot_times, StoreSlotTimesParams};
use tokio_postgres::Client;

const EPOCH: u64 = 1080;

fn slot_time(vote_account: &str, median: f64, ibrl_score: Option<f64>) -> ValidatorSlotTime {
    ValidatorSlotTime {
        epoch: EPOCH,
        vote_account: vote_account.into(),
        slot_duration_median_ms: median,
        ibrl_score,
    }
}

async fn store(
    client: &mut Client,
    name: &str,
    created_at: &str,
    slot_times: Vec<ValidatorSlotTime>,
) {
    let snapshot = ValidatorsSlotTimesSnapshot {
        version: 1,
        source: "external".into(),
        from_epoch: EPOCH,
        loaded_at_epoch: EPOCH + 1,
        loaded_at_slot_index: 1000,
        created_at: created_at.into(),
        slot_times,
    };
    let path = write_yaml(name, &serde_yaml::to_string(&snapshot).unwrap());
    store_slot_times(
        StoreSlotTimesParams::parse_from(["store", "--snapshot-file", &path]),
        client,
    )
    .await
    .unwrap();
    std::fs::remove_file(path).unwrap();
}

async fn read(client: &Client, vote_account: &str) -> (String, f64, Option<f64>) {
    let row = client
        .query_one(
            "SELECT source, slot_duration_median_ms, ibrl_score
             FROM validators_slot_times WHERE vote_account = $1",
            &[&vote_account],
        )
        .await
        .unwrap();
    (
        row.get("source"),
        row.get("slot_duration_median_ms"),
        row.get("ibrl_score"),
    )
}

#[tokio::test]
async fn a_snapshot_row_lands_in_the_table() {
    let schema = "ds_test_store_slot_times_insert";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    store(
        &mut client,
        schema,
        "2026-11-09T00:00:00Z",
        vec![slot_time("voteA", 243.5, Some(87.5))],
    )
    .await;

    assert_eq!(
        read(&client, "voteA").await,
        ("external".into(), 243.5, Some(87.5))
    );
}

// The upstream returns no ibrl_score for any validator today.
#[tokio::test]
async fn a_missing_ibrl_score_is_stored_as_null() {
    let schema = "ds_test_store_slot_times_null";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    store(
        &mut client,
        schema,
        "2026-11-09T00:00:00Z",
        vec![slot_time("voteA", 200.0, None)],
    )
    .await;

    assert_eq!(read(&client, "voteA").await.2, None);
}

// The collector re-reads a window every run, so the same epoch arrives again and again.
#[tokio::test]
async fn a_second_run_updates_the_row_it_already_wrote() {
    let schema = "ds_test_store_slot_times_upsert";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    store(
        &mut client,
        schema,
        "2026-11-09T00:00:00Z",
        vec![slot_time("voteA", 243.5, None)],
    )
    .await;
    store(
        &mut client,
        schema,
        "2026-11-10T00:00:00Z",
        vec![slot_time("voteA", 244.0, Some(90.0))],
    )
    .await;

    let rows = client
        .query(
            "SELECT created_at, updated_at FROM validators_slot_times WHERE vote_account = $1",
            &[&"voteA"],
        )
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        read(&client, "voteA").await,
        ("external".into(), 244.0, Some(90.0))
    );

    let created_at: chrono::DateTime<chrono::Utc> = rows[0].get("created_at");
    let updated_at: chrono::DateTime<chrono::Utc> = rows[0].get("updated_at");
    assert_eq!(created_at.to_rfc3339(), "2026-11-09T00:00:00+00:00");
    assert_eq!(updated_at.to_rfc3339(), "2026-11-10T00:00:00+00:00");
}

#[tokio::test]
async fn a_repeated_row_in_one_snapshot_keeps_the_last() {
    let schema = "ds_test_store_slot_times_dedup";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    store(
        &mut client,
        schema,
        "2026-11-09T00:00:00Z",
        vec![
            slot_time("voteA", 243.5, None),
            slot_time("voteA", 250.0, None),
        ],
    )
    .await;

    assert_eq!(read(&client, "voteA").await.1, 250.0);
}
