mod common;

use clap::Parser;
use collect::validators_performance::{
    ClusterInflation, ValidatorPerformance, ValidatorRewards, ValidatorsPerformanceSnapshot,
};
use common::{
    migrated_client, skip_without_database, store_snapshot, validator_snapshot, write_yaml,
};
use rust_decimal::Decimal;
use std::collections::HashMap;
use store::close_epoch::{close_epoch, CloseEpochParams};
use tokio_postgres::Client;

const EPOCH: u64 = 1035;
const LISTED: &str = "voteListedInTheSnapshot";
const OUTSIDE: &str = "voteOutsideTheSnapshot";

async fn seed_validators(client: &mut Client, epoch: u64, schema_tag: &str) {
    let mut snapshot = validator_snapshot(epoch, "identityListed", LISTED);
    let mut outside = validator_snapshot(epoch, "identityOutside", OUTSIDE);
    snapshot.validators.append(&mut outside.validators);
    for validator in snapshot.validators.iter_mut() {
        validator.inflation_rewards_commission_bps = Some(1_001);
        validator.inflation_rewards_commission_bps_is_v4 = Some(true);
    }
    store_snapshot(client, schema_tag, &snapshot).await;
}

fn performance_snapshot(listed_reward: Option<u8>) -> String {
    let mut validators = HashMap::new();
    validators.insert(
        LISTED.to_string(),
        ValidatorPerformance {
            commission: 7,
            version: Some("2.0.0".into()),
            client_id: None,
            client_id_raw: None,
            feature_set: None,
            shred_version: None,
            credits: 10,
            leader_slots: 100,
            blocks_produced: 100,
            skip_rate: 0f64,
            delinquent: false,
        },
    );
    let mut rewards = HashMap::new();
    rewards.insert(
        LISTED.to_string(),
        ValidatorRewards {
            commission_effective: listed_reward,
        },
    );
    serde_yaml::to_string(&ValidatorsPerformanceSnapshot {
        epoch: EPOCH,
        epoch_slot: 432_000,
        transaction_count: 0,
        created_at: "2026-09-16T23:00:00Z".into(),
        slots_per_year: 78_892_310f64,
        cluster_inflation: Some(ClusterInflation {
            sol_total_supply: 0,
            inflation: 0f64,
            inflation_taper: 0f64,
        }),
        validators,
        nodes: Default::default(),
        rewards: Some(rewards),
    })
    .unwrap()
}

async fn run_close_epoch(client: &mut Client, schema_tag: &str, listed_reward: Option<u8>) {
    client
        .execute(
            "INSERT INTO cluster_info (epoch_slot, epoch, transaction_count, created_at)
             VALUES (0, $1, 0, NOW()), (432000, $1, 0, NOW())",
            &[&Decimal::from(EPOCH)],
        )
        .await
        .unwrap();
    let path = write_yaml(schema_tag, &performance_snapshot(listed_reward));
    close_epoch(
        CloseEpochParams::parse_from(["store", "--snapshot-file", &path]),
        client,
    )
    .await
    .unwrap();
    std::fs::remove_file(path).unwrap();
}

async fn read_effective(
    client: &Client,
    vote_account: &str,
    epoch: u64,
) -> (Option<i32>, Option<String>) {
    let row = client
        .query_one(
            "SELECT commission_effective, commission_effective_source
             FROM validators WHERE vote_account = $1 AND epoch = $2",
            &[&vote_account, &Decimal::from(epoch)],
        )
        .await
        .unwrap();
    (
        row.get("commission_effective"),
        row.get("commission_effective_source"),
    )
}

#[tokio::test]
async fn close_epoch_resolves_a_validator_the_snapshot_never_listed() {
    let schema = "ds_test_close_epoch_outside_snapshot";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    seed_validators(&mut client, EPOCH, schema).await;
    run_close_epoch(&mut client, schema, None).await;

    assert_eq!(
        read_effective(&client, OUTSIDE, EPOCH).await,
        (Some(11), Some("vote_state".to_string())),
        "a row the snapshot never listed must still resolve from its sampled vote state"
    );
    assert_eq!(
        read_effective(&client, LISTED, EPOCH).await,
        (Some(11), Some("vote_state".to_string())),
        "the snapshot path resolves the same way when no reward row carries a rate"
    );

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}

#[tokio::test]
async fn the_outside_write_leaves_a_reward_row_and_an_older_epoch_alone() {
    let schema = "ds_test_close_epoch_outside_snapshot_scoping";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    seed_validators(&mut client, EPOCH - 1, schema).await;
    seed_validators(&mut client, EPOCH, schema).await;
    run_close_epoch(&mut client, schema, Some(5)).await;

    assert_eq!(
        read_effective(&client, LISTED, EPOCH).await,
        (Some(5), Some("reward_row".to_string())),
        "a reward row still wins over the sampled vote state"
    );
    assert_eq!(
        read_effective(&client, OUTSIDE, EPOCH - 1).await,
        (None, None),
        "the outside write is scoped to the epoch being closed"
    );

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}
