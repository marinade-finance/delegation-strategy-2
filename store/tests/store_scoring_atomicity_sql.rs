mod common;

use common::{migrated_client, skip_without_database, POSTGRES_URL_ENV};
use rust_decimal::Decimal;
use store::dto::ValidatorScoringCsvRow;
use store::utils::{load_last_scoring_run, load_scores, store_scoring};
use tokio_postgres::{Client, NoTls};

fn score_row(vote_account: &str, rank: i32) -> ValidatorScoringCsvRow {
    ValidatorScoringCsvRow {
        vote_account: vote_account.to_string(),
        score: 1.0,
        rank,
        vemnde_votes: Decimal::ZERO,
        msol_votes: Decimal::ZERO,
        ui_hints: String::new(),
        eligible_stake_algo: true,
        eligible_stake_vemnde: true,
        eligible_stake_msol: true,
        normalized_dc_concentration: 1.0,
        normalized_grace_skip_rate: 1.0,
        normalized_adjusted_credits: 1.0,
        avg_dc_concentration: 1.0,
        avg_grace_skip_rate: 1.0,
        avg_adjusted_credits: 1.0,
        rank_dc_concentration: rank,
        rank_grace_skip_rate: rank,
        rank_adjusted_credits: rank,
        target_stake_algo: Decimal::ZERO,
        target_stake_vemnde: Decimal::ZERO,
        target_stake_msol: Decimal::ZERO,
    }
}

async fn observer(schema: &str) -> Client {
    let url = std::env::var(POSTGRES_URL_ENV).unwrap();
    let (client, connection) = tokio_postgres::connect(&url, NoTls).await.unwrap();
    tokio::spawn(async move {
        if let Err(err) = connection.await {
            panic!("postgres connection error: {err}");
        }
    });
    client
        .batch_execute(&format!("SET search_path TO {schema}"))
        .await
        .unwrap();
    client
}

// `scores` is the only table this test locks, so any other backend parked on a lock is the writer
// having already run its scoring_runs INSERT. Keyed on the wait rather than on
// pg_stat_activity.query, which still names the previous statement while the next one parses.
async fn await_writer_blocked_on_scores(observer: &Client) {
    for _ in 0..100 {
        let blocked: i64 = observer
            .query_one(
                "SELECT count(*) FROM pg_stat_activity
                WHERE datname = current_database()
                  AND wait_event_type = 'Lock'
                  AND pid <> pg_backend_pid()",
                &[],
            )
            .await
            .unwrap()
            .get(0);
        if blocked > 0 {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!(
        "the writer never reached its scores INSERT, so the assertion below would prove nothing"
    );
}

#[tokio::test]
async fn a_scoring_run_stays_invisible_until_its_scores_are_committed() {
    let schema = "ds_test_store_scoring_atomicity";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();
    let observer = observer(schema).await;

    observer
        .batch_execute("BEGIN; LOCK TABLE scores IN ACCESS EXCLUSIVE MODE")
        .await
        .unwrap();

    let writer = tokio::spawn(async move {
        store_scoring(
            &mut client,
            700,
            "atomicity".to_string(),
            vec!["COMMISSION_ADJUSTED_CREDITS"],
            vec![1.0],
            vec![score_row("voteOne", 1), score_row("voteTwo", 2)],
        )
        .await
        .unwrap();
        client
    });

    await_writer_blocked_on_scores(&observer).await;
    assert!(
        load_last_scoring_run(&observer).await.unwrap().is_none(),
        "the cache gate reads MAX(scoring_run_id), so the run must not surface before its scores"
    );

    observer.batch_execute("COMMIT").await.unwrap();
    let client = writer.await.unwrap();

    let run = load_last_scoring_run(&observer)
        .await
        .unwrap()
        .expect("the run surfaces once committed");
    assert_eq!(
        load_scores(&observer, run.scoring_run_id)
            .await
            .unwrap()
            .len(),
        2,
        "the run and every one of its scores become visible together"
    );

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}
