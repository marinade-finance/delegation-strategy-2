mod common;

use common::{empty_client, migrated_client, skip_without_database};
use store::migrations::{apply, migrate, migration_files};
use tokio_postgres::Client;

const ADOPT: &str = "0000-adopt.sql";

fn file_names() -> Vec<String> {
    migration_files()
        .unwrap()
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
    for file in migration_files().unwrap() {
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

#[tokio::test]
async fn database_migrated_by_hand_is_adopted() {
    for last_by_hand in ["0040-validators-vat-balance.sql"] {
        let schema = "ds_test_migrations_adopt";
        if skip_without_database(schema) {
            return;
        }
        let mut client = empty_client(schema).await.unwrap();
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
async fn database_without_0040_is_not_adopted() {
    let schema = "ds_test_migrations_adopt_without_0040";
    if skip_without_database(schema) {
        return;
    }
    let mut client = empty_client(schema).await.unwrap();
    apply_by_hand_through(&client, "0039-validators-skip-rate-nullable.sql").await;

    let err = migrate(&mut client).await.unwrap_err();

    assert!(format!("{err:#}").contains("up to 0040"), "{err:#}");
    assert!(recorded_names(&client).await.is_empty());

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}

#[tokio::test]
async fn failed_file_rolls_back_every_file_of_the_run() {
    let schema = "ds_test_migrations_one_transaction";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();
    let before = recorded_names(&client).await;

    let files = [
        (
            "9998-create.sql".to_string(),
            "CREATE TABLE one_transaction (id INT)",
        ),
        ("9999-fail.sql".to_string(), "SELECT 1 / 0"),
    ];
    let err = apply(&mut client, &files).await.unwrap_err();

    assert!(format!("{err:#}").contains("9999-fail.sql"), "{err:#}");
    assert_eq!(recorded_names(&client).await, before);
    let table: Option<String> = client
        .query_one("SELECT to_regclass('one_transaction')::TEXT", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(table, None);

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}

async fn table_exists(client: &Client, table: &str) -> bool {
    client
        .query_one("SELECT to_regclass($1) IS NOT NULL", &[&table])
        .await
        .unwrap()
        .get(0)
}

#[tokio::test]
async fn file_that_commits_rolls_back_every_file_of_the_run() {
    let schema = "ds_test_migrations_commit";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();
    let before = recorded_names(&client).await;

    for commit in [
        "COMMIT;",
        "commit",
        "END;",
        "END TRANSACTION;",
        "COMMIT AND CHAIN;",
        "SELECT 1; COMMIT; SELECT 2;",
    ] {
        let files = [
            (
                "9998-create.sql".to_string(),
                "CREATE TABLE before_commit (id INT)",
            ),
            (
                "9999-commit.sql".to_string(),
                &*format!("CREATE TABLE in_commit (id INT); {commit}"),
            ),
        ];
        let err = apply(&mut client, &files).await.unwrap_err();

        assert!(
            format!("{err:#}").contains("9999-commit.sql"),
            "{commit}: {err:#}"
        );
        assert_eq!(recorded_names(&client).await, before, "{commit}");
        assert!(!table_exists(&client, "before_commit").await, "{commit}");
        assert!(!table_exists(&client, "in_commit").await, "{commit}");
    }

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}

#[tokio::test]
async fn file_that_rolls_back_fails_the_run() {
    let schema = "ds_test_migrations_rollback";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();
    let before = recorded_names(&client).await;

    for rollback in ["ROLLBACK;", "ABORT;", "SELECT 1; ROLLBACK AND CHAIN;"] {
        let files = [
            (
                "9998-create.sql".to_string(),
                "CREATE TABLE before_rollback (id INT)",
            ),
            ("9999-rollback.sql".to_string(), rollback),
            (
                "9999-z-after.sql".to_string(),
                "CREATE TABLE after_rollback (id INT)",
            ),
        ];
        let err = apply(&mut client, &files).await.unwrap_err();

        assert!(
            format!("{err:#}").contains("9999-rollback.sql"),
            "{rollback}: {err:#}"
        );
        assert_eq!(recorded_names(&client).await, before, "{rollback}");
        assert!(
            !table_exists(&client, "before_rollback").await,
            "{rollback}"
        );
        assert!(!table_exists(&client, "after_rollback").await, "{rollback}");
    }

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}

#[tokio::test]
async fn second_run_on_one_connection_succeeds() {
    let schema = "ds_test_migrations_second_run";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    let files = [(
        "9999-create.sql".to_string(),
        "CREATE TABLE second_run (id INT)",
    )];
    apply(&mut client, &files).await.unwrap();
    migrate(&mut client).await.unwrap();

    assert!(table_exists(&client, "second_run").await);

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}
