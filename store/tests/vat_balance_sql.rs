mod common;

use clap::Parser;
use collect::validators::Snapshot;
use collect::validators_performance::ValidatorsPerformanceSnapshot;
use common::{
    migrated_client, performance_snapshot, skip_without_database, store_snapshot,
    validator_snapshot, write_yaml,
};
use rust_decimal::Decimal;
use std::collections::BTreeMap;
use store::cluster_info::{store_cluster_info, StoreClusterInfoParams};
use store::dto::ValidatorRecord;
use store::utils::{
    load_validators, load_vat_minimum_balances, EpochVatMinimum, ValidatorOverlays,
};
use tokio_postgres::Client;

const EPOCH: u64 = 1058;
const VOTE_ACCOUNT: &str = "VatBalanceVote11111111111111111111111111111";
const MINIMUM: u64 = 819_761_200;
const VAT: u64 = 800_000_000;

fn admissible_snapshot(lamports: Option<u64>) -> Snapshot {
    let mut snapshot = validator_snapshot(EPOCH, "identityVatBalance", VOTE_ACCOUNT);
    let validator = &mut snapshot.validators[0];
    validator.vote_account_lamports = lamports;
    validator.inflation_rewards_commission_bps_is_v4 = Some(true);
    validator.bls_pubkey = Some("blsKey".into());
    snapshot
}

async fn store_balance(client: &mut Client, name: &str, lamports: Option<u64>) {
    store_snapshot(client, name, &admissible_snapshot(lamports)).await;
}

async fn store_minimum(client: &Client, minimum: Option<u64>, created_at: &str) {
    client
        .execute(
            "INSERT INTO cluster_info (epoch_slot, epoch, transaction_count, created_at, vat_minimum_balance, vat_lamports_per_epoch)
             VALUES (0, $1, 0, $2::TEXT::TIMESTAMPTZ, $3, $4)",
            &[
                &Decimal::from(EPOCH),
                &created_at,
                &minimum.map(Decimal::from),
                &minimum.map(|_| Decimal::from(VAT)),
            ],
        )
        .await
        .unwrap();
}

fn epoch_minimum(balance: u64) -> EpochVatMinimum {
    EpochVatMinimum {
        balance: Decimal::from(balance),
        vat_lamports_per_epoch: Some(Decimal::from(VAT)),
    }
}

async fn record(client: &Client) -> ValidatorRecord {
    let unreachable_scoring_url = "http://127.0.0.1:1".to_string();
    load_validators(
        client,
        unreachable_scoring_url,
        3,
        3,
        &ValidatorOverlays::default(),
        &load_vat_minimum_balances(client, 3).await.unwrap(),
    )
    .await
    .unwrap()
    .remove(VOTE_ACCOUNT)
    .expect("the validator must load")
}

#[tokio::test]
async fn a_balance_below_the_minimum_is_not_sufficient() {
    let schema = "ds_test_vat_balance_low";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();
    store_minimum(&client, Some(MINIMUM), "2026-10-20T00:00:00Z").await;
    store_balance(&mut client, schema, Some(MINIMUM - 1)).await;

    let record = record(&client).await;
    assert_eq!(record.vat_eligible, Some(false));
    assert_eq!(record.epoch_stats[0].vat_eligible, Some(false));
}

#[tokio::test]
async fn a_balance_equal_to_the_minimum_is_sufficient() {
    let schema = "ds_test_vat_balance_equal";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();
    store_minimum(&client, Some(MINIMUM), "2026-10-20T00:00:00Z").await;
    store_balance(&mut client, schema, Some(MINIMUM)).await;

    assert_eq!(record(&client).await.vat_eligible, Some(true));
}

#[tokio::test]
async fn the_newest_cluster_info_row_with_a_minimum_wins() {
    let schema = "ds_test_vat_balance_newest";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();
    store_minimum(&client, Some(MINIMUM + 1), "2026-10-20T00:00:00Z").await;
    store_minimum(&client, Some(MINIMUM), "2026-10-20T01:00:00Z").await;
    store_minimum(&client, None, "2026-10-20T02:00:00Z").await;
    store_balance(&mut client, schema, Some(MINIMUM)).await;

    assert_eq!(
        load_vat_minimum_balances(&client, 3).await.unwrap()[&EPOCH],
        epoch_minimum(MINIMUM)
    );
    assert_eq!(record(&client).await.vat_eligible, Some(true));
}

#[tokio::test]
async fn a_later_snapshot_without_a_balance_keeps_the_stored_one() {
    let schema = "ds_test_vat_balance_kept";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();
    store_minimum(&client, Some(MINIMUM), "2026-10-20T00:00:00Z").await;
    store_balance(&mut client, &format!("{schema}-1"), Some(MINIMUM - 1)).await;
    store_balance(&mut client, &format!("{schema}-2"), None).await;

    let record = record(&client).await;
    assert_eq!(record.vat_eligible, Some(false));
    assert_eq!(
        record.vote_account_lamports,
        Some(Decimal::from(MINIMUM - 1))
    );
}

#[tokio::test]
async fn an_epoch_without_a_minimum_has_no_verdict() {
    let schema = "ds_test_vat_balance_none";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();
    store_minimum(&client, None, "2026-10-20T00:00:00Z").await;
    store_balance(&mut client, schema, Some(MINIMUM)).await;

    assert!(load_vat_minimum_balances(&client, 3)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(record(&client).await.vat_eligible, None);
}

#[tokio::test]
async fn store_cluster_info_writes_the_minimum_of_the_snapshot() {
    let schema = "ds_test_vat_balance_cluster_info";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();
    let snapshot = ValidatorsPerformanceSnapshot {
        vat_minimum_balance: Some(MINIMUM),
        vat_lamports_per_epoch: Some(VAT),
        ..performance_snapshot(EPOCH, Default::default(), Default::default())
    };
    let path = write_yaml(schema, &serde_yaml::to_string(&snapshot).unwrap());
    store_cluster_info(
        StoreClusterInfoParams::parse_from(["store", "--snapshot-file", &path]),
        &mut client,
    )
    .await
    .unwrap();

    assert_eq!(
        load_vat_minimum_balances(&client, 3).await.unwrap(),
        BTreeMap::from([(EPOCH, epoch_minimum(MINIMUM))])
    );
}

#[tokio::test]
async fn a_vote_account_without_a_bls_key_is_not_admitted() {
    let schema = "ds_test_vat_admission_no_key";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();
    store_minimum(&client, Some(MINIMUM), "2026-10-20T00:00:00Z").await;
    let mut snapshot = admissible_snapshot(Some(MINIMUM));
    snapshot.validators[0].bls_pubkey = None;
    store_snapshot(&mut client, schema, &snapshot).await;

    let record = record(&client).await;
    assert_eq!(record.vat_eligible, Some(false));
    assert_eq!(record.vat_rejection_reason.as_deref(), Some("no_bls_key"));
}

#[tokio::test]
async fn the_stake_projected_to_the_boundary_decides_no_stake() {
    let schema = "ds_test_vat_admission_no_stake";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();
    store_minimum(&client, Some(MINIMUM), "2026-10-20T00:00:00Z").await;
    let mut snapshot = admissible_snapshot(Some(MINIMUM));
    snapshot.validators[0].deactivating_stake = Some(100);
    store_snapshot(&mut client, schema, &snapshot).await;

    let record = record(&client).await;
    assert_eq!(record.vat_eligible, Some(false));
    assert_eq!(record.vat_rejection_reason.as_deref(), Some("no_stake"));
}

#[tokio::test]
async fn the_runway_uses_the_stored_vat_and_the_pending_rewards() {
    let schema = "ds_test_vat_admission_runway";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();
    store_minimum(&client, Some(MINIMUM), "2026-10-20T00:00:00Z").await;
    let rent = MINIMUM - VAT;
    let mut snapshot = admissible_snapshot(Some(rent + 3 * VAT + 1));
    snapshot.validators[0].pending_delegator_rewards = Some(2);
    store_snapshot(&mut client, schema, &snapshot).await;

    let record = record(&client).await;
    assert_eq!(record.vat_eligible, Some(true));
    assert_eq!(record.vat_rejection_reason, None);
    assert_eq!(record.epoch_stats[0].bls_pubkey.as_deref(), Some("blsKey"));
    assert_eq!(record.vat_runway_epochs, Some(Decimal::from(2)));
}
