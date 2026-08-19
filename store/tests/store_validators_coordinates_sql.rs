mod common;

use collect::validators::ValidatorDataCenter;
use common::{migrated_client, skip_without_database, store_snapshot, validator_snapshot};
use tokio_postgres::Client;

const EPOCH: u64 = 1000;
const VOTE_ACCOUNT: &str = "voteCoordinates";
const IDENTITY: &str = "identityCoordinates";

// Tokyo. The longitude is outside the range any latitude could hold, so a swap cannot pass as a
// plausible coordinate — and the two write paths bind these columns through a positional vector
// that no type check covers.
const LON: f64 = 139.6917;
const LAT: f64 = 35.6895;

async fn run_store_validators(client: &mut Client, name: &str) {
    let mut snapshot = validator_snapshot(EPOCH, IDENTITY, VOTE_ACCOUNT);
    snapshot.validators[0].data_center = Some(ValidatorDataCenter {
        coordinates: Some((LON, LAT)),
        ..Default::default()
    });
    store_snapshot(client, name, &snapshot).await;
}

async fn stored_coordinates(client: &Client) -> (Option<f64>, Option<f64>) {
    let rows = client
        .query(
            "SELECT dc_coordinates_lat, dc_coordinates_lon FROM validators WHERE vote_account = $1",
            &[&VOTE_ACCOUNT],
        )
        .await
        .unwrap();
    assert_eq!(rows.len(), 1, "one row per vote account and epoch");
    (
        rows[0].get("dc_coordinates_lat"),
        rows[0].get("dc_coordinates_lon"),
    )
}

// `ValidatorDataCenter.coordinates` is an unnamed (lon, lat) tuple and both write paths bind the
// columns positionally, so latitude and longitude can be exchanged twice over and still look right
// in isolation. Only the round trip proves which column each value lands in.
#[tokio::test]
async fn store_validators_round_trips_coordinates_without_swapping_them() {
    let schema = "ds_test_store_validators_coordinates";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    run_store_validators(&mut client, "coordinates-insert").await;
    assert_eq!(
        stored_coordinates(&client).await,
        (Some(LAT), Some(LON)),
        "INSERT path"
    );

    run_store_validators(&mut client, "coordinates-update").await;
    assert_eq!(
        stored_coordinates(&client).await,
        (Some(LAT), Some(LON)),
        "UPDATE path"
    );

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}
