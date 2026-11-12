mod common;

use clap::Parser;
use collect::blacklist_auctions::{AuctionListings, Listing};
use common::{migrated_client, skip_without_database, write_yaml};
use rust_decimal::Decimal;
use store::blacklist::auctions::{import_run, store_auctions, StoreAuctionsParams};
use tokio_postgres::Client;

const A: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const B: &str = "BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB";

fn run(auction_epoch: u64, run_id: &str, rows: &[(&str, &str)]) -> AuctionListings {
    AuctionListings {
        auction_epoch,
        run_id: run_id.into(),
        rows: rows
            .iter()
            .map(|(vote_account, code)| Listing {
                vote_account: vote_account.to_string(),
                code: code.to_string(),
            })
            .collect(),
    }
}

/// `(vote_account, code, run_id)` of the active auction rows, ordered.
async fn active(client: &Client) -> Vec<(String, String, String)> {
    client
        .query(
            "SELECT vote_account, code, run_id FROM blacklist
             WHERE source = 'auction' AND removed_epoch IS NULL ORDER BY vote_account, code",
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
        .map(|(a, c, r)| (a.to_string(), c.to_string(), r.to_string()))
        .collect()
}

async fn imports(client: &Client) -> Vec<(Decimal, String, i32)> {
    client
        .query(
            "SELECT auction_epoch, run_id, rows FROM blacklist_auction_imports ORDER BY auction_epoch",
            &[],
        )
        .await
        .unwrap()
        .into_iter()
        .map(|row| (row.get(0), row.get(1), row.get(2)))
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
             VALUES (0, 1086, 0, now())",
            &[],
        )
        .await
        .unwrap();
    Some(client)
}

#[tokio::test]
async fn a_first_import_lands_the_rows_and_the_run() {
    let schema = "ds_test_blacklist_auctions_first";
    let Some(mut client) = setup(schema).await else {
        return;
    };

    import_run(
        &mut client,
        &run(1085, "1085.100", &[(A, "MALICIOUS_SM_1084")]),
    )
    .await
    .unwrap();

    assert_eq!(
        active(&client).await,
        rows(&[(A, "MALICIOUS_SM_1084", "1085.100")])
    );
    assert_eq!(
        imports(&client).await,
        vec![(Decimal::from(1085), "1085.100".into(), 1)]
    );
    let added: Decimal = client
        .query_one("SELECT added_epoch FROM blacklist", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(added, Decimal::from(1085));
}

#[tokio::test]
async fn the_same_run_again_changes_nothing() {
    let schema = "ds_test_blacklist_auctions_same";
    let Some(mut client) = setup(schema).await else {
        return;
    };
    let listings = run(1085, "1085.100", &[(A, "MALICIOUS_SM_1084")]);

    import_run(&mut client, &listings).await.unwrap();
    import_run(&mut client, &listings).await.unwrap();

    let total: i64 = client
        .query_one("SELECT COUNT(*) FROM blacklist", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(total, 1);
}

#[tokio::test]
async fn a_newer_run_of_the_epoch_replaces_its_rows() {
    let schema = "ds_test_blacklist_auctions_rerun";
    let Some(mut client) = setup(schema).await else {
        return;
    };

    import_run(
        &mut client,
        &run(
            1085,
            "1085.100",
            &[(A, "MALICIOUS_SM_1084"), (B, "SLOW_SLOTS_1084")],
        ),
    )
    .await
    .unwrap();
    import_run(
        &mut client,
        &run(1085, "1085.900", &[(B, "SLOW_SLOTS_1084")]),
    )
    .await
    .unwrap();

    assert_eq!(
        active(&client).await,
        rows(&[(B, "SLOW_SLOTS_1084", "1085.900")])
    );
    let closed: Vec<(String, String)> = client
        .query(
            "SELECT vote_account, removed_reason FROM blacklist WHERE removed_epoch = 1086 ORDER BY vote_account",
            &[],
        )
        .await
        .unwrap()
        .into_iter()
        .map(|row| (row.get(0), row.get(1)))
        .collect();
    assert_eq!(
        closed,
        vec![
            (A.into(), "replaced by run 1085.900".into()),
            (B.into(), "replaced by run 1085.900".into()),
        ]
    );
    assert_eq!(
        imports(&client).await,
        vec![(Decimal::from(1085), "1085.900".into(), 1)]
    );
}

#[tokio::test]
async fn an_older_run_never_replaces_a_newer_one() {
    let schema = "ds_test_blacklist_auctions_older";
    let Some(mut client) = setup(schema).await else {
        return;
    };

    import_run(
        &mut client,
        &run(1085, "1085.900", &[(B, "SLOW_SLOTS_1084")]),
    )
    .await
    .unwrap();
    import_run(
        &mut client,
        &run(1085, "1085.100", &[(A, "MALICIOUS_SM_1084")]),
    )
    .await
    .unwrap();

    assert_eq!(
        active(&client).await,
        rows(&[(B, "SLOW_SLOTS_1084", "1085.900")])
    );
    assert_eq!(
        imports(&client).await,
        vec![(Decimal::from(1085), "1085.900".into(), 1)]
    );
}

#[tokio::test]
async fn an_exempt_account_is_closed_on_import() {
    let schema = "ds_test_blacklist_auctions_exempt";
    let Some(mut client) = setup(schema).await else {
        return;
    };
    client
        .execute(
            "INSERT INTO blacklist_exempt (vote_account, rule, code_prefix, until_epoch)
             VALUES ($1, 'slow_slots', 'SLOW_SLOTS_', NULL)",
            &[&A],
        )
        .await
        .unwrap();

    import_run(
        &mut client,
        &run(
            1085,
            "1085.100",
            &[
                (A, "MALICIOUS_SM_1084"),
                (A, "SLOW_SLOTS_1084"),
                (B, "SLOW_SLOTS_1084"),
            ],
        ),
    )
    .await
    .unwrap();

    assert_eq!(
        active(&client).await,
        rows(&[
            (A, "MALICIOUS_SM_1084", "1085.100"),
            (B, "SLOW_SLOTS_1084", "1085.100")
        ])
    );
    let reason: String = client
        .query_one(
            "SELECT removed_reason FROM blacklist WHERE vote_account = $1 AND code = 'SLOW_SLOTS_1084'",
            &[&A],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(reason, "exempt");
}

#[tokio::test]
async fn refused_runs_fail_the_store_after_the_rest_is_imported() {
    let schema = "ds_test_blacklist_auctions_refused";
    let Some(mut client) = setup(schema).await else {
        return;
    };
    let snapshot = write_yaml(
        schema,
        &format!(
            "version: 1\ncreated_at: now\nruns:\n- auction_epoch: 1085\n  run_id: '1085.100'\n  rows:\n  - vote_account: {A}\n    code: MALICIOUS_SM_1084\nrefused: ['1084.100']\n"
        ),
    );

    let err = store_auctions(
        StoreAuctionsParams::parse_from(["store", "--snapshot-file", &snapshot]),
        &mut client,
    )
    .await
    .unwrap_err();

    assert!(err.to_string().contains("1084.100"), "{err}");
    assert_eq!(
        active(&client).await,
        rows(&[(A, "MALICIOUS_SM_1084", "1085.100")])
    );
}

#[tokio::test]
async fn an_empty_run_closes_the_rows_of_its_epoch_only() {
    let schema = "ds_test_blacklist_auctions_empty";
    let Some(mut client) = setup(schema).await else {
        return;
    };

    import_run(
        &mut client,
        &run(1084, "1084.100", &[(A, "MALICIOUS_SM_1083")]),
    )
    .await
    .unwrap();
    import_run(
        &mut client,
        &run(1085, "1085.100", &[(B, "MALICIOUS_SM_1084")]),
    )
    .await
    .unwrap();
    import_run(&mut client, &run(1085, "1085.900", &[]))
        .await
        .unwrap();

    assert_eq!(
        active(&client).await,
        rows(&[(A, "MALICIOUS_SM_1083", "1084.100")])
    );
}

#[tokio::test]
async fn missed_epochs_are_each_imported() {
    let schema = "ds_test_blacklist_auctions_missed";
    let Some(mut client) = setup(schema).await else {
        return;
    };

    for listings in [
        run(1084, "1084.100", &[(A, "MALICIOUS_SM_1083")]),
        run(1085, "1085.100", &[(B, "SLOW_SLOTS_1084")]),
    ] {
        import_run(&mut client, &listings).await.unwrap();
    }

    assert_eq!(
        active(&client).await,
        rows(&[
            (A, "MALICIOUS_SM_1083", "1084.100"),
            (B, "SLOW_SLOTS_1084", "1085.100")
        ])
    );
    assert_eq!(imports(&client).await.len(), 2);
}

#[tokio::test]
async fn other_sources_are_never_touched() {
    let schema = "ds_test_blacklist_auctions_sources";
    let Some(mut client) = setup(schema).await else {
        return;
    };
    client
        .execute(
            "INSERT INTO blacklist (vote_account, source, code, created_at, updated_at)
             VALUES ($1, 'legacy', 'MALICIOUS_SM_1084', now(), now())",
            &[&A],
        )
        .await
        .unwrap();

    import_run(
        &mut client,
        &run(1085, "1085.100", &[(A, "MALICIOUS_SM_1084")]),
    )
    .await
    .unwrap();
    import_run(&mut client, &run(1085, "1085.900", &[]))
        .await
        .unwrap();

    let legacy: i64 = client
        .query_one(
            "SELECT COUNT(*) FROM blacklist WHERE source = 'legacy' AND removed_epoch IS NULL",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(legacy, 1);
}
