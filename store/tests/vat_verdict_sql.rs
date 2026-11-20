mod common;

use clap::Parser;
use collect::vat_debits::VatDebitsSnapshot;
use common::{
    migrated_client, performance_snapshot, run_close_epoch, skip_without_database, store_snapshot,
    validator_snapshot, write_yaml,
};
use rust_decimal::Decimal;
use std::collections::HashMap;
use store::utils::{load_validators, load_vat_minimum_balances, ValidatorOverlays};
use store::vat_verdict::{store_vat_verdict, StoreVatVerdictParams};
use tokio_postgres::Client;

const EPOCH: u64 = 1082;
const STAKED: usize = 20;
const UNSTAKED: &str = "VatVerdictUnstaked";

fn staked(index: usize) -> String {
    format!("VatVerdictStaked{index:02}")
}

fn staked_range(range: std::ops::Range<usize>) -> Vec<String> {
    range.map(staked).collect()
}

async fn seed_validators(client: &mut Client, schema: &str) {
    let mut snapshot = validator_snapshot(EPOCH, "identityUnstaked", UNSTAKED);
    snapshot.validators[0].activated_stake = 0;
    for index in 0..STAKED {
        let vote_account = staked(index);
        snapshot.validators.append(
            &mut validator_snapshot(EPOCH, &format!("identity{vote_account}"), &vote_account)
                .validators,
        );
    }
    store_snapshot(client, schema, &snapshot).await;
    let previous = staked(STAKED - 1);
    store_snapshot(
        client,
        schema,
        &validator_snapshot(EPOCH - 1, "identityPrevious", &previous),
    )
    .await;
}

async fn store_verdict(
    client: &mut Client,
    schema: &str,
    epoch: u64,
    vote_accounts: Vec<String>,
) -> anyhow::Result<()> {
    let snapshot = VatDebitsSnapshot {
        epoch,
        slot: 471_312_000,
        created_at: "2026-11-19T12:00:00Z".into(),
        vote_accounts,
    };
    let path = write_yaml(
        &format!("{schema}-vat-debits"),
        &serde_yaml::to_string(&snapshot).unwrap(),
    );
    let result = store_vat_verdict(
        StoreVatVerdictParams::parse_from(["store", "--snapshot-file", &path]),
        client,
    )
    .await;
    std::fs::remove_file(path).unwrap();
    result
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

async fn written_verdicts(client: &Client) -> i64 {
    client
        .query_one(
            "SELECT COUNT(*) FROM validators WHERE vat_admitted IS NOT NULL",
            &[],
        )
        .await
        .unwrap()
        .get(0)
}

async fn close(client: &mut Client, schema: &str) {
    run_close_epoch(
        client,
        schema,
        &performance_snapshot(EPOCH, HashMap::new(), HashMap::new()),
    )
    .await;
}

async fn seeded(schema: &str) -> Option<Client> {
    if skip_without_database(schema) {
        return None;
    }
    let mut client = migrated_client(schema).await.unwrap();
    seed_validators(&mut client, schema).await;
    Some(client)
}

#[tokio::test]
async fn debited_staked_and_unstaked_validators_get_their_verdict() {
    let schema = "ds_test_vat_verdict_marks";
    let Some(mut client) = seeded(schema).await else {
        return;
    };

    store_verdict(&mut client, schema, EPOCH, staked_range(0..STAKED - 1))
        .await
        .unwrap();

    assert_eq!(vat_admitted(&client, &staked(0), EPOCH).await, Some(true));
    assert_eq!(
        vat_admitted(&client, &staked(STAKED - 1), EPOCH).await,
        Some(false)
    );
    assert_eq!(vat_admitted(&client, UNSTAKED, EPOCH).await, None);
    assert_eq!(
        vat_admitted(&client, &staked(STAKED - 1), EPOCH - 1).await,
        None
    );
}

#[tokio::test]
async fn a_rerun_replaces_the_verdict() {
    let schema = "ds_test_vat_verdict_rerun";
    let Some(mut client) = seeded(schema).await else {
        return;
    };

    store_verdict(&mut client, schema, EPOCH, staked_range(0..STAKED - 1))
        .await
        .unwrap();
    store_verdict(&mut client, schema, EPOCH, staked_range(1..STAKED))
        .await
        .unwrap();

    assert_eq!(vat_admitted(&client, &staked(0), EPOCH).await, Some(false));
    assert_eq!(
        vat_admitted(&client, &staked(STAKED - 1), EPOCH).await,
        Some(true)
    );
}

#[tokio::test]
async fn the_verdict_of_an_open_epoch_survives_its_close() {
    let schema = "ds_test_vat_verdict_open";
    let Some(mut client) = seeded(schema).await else {
        return;
    };

    store_verdict(&mut client, schema, EPOCH, staked_range(0..STAKED - 1))
        .await
        .unwrap();
    close(&mut client, schema).await;

    assert_eq!(vat_admitted(&client, &staked(0), EPOCH).await, Some(true));
    assert_eq!(
        vat_admitted(&client, &staked(STAKED - 1), EPOCH).await,
        Some(false)
    );
}

#[tokio::test]
async fn a_closed_epoch_takes_the_verdict() {
    let schema = "ds_test_vat_verdict_closed";
    let Some(mut client) = seeded(schema).await else {
        return;
    };

    close(&mut client, schema).await;
    store_verdict(&mut client, schema, EPOCH, staked_range(0..STAKED - 1))
        .await
        .unwrap();

    assert_eq!(vat_admitted(&client, &staked(0), EPOCH).await, Some(true));
    assert_eq!(
        vat_admitted(&client, &staked(STAKED - 1), EPOCH).await,
        Some(false)
    );
}

#[tokio::test]
async fn an_empty_list_writes_nothing() {
    let schema = "ds_test_vat_verdict_empty";
    let Some(mut client) = seeded(schema).await else {
        return;
    };

    let err = store_verdict(&mut client, schema, EPOCH, vec![])
        .await
        .unwrap_err();

    assert!(err.to_string().contains("lists no vote account"), "{err}");
    assert_eq!(written_verdicts(&client).await, 0);
}

#[tokio::test]
async fn an_epoch_without_validators_is_refused() {
    let schema = "ds_test_vat_verdict_no_epoch";
    let Some(mut client) = seeded(schema).await else {
        return;
    };

    let err = store_verdict(&mut client, schema, EPOCH + 1, staked_range(0..STAKED))
        .await
        .unwrap_err();

    assert!(err.to_string().contains("holds no validators"), "{err}");
    assert_eq!(written_verdicts(&client).await, 0);
}

#[tokio::test]
async fn up_to_ten_percent_unknown_debited_accounts_are_accepted() {
    let schema = "ds_test_vat_verdict_unknown_limit";
    let Some(mut client) = seeded(schema).await else {
        return;
    };
    let mut debited = staked_range(0..STAKED - 2);
    debited.extend([
        "VatVerdictUnknown1".to_string(),
        "VatVerdictUnknown2".to_string(),
    ]);

    store_verdict(&mut client, schema, EPOCH, debited)
        .await
        .unwrap();

    assert_eq!(vat_admitted(&client, &staked(0), EPOCH).await, Some(true));
}

#[tokio::test]
async fn more_than_ten_percent_unknown_debited_accounts_write_nothing() {
    let schema = "ds_test_vat_verdict_unknown";
    let Some(mut client) = seeded(schema).await else {
        return;
    };
    let mut debited = staked_range(0..STAKED - 2);
    debited.extend((1..=3).map(|index| format!("VatVerdictUnknown{index}")));

    let err = store_verdict(&mut client, schema, EPOCH, debited)
        .await
        .unwrap_err();

    assert!(
        err.to_string()
            .contains("3 of 21 debited vote accounts are unknown"),
        "{err}"
    );
    assert_eq!(written_verdicts(&client).await, 0);
}

#[tokio::test]
async fn repeated_debits_do_not_dilute_the_unknown_share() {
    let schema = "ds_test_vat_verdict_repeated";
    let Some(mut client) = seeded(schema).await else {
        return;
    };
    let mut debited = staked_range(0..STAKED - 2);
    debited.extend(staked_range(0..10));
    debited.extend((1..=3).map(|index| format!("VatVerdictUnknown{index}")));

    let err = store_verdict(&mut client, schema, EPOCH, debited)
        .await
        .unwrap_err();

    assert!(
        err.to_string()
            .contains("3 of 21 debited vote accounts are unknown"),
        "{err}"
    );
    assert_eq!(written_verdicts(&client).await, 0);
}

#[tokio::test]
async fn more_than_ten_percent_undebited_staked_accounts_write_nothing() {
    let schema = "ds_test_vat_verdict_undebited";
    let Some(mut client) = seeded(schema).await else {
        return;
    };

    let err = store_verdict(&mut client, schema, EPOCH, staked_range(0..STAKED - 3))
        .await
        .unwrap_err();

    assert!(
        err.to_string()
            .contains("3 of 20 staked vote accounts are not debited"),
        "{err}"
    );
    assert_eq!(written_verdicts(&client).await, 0);
}

#[tokio::test]
async fn load_validators_serves_vat_admitted_per_epoch() {
    let schema = "ds_test_vat_verdict_served";
    let Some(mut client) = seeded(schema).await else {
        return;
    };
    store_verdict(&mut client, schema, EPOCH, staked_range(0..STAKED - 1))
        .await
        .unwrap();
    let refused = staked(STAKED - 1);
    let mut running_epoch = validator_snapshot(EPOCH + 1, "identityDebited", &staked(0));
    running_epoch
        .validators
        .append(&mut validator_snapshot(EPOCH + 1, "identityRefused", &refused).validators);
    store_snapshot(&mut client, schema, &running_epoch).await;
    client
        .execute(
            "INSERT INTO cluster_info (epoch_slot, epoch, transaction_count, created_at)
             VALUES (0, $1, 0, NOW())",
            &[&Decimal::from(EPOCH + 1)],
        )
        .await
        .unwrap();

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
        served(&staked(0)),
        vec![(EPOCH + 1, None), (EPOCH, Some(true))]
    );
    assert_eq!(
        served(&refused),
        vec![(EPOCH + 1, None), (EPOCH, Some(false)), (EPOCH - 1, None)]
    );
    assert_eq!(served(UNSTAKED), vec![(EPOCH, None)]);
}
