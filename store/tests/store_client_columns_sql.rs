mod common;

use collect::validators_performance::ValidatorPerformance;
use common::{migrated_client, skip_without_database};
use rust_decimal::Decimal;
use std::collections::{HashMap, HashSet};
use store::dto::UNKNOWN_CLIENT_NAME;
use store::utils::{load_validators, load_versions, ValidatorOverlays};

const EPOCH: u64 = 1000;
const VOTE_ACCOUNT: &str = "voteClientColumns";

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
