mod common;

use common::{migrated_client, skip_without_database, store_snapshot, validator_snapshot};
use rust_decimal::Decimal;
use std::collections::HashMap;
use store::dto::ValidatorRecord;
use store::utils::{load_validators, ValidatorOverlays};
use tokio_postgres::Client;

const EPOCH: u64 = 1043;
const VOTE_ACCOUNT: &str = "KmCRTozzcAXvFEH2xakNMV7GeWyftyVFKS32XGV4spW";

async fn seed(client: &mut Client, schema: &str, epoch: u64, activated_stake: u64) {
    let mut snapshot = validator_snapshot(epoch, "identityEpochStake", VOTE_ACCOUNT);
    snapshot.validators[0].activated_stake = activated_stake;
    store_snapshot(client, schema, &snapshot).await;
}

async fn epoch_stakes(client: &Client) -> HashMap<u64, Option<Decimal>> {
    client
        .execute(
            "INSERT INTO cluster_info (epoch_slot, epoch, transaction_count, created_at)
             VALUES (0, $1, 0, NOW()) ON CONFLICT DO NOTHING",
            &[&Decimal::from(EPOCH)],
        )
        .await
        .unwrap();
    let unreachable_scoring_url = "http://127.0.0.1:1".to_string();
    let record: ValidatorRecord = load_validators(
        client,
        unreachable_scoring_url,
        3,
        3,
        &ValidatorOverlays::default(),
    )
    .await
    .unwrap()
    .remove(VOTE_ACCOUNT)
    .expect("the validator must load");
    record
        .epoch_stats
        .iter()
        .map(|stats| (stats.epoch, stats.epoch_stake))
        .collect()
}

#[tokio::test]
async fn epoch_stake_is_the_activated_stake_of_the_previous_epoch() {
    let schema = "ds_test_epoch_stake_previous";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    seed(&mut client, schema, EPOCH - 1, 100).await;
    // stake activated during EPOCH - 1 is effective in EPOCH but missed epoch_stakes(EPOCH)
    seed(&mut client, schema, EPOCH, 150).await;

    let stakes = epoch_stakes(&client).await;
    assert_eq!(stakes.get(&EPOCH), Some(&Some(Decimal::from(100))));
    assert_eq!(stakes.get(&(EPOCH - 1)), Some(&None));
}

#[tokio::test]
async fn a_validator_unstaked_in_the_previous_epoch_is_not_a_member() {
    let schema = "ds_test_epoch_stake_unstaked";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    seed(&mut client, schema, EPOCH - 1, 0).await;
    seed(&mut client, schema, EPOCH, 150).await;

    let stakes = epoch_stakes(&client).await;
    assert_eq!(stakes.get(&EPOCH), Some(&None));
}

#[tokio::test]
async fn a_gap_epoch_leaves_the_next_one_without_a_member_stake() {
    let schema = "ds_test_epoch_stake_gap";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    seed(&mut client, schema, EPOCH - 2, 100).await;
    seed(&mut client, schema, EPOCH, 150).await;

    let stakes = epoch_stakes(&client).await;
    assert_eq!(stakes.get(&EPOCH), Some(&None));
    assert_eq!(stakes.get(&(EPOCH - 2)), Some(&None));
}

#[tokio::test]
async fn the_oldest_displayed_epoch_reads_its_previous_epoch_outside_the_window() {
    let schema = "ds_test_epoch_stake_window";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    seed(&mut client, schema, EPOCH - 3, 100).await;
    seed(&mut client, schema, EPOCH - 2, 150).await;

    let stakes = epoch_stakes(&client).await;
    assert_eq!(stakes.get(&(EPOCH - 3)), None);
    assert_eq!(stakes.get(&(EPOCH - 2)), Some(&Some(Decimal::from(100))));
}
