mod common;

use clap::Parser;
use common::{migrated_client, skip_without_database};
use rust_decimal::Decimal;
use store::blacklist::config_sync::{config_sync, ConfigSyncParams};
use tokio_postgres::Client;

async fn run(client: &mut Client, name: &str, config: &str) -> anyhow::Result<()> {
    let path = std::env::temp_dir().join(format!("ds-test-{name}-blacklist.yaml"));
    std::fs::write(&path, config).unwrap();
    let result = config_sync(
        ConfigSyncParams::parse_from(["store", "--config", path.to_str().unwrap()]),
        client,
    )
    .await;
    std::fs::remove_file(path).unwrap();
    result
}

async fn insert_row(client: &Client, vote_account: &str, source: &str, code: &str) {
    let auction = (source == "auction").then(|| Decimal::from(1085));
    client
        .execute(
            "INSERT INTO blacklist (vote_account, source, code, auction_epoch, run_id, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, now(), now())",
            &[
                &vote_account,
                &source,
                &code,
                &auction,
                &auction.map(|_| "1085.40290"),
            ],
        )
        .await
        .unwrap();
}

/// `(vote_account, source, code)` of the active rows, ordered.
async fn active(client: &Client) -> Vec<(String, String, String)> {
    client
        .query(
            "SELECT vote_account, source, code FROM blacklist WHERE removed_epoch IS NULL
             ORDER BY vote_account, source, code",
            &[],
        )
        .await
        .unwrap()
        .into_iter()
        .map(|row| (row.get(0), row.get(1), row.get(2)))
        .collect()
}

fn rows(expected: &[(&str, &str, &str)]) -> Vec<(String, String, String)> {
    expected
        .iter()
        .map(|(a, s, c)| (a.to_string(), s.to_string(), c.to_string()))
        .collect()
}

async fn setup(schema: &str) -> Option<Client> {
    if skip_without_database(schema) {
        return None;
    }
    let client = migrated_client(schema).await.unwrap();
    client
        .execute(
            "INSERT INTO cluster_info (epoch_slot, epoch, transaction_count, created_at)
             VALUES (0, 1085, 0, now())",
            &[],
        )
        .await
        .unwrap();
    Some(client)
}

const MANUAL_B: &str = "manual: [{ vote_account: BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB, code: MISSING_BLS, reason: 'SIMD-0357' }]";
const A: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const L: &str = "LLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLL";
const R: &str = "RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR";

#[tokio::test]
async fn sync_is_idempotent() {
    let schema = "ds_test_blacklist_sync_idempotent";
    let Some(mut client) = setup(schema).await else {
        return;
    };

    run(&mut client, schema, MANUAL_B).await.unwrap();
    run(&mut client, schema, MANUAL_B).await.unwrap();

    assert_eq!(
        active(&client).await,
        rows(&[("BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB", "manual", "MISSING_BLS")])
    );
    let total: i64 = client
        .query_one("SELECT COUNT(*) FROM blacklist", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(total, 1);
}

#[tokio::test]
async fn a_removed_manual_entry_closes_its_row() {
    let schema = "ds_test_blacklist_sync_manual_removed";
    let Some(mut client) = setup(schema).await else {
        return;
    };

    run(&mut client, schema, MANUAL_B).await.unwrap();
    run(&mut client, schema, "manual: []").await.unwrap();

    assert!(active(&client).await.is_empty());
    let row = client
        .query_one(
            "SELECT removed_epoch, removed_reason FROM blacklist WHERE vote_account = 'BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB'",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(row.get::<_, Decimal>(0), Decimal::from(1085));
    assert_eq!(row.get::<_, String>(1), "removed from config");
}

#[tokio::test]
async fn a_changed_reason_updates_the_active_row() {
    let schema = "ds_test_blacklist_sync_manual_reason";
    let Some(mut client) = setup(schema).await else {
        return;
    };

    run(&mut client, schema, MANUAL_B).await.unwrap();
    run(
        &mut client,
        schema,
        "manual: [{ vote_account: BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB, code: MISSING_BLS, reason: 'no BLS key' }]",
    )
    .await
    .unwrap();

    let reasons: Vec<String> = client
        .query("SELECT reason FROM blacklist", &[])
        .await
        .unwrap()
        .into_iter()
        .map(|row| row.get(0))
        .collect();
    assert_eq!(reasons, vec!["no BLS key"]);
}

#[tokio::test]
async fn sync_never_closes_legacy_or_auction_rows() {
    let schema = "ds_test_blacklist_sync_kept";
    let Some(mut client) = setup(schema).await else {
        return;
    };
    insert_row(&client, L, "legacy", "SLOW_VOTER").await;
    insert_row(&client, A, "auction", "MALICIOUS_SM_1084").await;

    run(&mut client, schema, "manual: []").await.unwrap();

    assert_eq!(
        active(&client).await,
        rows(&[
            (A, "auction", "MALICIOUS_SM_1084"),
            (L, "legacy", "SLOW_VOTER")
        ])
    );
}

#[tokio::test]
async fn exempt_closes_only_matching_rows() {
    let schema = "ds_test_blacklist_sync_exempt";
    let Some(mut client) = setup(schema).await else {
        return;
    };
    insert_row(&client, A, "auction", "SLOW_SLOTS_1084").await;
    insert_row(&client, A, "auction", "MALICIOUS_SM_1084").await;
    insert_row(&client, A, "legacy", "SLOW_VOTER").await;
    insert_row(&client, L, "legacy", "MALICIOUS_SM").await;
    insert_row(&client, L, "auction", "SLOW_SLOTS_1084").await;
    insert_row(&client, R, "auction", "SLOW_SLOTS_1084").await;
    insert_row(&client, R, "legacy", "SLOW_VOTER").await;

    run(
        &mut client,
        schema,
        "manual: [{ vote_account: RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR, code: MISSING_BLS, reason: 'no BLS key' }]
exempt:
  - { vote_account: AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA, rule: slow_slots, reason: 'appeal' }
  - { vote_account: LLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLL, rule: legacy, reason: 'appeal' }
  - { vote_account: RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR, rule: all, reason: 'appeal' }",
    )
    .await
    .unwrap();

    assert_eq!(
        active(&client).await,
        rows(&[
            (A, "auction", "MALICIOUS_SM_1084"),
            (A, "legacy", "SLOW_VOTER"),
            (L, "auction", "SLOW_SLOTS_1084"),
            (R, "manual", "MISSING_BLS"),
        ])
    );
    let reason: String = client
        .query_one(
            "SELECT removed_reason FROM blacklist WHERE vote_account = 'LLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLL' AND source = 'legacy'",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(reason, "exempt: appeal");
}

// An exemption limited by until_epoch closes only a listing judged on evidence up to it.
#[tokio::test]
async fn an_until_epoch_exemption_keeps_a_later_offence() {
    let schema = "ds_test_blacklist_sync_exempt_until";
    let Some(mut client) = setup(schema).await else {
        return;
    };
    insert_row(&client, A, "auction", "SLOW_SLOTS_1083").await;
    insert_row(&client, A, "auction", "SLOW_SLOTS_1084").await;

    run(
        &mut client,
        schema,
        "exempt: [{ vote_account: AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA, rule: slow_slots, until_epoch: 1083, reason: 'fixed client' }]",
    )
    .await
    .unwrap();

    assert_eq!(
        active(&client).await,
        rows(&[(A, "auction", "SLOW_SLOTS_1084")])
    );
}

#[tokio::test]
async fn the_exemptions_are_stored_without_their_reasons() {
    let schema = "ds_test_blacklist_sync_exempt_table";
    let Some(mut client) = setup(schema).await else {
        return;
    };
    let exemptions = "exempt:
  - { vote_account: AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA, rule: sandwich, until_epoch: 1083, reason: 'appeal' }
  - { vote_account: LLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLL, reason: 'appeal' }";
    run(&mut client, schema, exemptions).await.unwrap();
    run(&mut client, schema, exemptions).await.unwrap();

    let stored: Vec<(String, String, Option<Decimal>)> = client
        .query(
            "SELECT vote_account, rule, until_epoch FROM blacklist_exempt ORDER BY vote_account",
            &[],
        )
        .await
        .unwrap()
        .into_iter()
        .map(|row| (row.get(0), row.get(1), row.get(2)))
        .collect();
    assert_eq!(
        stored,
        vec![
            (A.into(), "sandwich".into(), Some(Decimal::from(1083))),
            (L.into(), "all".into(), None),
        ]
    );

    run(&mut client, schema, "exempt: []").await.unwrap();
    let left: i64 = client
        .query_one("SELECT COUNT(*) FROM blacklist_exempt", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(left, 0);
}

#[tokio::test]
async fn a_bad_config_writes_nothing() {
    let schema = "ds_test_blacklist_sync_bad_config";
    let Some(mut client) = setup(schema).await else {
        return;
    };

    let error = run(&mut client, schema, &format!("{MANUAL_B}\nbogus: 1"))
        .await
        .unwrap_err();

    assert!(format!("{error:#}").contains("bogus"), "{error:#}");
    assert!(active(&client).await.is_empty());
}
