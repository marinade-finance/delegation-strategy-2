mod common;

use common::{migrated_client, skip_without_database};
use rust_decimal::Decimal;
use store::blacklist::db::{last_evaluated_epoch, load_active};
use tokio_postgres::Client;

async fn row(
    client: &Client,
    vote_account: &str,
    code: &str,
    evidence: Option<&str>,
    removed_epoch: Option<u64>,
) {
    let rule = evidence.map(|_| "sandwich");
    let source = if rule.is_some() { "rule" } else { "legacy" };
    client
        .execute(
            "INSERT INTO blacklist (vote_account, source, rule, code, evidence, added_epoch,
                removed_epoch, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5::TEXT::JSONB, $6, $7, now(), now())",
            &[
                &vote_account,
                &source,
                &rule,
                &code,
                &evidence,
                &rule.map(|_| Decimal::from(1081)),
                &removed_epoch.map(Decimal::from),
            ],
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn load_active_orders_by_account_then_code_and_skips_closed_rows() {
    let schema = "ds_test_blacklist_db_active";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();
    row(&client, "B", "SLOW_VOTER", None, None).await;
    row(&client, "A", "SLOW_VOTER", None, None).await;
    row(&client, "A", "MALICIOUS_SM", None, None).await;
    row(&client, "C", "SLOW_VOTER", None, Some(1080)).await;

    let active: Vec<(String, String)> = load_active(&client)
        .await
        .unwrap()
        .into_iter()
        .map(|row| (row.vote_account, row.code))
        .collect();

    assert_eq!(
        active,
        vec![
            ("A".into(), "MALICIOUS_SM".into()),
            ("A".into(), "SLOW_VOTER".into()),
            ("B".into(), "SLOW_VOTER".into()),
        ]
    );
}

#[tokio::test]
async fn evidence_round_trips_as_json() {
    let schema = "ds_test_blacklist_db_evidence";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();
    row(
        &client,
        "A",
        "MALICIOUS_SANDWICHED_ME_SINGLE_1080",
        Some(r#"{"epoch": 1080, "single": {"p_value": 1e-20}, "wide": null}"#),
        None,
    )
    .await;

    let loaded = load_active(&client).await.unwrap().remove(0);

    assert_eq!(loaded.rule.as_deref(), Some("sandwich"));
    assert_eq!(loaded.added_epoch, Some(1081));
    assert_eq!(
        loaded.evidence,
        Some(serde_json::json!({"epoch": 1080, "single": {"p_value": 1e-20}, "wide": null}))
    );
}

#[tokio::test]
async fn no_evaluation_yet_has_no_evaluated_epoch() {
    let schema = "ds_test_blacklist_db_evaluated";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();
    assert_eq!(last_evaluated_epoch(&client).await.unwrap(), None);

    client
        .execute(
            "INSERT INTO blacklist_evaluations (epoch, rows_added, evaluated_at) VALUES (1080, 0, now()), (1081, 2, now())",
            &[],
        )
        .await
        .unwrap();

    assert_eq!(last_evaluated_epoch(&client).await.unwrap(), Some(1081));
}
