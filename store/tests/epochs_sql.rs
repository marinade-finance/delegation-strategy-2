mod common;

use chrono::{DateTime, Duration, Utc};
use common::{migrated_client, skip_without_database};
use rust_decimal::Decimal;
use store::dto::EpochRecord;
use store::epochs::load_epochs;

fn epoch_start(epoch: u64) -> DateTime<Utc> {
    "2024-01-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap() + Duration::days(2 * epoch as i64)
}

fn record(epoch: u64) -> EpochRecord {
    EpochRecord {
        epoch,
        start_at: epoch_start(epoch),
        end_at: epoch_start(epoch + 1),
    }
}

#[tokio::test]
async fn load_epochs_returns_the_newest_closed_epochs_newest_first() {
    let schema = "ds_test_load_epochs";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();
    for epoch in [900, 902, 901] {
        client
            .execute(
                "INSERT INTO epochs (
                    epoch, start_at, end_at, transaction_count, supply, inflation,
                    inflation_taper, slots_per_year
                ) VALUES ($1, $2, $3, 0, 0, 0, 0, 78892314)",
                &[
                    &Decimal::from(epoch),
                    &epoch_start(epoch),
                    &epoch_start(epoch + 1),
                ],
            )
            .await
            .unwrap();
    }

    assert_eq!(
        load_epochs(&client, 2).await.unwrap(),
        vec![record(902), record(901)]
    );
    assert_eq!(
        load_epochs(&client, 10).await.unwrap(),
        vec![record(902), record(901), record(900)],
        "asking for more epochs than exist returns what there is"
    );

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}
