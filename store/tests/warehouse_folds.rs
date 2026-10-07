use chrono::{DateTime, Utc};
use collect::validators_performance::ValidatorPerformance;
use std::collections::{HashMap, HashSet};
use store::docs::{VersionSample, VersionState};
use store::dto::{Validator, UNKNOWN_CLIENT_NAME};
use store::utils::{
    load_client_diversity_stats, load_client_lineage_stats, load_dc_concentration_stats,
    load_validators, load_versions, ValidatorOverlays,
};
use store::warehouse::Warehouse;

mod common;

const LAST_EPOCH: u64 = 1000;
const EPOCHS: u64 = 7;
const GAP_EPOCH: u64 = 997;

fn validator(
    vote_account: &str,
    epoch: u64,
    activated_stake: u64,
    credits: u64,
    client_id: Option<i32>,
    client_id_raw: Option<&str>,
) -> Validator {
    Validator {
        activated_stake: activated_stake.into(),
        credits: Some(credits.into()),
        leader_slots: 100.into(),
        blocks_produced: 100.into(),
        client_id,
        client_id_raw: client_id_raw.map(str::to_string),
        updated_at: Some(Utc::now()),
        ..common::validator(vote_account, epoch)
    }
}

fn store_validator(warehouse: &mut Warehouse, epoch: u64, validator: Validator) {
    warehouse
        .snapshots
        .entry(epoch)
        .or_default()
        .insert(validator.vote_account.clone(), validator);
}

#[test]
fn stake_distribution_emits_every_requested_epoch_including_gaps() {
    let mut warehouse = Warehouse::default();
    let first_epoch = LAST_EPOCH - EPOCHS + 1;
    for epoch in first_epoch..=LAST_EPOCH {
        if epoch == GAP_EPOCH {
            continue;
        }
        store_validator(
            &mut warehouse,
            epoch,
            validator("voteA", epoch, 100, 10, Some(1), None),
        );
        store_validator(
            &mut warehouse,
            epoch,
            validator("voteB", epoch, 200, 10, Some(1), None),
        );
        store_validator(
            &mut warehouse,
            epoch,
            validator("voteC", epoch, 700, 10, None, None),
        );
    }

    let diversity = load_client_diversity_stats(&warehouse, EPOCHS).unwrap();

    let epochs: Vec<u64> = diversity.iter().map(|stats| stats.epoch).collect();
    assert_eq!(
        epochs,
        (first_epoch..=LAST_EPOCH).rev().collect::<Vec<u64>>(),
        "every requested epoch must be emitted exactly once, newest first"
    );

    let gap = diversity
        .iter()
        .find(|stats| stats.epoch == GAP_EPOCH)
        .unwrap();
    assert_eq!(gap.total_activated_stake, 0);
    assert!(gap.client_stake.is_empty());
    assert!(gap.client_share.is_empty());
    assert!(gap.client_validator_count.is_empty());

    let populated = diversity
        .iter()
        .find(|stats| stats.epoch == LAST_EPOCH)
        .unwrap();
    assert_eq!(populated.total_activated_stake, 1000);
    assert_eq!(populated.client_stake.get("jito"), Some(&300));
    assert_eq!(populated.client_stake.get("unknown"), Some(&700));
    assert_eq!(populated.client_validator_count.get("jito"), Some(&2));
    assert_eq!(populated.client_validator_count.get("unknown"), Some(&1));
    assert!((populated.client_share.get("jito").unwrap() - 0.3).abs() < 1e-9);
    assert!((populated.client_share.get("unknown").unwrap() - 0.7).abs() < 1e-9);

    let concentration = load_dc_concentration_stats(&warehouse, EPOCHS).unwrap();
    assert_eq!(
        concentration
            .iter()
            .map(|stats| stats.epoch)
            .collect::<Vec<u64>>(),
        epochs,
        "cluster-stats series must cover the same epoch window"
    );
}

// Ids 9, 10 and 11 are the three lineages harmonic ships.
#[test]
fn client_diversity_merges_the_ids_a_vendor_ships_across_lineages() {
    let mut warehouse = Warehouse::default();
    for (vote_account, client_id, stake) in [
        ("voteHarmonicFiredancer", 9, 100),
        ("voteHarmonicAgave", 10, 200),
        ("voteHarmonicFrankendancer", 11, 300),
        ("voteAgave", 3, 400),
    ] {
        store_validator(
            &mut warehouse,
            LAST_EPOCH,
            validator(vote_account, LAST_EPOCH, stake, 10, Some(client_id), None),
        );
    }

    let diversity = load_client_diversity_stats(&warehouse, 1).unwrap();
    let stats = diversity
        .iter()
        .find(|stats| stats.epoch == LAST_EPOCH)
        .unwrap();

    assert_eq!(stats.total_activated_stake, 1000);
    assert_eq!(
        stats.client_stake.get("harmonic"),
        Some(&600),
        "all three harmonic ids contribute to one bucket"
    );
    assert_eq!(
        stats.client_validator_count.get("harmonic"),
        Some(&3),
        "the validator count merges alongside the stake"
    );
    assert!((stats.client_share.get("harmonic").unwrap() - 0.6).abs() < 1e-9);
    assert_eq!(stats.client_stake.get("agave"), Some(&400));

    let lineage = load_client_lineage_stats(&warehouse, 1).unwrap();
    let stats = lineage
        .iter()
        .find(|stats| stats.epoch == LAST_EPOCH)
        .unwrap();
    assert_eq!(
        stats.lineage_stake.get("agave"),
        Some(&600),
        "id 10 is an agave fork, so it merges with plain agave on the lineage axis"
    );
    assert_eq!(stats.lineage_stake.get("firedancer"), Some(&100));
    assert_eq!(stats.lineage_stake.get("frankendancer"), Some(&300));
}

#[test]
fn client_diversity_classifies_a_validator_from_its_raw_rendering_alone() {
    let mut warehouse = Warehouse::default();
    for (vote_account, stake, client_id, client_id_raw) in [
        ("voteStored", 100, Some(1), Some("JitoLabs")),
        ("voteRawName", 200, None, Some("JitoLabs")),
        ("voteRawNumber", 300, None, Some("Unknown(1)")),
        ("voteUnregistered", 400, None, Some("Raiku2")),
        ("voteNoClient", 500, None, None),
    ] {
        store_validator(
            &mut warehouse,
            LAST_EPOCH,
            validator(
                vote_account,
                LAST_EPOCH,
                stake,
                10,
                client_id,
                client_id_raw,
            ),
        );
    }

    let diversity = load_client_diversity_stats(&warehouse, 1).unwrap();
    let stats = diversity
        .iter()
        .find(|stats| stats.epoch == LAST_EPOCH)
        .unwrap();

    assert_eq!(stats.total_activated_stake, 1500);
    assert_eq!(
        stats.client_stake.get("jito"),
        Some(&600),
        "a stored id and both raw renderings of the same client land in one bucket"
    );
    assert_eq!(stats.client_validator_count.get("jito"), Some(&3));
    assert_eq!(
        stats.client_stake.get("unknown"),
        Some(&900),
        "an unregistered rendering and no client at all both stay unclassified"
    );
    assert_eq!(stats.client_validator_count.get("unknown"), Some(&2));
}

fn version_sample(client_id: Option<i32>, client_id_raw: Option<&str>) -> VersionSample {
    VersionSample {
        epoch: LAST_EPOCH,
        epoch_slot: 1,
        version: Some("2.0.0".to_string()),
        client_id,
        client_id_raw: client_id_raw.map(str::to_string),
        feature_set: None,
        shred_version: None,
        created_at: "2026-08-03T00:00:00Z".parse::<DateTime<Utc>>().unwrap(),
    }
}

// Re-resolving client_id_raw is what makes a later client-ids.csv row reclassify old changes.
#[test]
fn load_versions_classifies_from_the_raw_rendering_when_no_id_was_stored() {
    let mut warehouse = Warehouse::default();
    warehouse.live.cluster_info.epoch = LAST_EPOCH;
    let changes: Vec<VersionSample> = [
        (None, None),
        (None, Some("Raiku2")),
        (None, Some("Agave")),
        (None, Some("Unknown(12)")),
        (Some(12), Some("FireBAM")),
    ]
    .into_iter()
    .map(|(client_id, raw)| version_sample(client_id, raw))
    .collect();
    warehouse.live.versions.insert(
        "voteClientColumns".to_string(),
        VersionState {
            last: changes[4].clone(),
            changes,
        },
    );

    let versions = load_versions(&warehouse, 1).unwrap();
    let records = versions
        .get("voteClientColumns")
        .expect("every stored change must load");
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
}

// `load_validators` derives independently of `load_versions`, twice — record and epoch stats.
#[tokio::test]
async fn load_validators_classifies_the_record_and_its_epoch_stats() {
    let mut warehouse = Warehouse::default();
    warehouse.live.cluster_info.epoch = LAST_EPOCH;
    for (vote_account, client_id, client_id_raw) in [
        ("voteRegistered", Some(12), Some("FireBAM")),
        ("voteRawOnly", None, Some("JitoLabs")),
        ("voteReported", None, Some("Raiku2")),
        ("voteNoClient", None, None),
    ] {
        store_validator(
            &mut warehouse,
            LAST_EPOCH,
            validator(vote_account, LAST_EPOCH, 100, 0, client_id, client_id_raw),
        );
    }

    let overlays = ValidatorOverlays {
        verified: HashSet::from(["voteReported".to_string()]),
        protected: HashSet::from(["voteRegistered".to_string()]),
        net_apy: HashMap::from([("voteRegistered".to_string(), 0.0712389)]),
        ..Default::default()
    };
    let validators = load_validators(&warehouse, 1, 1, &overlays).await.unwrap();

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
}

#[test]
fn legacy_snapshot_client_id_string_still_deserializes() {
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
