mod common;

use common::{migrated_client, skip_without_database, store_snapshot, validator_snapshot};
use rust_decimal::Decimal;
use store::dto::ValidatorRecord;
use store::utils::{load_validators, load_vat_minimum_balances, ValidatorOverlays};
use tokio_postgres::Client;

const EPOCH: u64 = 1058;
const VOTE_ACCOUNT: &str = "VatBalanceVote11111111111111111111111111111";
const MINIMUM: u64 = 819_761_200;

async fn store_balance(client: &mut Client, name: &str, lamports: Option<u64>) {
    let mut snapshot = validator_snapshot(EPOCH, "identityVatBalance", VOTE_ACCOUNT);
    snapshot.validators[0].vote_account_lamports = lamports;
    store_snapshot(client, name, &snapshot).await;
}

async fn store_minimum(client: &Client, minimum: Option<u64>, created_at: &str) {
    client
        .execute(
            "INSERT INTO cluster_info (epoch_slot, epoch, transaction_count, created_at, vat_minimum_balance)
             VALUES (0, $1, 0, $2::TEXT::TIMESTAMPTZ, $3)",
            &[
                &Decimal::from(EPOCH),
                &created_at,
                &minimum.map(Decimal::from),
            ],
        )
        .await
        .unwrap();
}

async fn record(client: &Client) -> ValidatorRecord {
    let unreachable_scoring_url = "http://127.0.0.1:1".to_string();
    load_validators(
        client,
        unreachable_scoring_url,
        3,
        3,
        &ValidatorOverlays::default(),
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
        load_vat_minimum_balances(&client, EPOCH, EPOCH)
            .await
            .unwrap()[&EPOCH],
        Decimal::from(MINIMUM)
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

    assert!(load_vat_minimum_balances(&client, EPOCH, EPOCH)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(record(&client).await.vat_eligible, None);
}
