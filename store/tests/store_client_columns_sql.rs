mod common;

use collect::slot_params::baseline_slots_per_year;
use collect::validators_performance::{ValidatorPerformance, ValidatorsPerformanceSnapshot};
use common::{
    migrated_client, skip_without_database, store_snapshot, validator_snapshot, write_yaml,
};
use rust_decimal::Decimal;
use std::collections::{HashMap, HashSet};
use store::dto::UNKNOWN_CLIENT_NAME;
use store::utils::{load_validators, load_versions, ValidatorOverlays};
use store::versions::{store_versions, StoreVersionsParams};
use structopt::StructOpt;
use tokio_postgres::Client;

const EPOCH: u64 = 1000;
const VOTE_ACCOUNT: &str = "voteClientColumns";
const IDENTITY: &str = "identityClientColumns";

struct ClientFields {
    client_id: Option<u16>,
    client_id_raw: Option<String>,
}

fn agave() -> ClientFields {
    ClientFields {
        client_id: Some(3),
        client_id_raw: Some("Agave".into()),
    }
}

fn no_client() -> ClientFields {
    ClientFields {
        client_id: None,
        client_id_raw: None,
    }
}

// Unlike `no_client()`, an actual observation: the node reported a client absent from client-ids.csv.
fn unrecognized() -> ClientFields {
    ClientFields {
        client_id: None,
        client_id_raw: Some("Unknown(97)".into()),
    }
}

fn performance(client: &ClientFields) -> ValidatorPerformance {
    ValidatorPerformance {
        commission: 7,
        version: Some("2.0.0".into()),
        client_id: client.client_id,
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

async fn run_store_validators(client: &mut Client, name: &str, fields: &ClientFields) {
    let mut snapshot = validator_snapshot(EPOCH, IDENTITY, VOTE_ACCOUNT);
    snapshot.validators[0].performance = performance(fields);
    store_snapshot(client, name, &snapshot).await;
}

async fn run_store_versions(client: &mut Client, name: &str, fields: &ClientFields) {
    let mut validators = HashMap::new();
    validators.insert(VOTE_ACCOUNT.to_string(), performance(fields));
    let snapshot = ValidatorsPerformanceSnapshot {
        epoch: EPOCH,
        epoch_slot: 1,
        transaction_count: 1,
        created_at: "2026-07-31T00:00:00Z".into(),
        slots_per_year: baseline_slots_per_year(),
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
                "SELECT client_id, client_id_raw
                 FROM {table} WHERE vote_account = $1 ORDER BY client_id NULLS LAST"
            ),
            &[&VOTE_ACCOUNT],
        )
        .await
        .unwrap()
        .iter()
        .map(|row| ClientFields {
            client_id: row.get::<_, Option<i32>>("client_id").map(|n| n as u16),
            client_id_raw: row.get("client_id_raw"),
        })
        .collect()
}

fn assert_matches(actual: &ClientFields, expected: &ClientFields, context: &str) {
    assert_eq!(actual.client_id, expected.client_id, "client_id: {context}");
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
    rerendered.client_id_raw = Some("Unknown(3)".into());
    run_store_versions(&mut client, "versions-rerendered", &rerendered).await;
    assert_eq!(
        stored_client_columns(&client, "versions").await.len(),
        1,
        "the answering RPC rendering the same id differently is not a client change"
    );

    let mut switched = agave();
    switched.client_id = Some(1);
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

// Re-resolving client_id_raw is what makes a later client-ids.csv row reclassify old rows.
#[tokio::test]
async fn load_versions_classifies_from_the_raw_rendering_when_no_id_was_stored() {
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
            "INSERT INTO versions (vote_account, epoch_slot, epoch, created_at, client_id, client_id_raw)
             VALUES ($1, 1, $2, NOW(), NULL, NULL),
                    ($1, 1, $2, NOW(), NULL, 'Raiku2'),
                    ($1, 1, $2, NOW(), NULL, 'Agave'),
                    ($1, 1, $2, NOW(), NULL, 'Unknown(12)'),
                    ($1, 1, $2, NOW(), 12, 'FireBAM')",
            &[&VOTE_ACCOUNT, &Decimal::from(EPOCH)],
        )
        .await
        .unwrap();

    let versions = load_versions(&client, 1).await.unwrap();
    let records = versions
        .get(VOTE_ACCOUNT)
        .expect("every stored row must load");
    let mut derived: Vec<_> = records
        .iter()
        .map(|r| {
            (
                r.client_id_raw.clone(),
                r.client_id,
                r.client_name.clone(),
                r.client_label.clone(),
                r.client_vendor.clone(),
                r.client_lineage.clone(),
            )
        })
        .collect();
    derived.sort();

    let unknown = |raw: Option<&str>| {
        (
            raw.map(str::to_string),
            None,
            UNKNOWN_CLIENT_NAME.to_string(),
            UNKNOWN_CLIENT_NAME.to_string(),
            None,
            None,
        )
    };
    let firebam = |raw: &str, stored: Option<u16>| {
        (
            Some(raw.to_string()),
            stored.or(Some(12)),
            "FireBAM".to_string(),
            "Frankendancer + JitoBAM".to_string(),
            Some("bam".to_string()),
            Some("frankendancer".to_string()),
        )
    };
    assert_eq!(
        derived,
        vec![
            unknown(None),
            (
                Some("Agave".to_string()),
                Some(3),
                "Agave".to_string(),
                "Agave".to_string(),
                Some("agave".to_string()),
                Some("agave".to_string()),
            ),
            firebam("FireBAM", Some(12)),
            unknown(Some("Raiku2")),
            firebam("Unknown(12)", None),
        ],
        "a registry name or an Unknown(N) rendering classifies even with no stored id; \
         a client the registry does not know stays Unknown with its raw rendering intact"
    );

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}

// `load_validators` derives independently of `load_versions`, twice — record and epoch stats.
#[tokio::test]
async fn load_validators_classifies_the_record_and_its_epoch_stats() {
    let schema = "ds_test_load_validators_client_label";
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
            "INSERT INTO validators (
                identity, vote_account, epoch, activated_stake, marinade_stake,
                marinade_native_stake, superminority, stake_to_become_superminority, credits,
                leader_slots, blocks_produced, skip_rate, updated_at, client_id, client_id_raw
            ) VALUES
                ('identityRegistered', 'voteRegistered', $1, 100, 0, 0, false, 0, 0, 0, 0, 0, NOW(), 12, 'FireBAM'),
                ('identityRawOnly', 'voteRawOnly', $1, 100, 0, 0, false, 0, 0, 0, 0, 0, NOW(), NULL, 'JitoLabs'),
                ('identityReported', 'voteReported', $1, 100, 0, 0, false, 0, 0, 0, 0, 0, NOW(), NULL, 'Raiku2'),
                ('identityNoClient', 'voteNoClient', $1, 100, 0, 0, false, 0, 0, 0, 0, 0, NOW(), NULL, NULL)",
            &[&Decimal::from(EPOCH)],
        )
        .await
        .unwrap();

    let unreachable_scoring_url = "http://127.0.0.1:1".to_string();
    let overlays = ValidatorOverlays {
        verified: HashSet::from(["voteReported".to_string()]),
        protected: HashSet::from(["voteRegistered".to_string()]),
        net_apy: HashMap::from([("voteRegistered".to_string(), 0.0712389)]),
        ..Default::default()
    };
    let validators = load_validators(&client, unreachable_scoring_url, 1, 1, &overlays)
        .await
        .unwrap();

    assert!(
        validators.get("voteRegistered").unwrap().protected,
        "a vote account the caller resolved as protected must be flagged"
    );
    assert!(
        !validators.get("voteReported").unwrap().protected,
        "one it did not must not be"
    );
    assert!(
        validators.get("voteReported").unwrap().verified,
        "the two flags are stamped independently"
    );
    assert!(!validators.get("voteRegistered").unwrap().verified);

    assert_eq!(
        validators.get("voteRegistered").unwrap().net_apy,
        Some(0.0712389),
        "the apy-api value must reach the record unrounded"
    );
    assert_eq!(
        validators.get("voteReported").unwrap().net_apy,
        None,
        "a vote account apy-api has no value for stays null instead of sorting as zero-ish data"
    );

    let unknown = (UNKNOWN_CLIENT_NAME, UNKNOWN_CLIENT_NAME, None, None);
    for (vote_account, expected) in [
        (
            "voteRegistered",
            (
                "FireBAM",
                "Frankendancer + JitoBAM",
                Some("bam"),
                Some("frankendancer"),
            ),
        ),
        (
            "voteRawOnly",
            ("Jito Labs", "Agave + Jito", Some("jito"), Some("agave")),
        ),
        ("voteReported", unknown),
        ("voteNoClient", unknown),
    ] {
        let record = validators
            .get(vote_account)
            .unwrap_or_else(|| panic!("{vote_account} must load"));
        let expected = (
            expected.0.to_string(),
            expected.1.to_string(),
            expected.2.map(str::to_string),
            expected.3.map(str::to_string),
        );
        assert_eq!(
            (
                record.client_name.clone(),
                record.client_label.clone(),
                record.client_vendor.clone(),
                record.client_lineage.clone(),
            ),
            expected,
            "a stored id or a resolvable raw rendering classifies the record, otherwise Unknown: {vote_account}"
        );
        assert_eq!(
            record.epoch_stats.len(),
            1,
            "one epoch stats entry per stored epoch: {vote_account}"
        );
        assert_eq!(
            (
                record.epoch_stats[0].client_name.clone(),
                record.epoch_stats[0].client_label.clone(),
                record.epoch_stats[0].client_vendor.clone(),
                record.epoch_stats[0].client_lineage.clone(),
            ),
            expected,
            "the epoch stats must derive identically to the record: {vote_account}"
        );
    }

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
async fn migration_drops_the_columns_now_derived_from_the_registry() {
    let schema = "ds_test_migration_drops_derived";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    for table in ["validators", "versions"] {
        let columns: Vec<String> = client
            .query(
                "SELECT column_name FROM information_schema.columns
                 WHERE table_schema = $1 AND table_name = $2 AND column_name LIKE 'client%'",
                &[&schema, &table],
            )
            .await
            .unwrap()
            .iter()
            .map(|row| row.get::<_, String>("column_name"))
            .collect();
        let mut columns = columns;
        columns.sort();
        assert_eq!(
            columns,
            vec!["client_id".to_string(), "client_id_raw".to_string()],
            "{table} must keep the stored identity only, everything else is derived on read"
        );
    }

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}
