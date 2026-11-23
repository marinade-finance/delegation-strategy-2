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
const MINIMUM: u64 = 1_000_000;

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
    for validator in &mut snapshot.validators {
        validator.vote_account_lamports = Some(2 * MINIMUM);
        validator.inflation_rewards_commission_bps_is_v4 = Some(true);
        validator.bls_pubkey = Some("blsKey".into());
    }
    store_snapshot(client, schema, &snapshot).await;
    store_next_epoch(client, schema, staked_range(0..STAKED), 100).await;
    client
        .execute(
            "INSERT INTO cluster_info (epoch_slot, epoch, transaction_count, created_at, vat_minimum_balance)
             VALUES (0, $1, 0, NOW(), $2)",
            &[&Decimal::from(EPOCH), &Decimal::from(MINIMUM)],
        )
        .await
        .unwrap();
    let previous = staked(STAKED - 1);
    store_snapshot(
        client,
        schema,
        &validator_snapshot(EPOCH - 1, "identityPrevious", &previous),
    )
    .await;
}

async fn store_next_epoch(client: &mut Client, schema: &str, accounts: Vec<String>, stake: u64) {
    let mut snapshot = validator_snapshot(EPOCH + 1, "identityNext", &accounts[0]);
    snapshot.validators.clear();
    for vote_account in accounts {
        let mut validators =
            validator_snapshot(EPOCH + 1, &format!("identity{vote_account}"), &vote_account)
                .validators;
        validators[0].activated_stake = stake;
        snapshot.validators.append(&mut validators);
    }
    store_snapshot(client, &format!("{schema}-next"), &snapshot).await;
}

async fn delete_next_epoch(client: &Client, vote_account: &str) {
    client
        .execute(
            "DELETE FROM validators WHERE vote_account = $1 AND epoch = $2",
            &[&vote_account, &Decimal::from(EPOCH + 1)],
        )
        .await
        .unwrap();
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

async fn set_column(client: &Client, column: &str, value: Option<bool>, accounts: Vec<String>) {
    client
        .execute(
            &format!(
                "UPDATE validators SET {column} = $1 WHERE vote_account = ANY($2) AND epoch = $3"
            ),
            &[&value, &accounts, &Decimal::from(EPOCH)],
        )
        .await
        .unwrap();
}

async fn set_lamports(client: &Client, lamports: Option<u64>, accounts: Vec<String>) {
    client
        .execute(
            "UPDATE validators SET vote_account_lamports = $1 WHERE vote_account = ANY($2) AND epoch = $3",
            &[&lamports.map(Decimal::from), &accounts, &Decimal::from(EPOCH)],
        )
        .await
        .unwrap();
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

    let err = store_verdict(&mut client, schema, EPOCH + 2, staked_range(0..STAKED))
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
async fn more_than_ten_percent_undebited_forecast_admitted_accounts_write_nothing() {
    let schema = "ds_test_vat_verdict_undebited";
    let Some(mut client) = seeded(schema).await else {
        return;
    };

    let err = store_verdict(&mut client, schema, EPOCH, staked_range(0..STAKED - 3))
        .await
        .unwrap_err();

    assert!(
        err.to_string()
            .contains("3 of 20 forecast-admitted vote accounts are not debited"),
        "{err}"
    );
    assert_eq!(written_verdicts(&client).await, 0);
}

#[tokio::test]
async fn exactly_ten_percent_undebited_forecast_admitted_accounts_are_accepted() {
    let schema = "ds_test_vat_verdict_undebited_limit";
    let Some(mut client) = seeded(schema).await else {
        return;
    };

    store_verdict(&mut client, schema, EPOCH, staked_range(0..STAKED - 2))
        .await
        .unwrap();

    assert_eq!(vat_admitted(&client, &staked(0), EPOCH).await, Some(true));
}

#[tokio::test]
async fn many_forecast_refused_undebited_accounts_are_accepted() {
    let schema = "ds_test_vat_verdict_forecast_refused";
    let Some(mut client) = seeded(schema).await else {
        return;
    };
    set_lamports(&client, Some(MINIMUM - 1), staked_range(STAKED - 6..STAKED)).await;

    store_verdict(&mut client, schema, EPOCH, staked_range(0..STAKED - 6))
        .await
        .unwrap();

    assert_eq!(vat_admitted(&client, &staked(0), EPOCH).await, Some(true));
}

#[tokio::test]
async fn rows_without_a_forecast_stay_out_of_the_undebited_share() {
    let schema = "ds_test_vat_verdict_forecast_less";
    let Some(mut client) = seeded(schema).await else {
        return;
    };
    set_lamports(&client, None, staked_range(STAKED - 3..STAKED)).await;
    set_column(
        &client,
        "inflation_rewards_commission_bps_is_v4",
        None,
        staked_range(STAKED - 6..STAKED - 3),
    )
    .await;

    store_verdict(&mut client, schema, EPOCH, staked_range(0..STAKED - 6))
        .await
        .unwrap();

    assert_eq!(vat_admitted(&client, &staked(0), EPOCH).await, Some(true));
}

#[tokio::test]
async fn an_epoch_without_a_forecast_writes_nothing() {
    let schema = "ds_test_vat_verdict_no_forecast";
    let Some(mut client) = seeded(schema).await else {
        return;
    };
    client
        .execute("UPDATE cluster_info SET vat_minimum_balance = NULL", &[])
        .await
        .unwrap();

    let err = store_verdict(&mut client, schema, EPOCH, staked_range(0..STAKED))
        .await
        .unwrap_err();

    assert!(err.to_string().contains("holds no VAT forecast"), "{err}");
    assert_eq!(written_verdicts(&client).await, 0);
}

#[tokio::test]
async fn an_epoch_older_than_the_api_window_still_gets_its_verdict() {
    let schema = "ds_test_vat_verdict_old_epoch";
    let Some(mut client) = seeded(schema).await else {
        return;
    };
    client
        .execute(
            "INSERT INTO cluster_info (epoch_slot, epoch, transaction_count, created_at, vat_minimum_balance)
             VALUES (0, $1, 0, NOW(), $2)",
            &[&Decimal::from(EPOCH + 100), &Decimal::from(MINIMUM)],
        )
        .await
        .unwrap();

    store_verdict(&mut client, schema, EPOCH, staked_range(0..STAKED - 1))
        .await
        .unwrap();

    assert_eq!(vat_admitted(&client, &staked(0), EPOCH).await, Some(true));
}

#[tokio::test]
async fn a_refused_verdict_keeps_the_stored_one() {
    let schema = "ds_test_vat_verdict_keeps";
    let Some(mut client) = seeded(schema).await else {
        return;
    };
    store_verdict(&mut client, schema, EPOCH, staked_range(0..STAKED - 1))
        .await
        .unwrap();

    store_verdict(&mut client, schema, EPOCH, staked_range(0..STAKED - 5))
        .await
        .unwrap_err();

    assert_eq!(
        vat_admitted(&client, &staked(STAKED - 2), EPOCH).await,
        Some(true)
    );
    assert_eq!(
        vat_admitted(&client, &staked(STAKED - 1), EPOCH).await,
        Some(false)
    );
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

#[tokio::test]
async fn an_account_deactivated_at_the_boundary_gets_no_verdict() {
    let schema = "ds_test_vat_verdict_deactivated";
    let Some(mut client) = seeded(schema).await else {
        return;
    };
    store_next_epoch(&mut client, schema, vec![staked(STAKED - 1)], 0).await;

    store_verdict(&mut client, schema, EPOCH, staked_range(0..STAKED - 1))
        .await
        .unwrap();

    assert_eq!(
        vat_admitted(&client, &staked(STAKED - 1), EPOCH).await,
        None
    );
}

#[tokio::test]
async fn an_account_without_a_next_epoch_row_gets_no_verdict() {
    let schema = "ds_test_vat_verdict_no_next";
    let Some(mut client) = seeded(schema).await else {
        return;
    };
    delete_next_epoch(&client, &staked(STAKED - 1)).await;

    store_verdict(&mut client, schema, EPOCH, staked_range(0..STAKED - 1))
        .await
        .unwrap();

    assert_eq!(
        vat_admitted(&client, &staked(STAKED - 1), EPOCH).await,
        None
    );
}

#[tokio::test]
async fn an_undebited_account_staked_in_the_next_epoch_is_refused() {
    let schema = "ds_test_vat_verdict_next_staked";
    let Some(mut client) = seeded(schema).await else {
        return;
    };

    store_verdict(&mut client, schema, EPOCH, staked_range(0..STAKED - 1))
        .await
        .unwrap();

    assert_eq!(
        vat_admitted(&client, &staked(STAKED - 1), EPOCH).await,
        Some(false)
    );
}

#[tokio::test]
async fn a_debited_account_is_admitted_whatever_its_next_epoch_row() {
    let schema = "ds_test_vat_verdict_debited_no_next";
    let Some(mut client) = seeded(schema).await else {
        return;
    };
    delete_next_epoch(&client, &staked(0)).await;

    store_verdict(&mut client, schema, EPOCH, staked_range(0..STAKED - 1))
        .await
        .unwrap();

    assert_eq!(vat_admitted(&client, &staked(0), EPOCH).await, Some(true));
}

#[tokio::test]
async fn a_rerun_after_the_next_epoch_row_arrives_stores_the_refusal() {
    let schema = "ds_test_vat_verdict_late_next";
    let Some(mut client) = seeded(schema).await else {
        return;
    };
    let refused = staked(STAKED - 1);
    delete_next_epoch(&client, &refused).await;

    store_verdict(&mut client, schema, EPOCH, staked_range(0..STAKED - 1))
        .await
        .unwrap();
    assert_eq!(vat_admitted(&client, &refused, EPOCH).await, None);

    store_next_epoch(&mut client, schema, vec![refused.clone()], 100).await;
    store_verdict(&mut client, schema, EPOCH, staked_range(0..STAKED - 1))
        .await
        .unwrap();
    assert_eq!(vat_admitted(&client, &refused, EPOCH).await, Some(false));

    store_next_epoch(&mut client, schema, vec![refused.clone()], 0).await;
    store_verdict(&mut client, schema, EPOCH, staked_range(0..STAKED - 1))
        .await
        .unwrap();
    assert_eq!(vat_admitted(&client, &refused, EPOCH).await, None);
}

#[tokio::test]
async fn an_account_with_only_activating_stake_is_refused_on_its_next_epoch_stake() {
    let schema = "ds_test_vat_verdict_activating";
    let Some(mut client) = seeded(schema).await else {
        return;
    };
    let refused = staked(STAKED - 1);
    client
        .execute(
            "UPDATE validators SET activated_stake = 0, activating_stake = 100
             WHERE vote_account = $1 AND epoch = $2",
            &[&refused, &Decimal::from(EPOCH)],
        )
        .await
        .unwrap();

    store_verdict(&mut client, schema, EPOCH, staked_range(0..STAKED - 1))
        .await
        .unwrap();

    assert_eq!(vat_admitted(&client, &refused, EPOCH).await, Some(false));
}
