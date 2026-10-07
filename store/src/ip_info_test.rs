use crate::docs::{
    IpInfoDoc, IpInfoEntry, NodeObservation, NodeObservationState, NodeObservationsDoc,
};
use crate::ip_info::{
    apply_fetched, ips_in_use, is_worth_looking_up, select_stale_ips, select_unknown_ips,
};
use chrono::{DateTime, Duration, Utc};
use collect::whois_service::{Coordinates, IpInfo};
use std::collections::HashSet;

fn at(hours_ago: i64) -> DateTime<Utc> {
    "2026-10-01T12:00:00Z".parse::<DateTime<Utc>>().unwrap() - Duration::hours(hours_ago)
}

fn observation(
    ip: &str,
    created_at: DateTime<Utc>,
    last_seen_at: DateTime<Utc>,
) -> NodeObservation {
    NodeObservation {
        ip: Some(ip.to_string()),
        gossip_port: Some(8001),
        version: Some("2.3.0".into()),
        client_id: None,
        client_id_raw: None,
        feature_set: None,
        shred_version: None,
        rpc_public: Some(false),
        pubsub_public: Some(false),
        epoch: 900,
        epoch_slot: 10,
        created_at,
        last_seen_at,
    }
}

fn known(ip: &str, fetched_at: DateTime<Utc>) -> (String, IpInfoEntry) {
    (
        ip.to_string(),
        IpInfoEntry {
            asn: Some(1),
            aso: Some("aso".into()),
            continent: None,
            country_iso: None,
            country: None,
            city: None,
            coordinates_lat: None,
            coordinates_lon: None,
            fetched_at,
        },
    )
}

#[test]
fn routable_addresses_are_looked_up() {
    assert!(is_worth_looking_up("1.1.1.1"));
    assert!(is_worth_looking_up("8.8.8.8"));
    assert!(is_worth_looking_up("2606:4700:4700::1111"));
}

#[test]
fn unroutable_addresses_are_skipped() {
    for ip in [
        "127.0.0.1",
        "10.0.0.1",
        "172.16.0.1",
        "192.168.1.1",
        "169.254.0.1",
        "0.0.0.0",
        "255.255.255.255",
        "::1",
        "::",
        "fc00::1",
        "fe80::1",
        "ff02::1",
    ] {
        assert!(!is_worth_looking_up(ip), "{ip}");
    }
}

// parse_socket_addr splits on the last colon without validating, so the field can hold this.
#[test]
fn a_non_address_is_skipped_rather_than_sent_to_whois() {
    assert!(!is_worth_looking_up("some-host.example.com"));
    assert!(!is_worth_looking_up(""));
}

// An address a node moved away from stays in use until the window passes it.
#[test]
fn addresses_in_use_cover_the_recent_history_not_only_the_current_one() {
    let mut observations = NodeObservationsDoc::new();
    observations.insert(
        "node".into(),
        NodeObservationState {
            last: observation("3.3.3.3", at(2), at(0)),
            changes: vec![
                observation("1.1.1.1", at(400), at(300)),
                observation("2.2.2.2", at(300), at(3)),
                observation("3.3.3.3", at(2), at(0)),
            ],
        },
    );

    let in_use = ips_in_use(&observations, at(24 * 7));

    assert_eq!(
        in_use,
        HashSet::from(["2.2.2.2".to_string(), "3.3.3.3".to_string()])
    );
}

#[test]
fn unknown_addresses_are_the_routable_ones_in_use_with_no_answer_yet() {
    let in_use: HashSet<String> = ["1.1.1.1", "2.2.2.2", "10.0.0.1"]
        .into_iter()
        .map(String::from)
        .collect();
    let info: IpInfoDoc = [known("2.2.2.2", at(1))].into_iter().collect();

    assert_eq!(
        select_unknown_ips(&in_use, &info),
        vec!["1.1.1.1".to_string()]
    );
}

#[test]
fn stale_addresses_are_refreshed_oldest_first_and_only_while_in_use() {
    let in_use: HashSet<String> = ["1.1.1.1", "2.2.2.2", "3.3.3.3"]
        .into_iter()
        .map(String::from)
        .collect();
    let info: IpInfoDoc = [
        known("1.1.1.1", at(5)),
        known("2.2.2.2", at(50)),
        known("3.3.3.3", at(20)),
        known("4.4.4.4", at(500)),
    ]
    .into_iter()
    .collect();

    assert_eq!(
        select_stale_ips(&in_use, &info, 2),
        vec!["2.2.2.2".to_string(), "3.3.3.3".to_string()],
        "the address nobody advertises any more is not worth a round trip"
    );
}

#[test]
fn a_fetched_answer_replaces_the_stored_one_whole() {
    let mut info: IpInfoDoc = [known("1.1.1.1", at(50))].into_iter().collect();
    let answer = IpInfo {
        asn: None,
        aso: None,
        coordinates: Some(Coordinates { lat: 1.5, lon: 2.5 }),
        continent: Some("EU".into()),
        country_iso: None,
        country: None,
        city: None,
    };

    apply_fetched(&mut info, &[("1.1.1.1".to_string(), answer)], at(0));

    let entry = &info["1.1.1.1"];
    assert_eq!(
        entry.asn, None,
        "a null in the new answer is not the old value"
    );
    assert_eq!(entry.continent.as_deref(), Some("EU"));
    assert_eq!(entry.coordinates_lat, Some(1.5));
    assert_eq!(entry.fetched_at, at(0));
}
