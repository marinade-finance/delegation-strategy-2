mod common;

use collect::validators::{Snapshot, ValidatorDataCenter, ValidatorSnapshot};
use collect::validators_performance::ValidatorPerformance;
use common::{migrated_client, skip_without_database};
use store::validators::{store_validators, StoreValidatorsParams};
use structopt::StructOpt;
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
    let snapshot = Snapshot {
        epoch: EPOCH,
        created_at: "2026-08-03T00:00:00Z".into(),
        validators: vec![ValidatorSnapshot {
            identity: IDENTITY.into(),
            vote_account: VOTE_ACCOUNT.into(),
            node_ip: None,
            gossip_port: None,
            rpc_public: None,
            pubsub_public: None,
            info_name: None,
            info_url: None,
            info_details: None,
            info_keybase: None,
            info_icon_url: None,
            data_center: Some(ValidatorDataCenter {
                coordinates: Some((LON, LAT)),
                ..Default::default()
            }),
            activated_stake: 100,
            foundation_stake: 0,
            self_stake: 0,
            marinade_stake: 0,
            marinade_native_stake: 0,
            institutional_stake: 0,
            superminority: false,
            stake_to_become_superminority: 0,
            performance: ValidatorPerformance {
                commission: 7,
                version: Some("2.0.0".into()),
                client_id: None,
                client_id_raw: None,
                feature_set: None,
                shred_version: None,
                credits: 10,
                leader_slots: 100,
                blocks_produced: 100,
                skip_rate: 0f64,
                delinquent: false,
            },
        }],
    };
    let path = std::env::temp_dir().join(format!("ds-test-{name}.yaml"));
    std::fs::write(&path, serde_yaml::to_string(&snapshot).unwrap()).unwrap();
    store_validators(
        StoreValidatorsParams::from_iter(["store", "--snapshot-file", path.to_str().unwrap()]),
        client,
    )
    .await
    .unwrap();
    std::fs::remove_file(path).unwrap();
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
