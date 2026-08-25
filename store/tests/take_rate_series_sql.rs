mod common;

use chrono::{DateTime, Duration, Utc};
use common::{migrated_client, skip_without_database};
use rust_decimal::Decimal;
use store::take_rates::get_take_rate_series;
use tokio_postgres::Client;

const LAST_EPOCH: u64 = 1000;
const EPOCHS: u64 = 120;
const VOTE: &str = "voteTakeRates";

fn epoch_start(epoch: u64) -> DateTime<Utc> {
    "2024-01-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap() + Duration::days(2 * epoch as i64)
}

async fn insert_reward(client: &Client, vote_account: &str, epoch: u64, take_rate: f64) {
    client
        .execute(
            "INSERT INTO validators_rewards (
                vote_account, epoch, validator_rewards, total_rewards, inflation_rewards,
                mev_rewards, block_rewards, take_rate, created_at, updated_at
            ) VALUES ($1, $2, 10, 100, 100, 0, 0, $3, NOW(), NOW())",
            &[&vote_account, &Decimal::from(epoch), &take_rate],
        )
        .await
        .unwrap();
}

async fn insert_epoch(client: &Client, epoch: u64) {
    client
        .execute(
            "INSERT INTO epochs (
                epoch, start_at, end_at, transaction_count, supply, inflation, inflation_taper,
                slots_per_year
            ) VALUES ($1, $2, $3, 0, 0, 0, 0, 0)",
            &[
                &Decimal::from(epoch),
                &epoch_start(epoch),
                &(epoch_start(epoch) + Duration::days(2)),
            ],
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn take_rate_series_spans_the_whole_stored_history() {
    let schema = "ds_test_take_rate_series";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    let first_epoch = LAST_EPOCH - EPOCHS + 1;
    for epoch in first_epoch..=LAST_EPOCH {
        insert_reward(&client, VOTE, epoch, 0.05).await;
        // No `epochs` row for the last one, so its boundaries come back null.
        if epoch < LAST_EPOCH {
            insert_epoch(&client, epoch).await;
        }
    }
    insert_reward(&client, "voteOther", LAST_EPOCH, 0.1).await;

    let series = get_take_rate_series(&client, VOTE, None).await.unwrap();
    let epochs: Vec<u64> = series.iter().map(|record| record.epoch).collect();
    assert_eq!(epochs, (first_epoch..=LAST_EPOCH).collect::<Vec<_>>());

    let epoch_without_boundaries = series.last().unwrap();
    assert_eq!(epoch_without_boundaries.epoch, LAST_EPOCH);
    assert_eq!(epoch_without_boundaries.epoch_start_at, None);
    assert_eq!(epoch_without_boundaries.epoch_end_at, None);

    let closed_epoch = &series[0];
    assert_eq!(closed_epoch.epoch_start_at, Some(epoch_start(first_epoch)));

    let bounded = get_take_rate_series(&client, VOTE, Some(LAST_EPOCH - 2))
        .await
        .unwrap();
    let epochs: Vec<u64> = bounded.iter().map(|record| record.epoch).collect();
    assert_eq!(epochs, vec![LAST_EPOCH - 2, LAST_EPOCH - 1, LAST_EPOCH]);

    let other = get_take_rate_series(&client, "voteOther", None)
        .await
        .unwrap();
    assert_eq!(other.len(), 1);

    let unknown = get_take_rate_series(&client, "voteUnknown", None)
        .await
        .unwrap();
    assert!(unknown.is_empty());

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}
