mod common;

use chrono::{DateTime, Duration, Utc};
use common::{migrated_client, skip_without_database};
use rust_decimal::Decimal;
use store::group_history::load_group_first_seen;
use tokio_postgres::Client;

fn epoch_start(epoch: u64) -> DateTime<Utc> {
    "2022-01-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap() + Duration::days(2 * epoch as i64)
}

async fn close_epoch(client: &Client, epoch: u64) {
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

async fn record(
    client: &Client,
    identity: &str,
    epoch: u64,
    dc_aso: Option<&str>,
    client_id: Option<i32>,
    client_id_raw: Option<&str>,
) {
    client
        .execute(
            "INSERT INTO validators (
                identity, vote_account, epoch, dc_aso, client_id, client_id_raw,
                activated_stake, marinade_stake, marinade_native_stake, superminority,
                stake_to_become_superminority, credits, leader_slots, blocks_produced,
                skip_rate, updated_at
            ) VALUES ($1, $1, $2, $3, $4, $5, 100, 0, 0, false, 0, 0, 0, 0, 0, now())",
            &[
                &identity,
                &Decimal::from(epoch),
                &dc_aso,
                &client_id,
                &client_id_raw,
            ],
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn a_group_is_first_seen_in_the_oldest_epoch_naming_it() {
    let schema = "ds_test_group_first_seen";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    for epoch in [300, 301, 500] {
        close_epoch(&client, epoch).await;
    }

    // The same organisation re-cased between epochs, as the geolocation source does.
    record(&client, "old", 300, Some("Hetzner"), None, None).await;
    record(&client, "new", 301, Some("hetzner"), None, None).await;
    record(&client, "latitude", 500, Some("Latitude"), None, None).await;
    // Both renderings of an unresolved provider stay out of the map.
    record(&client, "unknown", 300, Some("Unknown"), None, None).await;
    record(&client, "unknown-id", 300, Some("Unknown(8)"), None, None).await;
    record(&client, "blank", 300, Some("  "), None, None).await;

    let first_seen = load_group_first_seen(&client).await.unwrap();

    assert_eq!(first_seen.providers["hetzner"].epoch, 300);
    assert_eq!(
        first_seen.providers["hetzner"].at,
        Some(epoch_start(300)),
        "the date comes from the epoch it was first seen in"
    );
    assert_eq!(first_seen.providers["latitude"].epoch, 500);
    assert_eq!(first_seen.providers.len(), 2);
}

#[tokio::test]
async fn clients_fold_onto_their_lineage_and_report_the_floor() {
    let schema = "ds_test_group_first_seen_clients";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    for epoch in [700, 710, 720] {
        close_epoch(&client, epoch).await;
    }

    // 3 `Agave` and 6 `Agave Bam` share the agave lineage; 2 is `Frankendancer`.
    record(&client, "agave", 710, None, Some(3), None).await;
    record(&client, "bam", 700, None, Some(6), None).await;
    record(&client, "frank", 720, None, Some(2), None).await;
    // An id the registry does not know is folded onto no lineage, but still dates the floor.
    record(
        &client,
        "unregistered",
        700,
        None,
        None,
        Some("Unknown(255)"),
    )
    .await;

    let first_seen = load_group_first_seen(&client).await.unwrap();

    assert_eq!(first_seen.client_lineages["agave"].epoch, 700);
    assert_eq!(first_seen.client_lineages["frankendancer"].epoch, 720);
    assert_eq!(first_seen.client_lineages.len(), 2);
    assert_eq!(
        first_seen.client_floor_epoch,
        Some(700),
        "no client can be dated before the epoch the columns start carrying one"
    );
}

#[tokio::test]
async fn an_open_epoch_dates_nothing() {
    let schema = "ds_test_group_first_seen_open";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    record(&client, "one", 900, Some("Hetzner"), None, None).await;

    let first_seen = load_group_first_seen(&client).await.unwrap();

    assert_eq!(first_seen.providers["hetzner"].epoch, 900);
    assert_eq!(first_seen.providers["hetzner"].at, None);
}
