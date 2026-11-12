mod common;

use common::{migrated_client, skip_without_database};
use rust_decimal::Decimal;
use store::validator_quality::{load_validator_quality, ValidatorQualityRecord};
use tokio_postgres::Client;

async fn sandwiches(client: &Client, epoch: u64, vote_account: &str, wide: Option<f64>) {
    client
        .execute(
            "INSERT INTO validators_sandwiches (epoch, vote_account, blocks_produced, blocks_with_sandwiches,
                sandwich_rate_30d, wide_sandwich_rate_30d, created_at, updated_at)
             VALUES ($1, $2, 5000, 300, 6.0, $3, now(), now())",
            &[&Decimal::from(epoch), &vote_account, &wide],
        )
        .await
        .unwrap();
}

async fn slot_time(client: &Client, epoch: u64, vote_account: &str, ibrl_score: Option<f64>) {
    client
        .execute(
            "INSERT INTO validators_slot_times (epoch, vote_account, source, slot_duration_median_ms,
                ibrl_score, created_at, updated_at)
             VALUES ($1, $2, 'external', 243.5, $3, now(), now())",
            &[&Decimal::from(epoch), &vote_account, &ibrl_score],
        )
        .await
        .unwrap();
}

fn empty(vote_account: &str) -> ValidatorQualityRecord {
    ValidatorQualityRecord {
        vote_account: vote_account.into(),
        blocks_produced: None,
        blocks_with_sandwiches: None,
        sandwich_rate_30d: None,
        wide_sandwich_rate_30d: None,
        slot_duration_median_ms: None,
        ibrl_score: None,
    }
}

#[tokio::test]
async fn a_validator_in_one_table_only_has_nulls_for_the_other() {
    let schema = "ds_test_validator_quality_join";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();
    sandwiches(&client, 1080, "both", Some(4.3)).await;
    slot_time(&client, 1080, "both", Some(87.5)).await;
    sandwiches(&client, 1080, "sandwich-only", None).await;
    slot_time(&client, 1080, "slot-only", None).await;

    let loaded = load_validator_quality(&client, 1080).await.unwrap();

    assert_eq!(
        loaded,
        vec![
            ValidatorQualityRecord {
                blocks_produced: Some(5000),
                blocks_with_sandwiches: Some(300),
                sandwich_rate_30d: Some(6.0),
                wide_sandwich_rate_30d: Some(4.3),
                slot_duration_median_ms: Some(243.5),
                ibrl_score: Some(87.5),
                ..empty("both")
            },
            ValidatorQualityRecord {
                blocks_produced: Some(5000),
                blocks_with_sandwiches: Some(300),
                sandwich_rate_30d: Some(6.0),
                ..empty("sandwich-only")
            },
            ValidatorQualityRecord {
                slot_duration_median_ms: Some(243.5),
                ..empty("slot-only")
            },
        ]
    );
}

#[tokio::test]
async fn only_the_requested_epoch_is_returned() {
    let schema = "ds_test_validator_quality_epoch";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();
    sandwiches(&client, 1079, "old", None).await;
    slot_time(&client, 1079, "old", None).await;
    sandwiches(&client, 1080, "new", None).await;

    let loaded = load_validator_quality(&client, 1080).await.unwrap();

    assert_eq!(
        loaded
            .iter()
            .map(|r| r.vote_account.as_str())
            .collect::<Vec<_>>(),
        vec!["new"]
    );
}

// The upstream returns no ibrl_score for any validator today; the field is served, as null.
#[tokio::test]
async fn a_null_ibrl_score_is_served_as_null() {
    let schema = "ds_test_validator_quality_ibrl";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();
    slot_time(&client, 1080, "voteA", None).await;

    let loaded = load_validator_quality(&client, 1080).await.unwrap();

    assert_eq!(loaded[0].ibrl_score, None);
    assert_eq!(
        serde_json::to_value(&loaded[0]).unwrap()["ibrl_score"],
        serde_json::Value::Null
    );
}
