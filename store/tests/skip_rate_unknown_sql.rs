mod common;

use collect::validators_performance::{ValidatorPerformance, ValidatorRewards};
use common::{
    migrated_client, performance_snapshot, run_close_epoch, skip_without_database,
    store_snapshot, validator_performance, validator_snapshot,
};
use rust_decimal::Decimal;
use std::collections::HashMap;
use tokio_postgres::Client;

const EPOCH: u64 = 1048;
const VOTE_ACCOUNT: &str = "voteSkipRateUnknown";

fn unresolved_performance() -> ValidatorPerformance {
    ValidatorPerformance {
        leader_slots: 0,
        blocks_produced: 0,
        skip_rate: None,
        ..validator_performance()
    }
}

async fn read_skip_rate(client: &Client) -> (Decimal, Option<f64>) {
    let row = client
        .query_one(
            "SELECT leader_slots, skip_rate FROM validators WHERE vote_account = $1 AND epoch = $2",
            &[&VOTE_ACCOUNT, &Decimal::from(EPOCH)],
        )
        .await
        .unwrap();
    (row.get("leader_slots"), row.get("skip_rate"))
}

#[tokio::test]
async fn an_unresolved_snapshot_stores_an_unknown_skip_rate() {
    let schema = "ds_test_skip_rate_unknown_snapshot";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    let mut snapshot = validator_snapshot(EPOCH, "identitySkipRateUnknown", VOTE_ACCOUNT);
    snapshot.validators[0].performance = unresolved_performance();
    store_snapshot(&mut client, schema, &snapshot).await;

    assert_eq!(read_skip_rate(&client).await, (Decimal::ZERO, None));

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}

#[tokio::test]
async fn close_epoch_overwrites_a_skip_rate_with_unknown() {
    let schema = "ds_test_skip_rate_unknown_close";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    let snapshot = validator_snapshot(EPOCH, "identitySkipRateUnknown", VOTE_ACCOUNT);
    store_snapshot(&mut client, schema, &snapshot).await;
    assert_eq!(read_skip_rate(&client).await, (Decimal::from(100), Some(0.0)));

    let closed = performance_snapshot(
        EPOCH,
        HashMap::from([(VOTE_ACCOUNT.to_string(), unresolved_performance())]),
        HashMap::from([(
            VOTE_ACCOUNT.to_string(),
            ValidatorRewards {
                commission_effective: None,
            },
        )]),
    );
    run_close_epoch(&mut client, schema, &closed).await;

    assert_eq!(read_skip_rate(&client).await, (Decimal::ZERO, None));

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}
