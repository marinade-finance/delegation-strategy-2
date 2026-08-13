mod common;

use common::{migrated_client, skip_without_database};
use std::collections::HashMap;
use store::rewards::{get_estimated_inflation_rewards, get_running_epoch_slots_per_year};
use tokio_postgres::Client;

const SUPPLY_LAMPORTS: f64 = 600_000_000_000_000_000.0;
const INFLATION: f64 = 0.043;

const BASELINE_SLOTS_PER_YEAR: f64 = 78_892_314.984;
const SLOTS_PER_YEAR_350MS: f64 = 90_162_645.696;

/// What `get_estimated_inflation_rewards` divided by before the column existed.
const LEGACY_EPOCHS_PER_YEAR: f64 = 365.25 / 2.0;

async fn insert_epoch(client: &Client, epoch: i64, slots_per_year: f64) {
    client
        .execute(
            "INSERT INTO epochs (epoch, start_at, end_at, transaction_count, supply, inflation, inflation_taper, slots_per_year)
             VALUES ($1, NOW(), NOW(), 0, $2, $3, 0.15, $4)",
            &[
                &rust_decimal::Decimal::from(epoch),
                &rust_decimal::Decimal::from_f64_retain(SUPPLY_LAMPORTS).unwrap(),
                &INFLATION,
                &slots_per_year,
            ],
        )
        .await
        .unwrap();
}

async fn insert_cluster_info(client: &Client, epoch: i64, epoch_slot: i64, slots_per_year: f64) {
    client
        .execute(
            "INSERT INTO cluster_info (epoch, epoch_slot, transaction_count, created_at, slots_per_year)
             VALUES ($1, $2, 0, NOW(), $3)",
            &[
                &rust_decimal::Decimal::from(epoch),
                &rust_decimal::Decimal::from(epoch_slot),
                &slots_per_year,
            ],
        )
        .await
        .unwrap();
}

// Routing through a stored column is only safe if no historical figure moves.
#[tokio::test]
async fn backfilled_epochs_reproduce_the_legacy_inflation_estimate() {
    let schema = "ds_test_rewards_slots_per_year";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    insert_epoch(&client, 1000, BASELINE_SLOTS_PER_YEAR).await;
    insert_epoch(&client, 1001, SLOTS_PER_YEAR_350MS).await;

    let rows = get_estimated_inflation_rewards(&client, 10).await.unwrap();
    let rewards: HashMap<_, _> = rows
        .iter()
        .map(|(epoch, amount, _)| (*epoch, *amount))
        .collect();
    let provenance: HashMap<_, _> = rows
        .iter()
        .map(|(epoch, _, slots_per_year)| (*epoch, *slots_per_year))
        .collect();

    let legacy = SUPPLY_LAMPORTS * INFLATION / 1e9 / LEGACY_EPOCHS_PER_YEAR;
    let baseline = rewards[&1000];
    assert!(
        (baseline - legacy).abs() / legacy < 1e-4,
        "backfilled epoch moved: {baseline} vs {legacy}"
    );

    // Stage 1 mints 350/400 of the baseline per epoch.
    let stage_1 = rewards[&1001];
    assert!(
        (stage_1 / baseline - 350.0 / 400.0).abs() < 1e-9,
        "stage 1 epoch is {stage_1}, baseline {baseline}"
    );

    assert_eq!(provenance[&1000], BASELINE_SLOTS_PER_YEAR);
    assert_eq!(provenance[&1001], SLOTS_PER_YEAR_350MS);

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}

// The transition epoch is auctioned before it closes, so its regime has to be readable while it runs.
#[tokio::test]
async fn the_running_epoch_reports_its_own_regime_before_it_closes() {
    let schema = "ds_test_running_epoch_slots_per_year";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    insert_epoch(&client, 1000, BASELINE_SLOTS_PER_YEAR).await;
    insert_cluster_info(&client, 1000, 431_000, BASELINE_SLOTS_PER_YEAR).await;
    assert_eq!(
        get_running_epoch_slots_per_year(&client).await.unwrap(),
        None,
        "a closed epoch is already covered by the epochs table"
    );

    insert_cluster_info(&client, 1001, 100, SLOTS_PER_YEAR_350MS).await;
    insert_cluster_info(&client, 1001, 20_000, SLOTS_PER_YEAR_350MS).await;
    assert_eq!(
        get_running_epoch_slots_per_year(&client).await.unwrap(),
        Some((1001, SLOTS_PER_YEAR_350MS))
    );

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}
