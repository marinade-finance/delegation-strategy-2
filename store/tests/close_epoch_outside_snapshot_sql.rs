mod common;

use clap::Parser;
use collect::slot_params::baseline_slots_per_year;
use collect::validators_performance::{
    ClusterInflation, ValidatorPerformance, ValidatorRewards, ValidatorsPerformanceSnapshot,
};
use common::{
    migrated_client, skip_without_database, store_snapshot, validator_performance,
    validator_snapshot, write_yaml,
};
use rust_decimal::Decimal;
use std::collections::HashMap;
use store::close_epoch::{close_epoch, CloseEpochParams};
use tokio_postgres::Client;

const EPOCH: u64 = 1035;
const LISTED: &str = "voteListedInTheSnapshot";
const OUTSIDE: &str = "voteOutsideTheSnapshot";
const UNSAMPLED: &str = "voteOutsideWithNoSample";

async fn seed_validators(client: &mut Client, epoch: u64, schema_tag: &str) {
    let mut snapshot = validator_snapshot(epoch, "identityListed", LISTED);
    let mut outside = validator_snapshot(epoch, "identityOutside", OUTSIDE);
    let mut unsampled = validator_snapshot(epoch, "identityUnsampled", UNSAMPLED);
    snapshot.validators.append(&mut outside.validators);
    for validator in snapshot.validators.iter_mut() {
        validator.inflation_rewards_commission_bps = Some(1_001);
        validator.inflation_rewards_commission_bps_is_v4 = Some(true);
        // agave floors the whole-percent field, so 1001 bps is advertised as 10
        validator.performance.commission = 10;
    }
    snapshot.validators.append(&mut unsampled.validators);
    store_snapshot(client, schema_tag, &snapshot).await;
}

fn performance_snapshot(listed_reward: Option<u8>) -> String {
    let mut validators = HashMap::new();
    validators.insert(
        LISTED.to_string(),
        ValidatorPerformance {
            commission: 10,
            ..validator_performance()
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
        slots_per_year: baseline_slots_per_year(),
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

async fn read_floor(
    client: &Client,
    vote_account: &str,
    epoch: u64,
) -> (Option<i32>, Option<i32>, Option<f64>) {
    let row = client
        .query_one(
            "SELECT commission_min_observed, commission_max_observed, uptime_pct
             FROM validators WHERE vote_account = $1 AND epoch = $2",
            &[&vote_account, &Decimal::from(epoch)],
        )
        .await
        .unwrap();
    (
        row.get("commission_min_observed"),
        row.get("commission_max_observed"),
        row.get("uptime_pct"),
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
    assert_eq!(
        read_effective(&client, UNSAMPLED, EPOCH).await,
        (None, None),
        "a row outside the snapshot with no sampled rate is left unresolved rather than written"
    );
    assert_eq!(
        read_floor(&client, OUTSIDE, EPOCH).await,
        (Some(10), Some(11), Some(1f64)),
        "the ceiled sample folds into the bounds above the floored advertised rate, straddling 1pp"
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

#[tokio::test]
async fn a_failed_outside_write_still_leaves_the_epoch_its_floor() {
    let schema = "ds_test_close_epoch_outside_snapshot_floor";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    seed_validators(&mut client, EPOCH, schema).await;
    client
        .batch_execute(&format!(
            "ALTER TABLE validators ADD CONSTRAINT reject_the_outside_write
             CHECK (vote_account <> '{OUTSIDE}' OR commission_effective IS NULL)"
        ))
        .await
        .unwrap();
    run_close_epoch(&mut client, schema, None).await;

    assert_eq!(
        read_effective(&client, OUTSIDE, EPOCH).await,
        (None, None),
        "the constraint has to have rejected the outside write for this test to mean anything"
    );
    assert_eq!(
        read_floor(&client, OUTSIDE, EPOCH).await,
        (Some(10), Some(10), Some(1f64)),
        "a closed epoch is never re-listed, so a failed outside write must not cost it the floor"
    );
    assert_eq!(
        read_effective(&client, LISTED, EPOCH).await,
        (Some(11), Some("vote_state".to_string())),
        "the snapshot path is written before the outside one and is unaffected by its failure"
    );

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}
