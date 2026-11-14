mod common;

use common::{
    migrated_client, performance_snapshot, run_close_epoch, skip_without_database, store_snapshot,
    validator_snapshot,
};
use rust_decimal::Decimal;
use std::collections::HashMap;
use store::utils::{load_validators, load_vat_minimum_balances, ValidatorOverlays};
use tokio_postgres::Client;

const EPOCH: u64 = 1082;
const DEBITED: &str = "VatAdmittedDebited1111111111111111111111111";
const REFUSED: &str = "VatAdmittedRefused1111111111111111111111111";
const UNSTAKED: &str = "VatAdmittedUnstaked111111111111111111111111";

async fn seed_validators(client: &mut Client, schema: &str) {
    let mut snapshot = validator_snapshot(EPOCH, "identityDebited", DEBITED);
    snapshot
        .validators
        .append(&mut validator_snapshot(EPOCH, "identityRefused", REFUSED).validators);
    let mut unstaked = validator_snapshot(EPOCH, "identityUnstaked", UNSTAKED);
    unstaked.validators[0].activated_stake = 0;
    snapshot.validators.append(&mut unstaked.validators);
    store_snapshot(client, schema, &snapshot).await;
    store_snapshot(
        client,
        schema,
        &validator_snapshot(EPOCH - 1, "identityRefused", REFUSED),
    )
    .await;
}

async fn close(client: &mut Client, schema: &str, vat_debited: Option<Vec<String>>) {
    let mut snapshot = performance_snapshot(EPOCH, HashMap::new(), HashMap::new());
    snapshot.vat_debited = vat_debited;
    run_close_epoch(client, schema, &snapshot).await;
}

async fn vat_admitted(client: &Client, vote_account: &str, epoch: u64) -> Option<bool> {
    client
        .query_one(
            "SELECT vat_admitted FROM validators WHERE vote_account = $1 AND epoch = $2",
            &[&vote_account, &Decimal::from(epoch)],
        )
        .await
        .unwrap()
        .get(0)
}

#[tokio::test]
async fn close_epoch_marks_debited_validators_admitted() {
    let schema = "ds_test_vat_admitted_debits";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();
    seed_validators(&mut client, schema).await;

    close(&mut client, schema, Some(vec![DEBITED.to_string()])).await;

    assert_eq!(vat_admitted(&client, DEBITED, EPOCH).await, Some(true));
    assert_eq!(vat_admitted(&client, REFUSED, EPOCH).await, Some(false));
    assert_eq!(vat_admitted(&client, UNSTAKED, EPOCH).await, None);
    assert_eq!(vat_admitted(&client, REFUSED, EPOCH - 1).await, None);
}

#[tokio::test]
async fn close_epoch_leaves_vat_admitted_null_without_debits() {
    let schema = "ds_test_vat_admitted_unknown";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();
    seed_validators(&mut client, schema).await;

    close(&mut client, schema, None).await;

    for vote_account in [DEBITED, REFUSED, UNSTAKED] {
        assert_eq!(vat_admitted(&client, vote_account, EPOCH).await, None);
    }
}

#[tokio::test]
async fn load_validators_serves_vat_admitted_per_epoch() {
    let schema = "ds_test_vat_admitted_served";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();
    seed_validators(&mut client, schema).await;
    close(&mut client, schema, Some(vec![DEBITED.to_string()])).await;
    let mut open_epoch = validator_snapshot(EPOCH + 1, "identityDebited", DEBITED);
    open_epoch
        .validators
        .append(&mut validator_snapshot(EPOCH + 1, "identityRefused", REFUSED).validators);
    store_snapshot(&mut client, schema, &open_epoch).await;

    let unreachable_scoring_url = "http://127.0.0.1:1".to_string();
    let records = load_validators(
        &client,
        unreachable_scoring_url,
        3,
        3,
        &ValidatorOverlays::default(),
        &load_vat_minimum_balances(&client, 3).await.unwrap(),
    )
    .await
    .unwrap();
    let served = |vote_account: &str| -> Vec<(u64, Option<bool>)> {
        records[vote_account]
            .epoch_stats
            .iter()
            .map(|stat| (stat.epoch, stat.vat_admitted))
            .collect()
    };
    assert_eq!(
        served(DEBITED),
        vec![(EPOCH + 1, None), (EPOCH, Some(true))]
    );
    assert_eq!(
        served(REFUSED),
        vec![(EPOCH + 1, None), (EPOCH, Some(false)), (EPOCH - 1, None)]
    );
    assert_eq!(served(UNSTAKED), vec![(EPOCH, None)]);
}
