mod common;

use chrono::{DateTime, Duration, Utc};
use clap::Parser;
use collect::releases::{ReleaseEntry, ReleaseSource, ReleasesSnapshot};
use common::{migrated_client, skip_without_database, write_yaml};
use rust_decimal::Decimal;
use store::dto::ReleaseRecord;
use store::releases::{
    get_sfdp_floor_at_epoch, load_releases, store_releases, StoreReleasesParams,
};
use tokio_postgres::Client;

const SNAPSHOT_VERSION: u16 = 1;

fn epoch_start(epoch: u64) -> DateTime<Utc> {
    "2024-01-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap() + Duration::days(2 * epoch as i64)
}

async fn close_epochs(client: &Client, epochs: std::ops::RangeInclusive<u64>) {
    for epoch in epochs {
        client
            .execute(
                "INSERT INTO epochs (
                    epoch, start_at, end_at, transaction_count, supply, inflation,
                    inflation_taper, slots_per_year
                ) VALUES ($1, $2, $3, 0, 0, 0, 0, 78892314)",
                &[
                    &Decimal::from(epoch),
                    &epoch_start(epoch),
                    &epoch_start(epoch + 1),
                ],
            )
            .await
            .unwrap();
    }
}

async fn run_epoch(client: &Client, epoch: u64) {
    client
        .execute(
            "INSERT INTO cluster_info (epoch_slot, epoch, transaction_count, created_at)
             VALUES (0, $1, 0, NOW())",
            &[&Decimal::from(epoch)],
        )
        .await
        .unwrap();
}

fn entry(client_lineage: &str, client_version: &str, source: ReleaseSource) -> ReleaseEntry {
    ReleaseEntry {
        client_lineage: client_lineage.into(),
        client_version: client_version.into(),
        released_at: None,
        release_url: None,
        sfdp_floor_epoch: None,
        source,
    }
}

fn shipped(version: &str, released_at: DateTime<Utc>) -> ReleaseEntry {
    ReleaseEntry {
        released_at: Some(released_at),
        ..entry("agave", version, ReleaseSource::Github)
    }
}

fn floor(version: &str, effective_epoch: u64) -> ReleaseEntry {
    ReleaseEntry {
        sfdp_floor_epoch: Some(effective_epoch),
        ..entry("agave", version, ReleaseSource::Sfdp)
    }
}

async fn store(client: &mut Client, name: &str, releases: Vec<ReleaseEntry>) {
    let snapshot = ReleasesSnapshot {
        version: SNAPSHOT_VERSION,
        created_at: "2026-09-09T00:00:00Z".into(),
        releases,
    };
    let path = write_yaml(name, &serde_yaml::to_string(&snapshot).unwrap());
    store_releases(
        StoreReleasesParams::parse_from(["store", "--snapshot-file", &path]),
        client,
    )
    .await
    .unwrap();
    std::fs::remove_file(path).unwrap();
}

fn find<'a>(releases: &'a [ReleaseRecord], client_version: &str) -> &'a ReleaseRecord {
    releases
        .iter()
        .find(|r| r.client_version == client_version)
        .unwrap_or_else(|| panic!("no {client_version} row in {releases:#?}"))
}

#[tokio::test]
async fn releases_land_in_the_epoch_their_timestamp_falls_in() {
    let schema = "releases_available_epoch";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    close_epochs(&client, 1000..=1002).await;
    run_epoch(&client, 1003).await;

    store(
        &mut client,
        schema,
        vec![
            shipped("4.0.0", epoch_start(1000)),
            shipped("4.0.1", epoch_start(1002) + Duration::hours(1)),
            // After the last closed epoch: the running epoch is the only one it can belong to.
            shipped("4.0.2", epoch_start(1003) + Duration::hours(1)),
            // Older than every epoch we hold.
            shipped("1.0.0", epoch_start(900)),
        ],
    )
    .await;

    let releases = load_releases(&client, None, None).await.unwrap();

    assert_eq!(find(&releases, "4.0.0").available_epoch, Some(1000));
    assert_eq!(find(&releases, "4.0.1").available_epoch, Some(1002));
    assert_eq!(find(&releases, "4.0.2").available_epoch, Some(1003));
    let unplaced = find(&releases, "1.0.0");
    assert_eq!(unplaced.available_epoch, None);
    // The timestamp is still the record even where no epoch can hold it.
    assert!(unplaced.released_at.is_some());
}

#[tokio::test]
async fn the_two_fetchers_fill_one_row_without_blanking_each_other() {
    let schema = "releases_sources_coexist";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    close_epochs(&client, 980..=1002).await;

    store(
        &mut client,
        schema,
        vec![shipped("4.0.2", epoch_start(985))],
    )
    .await;
    store(&mut client, schema, vec![floor("4.0.2", 992)]).await;

    let one_row = |releases: Vec<ReleaseRecord>| {
        assert_eq!(releases.len(), 1, "{releases:#?}");
        let release = releases.into_iter().next().unwrap();
        assert_eq!(release.available_epoch, Some(985));
        assert_eq!(release.sfdp_floor_epoch, Some(992));
        assert!(release.released_at.is_some());
    };

    one_row(load_releases(&client, None, None).await.unwrap());

    // Either fetcher running again leaves the other's columns alone.
    store(
        &mut client,
        schema,
        vec![shipped("4.0.2", epoch_start(985))],
    )
    .await;
    one_row(load_releases(&client, None, None).await.unwrap());
    store(&mut client, schema, vec![floor("4.0.2", 992)]).await;
    one_row(load_releases(&client, None, None).await.unwrap());
}

#[tokio::test]
async fn storing_the_same_snapshot_twice_stores_the_same_rows() {
    let schema = "releases_idempotent";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    close_epochs(&client, 1000..=1002).await;
    let releases = vec![
        shipped("4.0.0", epoch_start(1000)),
        shipped("4.0.1", epoch_start(1001)),
    ];

    store(&mut client, schema, releases.clone()).await;
    store(&mut client, schema, releases).await;

    assert_eq!(load_releases(&client, None, None).await.unwrap().len(), 2);
}

#[tokio::test]
async fn since_epoch_matches_either_epoch() {
    let schema = "releases_since_epoch";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    close_epochs(&client, 980..=1002).await;

    store(
        &mut client,
        schema,
        vec![
            shipped("4.0.1", epoch_start(981)),
            // Shipped before the window, made the floor inside it.
            floor("4.0.0-rc.1", 992),
        ],
    )
    .await;

    let in_window = load_releases(&client, None, Some(990)).await.unwrap();
    assert_eq!(in_window.len(), 1);
    assert_eq!(in_window[0].client_version, "4.0.0-rc.1");

    assert_eq!(
        load_releases(&client, None, Some(980)).await.unwrap().len(),
        2
    );
    assert!(load_releases(&client, None, Some(1000))
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn the_client_filter_narrows_to_one_lineage() {
    let schema = "releases_client_filter";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    store(
        &mut client,
        schema,
        vec![
            entry("agave", "4.2.2", ReleaseSource::Github),
            entry("frankendancer", "0.1106.40201", ReleaseSource::Github),
        ],
    )
    .await;

    let frankendancer = load_releases(&client, Some("frankendancer"), None)
        .await
        .unwrap();
    assert_eq!(frankendancer.len(), 1);
    assert_eq!(frankendancer[0].client_version, "0.1106.40201");

    // A lineage nothing was recorded for is an empty answer, not an error.
    assert!(load_releases(&client, Some("sig"), None)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn the_sfdp_floor_in_force_is_the_most_recently_effective_one() {
    let schema = "releases_floor_at_epoch";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    store(
        &mut client,
        schema,
        vec![
            // The real mainnet series.
            floor("4.0.0-rc.1", 975),
            floor("4.0.2", 992),
            floor("4.2.2", 1033),
            ReleaseEntry {
                sfdp_floor_epoch: Some(1023),
                ..entry("frankendancer", "0.1106.40201", ReleaseSource::Sfdp)
            },
        ],
    )
    .await;

    let at_991 = get_sfdp_floor_at_epoch(&client, Some("agave"), 991)
        .await
        .unwrap();
    assert_eq!(at_991.len(), 1);
    assert_eq!(at_991[0].client_version, "4.0.0-rc.1");
    assert_eq!(at_991[0].sfdp_floor_epoch, 975);

    assert_eq!(
        get_sfdp_floor_at_epoch(&client, Some("agave"), 992)
            .await
            .unwrap()[0]
            .client_version,
        "4.0.2"
    );

    // Nothing was a floor yet at epoch 974.
    assert!(get_sfdp_floor_at_epoch(&client, Some("agave"), 974)
        .await
        .unwrap()
        .is_empty());

    // One row per lineage, all lineages at once.
    let every_lineage = get_sfdp_floor_at_epoch(&client, None, 1030).await.unwrap();
    assert_eq!(
        every_lineage
            .iter()
            .map(|floor| (floor.client_lineage.as_str(), floor.client_version.as_str()))
            .collect::<Vec<_>>(),
        vec![("agave", "4.0.2"), ("frankendancer", "0.1106.40201")]
    );
}
