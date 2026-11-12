mod common;

use common::{migrated_client, skip_without_database};
use rust_decimal::Decimal;
use store::dto::UnstakeHint;
use store::scoring::load_unstake_hints;
use tokio_postgres::Client;

const EPOCH: u64 = 1080;

async fn blacklisted(client: &Client, vote_account: &str, removed_epoch: Option<u64>) {
    client
        .execute(
            "INSERT INTO blacklist (vote_account, source, code, removed_epoch, created_at, updated_at)
             VALUES ($1, 'legacy', 'SLOW_VOTER', $2, now(), now())",
            &[&vote_account, &removed_epoch.map(Decimal::from)],
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn an_active_blacklist_row_is_an_unstake_hint() {
    let schema = "ds_test_unstake_hints_blacklist_active";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();
    blacklisted(&client, "voteA", None).await;

    let hints = load_unstake_hints(&client, EPOCH).await.unwrap();

    assert_eq!(
        hints
            .get("voteA")
            .map(|h| h.iter().cloned().collect::<Vec<_>>()),
        Some(vec![UnstakeHint::Blacklist])
    );
}

#[tokio::test]
async fn a_closed_row_is_not_an_unstake_hint() {
    let schema = "ds_test_unstake_hints_blacklist_closed";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();
    blacklisted(&client, "voteA", Some(EPOCH)).await;

    let hints = load_unstake_hints(&client, EPOCH).await.unwrap();

    assert!(!hints.contains_key("voteA"));
}
