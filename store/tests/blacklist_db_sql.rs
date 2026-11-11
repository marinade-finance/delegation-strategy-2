mod common;

use common::{migrated_client, skip_without_database};
use rust_decimal::Decimal;
use store::blacklist::db::{
    import_lag_epochs, load_active_accounts, load_blacklist, BlacklistExempt, BlacklistRow,
};
use tokio_postgres::Client;

async fn row(
    client: &Client,
    vote_account: &str,
    source: &str,
    code: &str,
    removed_epoch: Option<u64>,
) {
    let auction = (source == "auction").then(|| Decimal::from(1085));
    client
        .execute(
            "INSERT INTO blacklist (vote_account, source, code, auction_epoch, run_id, removed_epoch,
                created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, now(), now())",
            &[
                &vote_account,
                &source,
                &code,
                &auction,
                &auction.map(|_| "1085.40290"),
                &removed_epoch.map(Decimal::from),
            ],
        )
        .await
        .unwrap();
}

async fn client(schema: &str) -> Option<Client> {
    if skip_without_database(schema) {
        return None;
    }
    Some(migrated_client(schema).await.unwrap())
}

#[tokio::test]
async fn nothing_is_served_before_the_legacy_import() {
    let schema = "ds_test_blacklist_db_bootstrap";
    let Some(client) = client(schema).await else {
        return;
    };
    row(&client, "M", "manual", "MISSING_BLS", None).await;

    assert_eq!(load_blacklist(&client).await.unwrap(), None);
    assert!(
        format!("{:#}", load_active_accounts(&client).await.unwrap_err())
            .contains("not bootstrapped")
    );

    row(&client, "L", "legacy", "SLOW_VOTER", None).await;

    assert_eq!(
        load_blacklist(&client).await.unwrap().unwrap().rows.len(),
        2
    );
    assert_eq!(load_active_accounts(&client).await.unwrap().len(), 2);
}

#[tokio::test]
async fn load_active_orders_by_account_then_code_and_skips_closed_rows() {
    let schema = "ds_test_blacklist_db_active";
    let Some(client) = client(schema).await else {
        return;
    };
    row(&client, "B", "legacy", "SLOW_VOTER", None).await;
    row(&client, "A", "legacy", "SLOW_VOTER", None).await;
    row(&client, "A", "auction", "MALICIOUS_SM_1084", None).await;
    row(&client, "C", "legacy", "SLOW_VOTER", Some(1080)).await;

    let rows = load_blacklist(&client).await.unwrap().unwrap().rows;

    assert_eq!(
        rows,
        vec![
            BlacklistRow {
                vote_account: "A".into(),
                source: "auction".into(),
                code: "MALICIOUS_SM_1084".into(),
                auction_epoch: Some(1085),
            },
            BlacklistRow {
                vote_account: "A".into(),
                source: "legacy".into(),
                code: "SLOW_VOTER".into(),
                auction_epoch: None,
            },
            BlacklistRow {
                vote_account: "B".into(),
                source: "legacy".into(),
                code: "SLOW_VOTER".into(),
                auction_epoch: None,
            },
        ]
    );
}

#[tokio::test]
async fn the_exemptions_are_served_with_the_rows() {
    let schema = "ds_test_blacklist_db_exempt";
    let Some(client) = client(schema).await else {
        return;
    };
    row(&client, "L", "legacy", "SLOW_VOTER", None).await;
    client
        .execute(
            "INSERT INTO blacklist_exempt (vote_account, rule, until_epoch) VALUES ('A', 'sandwich', 1083), ('B', 'all', NULL)",
            &[],
        )
        .await
        .unwrap();

    assert_eq!(
        load_blacklist(&client).await.unwrap().unwrap().exempt,
        vec![
            BlacklistExempt {
                vote_account: "A".into(),
                rule: "sandwich".into(),
                until_epoch: Some(1083),
            },
            BlacklistExempt {
                vote_account: "B".into(),
                rule: "all".into(),
                until_epoch: None,
            },
        ]
    );
}

#[tokio::test]
async fn an_auction_row_needs_its_epoch_and_run() {
    let schema = "ds_test_blacklist_db_auction_check";
    let Some(client) = client(schema).await else {
        return;
    };
    let error = client
        .execute(
            "INSERT INTO blacklist (vote_account, source, code, created_at, updated_at)
             VALUES ('A', 'auction', 'MALICIOUS_SM_1084', now(), now())",
            &[],
        )
        .await
        .unwrap_err();
    assert_eq!(
        error.code(),
        Some(&tokio_postgres::error::SqlState::CHECK_VIOLATION),
        "{error:?}"
    );
}

async fn current_epoch(client: &Client, epoch: u64) {
    client
        .execute(
            "INSERT INTO cluster_info (epoch_slot, epoch, transaction_count, created_at)
             VALUES (0, $1, 0, now())",
            &[&Decimal::from(epoch)],
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn the_import_lag_is_zero_before_the_first_import() {
    let schema = "ds_test_blacklist_db_lag_none";
    let Some(client) = client(schema).await else {
        return;
    };
    current_epoch(&client, 1085).await;

    assert_eq!(import_lag_epochs(&client).await.unwrap(), 0);
}

#[tokio::test]
async fn the_import_lag_counts_from_the_last_imported_auction() {
    let schema = "ds_test_blacklist_db_lag";
    let Some(client) = client(schema).await else {
        return;
    };
    current_epoch(&client, 1085).await;
    client
        .execute(
            "INSERT INTO blacklist_auction_imports (auction_epoch, run_id, rows, imported_at)
             VALUES (1082, '1082.1', 0, now()), (1083, '1083.1', 1, now())",
            &[],
        )
        .await
        .unwrap();

    assert_eq!(import_lag_epochs(&client).await.unwrap(), 2);
}
