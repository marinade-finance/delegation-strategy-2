mod common;

use clap::Parser;
use common::{migrated_client, skip_without_database};
use std::collections::HashSet;
use store::blacklist::import::{import_legacy, ImportParams};
use tokio_postgres::Client;

const REPO_BLACKLIST: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../blacklist.csv");

async fn import(client: &mut Client, path: &str) -> anyhow::Result<()> {
    import_legacy(
        ImportParams::parse_from(["store", "--csv", path, "--source-ref", "abc123"]),
        client,
    )
    .await
}

fn write_csv(name: &str, text: &str) -> String {
    let path = std::env::temp_dir().join(format!("ds-test-{name}.csv"));
    std::fs::write(&path, text).unwrap();
    path.to_string_lossy().into_owned()
}

async fn active_pairs(client: &Client) -> HashSet<(String, String)> {
    client
        .query(
            "SELECT vote_account, code FROM blacklist WHERE removed_epoch IS NULL",
            &[],
        )
        .await
        .unwrap()
        .into_iter()
        .map(|row| (row.get("vote_account"), row.get("code")))
        .collect()
}

#[tokio::test]
async fn every_csv_row_is_imported_as_legacy() {
    let schema = "ds_test_blacklist_import_rows";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();
    let path = write_csv(
        schema,
        "vote_account,code\nvoteA,SLOW_VOTER\nvoteA,MALICIOUS_SM_800\nvoteB,COMMISSION_RUG_992\n",
    );

    import(&mut client, &path).await.unwrap();
    std::fs::remove_file(path).unwrap();

    let rows = client
        .query(
            "SELECT source, auction_epoch IS NULL AS no_auction, reason, added_epoch IS NULL AS no_epoch FROM blacklist",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(rows.len(), 3);
    for row in rows {
        assert_eq!(row.get::<_, String>("source"), "legacy");
        assert!(row.get::<_, bool>("no_auction"));
        assert_eq!(
            row.get::<_, String>("reason"),
            "imported from ds2 blacklist.csv @ abc123"
        );
        assert!(row.get::<_, bool>("no_epoch"));
    }
}

#[tokio::test]
async fn a_second_import_is_refused() {
    let schema = "ds_test_blacklist_import_twice";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();
    let path = write_csv(schema, "vote_account,code\nvoteA,SLOW_VOTER\n");

    import(&mut client, &path).await.unwrap();
    let error = import(&mut client, &path).await.unwrap_err().to_string();
    std::fs::remove_file(path).unwrap();

    assert!(error.contains("refusing"), "{error}");
    assert_eq!(active_pairs(&client).await.len(), 1);
}

// The import replaces the file every reader used, so it has to carry every row of it.
#[tokio::test]
async fn the_repos_blacklist_imports_whole() {
    let schema = "ds_test_blacklist_import_repo";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    import(&mut client, REPO_BLACKLIST).await.unwrap();

    let expected: HashSet<(String, String)> = std::fs::read_to_string(REPO_BLACKLIST)
        .unwrap()
        .lines()
        .skip(1)
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let (vote_account, code) = line.split_once(',').unwrap();
            (vote_account.trim().into(), code.trim().into())
        })
        .collect();
    let imported = active_pairs(&client).await;
    assert!(expected.len() > 500, "{}", expected.len());
    assert_eq!(imported, expected);
}
