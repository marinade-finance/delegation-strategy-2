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
use std::collections::{HashMap, HashSet};
use store::close_epoch::{close_epoch, CloseEpochParams};
use store::dto::ValidatorRecord;
use store::uptime::{store_uptime, StoreUptimeParams};
use store::utils::{load_validators, ValidatorOverlays};
use tokio_postgres::Client;

const EPOCH: u64 = 1043;
const VOTE_ACCOUNT: &str = "KmCRTozzcAXvFEH2xakNMV7GeWyftyVFKS32XGV4spW";

fn alpenglow_performance() -> ValidatorPerformance {
    ValidatorPerformance {
        credits: None,
        vote_reward_lamports: Some(298929716564),
        last_vote: Some(0),
        credits_total: Some(590625720152),
        ..validator_performance()
    }
}

fn missing_performance() -> ValidatorPerformance {
    ValidatorPerformance {
        credits: None,
        credits_total: None,
        ..validator_performance()
    }
}

fn performance_snapshot(created_at: &str, performance: ValidatorPerformance) -> String {
    serde_yaml::to_string(&ValidatorsPerformanceSnapshot {
        epoch: EPOCH,
        epoch_slot: 432_000,
        transaction_count: 0,
        created_at: created_at.into(),
        slots_per_year: baseline_slots_per_year(),
        cluster_inflation: Some(ClusterInflation {
            sol_total_supply: 0,
            inflation: 0f64,
            inflation_taper: 0f64,
        }),
        validators: HashMap::from([(VOTE_ACCOUNT.to_string(), performance)]),
        nodes: Default::default(),
        rewards: Some(HashMap::from([(
            VOTE_ACCOUNT.to_string(),
            ValidatorRewards {
                commission_effective: None,
            },
        )])),
    })
    .unwrap()
}

async fn seed(client: &mut Client, schema: &str, performance: ValidatorPerformance) {
    let mut snapshot = validator_snapshot(EPOCH, "identityAlpenglow", VOTE_ACCOUNT);
    snapshot.validators[0].performance = performance;
    store_snapshot(client, schema, &snapshot).await;
}

async fn run_close_epoch(client: &mut Client, schema: &str, performance: ValidatorPerformance) {
    client
        .execute(
            "INSERT INTO cluster_info (epoch_slot, epoch, transaction_count, created_at)
             VALUES (0, $1, 0, NOW()), (432000, $1, 0, NOW())
             ON CONFLICT DO NOTHING",
            &[&Decimal::from(EPOCH)],
        )
        .await
        .unwrap();
    let path = write_yaml(
        schema,
        &performance_snapshot("2026-09-30T00:00:00Z", performance),
    );
    close_epoch(
        CloseEpochParams::parse_from(["store", "--snapshot-file", &path]),
        client,
    )
    .await
    .unwrap();
    std::fs::remove_file(path).unwrap();
}

async fn read_credits(client: &Client) -> (Option<Decimal>, Option<Decimal>) {
    let row = client
        .query_one(
            "SELECT credits, vote_reward_lamports
             FROM validators WHERE vote_account = $1 AND epoch = $2",
            &[&VOTE_ACCOUNT, &Decimal::from(EPOCH)],
        )
        .await
        .unwrap();
    (row.get("credits"), row.get("vote_reward_lamports"))
}

#[tokio::test]
async fn an_alpenglow_epoch_stores_null_credits() {
    let schema = "ds_test_vote_reward_lamports_alpenglow";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    seed(&mut client, schema, alpenglow_performance()).await;
    let (credits, vote_reward_lamports) = read_credits(&client).await;
    assert_eq!(credits, None);
    assert_eq!(vote_reward_lamports, Some(Decimal::from(298929716564u64)));

    run_close_epoch(&mut client, schema, alpenglow_performance()).await;
    let (credits, vote_reward_lamports) = read_credits(&client).await;
    assert_eq!(credits, None);
    assert_eq!(vote_reward_lamports, Some(Decimal::from(298929716564u64)));

    // EPOCH is the first epoch with vote_reward_lamports, so it is the migration epoch
    let record = load_record(&client).await;
    assert_eq!(record.credits, 0);
    assert_eq!(record.vote_reward_lamports, Some(298929716564));
    assert_eq!(record.epoch_stats[0].credits, 0);
    assert!(record.epoch_stats[0].apy.is_some());

    let mut snapshot = validator_snapshot(EPOCH - 1, "identityAlpenglow", VOTE_ACCOUNT);
    snapshot.validators[0].performance = alpenglow_performance();
    store_snapshot(&mut client, schema, &snapshot).await;

    let record = load_record(&client).await;
    assert_eq!(record.credits, 0);
    assert_eq!(record.vote_reward_lamports, Some(298929716564));
    assert_eq!(record.epoch_stats[0].credits, 0);
}

async fn load_record(client: &Client) -> ValidatorRecord {
    let unreachable_scoring_url = "http://127.0.0.1:1".to_string();
    load_validators(
        client,
        unreachable_scoring_url,
        1,
        1,
        &ValidatorOverlays::default(),
    )
    .await
    .unwrap()
    .remove(VOTE_ACCOUNT)
    .expect("a row with NULL credits must load")
}

#[tokio::test]
async fn an_epoch_missing_from_the_window_keeps_the_stored_credits() {
    let schema = "ds_test_vote_reward_lamports_missing";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    seed(&mut client, schema, validator_performance()).await;
    run_close_epoch(&mut client, schema, missing_performance()).await;

    let (credits, vote_reward_lamports) = read_credits(&client).await;
    assert_eq!(credits, Some(Decimal::from(10)));
    assert_eq!(vote_reward_lamports, None);
}

async fn run_uptime(client: &mut Client, schema: &str, created_at: &str, credits_total: u64) {
    let performance = ValidatorPerformance {
        credits_total: Some(credits_total),
        ..alpenglow_performance()
    };
    let path = write_yaml(
        &format!("{schema}-uptime"),
        &performance_snapshot(created_at, performance),
    );
    store_uptime(
        StoreUptimeParams::parse_from(["store", "--snapshot-file", &path]),
        client,
    )
    .await
    .unwrap();
    std::fs::remove_file(path).unwrap();
}

async fn read_statuses(client: &Client) -> Vec<(String, Option<Decimal>)> {
    client
        .query(
            "SELECT status, last_credits FROM uptimes WHERE vote_account = $1 ORDER BY start_at",
            &[&VOTE_ACCOUNT],
        )
        .await
        .unwrap()
        .iter()
        .map(|row| (row.get("status"), row.get("last_credits")))
        .collect()
}

#[tokio::test]
async fn without_votes_flat_credits_turn_a_validator_down() {
    let schema = "ds_test_vote_reward_lamports_uptime";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    run_uptime(&mut client, schema, "2026-09-30T00:00:00Z", 100).await;
    run_uptime(&mut client, schema, "2026-09-30T00:01:00Z", 200).await;
    run_uptime(&mut client, schema, "2026-09-30T00:02:00Z", 200).await;
    run_uptime(&mut client, schema, "2026-09-30T00:03:00Z", 300).await;

    let statuses = read_statuses(&client).await;
    let status_names: Vec<&str> = statuses.iter().map(|(s, _)| s.as_str()).collect();
    assert_eq!(status_names, vec!["UP", "DOWN", "UP"]);
    assert_eq!(
        statuses.iter().map(|(_, c)| *c).collect::<HashSet<_>>(),
        HashSet::from([Some(Decimal::from(200)), Some(Decimal::from(300)),])
    );
}
