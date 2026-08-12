mod common;

use common::{migrated_client, skip_without_database};
use store::rewards::{get_estimated_inflation_rewards, get_slots_per_year};
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

    let rewards: std::collections::HashMap<_, _> = get_estimated_inflation_rewards(&client, 10)
        .await
        .unwrap()
        .into_iter()
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

    let provenance: std::collections::HashMap<_, _> = get_slots_per_year(&client, 10)
        .await
        .unwrap()
        .into_iter()
        .collect();
    assert_eq!(provenance[&1000], BASELINE_SLOTS_PER_YEAR);
    assert_eq!(provenance[&1001], SLOTS_PER_YEAR_350MS);

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}
