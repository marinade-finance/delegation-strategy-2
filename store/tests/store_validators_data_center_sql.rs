mod common;

use collect::validators::{Snapshot, ValidatorDataCenter};
use common::{migrated_client, skip_without_database, store_snapshot, validator_snapshot};
use tokio_postgres::Client;

const EPOCH: u64 = 1000;
const VOTE_ACCOUNT: &str = "voteDataCenter";
const IDENTITY: &str = "identityDataCenter";

type StoredDataCenter = (Option<String>, Option<String>, Option<i32>, Option<String>);

fn resolved_data_center(aso: &str, country: &str, asn: u32, city: &str) -> ValidatorDataCenter {
    ValidatorDataCenter {
        country: Some(country.into()),
        city: Some(city.into()),
        asn: Some(asn),
        aso: Some(aso.into()),
        ..Default::default()
    }
}

fn expected(aso: &str, country: &str, asn: i32, city: &str) -> StoredDataCenter {
    (
        Some(aso.into()),
        Some(country.into()),
        Some(asn),
        Some(city.into()),
    )
}

fn snapshot(node_ip: Option<&str>, data_center: Option<ValidatorDataCenter>) -> Snapshot {
    let mut snapshot = validator_snapshot(EPOCH, IDENTITY, VOTE_ACCOUNT);
    snapshot.validators[0].node_ip = node_ip.map(Into::into);
    snapshot.validators[0].data_center = data_center;
    snapshot
}

async fn stored_data_center(client: &Client) -> StoredDataCenter {
    let rows = client
        .query(
            "SELECT dc_aso, dc_country, dc_asn, dc_city FROM validators WHERE vote_account = $1",
            &[&VOTE_ACCOUNT],
        )
        .await
        .unwrap();
    assert_eq!(rows.len(), 1, "one row per vote account and epoch");
    (
        rows[0].get("dc_aso"),
        rows[0].get("dc_country"),
        rows[0].get("dc_asn"),
        rows[0].get("dc_city"),
    )
}

// The collector runs hourly against one row per epoch, and get_data_centers reports a whois failure
// as an absent data center, so without the guard the last run of the epoch decides these columns.
#[tokio::test]
async fn store_validators_keeps_the_data_center_an_unresolved_run_cannot_report() {
    let schema = "ds_test_store_validators_data_center";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    let hetzner = || resolved_data_center("Hetzner", "Germany", 24940, "Nuremberg");

    store_snapshot(
        &mut client,
        "dc-insert",
        &snapshot(Some("A"), Some(hetzner())),
    )
    .await;
    assert_eq!(
        stored_data_center(&client).await,
        expected("Hetzner", "Germany", 24940, "Nuremberg"),
        "INSERT path"
    );

    store_snapshot(&mut client, "dc-unresolved", &snapshot(Some("A"), None)).await;
    assert_eq!(
        stored_data_center(&client).await,
        expected("Hetzner", "Germany", 24940, "Nuremberg"),
        "an unresolved whois lookup must not blank the epoch's known data center"
    );

    store_snapshot(
        &mut client,
        "dc-moved",
        &snapshot(
            Some("A"),
            Some(resolved_data_center("OVH", "France", 16276, "Roubaix")),
        ),
    )
    .await;
    assert_eq!(
        stored_data_center(&client).await,
        expected("OVH", "France", 16276, "Roubaix"),
        "a resolved lookup must still replace the stored data center"
    );

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}

// Every field of IpInfo is independently optional, so a lookup can answer for the network and stay
// silent on the city. Carrying the previous city across would place the new ASO in the old town.
#[tokio::test]
async fn store_validators_does_not_complete_a_partial_answer_from_the_previous_data_center() {
    let schema = "ds_test_store_validators_data_center_partial";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    store_snapshot(
        &mut client,
        "dc-partial-seed",
        &snapshot(
            Some("A"),
            Some(resolved_data_center(
                "Hetzner",
                "Germany",
                24940,
                "Nuremberg",
            )),
        ),
    )
    .await;

    let network_only = ValidatorDataCenter {
        country: Some("France".into()),
        asn: Some(16276),
        aso: Some("OVH".into()),
        city: None,
        ..Default::default()
    };
    store_snapshot(
        &mut client,
        "dc-partial",
        &snapshot(Some("A"), Some(network_only)),
    )
    .await;

    assert_eq!(
        stored_data_center(&client).await,
        (
            Some("OVH".to_string()),
            Some("France".to_string()),
            Some(16276),
            None
        ),
        "a resolved answer replaces all eight columns together; keeping Nuremberg would put OVH in a German city"
    );

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}

// node_ip is assigned unconditionally, so preserving the location across an IP change would pair the
// new address with the data center of the old one until some later lookup happens to succeed.
#[tokio::test]
async fn store_validators_drops_the_data_center_when_the_node_ip_changes() {
    let schema = "ds_test_store_validators_data_center_moved_ip";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    store_snapshot(
        &mut client,
        "dc-ip-seed",
        &snapshot(
            Some("A"),
            Some(resolved_data_center(
                "Hetzner",
                "Germany",
                24940,
                "Nuremberg",
            )),
        ),
    )
    .await;

    store_snapshot(&mut client, "dc-ip-moved", &snapshot(Some("B"), None)).await;

    assert_eq!(
        stored_data_center(&client).await,
        (None, None, None, None),
        "an unresolved lookup for a new IP must not inherit the previous address's data center"
    );

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}
