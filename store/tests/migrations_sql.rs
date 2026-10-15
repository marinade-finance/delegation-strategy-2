mod common;

use common::{migrated_client, skip_without_database, POSTGRES_URL_ENV};
use store::migrations::{migrate, migration_files};
use tokio_postgres::{Client, NoTls};

const ADOPT: &str = "0000-adopt.sql";

fn file_names() -> Vec<String> {
    migration_files()
        .iter()
        .map(|file| file.path().to_string_lossy().into_owned())
        .collect()
}

async fn recorded_names(client: &Client) -> Vec<String> {
    client
        .query("SELECT name FROM migrations ORDER BY name", &[])
        .await
        .unwrap()
        .iter()
        .map(|row| row.get(0))
        .collect()
}

async fn skip_rate_is_nullable(client: &Client) -> bool {
    client
        .query_one(
            "SELECT is_nullable = 'YES' FROM information_schema.columns
             WHERE table_schema = current_schema() AND table_name = 'validators' AND column_name = 'skip_rate'",
            &[],
        )
        .await
        .unwrap()
        .get(0)
}

async fn empty_schema(schema: &str) -> Client {
    let url = std::env::var(POSTGRES_URL_ENV).unwrap();
    let (client, connection) = tokio_postgres::connect(&url, NoTls).await.unwrap();
    tokio::spawn(async move {
        if let Err(err) = connection.await {
            panic!("postgres connection error: {err}");
        }
    });
    client
        .batch_execute(&format!(
            "DROP SCHEMA IF EXISTS {schema} CASCADE;
             CREATE SCHEMA {schema};
             SET search_path TO {schema}"
        ))
        .await
        .unwrap();
    client
}

#[tokio::test]
async fn empty_database_records_every_file_and_a_second_run_changes_nothing() {
    let schema = "ds_test_migrations_empty";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();
    migrate(&mut client).await.unwrap();

    assert_eq!(recorded_names(&client).await, file_names());
    assert!(skip_rate_is_nullable(&client).await);

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}

async fn apply_by_hand_through(client: &Client, last_by_hand: &str) {
    for file in migration_files() {
        let name = file.path().to_string_lossy();
        if name == ADOPT {
            continue;
        }
        if *name > *last_by_hand {
            break;
        }
        client
            .batch_execute(file.contents_utf8().unwrap())
            .await
            .unwrap();
    }
}

// Prod on 2026-10-15 without 0039, and a database with every file.
#[tokio::test]
async fn database_migrated_by_hand_is_adopted() {
    for last_by_hand in [
        "0038-validators-rewards-expected-take-rate.sql",
        "0039-validators-skip-rate-nullable.sql",
    ] {
        let schema = "ds_test_migrations_adopt";
        if skip_without_database(schema) {
            return;
        }
        let mut client = empty_schema(schema).await;
        apply_by_hand_through(&client, last_by_hand).await;

        migrate(&mut client)
            .await
            .unwrap_or_else(|err| panic!("after {last_by_hand} by hand: {err:#}"));

        assert_eq!(
            recorded_names(&client).await,
            file_names(),
            "{last_by_hand}"
        );
        assert!(skip_rate_is_nullable(&client).await, "{last_by_hand}");

        client
            .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn database_without_0038_is_not_adopted() {
    let schema = "ds_test_migrations_adopt_without_0038";
    if skip_without_database(schema) {
        return;
    }
    let mut client = empty_schema(schema).await;
    apply_by_hand_through(&client, "0037-validators-client-id-source.sql").await;

    let err = migrate(&mut client).await.unwrap_err();

    assert!(format!("{err:#}").contains("up to 0038"), "{err:#}");
    assert!(recorded_names(&client).await.is_empty());

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}
