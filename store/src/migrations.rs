use anyhow::{Context, Result};
use include_dir::{include_dir, Dir};
use log::info;
use tokio_postgres::Client;

pub static MIGRATIONS: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/../migrations");

/// Runs every file in `migrations/` that the `migrations` table does not list, in name order,
/// each file in its own transaction. Stops at the first error and keeps the files before it.
pub async fn migrate(client: &mut Client) -> Result<()> {
    let tx = client.transaction().await?;
    tx.batch_execute(
        "SELECT pg_advisory_xact_lock(hashtext('migrations'));
         CREATE TABLE IF NOT EXISTS migrations (
             name TEXT PRIMARY KEY,
             applied_at TIMESTAMPTZ NOT NULL DEFAULT now()
         )",
    )
    .await?;
    tx.commit().await?;

    for file in migration_files() {
        let name = file.path().to_string_lossy();
        let sql = file
            .contents_utf8()
            .with_context(|| format!("migration {name} is not UTF-8"))?;
        let tx = client.transaction().await?;
        tx.batch_execute("SELECT pg_advisory_xact_lock(hashtext('migrations'))")
            .await?;
        let applied = tx
            .query_opt("SELECT 1 FROM migrations WHERE name = $1", &[&name])
            .await?
            .is_some();
        if !applied {
            info!("Applying migration {name}");
            tx.batch_execute(sql)
                .await
                .with_context(|| format!("applying migration {name}"))?;
            tx.execute("INSERT INTO migrations (name) VALUES ($1)", &[&name])
                .await?;
        }
        tx.commit().await?;
    }
    Ok(())
}

pub fn migration_files() -> Vec<&'static include_dir::File<'static>> {
    let mut files: Vec<_> = MIGRATIONS
        .files()
        .filter(|file| file.path().extension().is_some_and(|ext| ext == "sql"))
        .collect();
    files.sort_by_key(|file| file.path());
    files
}
