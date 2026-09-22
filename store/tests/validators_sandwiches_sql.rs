mod common;

use clap::Parser;
use collect::validators_sandwiches::{ValidatorSandwich, ValidatorsSandwichesSnapshot};
use common::{migrated_client, skip_without_database, write_yaml};
use rust_decimal::Decimal;
use store::validators_sandwiches::{store_sandwiches, StoreSandwichesParams};
use tokio_postgres::Client;

const SNAPSHOT_VERSION: u16 = 1;
const EPOCH: u64 = 1030;

fn sandwich(
    vote_account: &str,
    sandwich_rate_30d: f64,
    sandwich_rate_60d: Option<f64>,
) -> ValidatorSandwich {
    ValidatorSandwich {
        epoch: EPOCH,
        vote_account: vote_account.into(),
        blocks_produced: 5000,
        blocks_with_sandwiches: 300,
        sandwich_rate_30d,
        sandwich_rate_60d,
    }
}

async fn store(
    client: &mut Client,
    name: &str,
    created_at: &str,
    sandwiches: Vec<ValidatorSandwich>,
) {
    let snapshot = ValidatorsSandwichesSnapshot {
        version: SNAPSHOT_VERSION,
        from_epoch: EPOCH,
        loaded_at_epoch: EPOCH + 1,
        loaded_at_slot_index: 1000,
        created_at: created_at.into(),
        sandwiches,
    };
    let path = write_yaml(name, &serde_yaml::to_string(&snapshot).unwrap());
    store_sandwiches(
        StoreSandwichesParams::parse_from(["store", "--snapshot-file", &path]),
        client,
    )
    .await
    .unwrap();
    std::fs::remove_file(path).unwrap();
}

/// 30d rate, 60d rate and block counts for one vote account.
async fn read(client: &Client, vote_account: &str) -> (f64, Option<f64>, Decimal, Decimal) {
    let row = client
        .query_one(
            "SELECT sandwich_rate_30d, sandwich_rate_60d, blocks_produced, blocks_with_sandwiches
             FROM validators_sandwiches WHERE vote_account = $1",
            &[&vote_account],
        )
        .await
        .unwrap();
    (
        row.get("sandwich_rate_30d"),
        row.get("sandwich_rate_60d"),
        row.get("blocks_produced"),
        row.get("blocks_with_sandwiches"),
    )
}

#[tokio::test]
async fn a_snapshot_row_lands_in_the_table() {
    let schema = "ds_test_store_sandwiches_insert";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    store(
        &mut client,
        schema,
        "2026-09-09T00:00:00Z",
        vec![sandwich("voteA", 44.4, Some(31.1))],
    )
    .await;

    assert_eq!(
        read(&client, "voteA").await,
        (44.4, Some(31.1), Decimal::from(5000), Decimal::from(300))
    );
}

// Epochs before 820 published no 60d rate at all, so the column has to take a null.
#[tokio::test]
async fn a_missing_60d_rate_is_stored_as_null() {
    let schema = "ds_test_store_sandwiches_null";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    store(
        &mut client,
        schema,
        "2026-09-09T00:00:00Z",
        vec![sandwich("voteA", 64.1, None)],
    )
    .await;

    assert_eq!(read(&client, "voteA").await.1, None);
}

// The collector re-reads a window every run, so the same epoch arrives again and again.
#[tokio::test]
async fn a_second_run_updates_the_row_it_already_wrote() {
    let schema = "ds_test_store_sandwiches_upsert";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    store(
        &mut client,
        schema,
        "2026-09-09T00:00:00Z",
        vec![sandwich("voteA", 44.4, Some(31.1))],
    )
    .await;
    store(
        &mut client,
        schema,
        "2026-09-10T00:00:00Z",
        vec![ValidatorSandwich {
            blocks_with_sandwiches: 400,
            ..sandwich("voteA", 45.0, None)
        }],
    )
    .await;

    let rows = client
        .query(
            "SELECT created_at, updated_at FROM validators_sandwiches WHERE vote_account = $1",
            &[&"voteA"],
        )
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        read(&client, "voteA").await,
        (45.0, None, Decimal::from(5000), Decimal::from(400))
    );

    // Only `updated_at` moves: the row keeps the run that first wrote it.
    let created_at: chrono::DateTime<chrono::Utc> = rows[0].get("created_at");
    let updated_at: chrono::DateTime<chrono::Utc> = rows[0].get("updated_at");
    assert_eq!(created_at.to_rfc3339(), "2026-09-09T00:00:00+00:00");
    assert_eq!(updated_at.to_rfc3339(), "2026-09-10T00:00:00+00:00");
}

// One epoch of the dataset is ~700 rows, over the chunk size the upsert pages at.
#[tokio::test]
async fn a_snapshot_over_one_chunk_is_stored_whole() {
    let schema = "ds_test_store_sandwiches_chunks";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    let sandwiches: Vec<ValidatorSandwich> = (0..1200)
        .map(|index| sandwich(&format!("vote{index}"), 1.0, Some(0.9)))
        .collect();
    store(&mut client, schema, "2026-09-09T00:00:00Z", sandwiches).await;

    let count: i64 = client
        .query_one("SELECT COUNT(*) AS count FROM validators_sandwiches", &[])
        .await
        .unwrap()
        .get("count");
    assert_eq!(count, 1200);
}
