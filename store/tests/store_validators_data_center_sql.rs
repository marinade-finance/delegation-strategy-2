mod common;

use collect::validators::{Snapshot, ValidatorDataCenter, ValidatorSnapshot};
use collect::validators_performance::ValidatorPerformance;
use common::{migrated_client, skip_without_database};
use store::validators::{store_validators, StoreValidatorsParams};
use structopt::StructOpt;
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

async fn run_store_validators(
    client: &mut Client,
    name: &str,
    data_center: Option<ValidatorDataCenter>,
) {
    let snapshot = Snapshot {
        epoch: EPOCH,
        created_at: "2026-08-17T00:00:00Z".into(),
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
            data_center,
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
// as an absent data center, so without COALESCE the last run of the epoch decides these columns.
#[tokio::test]
async fn store_validators_keeps_the_data_center_an_unresolved_run_cannot_report() {
    let schema = "ds_test_store_validators_data_center";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    run_store_validators(
        &mut client,
        "dc-insert",
        Some(resolved_data_center(
            "Hetzner",
            "Germany",
            24940,
            "Nuremberg",
        )),
    )
    .await;
    assert_eq!(
        stored_data_center(&client).await,
        expected("Hetzner", "Germany", 24940, "Nuremberg"),
        "INSERT path"
    );

    run_store_validators(&mut client, "dc-unresolved", None).await;
    assert_eq!(
        stored_data_center(&client).await,
        expected("Hetzner", "Germany", 24940, "Nuremberg"),
        "an unresolved whois lookup must not blank the epoch's known data center"
    );

    run_store_validators(
        &mut client,
        "dc-moved",
        Some(resolved_data_center("OVH", "France", 16276, "Roubaix")),
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
