mod common;

use common::{
    migrated_client, performance_snapshot, run_close_epoch, skip_without_database, store_snapshot,
    validator_snapshot,
};
use rust_decimal::Decimal;
use std::collections::HashMap;
use tokio_postgres::Client;

const EPOCH: u64 = 1082;
const ADMITTED: &str = "VatCheckAdmitted";
const STORED_ONLY: &str = "VatCheckStoredOnly";
const REFUSED: &str = "VatCheckRefused";
const BIGQUERY_ONLY: &str = "VatCheckBigQueryOnly";

async fn seeded(schema: &str) -> Option<Client> {
    if skip_without_database(schema) {
        return None;
    }
    let mut client = migrated_client(schema).await.unwrap();
    let mut snapshot = validator_snapshot(EPOCH, "identityAdmitted", ADMITTED);
    for (identity, vote_account) in [
        ("identityStoredOnly", STORED_ONLY),
        ("identityRefused", REFUSED),
    ] {
        snapshot
            .validators
            .append(&mut validator_snapshot(EPOCH, identity, vote_account).validators);
    }
    store_snapshot(&mut client, schema, &snapshot).await;
    client
        .execute(
            "UPDATE validators SET vat_admitted = vote_account <> $2 WHERE epoch = $1",
            &[&Decimal::from(EPOCH), &REFUSED],
        )
        .await
        .unwrap();
    Some(client)
}

async fn close(client: &mut Client, schema: &str, bigquery: Option<&[&str]>) {
    let mut snapshot = performance_snapshot(EPOCH, HashMap::new(), HashMap::new());
    snapshot.vat_debited_bigquery =
        bigquery.map(|accounts| accounts.iter().map(|a| a.to_string()).collect());
    run_close_epoch(client, schema, &snapshot).await;
}

async fn verdicts(client: &Client) -> Vec<(String, Option<bool>)> {
    client
        .query(
            "SELECT vote_account, vat_admitted FROM validators ORDER BY vote_account",
            &[],
        )
        .await
        .unwrap()
        .iter()
        .map(|row| (row.get(0), row.get(1)))
        .collect()
}

async fn check_row(client: &Client) -> Option<(i32, Vec<String>, Vec<String>)> {
    client
        .query_opt(
            "SELECT mismatched, only_bigquery, only_stored FROM vat_verdict_checks WHERE epoch = $1",
            &[&Decimal::from(EPOCH)],
        )
        .await
        .unwrap()
        .map(|row| (row.get(0), row.get(1), row.get(2)))
}

#[tokio::test]
async fn equal_sets_record_no_mismatch() {
    let schema = "ds_test_vat_check_equal";
    let Some(mut client) = seeded(schema).await else {
        return;
    };
    let before = verdicts(&client).await;

    close(&mut client, schema, Some(&[ADMITTED, STORED_ONLY])).await;

    assert_eq!(check_row(&client).await, Some((0, vec![], vec![])));
    assert_eq!(verdicts(&client).await, before);
}

#[tokio::test]
async fn differing_sets_record_both_sides() {
    let schema = "ds_test_vat_check_differ";
    let Some(mut client) = seeded(schema).await else {
        return;
    };
    let before = verdicts(&client).await;

    close(&mut client, schema, Some(&[ADMITTED, BIGQUERY_ONLY])).await;

    assert_eq!(
        check_row(&client).await,
        Some((
            2,
            vec![BIGQUERY_ONLY.to_string()],
            vec![STORED_ONLY.to_string()]
        ))
    );
    assert_eq!(verdicts(&client).await, before);
}

#[tokio::test]
async fn the_close_replaces_an_earlier_check() {
    let schema = "ds_test_vat_check_replace";
    let Some(mut client) = seeded(schema).await else {
        return;
    };
    client
        .execute(
            "INSERT INTO vat_verdict_checks VALUES ($1, NOW(), 7, '{stale}', '{stale}')",
            &[&Decimal::from(EPOCH)],
        )
        .await
        .unwrap();

    close(&mut client, schema, Some(&[ADMITTED, STORED_ONLY])).await;

    assert_eq!(check_row(&client).await, Some((0, vec![], vec![])));
}

#[tokio::test]
async fn no_bigquery_debits_record_no_check() {
    let schema = "ds_test_vat_check_none";
    let Some(mut client) = seeded(schema).await else {
        return;
    };
    let before = verdicts(&client).await;

    close(&mut client, schema, None).await;

    assert_eq!(check_row(&client).await, None);
    assert_eq!(verdicts(&client).await, before);
}

#[tokio::test]
async fn a_failing_check_never_fails_the_close() {
    let schema = "ds_test_vat_check_fails";
    let Some(mut client) = seeded(schema).await else {
        return;
    };
    client
        .batch_execute("DROP TABLE vat_verdict_checks")
        .await
        .unwrap();

    close(&mut client, schema, Some(&[ADMITTED])).await;

    let closed: i64 = client
        .query_one(
            "SELECT COUNT(*) FROM epochs WHERE epoch = $1",
            &[&Decimal::from(EPOCH)],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(closed, 1);
}
