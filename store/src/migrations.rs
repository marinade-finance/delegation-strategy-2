use anyhow::{bail, Context, Result};
use include_dir::{include_dir, Dir, File};
use log::info;
use tokio_postgres::Client;

pub static MIGRATIONS: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/../migrations");

/// Runs every file in `migrations/` that the `migrations` table does not list, in number order,
/// each file in its own transaction. Stops at the first error and keeps the files before it.
pub async fn migrate(client: &mut Client) -> Result<()> {
    let files = migration_files()?;
    if files.is_empty() {
        bail!("no migration files found");
    }
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

    for file in files {
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

pub fn migration_files() -> Result<Vec<&'static File<'static>>> {
    let names = MIGRATIONS
        .files()
        .map(|file| {
            file.path()
                .to_str()
                .with_context(|| format!("migration {:?} is not UTF-8", file.path()))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(sort_migration_filenames_by_number(names)?
        .into_iter()
        .filter_map(|name| MIGRATIONS.get_file(name))
        .collect())
}

/// Each name is `<at least 4 digits>-<name>.sql`, and every name has the same digit count.
/// Names with the same number keep name order.
fn sort_migration_filenames_by_number<'a>(
    names: impl IntoIterator<Item = &'a str>,
) -> Result<Vec<&'a str>> {
    let mut numbered = names
        .into_iter()
        .map(|name| {
            let digits = number_prefix(name).with_context(|| {
                format!("migration {name} does not match <4 or more digits>-<name>.sql")
            })?;
            let number: u64 = digits
                .parse()
                .with_context(|| format!("migration {name} has a number that is too large"))?;
            Ok((digits.len(), number, name))
        })
        .collect::<Result<Vec<_>>>()?;

    if let Some(&(width, _, first)) = numbered.first() {
        if let Some(&(other, _, name)) = numbered.iter().find(|(digits, _, _)| *digits != width) {
            bail!("migration {name} has a {other}-digit number, but {first} has {width} digits");
        }
    }

    numbered.sort_by_key(|&(_, number, name)| (number, name));
    Ok(numbered.into_iter().map(|(_, _, name)| name).collect())
}

fn number_prefix(name: &str) -> Option<&str> {
    let (digits, rest) = name.split_once('-')?;
    let stem = rest.strip_suffix(".sql")?;
    let is_number = digits.len() >= 4 && digits.bytes().all(|b| b.is_ascii_digit());
    (is_number && !stem.is_empty()).then_some(digits)
}

#[cfg(test)]
mod tests {
    use super::sort_migration_filenames_by_number;

    #[test]
    fn sorts_by_number_then_name() {
        let cases: [(&[&str], &[&str]); 2] = [
            (
                &[
                    "1000-a.sql",
                    "0010-0001.sql",
                    "0002-a.sql",
                    "0100-a.sql",
                    "0001-z.sql",
                    "9999-a.sql",
                    "0009-a.sql",
                    "0000-init.sql",
                    "0001-a-b.sql",
                    "0000-adopt.sql",
                ],
                &[
                    "0000-adopt.sql",
                    "0000-init.sql",
                    "0001-a-b.sql",
                    "0001-z.sql",
                    "0002-a.sql",
                    "0009-a.sql",
                    "0010-0001.sql",
                    "0100-a.sql",
                    "1000-a.sql",
                    "9999-a.sql",
                ],
            ),
            (
                &["100000-a.sql", "000010-a.sql", "000002-a.sql"],
                &["000002-a.sql", "000010-a.sql", "100000-a.sql"],
            ),
        ];
        for (names, sorted) in cases {
            assert_eq!(
                sort_migration_filenames_by_number(names.iter().copied()).unwrap(),
                sorted
            );
        }
    }

    #[test]
    fn rejects_mixed_digit_counts() {
        for names in [
            vec!["0001-a.sql", "00002-b.sql"],
            vec!["00002-b.sql", "0001-a.sql"],
            vec!["0001-a.sql", "0002-b.sql", "000003-c.sql"],
        ] {
            let err = sort_migration_filenames_by_number(names.iter().copied())
                .unwrap_err()
                .to_string();
            assert!(err.contains("digit"), "{names:?}: {err}");
        }
    }

    #[test]
    fn rejects_a_name_that_does_not_match() {
        for name in [
            "",
            "init.sql",
            "001-a.sql",
            "0001a.sql",
            "0000something.sql",
            "0001.sql",
            "0001",
            "0001-",
            "0001-.sql",
            "0001-a",
            "0001-a.SQL",
            "0001-a.sql.bak",
            "0001-a.txt",
            "0001_a.sql",
            "-0001-a.sql",
            " 0001-a.sql",
            "a0001-a.sql",
            "README.md",
        ] {
            let err = sort_migration_filenames_by_number(["0002-a.sql", name])
                .unwrap_err()
                .to_string();
            assert!(err.contains("does not match"), "{name:?}: {err}");
        }
    }

    #[test]
    fn rejects_non_ascii_digits() {
        let err = sort_migration_filenames_by_number(["\u{0661}\u{0662}\u{0663}\u{0664}-a.sql"])
            .unwrap_err()
            .to_string();
        assert!(err.contains("does not match"), "{err}");
    }
}
