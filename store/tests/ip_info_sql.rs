mod common;

use chrono::{DateTime, Duration, Utc};
use collect::whois_service::{Coordinates, IpInfo};
use common::{migrated_client, skip_without_database};
use store::ip_info::{select_stale_ips, select_unknown_ips, upsert_ip_info};
use tokio_postgres::Client;

fn info(city: Option<&str>) -> IpInfo {
    IpInfo {
        asn: Some(64500),
        aso: Some("Test ISP".into()),
        coordinates: Some(Coordinates { lat: 1.5, lon: 2.5 }),
        continent: Some("Europe".into()),
        country_iso: Some("CZ".into()),
        country: Some("Czechia".into()),
        city: city.map(Into::into),
    }
}

// created_at deliberately far older than last_seen_at: that gap is what the rotation must survive,
// since a node only gets a new row when it changes.
async fn observe(client: &Client, identity: &str, ip: Option<&str>, last_seen_age: &str) {
    client
        .execute(
            "INSERT INTO node_observations (identity, ip, epoch_slot, epoch, created_at, last_seen_at)
             VALUES ($1, $2, 0, 1000, now() - interval '365 days', now() - $3::text::interval)",
            &[&identity, &ip, &last_seen_age],
        )
        .await
        .unwrap();
}

async fn enrich(client: &Client, ip: &str, fetched_at: DateTime<Utc>) {
    upsert_ip_info(client, &[(ip.to_string(), info(Some("Brno")))], fetched_at)
        .await
        .unwrap();
}

#[tokio::test]
async fn only_addresses_never_looked_up_are_offered() {
    let schema = "ds_test_ip_info_unknown";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    observe(&client, "identityA", Some("1.1.1.1"), "1 minute").await;
    observe(&client, "identityA", Some("1.1.1.1"), "2 minutes").await;
    observe(&client, "identityB", Some("2.2.2.2"), "1 minute").await;
    observe(&client, "identityC", None, "1 minute").await;
    observe(&client, "identityD", Some("127.0.0.1"), "1 minute").await;
    observe(&client, "identityE", Some("10.0.0.1"), "1 minute").await;
    observe(&client, "identityF", Some("3.3.3.3"), "30 days").await;
    enrich(&client, "2.2.2.2", Utc::now()).await;

    let unknown = select_unknown_ips(&client, 7).await.unwrap();

    assert_eq!(unknown, vec!["1.1.1.1".to_string()]);
}

// The whole point of last_seen_at: a node that has not changed in a year is still in the cluster,
// and its address must stay eligible for enrichment.
#[tokio::test]
async fn an_unchanged_node_is_still_offered_for_lookup_and_refresh() {
    let schema = "ds_test_ip_info_stable_node";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    observe(&client, "identityStable", Some("1.1.1.1"), "1 minute").await;

    assert_eq!(
        select_unknown_ips(&client, 7).await.unwrap(),
        vec!["1.1.1.1".to_string()]
    );

    enrich(&client, "1.1.1.1", Utc::now() - Duration::days(9)).await;

    assert_eq!(
        select_stale_ips(&client, 10, 7).await.unwrap(),
        vec!["1.1.1.1".to_string()]
    );
}

#[tokio::test]
async fn the_refresh_takes_the_oldest_addresses_up_to_the_limit() {
    let schema = "ds_test_ip_info_rotation_order";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    for (ip, days) in [("1.1.1.1", 1), ("2.2.2.2", 3), ("3.3.3.3", 2)] {
        observe(&client, "identityShared", Some(ip), "1 minute").await;
        enrich(&client, ip, Utc::now() - Duration::days(days)).await;
    }

    let stale = select_stale_ips(&client, 2, 7).await.unwrap();

    assert_eq!(stale, vec!["2.2.2.2".to_string(), "3.3.3.3".to_string()]);
}

#[tokio::test]
async fn an_address_the_cluster_left_is_not_refreshed() {
    let schema = "ds_test_ip_info_rotation_in_use";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    observe(&client, "identityStillHere", Some("1.1.1.1"), "1 hour").await;
    observe(&client, "identityGone", Some("2.2.2.2"), "30 days").await;
    enrich(&client, "1.1.1.1", Utc::now() - Duration::days(9)).await;
    enrich(&client, "2.2.2.2", Utc::now() - Duration::days(10)).await;

    let stale = select_stale_ips(&client, 10, 7).await.unwrap();

    assert_eq!(stale, vec!["1.1.1.1".to_string()]);
}

#[tokio::test]
async fn a_refresh_overwrites_every_field_including_back_to_null() {
    let schema = "ds_test_ip_info_upsert";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    // Fixed instants, not now(): timestamptz keeps microseconds and Utc::now() carries nanoseconds.
    let first: DateTime<Utc> = "2026-07-30T00:00:00Z".parse().unwrap();
    assert_eq!(
        upsert_ip_info(
            &client,
            &[("1.1.1.1".to_string(), info(Some("Brno")))],
            first
        )
        .await
        .unwrap(),
        1
    );

    let row = client
        .query_one("SELECT * FROM ip_info WHERE ip = '1.1.1.1'", &[])
        .await
        .unwrap();
    assert_eq!(row.get::<_, Option<i64>>("asn"), Some(64500));
    assert_eq!(
        row.get::<_, Option<String>>("city").as_deref(),
        Some("Brno")
    );
    assert_eq!(row.get::<_, Option<f64>>("coordinates_lat"), Some(1.5));
    assert_eq!(row.get::<_, DateTime<Utc>>("fetched_at"), first);

    let second: DateTime<Utc> = "2026-07-31T00:00:00Z".parse().unwrap();
    assert_eq!(
        upsert_ip_info(&client, &[("1.1.1.1".to_string(), info(None))], second)
            .await
            .unwrap(),
        1
    );

    let row = client
        .query_one("SELECT * FROM ip_info WHERE ip = '1.1.1.1'", &[])
        .await
        .unwrap();
    assert_eq!(row.get::<_, Option<String>>("city"), None);
    assert_eq!(row.get::<_, DateTime<Utc>>("fetched_at"), second);
    assert_eq!(
        client
            .query_one("SELECT count(*) FROM ip_info", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        1
    );
}

// 4-byte ASNs run past i32::MAX, and the column is what decides whether they survive the round trip.
#[tokio::test]
async fn a_32_bit_asn_survives_storage() {
    let schema = "ds_test_ip_info_wide_asn";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    let mut wide = info(Some("Brno"));
    wide.asn = Some(u32::MAX);
    upsert_ip_info(&client, &[("1.1.1.1".to_string(), wide)], Utc::now())
        .await
        .unwrap();

    let row = client
        .query_one("SELECT * FROM ip_info WHERE ip = '1.1.1.1'", &[])
        .await
        .unwrap();
    assert_eq!(row.get::<_, Option<i64>>("asn"), Some(u32::MAX as i64));
}
