mod common;

use chrono::{DateTime, Duration, TimeZone, Utc};
use collect::validators::Snapshot;
use collect::validators_performance::ValidatorRewards;
use common::{
    migrated_client, run_close_epoch, skip_without_database, store_snapshot, validator_performance,
    validator_snapshot,
};
use rust_decimal::Decimal;
use std::collections::HashMap;
use store::close_epoch::update_clients_from_versions;
use tokio_postgres::Client;

const EPOCH: u64 = 1063;

type Stored = (Option<i32>, Option<String>, Option<String>, Option<String>);

#[derive(Clone, Copy)]
struct Held {
    id: i32,
    raw: &'static str,
    version: &'static str,
}

const A: Held = Held {
    id: 6,
    raw: "AgaveBam",
    version: "4.3.2",
};
const B: Held = Held {
    id: 1,
    raw: "JitoLabs",
    version: "4.3.0",
};

fn t0() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 20, 0, 0, 0).unwrap()
}

fn at(hours: i64) -> DateTime<Utc> {
    t0() + Duration::hours(hours)
}

fn holding(client: Held, source: Option<&str>) -> Stored {
    (
        Some(client.id),
        Some(client.raw.to_string()),
        Some(client.version.to_string()),
        source.map(str::to_string),
    )
}

async fn seed(client: &mut Client, epoch: u64, vote_accounts: &[&str]) {
    let mut snapshot = Snapshot {
        validators: vec![],
        ..validator_snapshot(epoch, "unused", "unused")
    };
    for vote_account in vote_accounts {
        snapshot.validators.push(
            validator_snapshot(epoch, &format!("identity-{vote_account}"), vote_account)
                .validators
                .remove(0),
        );
    }
    store_snapshot(client, &format!("close-epoch-client-{epoch}"), &snapshot).await;
}

async fn set_stored(client: &Client, vote_account: &str, epoch: u64, stored: &Stored) {
    client
        .execute(
            "UPDATE validators SET client_id = $3, client_id_raw = $4, version = $5, client_id_source = $6
             WHERE vote_account = $1 AND epoch = $2",
            &[
                &vote_account,
                &Decimal::from(epoch),
                &stored.0,
                &stored.1,
                &stored.2,
                &stored.3,
            ],
        )
        .await
        .unwrap();
}

async fn close(client: &Client, epoch: u64, start_at: DateTime<Utc>, end_at: DateTime<Utc>) {
    client
        .execute(
            "INSERT INTO epochs (epoch, start_at, end_at, transaction_count, supply, inflation, inflation_taper, slots_per_year)
             VALUES ($1, $2, $3, 0, 0, 0, 0.15, 0)",
            &[&Decimal::from(epoch), &start_at, &end_at],
        )
        .await
        .unwrap();
}

async fn version_row(
    client: &Client,
    vote_account: &str,
    epoch: u64,
    created_at: DateTime<Utc>,
    held: Option<Held>,
) {
    client
        .execute(
            "INSERT INTO versions (vote_account, version, client_id, client_id_raw, epoch_slot, epoch, created_at)
             VALUES ($1, $2, $3, $4, 0, $5, $6)",
            &[
                &vote_account,
                &held.map(|c| c.version),
                &held.map(|c| c.id),
                &held.map(|c| c.raw),
                &Decimal::from(epoch),
                &created_at,
            ],
        )
        .await
        .unwrap();
}

async fn stored(client: &Client, vote_account: &str, epoch: u64) -> Stored {
    let row = client
        .query_one(
            "SELECT client_id, client_id_raw, version, client_id_source FROM validators
             WHERE vote_account = $1 AND epoch = $2",
            &[&vote_account, &Decimal::from(epoch)],
        )
        .await
        .unwrap();
    (
        row.get("client_id"),
        row.get("client_id_raw"),
        row.get("version"),
        row.get("client_id_source"),
    )
}

async fn drop_schema(client: &Client, schema: &str) {
    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}

#[tokio::test]
async fn a_short_flip_loses_to_the_client_held_most_of_the_epoch() {
    let schema = "ds_test_close_epoch_client_1";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    seed(&mut client, EPOCH, &["voteFlipped"]).await;
    set_stored(&client, "voteFlipped", EPOCH, &holding(B, None)).await;
    close(&client, EPOCH, t0(), at(10)).await;
    version_row(&client, "voteFlipped", EPOCH, at(0), Some(A)).await;
    version_row(&client, "voteFlipped", EPOCH, at(3), Some(B)).await;
    version_row(&client, "voteFlipped", EPOCH, at(7), Some(A)).await;

    assert_eq!(
        update_clients_from_versions(&client, EPOCH).await.unwrap(),
        1
    );
    assert_eq!(
        stored(&client, "voteFlipped", EPOCH).await,
        holding(A, None),
        "A's two spans sum to 6h against B's single 4h, so the summed time decides"
    );

    drop_schema(&client, schema).await;
}

#[tokio::test]
async fn a_validator_without_versions_rows_keeps_its_stored_values() {
    let schema = "ds_test_close_epoch_client_2";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    seed(&mut client, EPOCH, &["voteSeen", "voteUnseen"]).await;
    set_stored(&client, "voteSeen", EPOCH, &holding(B, None)).await;
    set_stored(&client, "voteUnseen", EPOCH, &holding(B, None)).await;
    close(&client, EPOCH, t0(), at(10)).await;
    version_row(&client, "voteSeen", EPOCH, at(0), Some(A)).await;

    assert_eq!(
        update_clients_from_versions(&client, EPOCH).await.unwrap(),
        1
    );
    assert_eq!(stored(&client, "voteSeen", EPOCH).await, holding(A, None));
    assert_eq!(
        stored(&client, "voteUnseen", EPOCH).await,
        holding(B, None),
        "with nothing in versions there is no evidence against the stored client"
    );

    drop_schema(&client, schema).await;
}

#[tokio::test]
async fn time_without_a_client_id_raw_counts_for_no_candidate() {
    let schema = "ds_test_close_epoch_client_3";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    seed(
        &mut client,
        EPOCH,
        &["voteLateA", "voteEarlyA", "voteNoClient"],
    )
    .await;
    for vote_account in ["voteLateA", "voteEarlyA", "voteNoClient"] {
        set_stored(&client, vote_account, EPOCH, &holding(B, None)).await;
    }
    close(&client, EPOCH, t0(), at(10)).await;
    version_row(&client, "voteLateA", EPOCH, at(0), None).await;
    version_row(&client, "voteLateA", EPOCH, at(8), Some(A)).await;
    version_row(&client, "voteEarlyA", EPOCH, at(0), Some(A)).await;
    version_row(&client, "voteEarlyA", EPOCH, at(1), None).await;
    version_row(&client, "voteNoClient", EPOCH, at(0), None).await;
    version_row(&client, "voteNoClient", EPOCH, at(5), None).await;

    assert_eq!(
        update_clients_from_versions(&client, EPOCH).await.unwrap(),
        2
    );
    assert_eq!(
        stored(&client, "voteLateA", EPOCH).await,
        holding(A, None),
        "8h without a client must not outweigh A's 2h"
    );
    assert_eq!(
        stored(&client, "voteEarlyA", EPOCH).await,
        holding(A, None),
        "a row without a client still ends A's span instead of vanishing"
    );
    assert_eq!(
        stored(&client, "voteNoClient", EPOCH).await,
        holding(B, None),
        "only clientless rows leave nothing to replace the stored client with"
    );

    drop_schema(&client, schema).await;
}

#[tokio::test]
async fn only_the_closing_epoch_is_rewritten() {
    let schema = "ds_test_close_epoch_client_4";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    seed(&mut client, EPOCH - 1, &["voteBoth"]).await;
    seed(&mut client, EPOCH, &["voteBoth"]).await;
    set_stored(&client, "voteBoth", EPOCH - 1, &holding(B, None)).await;
    set_stored(&client, "voteBoth", EPOCH, &holding(B, None)).await;
    close(&client, EPOCH - 1, at(-10), t0()).await;
    close(&client, EPOCH, t0(), at(10)).await;
    version_row(&client, "voteBoth", EPOCH - 1, at(-10), Some(A)).await;
    version_row(&client, "voteBoth", EPOCH, at(0), Some(A)).await;
    version_row(&client, "voteBoth", EPOCH + 1, at(1), Some(B)).await;

    assert_eq!(
        update_clients_from_versions(&client, EPOCH).await.unwrap(),
        1
    );
    assert_eq!(
        stored(&client, "voteBoth", EPOCH).await,
        holding(A, None),
        "a versions row of the next epoch must not cut the closing epoch's span short"
    );
    assert_eq!(
        stored(&client, "voteBoth", EPOCH - 1).await,
        holding(B, None),
        "an epoch closed earlier is not rewritten"
    );

    drop_schema(&client, schema).await;
}

#[tokio::test]
async fn a_tie_goes_to_the_lower_client_id_raw_and_a_missing_version_keeps_the_stored_one() {
    let schema = "ds_test_close_epoch_client_5";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    seed(&mut client, EPOCH, &["voteTied"]).await;
    set_stored(&client, "voteTied", EPOCH, &holding(B, Some("stakenet"))).await;
    close(&client, EPOCH, t0(), at(10)).await;
    client
        .execute(
            "INSERT INTO versions (vote_account, version, client_id, client_id_raw, epoch_slot, epoch, created_at)
             VALUES ($1, NULL, $2, $3, 0, $4, $5)",
            &[
                &"voteTied",
                &A.id,
                &A.raw,
                &Decimal::from(EPOCH),
                &at(0),
            ],
        )
        .await
        .unwrap();
    version_row(&client, "voteTied", EPOCH, at(5), Some(B)).await;

    let expected = (
        Some(A.id),
        Some(A.raw.to_string()),
        Some(B.version.to_string()),
        Some("stakenet".to_string()),
    );
    assert_eq!(
        update_clients_from_versions(&client, EPOCH).await.unwrap(),
        1
    );
    assert_eq!(
        stored(&client, "voteTied", EPOCH).await,
        expected,
        "5h each: AgaveBam sorts before JitoLabs; its NULL version keeps the stored one; the source is not touched"
    );
    assert_eq!(
        update_clients_from_versions(&client, EPOCH).await.unwrap(),
        0,
        "a second run finds nothing to change"
    );
    assert_eq!(stored(&client, "voteTied", EPOCH).await, expected);

    drop_schema(&client, schema).await;
}

#[tokio::test]
async fn an_upgrade_does_not_split_the_time_of_the_client_held_longest() {
    let schema = "ds_test_close_epoch_client_7";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();
    let before_upgrade = Held {
        version: "4.3.1",
        ..A
    };

    seed(&mut client, EPOCH, &["voteUpgraded"]).await;
    set_stored(&client, "voteUpgraded", EPOCH, &holding(B, None)).await;
    close(&client, EPOCH, t0(), at(9)).await;
    version_row(&client, "voteUpgraded", EPOCH, at(0), Some(before_upgrade)).await;
    version_row(&client, "voteUpgraded", EPOCH, at(2), Some(B)).await;
    version_row(&client, "voteUpgraded", EPOCH, at(6), Some(A)).await;

    assert_eq!(
        update_clients_from_versions(&client, EPOCH).await.unwrap(),
        1
    );
    assert_eq!(
        stored(&client, "voteUpgraded", EPOCH).await,
        holding(A, None),
        "AgaveBam ran 2h + 3h against JitoLabs' 4h; its longer-held version is stored"
    );

    drop_schema(&client, schema).await;
}

#[tokio::test]
async fn two_spellings_of_one_client_count_as_one_client() {
    let schema = "ds_test_close_epoch_client_8";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();
    let numbered = Held {
        raw: "Unknown(6)",
        ..A
    };

    seed(&mut client, EPOCH, &["voteRespelled"]).await;
    set_stored(&client, "voteRespelled", EPOCH, &holding(B, None)).await;
    close(&client, EPOCH, t0(), at(9)).await;
    version_row(&client, "voteRespelled", EPOCH, at(0), Some(A)).await;
    version_row(&client, "voteRespelled", EPOCH, at(3), Some(B)).await;
    version_row(&client, "voteRespelled", EPOCH, at(7), Some(numbered)).await;

    assert_eq!(
        update_clients_from_versions(&client, EPOCH).await.unwrap(),
        1
    );
    assert_eq!(
        stored(&client, "voteRespelled", EPOCH).await,
        holding(A, None),
        "client 6 ran 3h as AgaveBam + 2h as Unknown(6) against JitoLabs' 4h"
    );

    drop_schema(&client, schema).await;
}

#[tokio::test]
async fn close_epoch_stores_the_client_held_longest() {
    let schema = "ds_test_close_epoch_client_6";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    seed(&mut client, EPOCH, &["voteClosed"]).await;
    set_stored(&client, "voteClosed", EPOCH, &holding(B, None)).await;
    let now = Utc::now();
    version_row(
        &client,
        "voteClosed",
        EPOCH,
        now - Duration::hours(10),
        Some(A),
    )
    .await;
    version_row(
        &client,
        "voteClosed",
        EPOCH,
        now - Duration::hours(1),
        Some(B),
    )
    .await;

    let snapshot = common::performance_snapshot(
        EPOCH,
        HashMap::from([("voteClosed".to_string(), validator_performance())]),
        HashMap::from([(
            "voteClosed".to_string(),
            ValidatorRewards {
                commission_effective: None,
            },
        )]),
    );
    run_close_epoch(&mut client, schema, &snapshot).await;

    assert_eq!(
        stored(&client, "voteClosed", EPOCH).await,
        holding(A, None),
        "the last hour on B must not label an epoch A ran for nine hours"
    );

    drop_schema(&client, schema).await;
}
