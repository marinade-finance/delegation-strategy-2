mod common;

use clap::Parser;
use common::{migrated_client, skip_without_database};
use rust_decimal::Decimal;
use store::blacklist::evaluate::{evaluate, EvaluateParams};
use tokio_postgres::Client;

const RULES: &str = "rules:
  start_epoch: 1080
  slow_slots: { median_mult: 1.10, consecutive_epochs: 2 }
  sandwich: { rate_pct: 4.0 }
";

async fn run(client: &mut Client, name: &str, config: &str) -> anyhow::Result<()> {
    let path = std::env::temp_dir().join(format!("ds-test-{name}-blacklist.yaml"));
    std::fs::write(&path, config).unwrap();
    let result = evaluate(
        EvaluateParams::parse_from(["store", "--config", path.to_str().unwrap()]),
        client,
    )
    .await;
    std::fs::remove_file(path).unwrap();
    result
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

async fn closed_epoch(client: &Client, epoch: u64) {
    client
        .execute(
            "INSERT INTO epochs (epoch, start_at, end_at, transaction_count, supply, inflation, inflation_taper, slots_per_year)
             VALUES ($1, now(), now(), 0, 0, 0, 0.15, 0)",
            &[&Decimal::from(epoch)],
        )
        .await
        .unwrap();
}

async fn validator(client: &Client, epoch: u64, vote_account: &str, stake: u64, slot_ms: f64) {
    client
        .execute(
            "INSERT INTO validators (
                identity, vote_account, epoch, activated_stake, marinade_stake,
                marinade_native_stake, superminority, stake_to_become_superminority, credits,
                leader_slots, blocks_produced, skip_rate, updated_at
            ) VALUES ($1, $2, $3, $4, 0, 0, false, 0, 0, 100, 100, 0, now())",
            &[
                &format!("identity-{vote_account}"),
                &vote_account,
                &Decimal::from(epoch),
                &Decimal::from(stake),
            ],
        )
        .await
        .unwrap();
    client
        .execute(
            "INSERT INTO validators_slot_times (epoch, vote_account, source, slot_duration_median_ms, created_at, updated_at)
             VALUES ($1, $2, 'trillium', $3, now(), now())",
            &[&Decimal::from(epoch), &vote_account, &slot_ms],
        )
        .await
        .unwrap();
}

async fn sandwiches(client: &Client, epoch: u64, vote_account: &str, blocks: u64, sandwiched: u64) {
    let rate = (1000.0 * sandwiched as f64 / blocks as f64).round() / 10.0;
    client
        .execute(
            "INSERT INTO validators_sandwiches (epoch, vote_account, blocks_produced, blocks_with_sandwiches,
                sandwich_rate_30d, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, now(), now())",
            &[
                &Decimal::from(epoch),
                &vote_account,
                &Decimal::from(blocks),
                &Decimal::from(sandwiched),
                &rate,
            ],
        )
        .await
        .unwrap();
}

/// A closed epoch with every input stored: three staked validators at ~200 ms and a sandwich
/// cluster at ~0.8 %, plus `slow` at `slow_ms` and `sandwicher` with 400 of 5000 blocks.
async fn epoch_with_inputs(client: &Client, epoch: u64, slow_ms: f64, sandwicher: bool) {
    closed_epoch(client, epoch).await;
    for (account, ms) in [("n1", 200.0), ("n2", 200.0), ("n3", 205.0)] {
        validator(client, epoch, account, 1_000_000, ms).await;
    }
    validator(client, epoch, "slow", 10_000, slow_ms).await;
    for index in 0..40 {
        sandwiches(client, epoch, &format!("s{index:02}"), 5000, 40).await;
    }
    if sandwicher {
        sandwiches(client, epoch, "sandwicher", 5000, 400).await;
    }
}

async fn insert_row(
    client: &Client,
    vote_account: &str,
    source: &str,
    rule: Option<&str>,
    code: &str,
) {
    client
        .execute(
            "INSERT INTO blacklist (vote_account, source, rule, code, evidence_from_epoch, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, now(), now())",
            &[
                &vote_account,
                &source,
                &rule,
                &code,
                &rule.map(|_| Decimal::from(1070)),
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

async fn evaluated_epochs(client: &Client) -> Vec<Decimal> {
    client
        .query(
            "SELECT epoch FROM blacklist_evaluations ORDER BY epoch",
            &[],
        )
        .await
        .unwrap()
        .into_iter()
        .map(|row| row.get(0))
        .collect()
}

async fn setup(schema: &str) -> Option<Client> {
    if skip_without_database(schema) {
        return None;
    }
    let client = migrated_client(schema).await.unwrap();
    current_epoch(&client, 1085).await;
    Some(client)
}

const MANUAL_B: &str = "manual: [{ vote_account: B, code: MISSING_BLS, reason: 'SIMD-0357' }]";

#[tokio::test]
async fn sync_is_idempotent() {
    let schema = "ds_test_blacklist_eval_idempotent";
    let Some(mut client) = setup(schema).await else {
        return;
    };

    run(&mut client, schema, MANUAL_B).await.unwrap();
    run(&mut client, schema, MANUAL_B).await.unwrap();

    assert_eq!(
        active(&client).await,
        rows(&[("B", "manual", "MISSING_BLS")])
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
    let schema = "ds_test_blacklist_eval_manual_removed";
    let Some(mut client) = setup(schema).await else {
        return;
    };

    run(&mut client, schema, MANUAL_B).await.unwrap();
    run(&mut client, schema, "manual: []").await.unwrap();

    assert!(active(&client).await.is_empty());
    let row = client
        .query_one(
            "SELECT removed_epoch, removed_reason FROM blacklist WHERE vote_account = 'B'",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(row.get::<_, Decimal>(0), Decimal::from(1085));
    assert_eq!(row.get::<_, String>(1), "removed from config");
}

#[tokio::test]
async fn a_changed_reason_updates_the_active_row() {
    let schema = "ds_test_blacklist_eval_manual_reason";
    let Some(mut client) = setup(schema).await else {
        return;
    };

    run(&mut client, schema, MANUAL_B).await.unwrap();
    run(
        &mut client,
        schema,
        "manual: [{ vote_account: B, code: MISSING_BLS, reason: 'no BLS key' }]",
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
async fn sync_never_closes_legacy_rows() {
    let schema = "ds_test_blacklist_eval_legacy_kept";
    let Some(mut client) = setup(schema).await else {
        return;
    };
    insert_row(&client, "L", "legacy", None, "SLOW_VOTER").await;

    run(&mut client, schema, "manual: []").await.unwrap();

    assert_eq!(
        active(&client).await,
        rows(&[("L", "legacy", "SLOW_VOTER")])
    );
}

#[tokio::test]
async fn exempt_closes_only_matching_rows() {
    let schema = "ds_test_blacklist_eval_exempt";
    let Some(mut client) = setup(schema).await else {
        return;
    };
    insert_row(&client, "A", "rule", Some("slow_slots"), "SLOW_SLOTS_1070").await;
    insert_row(
        &client,
        "A",
        "rule",
        Some("sandwich"),
        "MALICIOUS_SANDWICHED_ME_SINGLE_1070",
    )
    .await;
    insert_row(&client, "A", "legacy", None, "SLOW_VOTER").await;
    insert_row(&client, "L", "legacy", None, "MALICIOUS_SM").await;
    insert_row(&client, "R", "rule", Some("slow_slots"), "SLOW_SLOTS_1070").await;
    insert_row(&client, "R", "legacy", None, "SLOW_VOTER").await;

    run(
        &mut client,
        schema,
        "manual: [{ vote_account: R, code: MISSING_BLS, reason: 'no BLS key' }]
exempt:
  - { vote_account: A, rule: slow_slots, reason: 'appeal' }
  - { vote_account: L, rule: legacy, reason: 'appeal' }
  - { vote_account: R, rule: all, reason: 'appeal' }",
    )
    .await
    .unwrap();

    assert_eq!(
        active(&client).await,
        rows(&[
            ("A", "legacy", "SLOW_VOTER"),
            ("A", "rule", "MALICIOUS_SANDWICHED_ME_SINGLE_1070"),
            ("R", "manual", "MISSING_BLS"),
        ])
    );
    let reason: String = client
        .query_one(
            "SELECT removed_reason FROM blacklist WHERE vote_account = 'L'",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(reason, "exempt: appeal");
}

// An exemption limited by until_epoch closes only the verdict built on evidence up to it.
#[tokio::test]
async fn an_until_epoch_exemption_keeps_a_later_offence() {
    let schema = "ds_test_blacklist_eval_exempt_until";
    let Some(mut client) = setup(schema).await else {
        return;
    };
    insert_row(&client, "A", "rule", Some("slow_slots"), "SLOW_SLOTS_1070").await;
    client
        .execute(
            "UPDATE blacklist SET evidence_from_epoch = 1084 WHERE vote_account = 'A'",
            &[],
        )
        .await
        .unwrap();

    run(
        &mut client,
        schema,
        "exempt: [{ vote_account: A, rule: slow_slots, until_epoch: 1083, reason: 'fixed client' }]",
    )
    .await
    .unwrap();

    assert_eq!(active(&client).await.len(), 1);
}

#[tokio::test]
async fn rules_do_not_run_without_start_epoch() {
    let schema = "ds_test_blacklist_eval_no_start";
    let Some(mut client) = setup(schema).await else {
        return;
    };
    epoch_with_inputs(&client, 1079, 250.0, true).await;
    epoch_with_inputs(&client, 1080, 250.0, true).await;

    run(
        &mut client,
        schema,
        "rules: { slow_slots: { median_mult: 1.10, consecutive_epochs: 2 }, sandwich: { rate_pct: 4.0 } }",
    )
    .await
    .unwrap();

    assert!(active(&client).await.is_empty());
    assert!(evaluated_epochs(&client).await.is_empty());
}

#[tokio::test]
async fn a_slow_slots_hit_lands_with_evidence() {
    let schema = "ds_test_blacklist_eval_slow";
    let Some(mut client) = setup(schema).await else {
        return;
    };
    epoch_with_inputs(&client, 1079, 245.0, false).await;
    epoch_with_inputs(&client, 1080, 250.0, false).await;

    run(&mut client, schema, RULES).await.unwrap();

    assert_eq!(
        active(&client).await,
        rows(&[("slow", "rule", "SLOW_SLOTS_1080")])
    );
    let row = client
        .query_one(
            "SELECT rule, evidence::TEXT, evidence_from_epoch, added_epoch FROM blacklist",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(row.get::<_, String>(0), "slow_slots");
    let evidence: serde_json::Value = serde_json::from_str(&row.get::<_, String>(1)).unwrap();
    assert_eq!(
        evidence["epochs"]["1079"],
        serde_json::json!({"slot_duration_median_ms": 245.0, "network_median_ms": 200.0})
    );
    assert_eq!(row.get::<_, Decimal>(2), Decimal::from(1079));
    assert_eq!(row.get::<_, Decimal>(3), Decimal::from(1085));
    assert_eq!(evaluated_epochs(&client).await, vec![Decimal::from(1080)]);
}

#[tokio::test]
async fn a_sandwich_hit_lands_with_evidence() {
    let schema = "ds_test_blacklist_eval_sandwich";
    let Some(mut client) = setup(schema).await else {
        return;
    };
    epoch_with_inputs(&client, 1080, 200.0, true).await;

    run(&mut client, schema, RULES).await.unwrap();

    assert_eq!(
        active(&client).await,
        rows(&[("sandwicher", "rule", "MALICIOUS_SANDWICHED_ME_SINGLE_1080")])
    );
    let evidence: String = client
        .query_one("SELECT evidence::TEXT FROM blacklist", &[])
        .await
        .unwrap()
        .get(0);
    let evidence: serde_json::Value = serde_json::from_str(&evidence).unwrap();
    assert_eq!(evidence["single"]["blocks_with_sandwiches"], 400);
    assert!(!evidence.to_string().contains("rate_pct\""));
}

#[tokio::test]
async fn an_epoch_with_missing_inputs_stops_without_being_marked() {
    let schema = "ds_test_blacklist_eval_missing";
    let Some(mut client) = setup(schema).await else {
        return;
    };
    epoch_with_inputs(&client, 1080, 200.0, false).await;
    closed_epoch(&client, 1081).await;
    validator(&client, 1081, "n1", 1_000_000, 200.0).await;

    run(&mut client, schema, RULES).await.unwrap();

    assert_eq!(evaluated_epochs(&client).await, vec![Decimal::from(1080)]);
}

#[tokio::test]
async fn an_evaluated_epoch_is_not_re_evaluated_when_its_data_changes() {
    let schema = "ds_test_blacklist_eval_sticky";
    let Some(mut client) = setup(schema).await else {
        return;
    };
    epoch_with_inputs(&client, 1080, 200.0, false).await;
    run(&mut client, schema, RULES).await.unwrap();

    sandwiches(&client, 1080, "late", 5000, 400).await;
    run(&mut client, schema, RULES).await.unwrap();

    assert!(active(&client).await.is_empty());
    assert_eq!(evaluated_epochs(&client).await, vec![Decimal::from(1080)]);
}

#[tokio::test]
async fn one_active_row_per_account_and_rule() {
    let schema = "ds_test_blacklist_eval_one_row";
    let Some(mut client) = setup(schema).await else {
        return;
    };
    epoch_with_inputs(&client, 1080, 200.0, true).await;
    epoch_with_inputs(&client, 1081, 200.0, true).await;

    run(&mut client, schema, RULES).await.unwrap();

    assert_eq!(
        active(&client).await,
        rows(&[("sandwicher", "rule", "MALICIOUS_SANDWICHED_ME_SINGLE_1080")])
    );
    assert_eq!(
        evaluated_epochs(&client).await,
        vec![Decimal::from(1080), Decimal::from(1081)]
    );
}

#[tokio::test]
async fn until_epoch_ignores_earlier_evidence() {
    let schema = "ds_test_blacklist_eval_until";
    let Some(mut client) = setup(schema).await else {
        return;
    };
    epoch_with_inputs(&client, 1079, 250.0, false).await;
    epoch_with_inputs(&client, 1080, 250.0, false).await;
    epoch_with_inputs(&client, 1081, 250.0, false).await;
    let config = format!(
        "{RULES}exempt: [{{ vote_account: slow, rule: slow_slots, until_epoch: 1079, reason: 'fixed client' }}]"
    );

    run(&mut client, schema, &config).await.unwrap();

    assert_eq!(
        active(&client).await,
        rows(&[("slow", "rule", "SLOW_SLOTS_1081")])
    );
}

#[tokio::test]
async fn a_bad_config_writes_nothing() {
    let schema = "ds_test_blacklist_eval_bad_config";
    let Some(mut client) = setup(schema).await else {
        return;
    };
    epoch_with_inputs(&client, 1080, 200.0, true).await;

    let error = run(&mut client, schema, &format!("{RULES}{MANUAL_B}\nbogus: 1"))
        .await
        .unwrap_err();

    assert!(format!("{error:#}").contains("bogus"), "{error:#}");
    assert!(active(&client).await.is_empty());
    assert!(evaluated_epochs(&client).await.is_empty());
}
