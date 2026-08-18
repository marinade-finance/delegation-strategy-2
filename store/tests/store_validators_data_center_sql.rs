mod common;

use collect::validators::{Snapshot, ValidatorDataCenter};
use common::{migrated_client, skip_without_database, store_snapshot, validator_snapshot};
use rust_decimal::Decimal;
use tokio_postgres::Client;

const EPOCH: u64 = 1000;
const NEXT_EPOCH: u64 = 1001;
const THIRD_EPOCH: u64 = 1002;
const EPOCH_BEYOND_CARRY_WINDOW: u64 = 1011;
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

fn snapshot(
    epoch: u64,
    node_ip: Option<&str>,
    data_center: Option<ValidatorDataCenter>,
) -> Snapshot {
    let mut snapshot = validator_snapshot(epoch, IDENTITY, VOTE_ACCOUNT);
    snapshot.validators[0].node_ip = node_ip.map(Into::into);
    snapshot.validators[0].data_center = data_center;
    snapshot
}

async fn stored_data_center(client: &Client, epoch: u64) -> StoredDataCenter {
    let rows = client
        .query(
            "SELECT dc_aso, dc_country, dc_asn, dc_city FROM validators
             WHERE vote_account = $1 AND epoch = $2",
            &[&VOTE_ACCOUNT, &Decimal::from(epoch)],
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
        &snapshot(EPOCH, Some("A"), Some(hetzner())),
    )
    .await;
    assert_eq!(
        stored_data_center(&client, EPOCH).await,
        expected("Hetzner", "Germany", 24940, "Nuremberg"),
        "INSERT path"
    );

    store_snapshot(
        &mut client,
        "dc-unresolved",
        &snapshot(EPOCH, Some("A"), None),
    )
    .await;
    assert_eq!(
        stored_data_center(&client, EPOCH).await,
        expected("Hetzner", "Germany", 24940, "Nuremberg"),
        "an unresolved whois lookup must not blank the epoch's known data center"
    );

    store_snapshot(
        &mut client,
        "dc-moved",
        &snapshot(
            EPOCH,
            Some("A"),
            Some(resolved_data_center("OVH", "France", 16276, "Roubaix")),
        ),
    )
    .await;
    assert_eq!(
        stored_data_center(&client, EPOCH).await,
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
            EPOCH,
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
        &snapshot(EPOCH, Some("A"), Some(network_only)),
    )
    .await;

    assert_eq!(
        stored_data_center(&client, EPOCH).await,
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
            EPOCH,
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

    store_snapshot(
        &mut client,
        "dc-ip-moved",
        &snapshot(EPOCH, Some("B"), None),
    )
    .await;

    assert_eq!(
        stored_data_center(&client, EPOCH).await,
        (None, None, None, None),
        "an unresolved lookup for a new IP must not inherit the previous address's data center"
    );

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}

// The epoch's first store has no row to preserve, so the guard on the UPDATE branch cannot reach
// this case and the location the previous epoch resolved would be lost for the whole epoch.
#[tokio::test]
async fn store_validators_carries_the_data_center_into_a_new_epoch_an_unresolved_run_cannot_report()
{
    let schema = "ds_test_store_validators_data_center_new_epoch";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    store_snapshot(
        &mut client,
        "dc-epoch-seed",
        &snapshot(
            EPOCH,
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

    store_snapshot(
        &mut client,
        "dc-epoch-unresolved",
        &snapshot(NEXT_EPOCH, Some("A"), None),
    )
    .await;

    assert_eq!(
        stored_data_center(&client, NEXT_EPOCH).await,
        expected("Hetzner", "Germany", 24940, "Nuremberg"),
        "the new epoch's first store must carry the data center the previous epoch resolved"
    );
    assert_eq!(
        stored_data_center(&client, EPOCH).await,
        expected("Hetzner", "Germany", 24940, "Nuremberg"),
        "the previous epoch's row must stay as it was"
    );

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}

#[tokio::test]
async fn store_validators_does_not_carry_the_data_center_into_a_new_epoch_when_the_node_ip_changes()
{
    let schema = "ds_test_store_validators_data_center_new_epoch_moved_ip";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    store_snapshot(
        &mut client,
        "dc-epoch-ip-seed",
        &snapshot(
            EPOCH,
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

    store_snapshot(
        &mut client,
        "dc-epoch-ip-moved",
        &snapshot(NEXT_EPOCH, Some("B"), None),
    )
    .await;

    assert_eq!(
        stored_data_center(&client, NEXT_EPOCH).await,
        (None, None, None, None),
        "a node on a new address must not inherit the previous epoch's data center"
    );

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}

// Same invariant as within an epoch: only an unresolved lookup may keep what came before, so a
// resolved answer that is silent on the city must not have the previous epoch's city filled in.
#[tokio::test]
async fn store_validators_does_not_complete_a_partial_answer_across_the_epoch_boundary() {
    let schema = "ds_test_store_validators_data_center_new_epoch_partial";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    store_snapshot(
        &mut client,
        "dc-epoch-partial-seed",
        &snapshot(
            EPOCH,
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
        "dc-epoch-partial",
        &snapshot(NEXT_EPOCH, Some("A"), Some(network_only)),
    )
    .await;

    assert_eq!(
        stored_data_center(&client, NEXT_EPOCH).await,
        (
            Some("OVH".to_string()),
            Some("France".to_string()),
            Some(16276),
            None
        ),
        "carrying Nuremberg across the boundary would put OVH in a German city"
    );

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}

// store_validators is not transactional, so a crash between the insert and the carry — or any epoch
// stored before the carry existed — leaves an epoch with no location that must not block the next one.
#[tokio::test]
async fn store_validators_carries_the_data_center_over_an_epoch_that_has_none() {
    let schema = "ds_test_store_validators_data_center_over_a_gap";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    store_snapshot(
        &mut client,
        "dc-gap-seed",
        &snapshot(
            EPOCH,
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
    store_snapshot(
        &mut client,
        "dc-gap-middle",
        &snapshot(NEXT_EPOCH, Some("A"), None),
    )
    .await;

    client
        .execute(
            "UPDATE validators SET dc_coordinates_lat = NULL, dc_coordinates_lon = NULL,
                dc_continent = NULL, dc_country_iso = NULL, dc_country = NULL, dc_city = NULL,
                dc_asn = NULL, dc_aso = NULL
             WHERE vote_account = $1 AND epoch = $2",
            &[&VOTE_ACCOUNT, &Decimal::from(NEXT_EPOCH)],
        )
        .await
        .unwrap();

    store_snapshot(
        &mut client,
        "dc-gap-next",
        &snapshot(THIRD_EPOCH, Some("A"), None),
    )
    .await;

    assert_eq!(
        stored_data_center(&client, THIRD_EPOCH).await,
        expected("Hetzner", "Germany", 24940, "Nuremberg"),
        "an epoch left without a location must be stepped over, not propagated forward"
    );

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}

// The reach-back is bounded: it keeps a boundary-wide failure off a full table scan, and a location
// this old is no longer evidence of where the node runs now.
#[tokio::test]
async fn store_validators_does_not_carry_a_data_center_older_than_the_carry_window() {
    let schema = "ds_test_store_validators_data_center_carry_window";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    store_snapshot(
        &mut client,
        "dc-window-seed",
        &snapshot(
            EPOCH,
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

    store_snapshot(
        &mut client,
        "dc-window-far",
        &snapshot(EPOCH_BEYOND_CARRY_WINDOW, Some("A"), None),
    )
    .await;

    assert_eq!(
        stored_data_center(&client, EPOCH_BEYOND_CARRY_WINDOW).await,
        (None, None, None, None),
        "a location last seen further back than the carry window must read as unknown"
    );

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}
