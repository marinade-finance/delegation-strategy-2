mod common;

use collect::validators::{Snapshot, ValidatorSnapshot};
use collect::validators_performance::{ValidatorPerformance, ValidatorsPerformanceSnapshot};
use common::{migrated_client, skip_without_database};
use rust_decimal::Decimal;
use std::collections::HashMap;
use store::dto::UNKNOWN_CLIENT_NAME;
use store::utils::load_versions;
use store::validators::{store_validators, StoreValidatorsParams};
use store::versions::{store_versions, StoreVersionsParams};
use structopt::StructOpt;
use tokio_postgres::Client;

const EPOCH: u64 = 1000;
const VOTE_ACCOUNT: &str = "voteClientColumns";
const IDENTITY: &str = "identityClientColumns";

struct ClientFields {
    client_id: Option<u16>,
    client_name: Option<String>,
    client_vendor: Option<String>,
    client_lineage: Option<String>,
    client_id_raw: Option<String>,
}

fn agave() -> ClientFields {
    ClientFields {
        client_id: Some(3),
        client_name: Some("Agave".into()),
        client_vendor: Some("agave".into()),
        client_lineage: Some("agave".into()),
        client_id_raw: Some("Agave".into()),
    }
}

fn no_client() -> ClientFields {
    ClientFields {
        client_id: None,
        client_name: None,
        client_vendor: None,
        client_lineage: None,
        client_id_raw: None,
    }
}

// Unlike `no_client()`, this is an actual observation: the node reported a client, we just don't
// have it in client-ids.csv yet. `client_id_raw`/`client_name` carry the raw rendering.
fn unrecognized() -> ClientFields {
    ClientFields {
        client_id: None,
        client_name: Some("Unknown(97)".into()),
        client_vendor: None,
        client_lineage: None,
        client_id_raw: Some("Unknown(97)".into()),
    }
}

fn performance(client: &ClientFields) -> ValidatorPerformance {
    ValidatorPerformance {
        commission: 7,
        version: Some("2.0.0".into()),
        client_id: client.client_id,
        client_name: client.client_name.clone(),
        client_vendor: client.client_vendor.clone(),
        client_lineage: client.client_lineage.clone(),
        client_id_raw: client.client_id_raw.clone(),
        feature_set: Some(123),
        shred_version: Some(456),
        credits: 10,
        leader_slots: 100,
        blocks_produced: 100,
        skip_rate: 0f64,
        delinquent: false,
    }
}

fn write_yaml(name: &str, contents: &str) -> String {
    let path = std::env::temp_dir().join(format!("ds-test-{name}.yaml"));
    std::fs::write(&path, contents).unwrap();
    path.to_str().unwrap().to_string()
}

async fn run_store_validators(client: &mut Client, name: &str, fields: &ClientFields) {
    let snapshot = Snapshot {
        epoch: EPOCH,
        created_at: "2026-07-31T00:00:00Z".into(),
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
            data_center: None,
            activated_stake: 100,
            foundation_stake: 0,
            self_stake: 0,
            marinade_stake: 0,
            marinade_native_stake: 0,
            institutional_stake: 0,
            superminority: false,
            stake_to_become_superminority: 0,
            performance: performance(fields),
        }],
    };
    let path = write_yaml(name, &serde_yaml::to_string(&snapshot).unwrap());
    store_validators(
        StoreValidatorsParams::from_iter(["store", "--snapshot-file", &path]),
        client,
    )
    .await
    .unwrap();
    std::fs::remove_file(path).unwrap();
}

async fn run_store_versions(client: &mut Client, name: &str, fields: &ClientFields) {
    let mut validators = HashMap::new();
    validators.insert(VOTE_ACCOUNT.to_string(), performance(fields));
    let snapshot = ValidatorsPerformanceSnapshot {
        epoch: EPOCH,
        epoch_slot: 1,
        transaction_count: 1,
        created_at: "2026-07-31T00:00:00Z".into(),
        cluster_inflation: None,
        validators,
        rewards: None,
    };
    let path = write_yaml(name, &serde_yaml::to_string(&snapshot).unwrap());
    store_versions(
        StoreVersionsParams::from_iter(["store", "--snapshot-file", &path]),
        client,
    )
    .await
    .unwrap();
    std::fs::remove_file(path).unwrap();
}

async fn stored_client_columns(client: &Client, table: &str) -> Vec<ClientFields> {
    client
        .query(
            &format!(
                "SELECT client_id, client_name, client_vendor, client_lineage, client_id_raw
                 FROM {table} WHERE vote_account = $1 ORDER BY client_id NULLS LAST"
            ),
            &[&VOTE_ACCOUNT],
        )
        .await
        .unwrap()
        .iter()
        .map(|row| ClientFields {
            client_id: row.get::<_, Option<i32>>("client_id").map(|n| n as u16),
            client_name: row.get("client_name"),
            client_vendor: row.get("client_vendor"),
            client_lineage: row.get("client_lineage"),
            client_id_raw: row.get("client_id_raw"),
        })
        .collect()
}

fn assert_matches(actual: &ClientFields, expected: &ClientFields, context: &str) {
    assert_eq!(actual.client_id, expected.client_id, "client_id: {context}");
    assert_eq!(
        actual.client_name, expected.client_name,
        "client_name: {context}"
    );
    assert_eq!(
        actual.client_vendor, expected.client_vendor,
        "client_vendor: {context}"
    );
    assert_eq!(
        actual.client_lineage, expected.client_lineage,
        "client_lineage: {context}"
    );
    assert_eq!(
        actual.client_id_raw, expected.client_id_raw,
        "client_id_raw: {context}"
    );
}

// The UPDATE path casts parameters via a positional index map the compiler cannot check.
#[tokio::test]
async fn store_validators_round_trips_client_columns_on_insert_and_update() {
    let schema = "ds_test_store_validators_client";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    run_store_validators(&mut client, "validators-insert", &agave()).await;
    let inserted = stored_client_columns(&client, "validators").await;
    assert_eq!(inserted.len(), 1, "one row per vote account and epoch");
    assert_matches(&inserted[0], &agave(), "INSERT path");

    run_store_validators(&mut client, "validators-update", &agave()).await;
    let updated = stored_client_columns(&client, "validators").await;
    assert_eq!(updated.len(), 1, "the second run must update, not insert");
    assert_matches(&updated[0], &agave(), "UPDATE path");

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}

#[tokio::test]
async fn store_validators_keeps_the_last_known_client_when_gossip_reports_none() {
    let schema = "ds_test_store_validators_gossip_gap";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    run_store_validators(&mut client, "gossip-gap-seed", &agave()).await;
    run_store_validators(&mut client, "gossip-gap-empty", &no_client()).await;

    let stored = stored_client_columns(&client, "validators").await;
    assert_eq!(stored.len(), 1);
    assert_matches(
        &stored[0],
        &agave(),
        "a snapshot with no gossip data must not erase the stored client",
    );

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}

// Regression test for P1-R1#1: COALESCE-ing client_id/vendor/lineage against the resolved value
// (instead of against whether a client was observed at all) left a switch to an unrecognized
// client indistinguishable from a transient gossip gap, so the old classification stuck around.
#[tokio::test]
async fn store_validators_clears_a_stale_classification_when_the_client_becomes_unrecognized() {
    let schema = "ds_test_store_validators_unrecognized_switch";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    run_store_validators(&mut client, "unrecognized-seed", &agave()).await;
    run_store_validators(&mut client, "unrecognized-switch", &unrecognized()).await;

    let stored = stored_client_columns(&client, "validators").await;
    assert_eq!(stored.len(), 1);
    assert_matches(
        &stored[0],
        &unrecognized(),
        "a validator switching to an unregistered client must not keep the old classification",
    );

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}

#[tokio::test]
async fn store_versions_logs_a_change_only_when_the_resolved_client_changes() {
    let schema = "ds_test_store_versions_client";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    run_store_versions(&mut client, "versions-first", &agave()).await;
    assert_eq!(
        stored_client_columns(&client, "versions").await.len(),
        1,
        "the first snapshot must be recorded"
    );

    run_store_versions(&mut client, "versions-unchanged", &agave()).await;
    assert_eq!(
        stored_client_columns(&client, "versions").await.len(),
        1,
        "an unchanged snapshot must not add a row"
    );

    let mut rerendered = agave();
    rerendered.client_name = Some("Unknown(3)".into());
    rerendered.client_id_raw = Some("Unknown(3)".into());
    run_store_versions(&mut client, "versions-rerendered", &rerendered).await;
    assert_eq!(
        stored_client_columns(&client, "versions").await.len(),
        1,
        "the answering RPC rendering the same id differently is not a client change"
    );

    let mut switched = agave();
    switched.client_id = Some(1);
    switched.client_name = Some("Jito Labs".into());
    switched.client_vendor = Some("jito".into());
    switched.client_id_raw = Some("JitoLabs".into());
    run_store_versions(&mut client, "versions-switched", &switched).await;
    assert_eq!(
        stored_client_columns(&client, "versions").await.len(),
        2,
        "a different resolved client id must be recorded"
    );

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}

// The never-null contract is kept at the read layer, not in the column: NULL stays storable so
// "the node reported no client" remains distinguishable from "the registry does not know this one".
#[tokio::test]
async fn load_versions_serves_unknown_when_no_client_name_is_stored() {
    let schema = "ds_test_load_versions_unknown_client";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    client
        .execute(
            "INSERT INTO cluster_info (epoch_slot, epoch, transaction_count, created_at)
             VALUES (1, $1, 0, NOW())",
            &[&Decimal::from(EPOCH)],
        )
        .await
        .unwrap();
    client
        .execute(
            "INSERT INTO versions (vote_account, epoch_slot, epoch, created_at, client_id, client_name)
             VALUES ($1, 1, $2, NOW(), NULL, NULL),
                    ($1, 1, $2, NOW(), NULL, 'Agave'),
                    ($1, 1, $2, NOW(), 12, 'FireBAM')",
            &[&VOTE_ACCOUNT, &Decimal::from(EPOCH)],
        )
        .await
        .unwrap();

    let versions = load_versions(&client, 1).await.unwrap();
    let records = versions
        .get(VOTE_ACCOUNT)
        .expect("every stored row must load");
    let mut names: Vec<String> = records.iter().map(|r| r.client_name.clone()).collect();
    names.sort();
    assert_eq!(
        names,
        vec![
            "Agave".to_string(),
            "FireBAM".to_string(),
            UNKNOWN_CLIENT_NAME.to_string()
        ],
        "a NULL client_name reads back as the fallback, a stored one unchanged"
    );

    let mut labels: Vec<String> = records.iter().map(|r| r.client_label.clone()).collect();
    labels.sort();
    assert_eq!(
        labels,
        vec![
            "Agave".to_string(),
            "Frankendancer + JitoBAM".to_string(),
            UNKNOWN_CLIENT_NAME.to_string()
        ],
        "a stored client_id labels the row, otherwise the reported name then the fallback"
    );

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}

#[tokio::test]
async fn legacy_snapshot_client_id_string_still_deserializes() {
    let yaml = "
commission: 7
version: 2.0.0
client_id: Unknown(8)
credits: 10
leader_slots: 100
blocks_produced: 100
skip_rate: 0.0
delinquent: false
";
    let performance: ValidatorPerformance = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(performance.client_id, Some(8));

    let numeric = yaml.replace("client_id: Unknown(8)", "client_id: 8");
    let performance: ValidatorPerformance = serde_yaml::from_str(&numeric).unwrap();
    assert_eq!(performance.client_id, Some(8));

    let named = yaml.replace("client_id: Unknown(8)", "client_id: Rakurai");
    let performance: ValidatorPerformance = serde_yaml::from_str(&named).unwrap();
    assert_eq!(performance.client_id, Some(8));

    let unknown = yaml.replace("client_id: Unknown(8)", "client_id: brand-new-client");
    let performance: ValidatorPerformance = serde_yaml::from_str(&unknown).unwrap();
    assert_eq!(performance.client_id, None);
}

#[tokio::test]
async fn migration_discards_client_data_collected_before_the_rename() {
    let schema = "ds_test_migration_client_wipe";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    let leftovers = client
        .query(
            "SELECT COUNT(*) AS stale FROM validators
             WHERE client_id_raw IS NOT NULL OR client_vendor IS NOT NULL OR client_lineage IS NOT NULL",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(leftovers[0].get::<_, i64>("stale"), 0);

    client
        .execute(
            "INSERT INTO validators (
                identity, vote_account, epoch, activated_stake, marinade_stake,
                marinade_native_stake, superminority, stake_to_become_superminority, credits,
                leader_slots, blocks_produced, skip_rate, client_id_raw, client_vendor, updated_at
            ) VALUES ($1, $2, $3, 0, 0, 0, false, 0, 0, 0, 0, 0, 'AgaveBam', 'bam', NOW())",
            &[&IDENTITY, &VOTE_ACCOUNT, &Decimal::from(EPOCH)],
        )
        .await
        .unwrap();

    client
        .execute(
            "INSERT INTO versions (
                vote_account, epoch_slot, epoch, created_at, client_id_raw, client_vendor
            ) VALUES ($1, 1, $2, NOW(), 'AgaveBam', 'bam')",
            &[&VOTE_ACCOUNT, &Decimal::from(EPOCH)],
        )
        .await
        .unwrap();

    let sql = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../migrations/0019-client-registry-fields.sql"
    ))
    .unwrap();
    let wipes: Vec<&str> = sql
        .split(';')
        .filter(|statement| statement.contains("client_id_raw = NULL"))
        .collect();
    assert_eq!(
        wipes.len(),
        2,
        "0019 must wipe both validators and versions"
    );
    client
        .batch_execute(&format!("{};", wipes.join(";")))
        .await
        .unwrap();

    let stored = stored_client_columns(&client, "validators").await;
    assert_eq!(stored.len(), 1);
    assert_matches(
        &stored[0],
        &no_client(),
        "the migration must leave no pre-rename client data behind",
    );

    let stored_versions = stored_client_columns(&client, "versions").await;
    assert_eq!(stored_versions.len(), 1);
    assert_matches(
        &stored_versions[0],
        &no_client(),
        "the migration must leave no pre-rename client data behind in versions",
    );

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}
