use tokio_postgres::{Client, NoTls};

pub const POSTGRES_URL_ENV: &str = "DS_TEST_POSTGRES_URL";

pub async fn migrated_client(schema: &str) -> Option<Client> {
    let url = std::env::var(POSTGRES_URL_ENV).ok()?;

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

    let migrations_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../migrations");
    let mut migrations: Vec<_> = std::fs::read_dir(migrations_dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "sql"))
        .collect();
    migrations.sort();
    for migration in migrations {
        let sql = std::fs::read_to_string(&migration).unwrap();
        client
            .batch_execute(&sql)
            .await
            .unwrap_or_else(|err| panic!("migration {} failed: {err}", migration.display()));
    }

    Some(client)
}

pub fn skip_without_database(schema: &str) -> bool {
    if std::env::var(POSTGRES_URL_ENV).is_ok() {
        return false;
    }
    eprintln!("skipping {schema}: {POSTGRES_URL_ENV} is not set");
    true
}
