use chrono::{DateTime, Utc};
use std::collections::HashMap;
use store::directory::{Fetch, Precondition};
use store::docs::{
    epoch_doc_path, ClusterInfoDoc, ClusterInfoSample, EpochDoc, SealedVersionsDoc, SnapshotDoc,
    VersionSample, EPOCHS_DIR, LIVE_CLUSTER_INFO, SNAPSHOT_DIR, VERSIONS_DIR,
};
use store::dto::Validator;
use store::utils::{
    load_cluster_stats, load_validators, load_validators_aggregated_flat, ValidatorOverlays,
    DEFAULT_CACHE_EPOCHS,
};
use store::warehouse::{has_validators, Warehouse};

mod common;

const EPOCH: u64 = 1000;
const VOTE_ACCOUNT: &str = "voteWarehouse";

fn at(moment: &str) -> DateTime<Utc> {
    moment.parse().expect("timestamp")
}

fn validator(activated_stake: u64) -> Validator {
    Validator {
        dc_continent: Some("Europe".to_string()),
        dc_country: Some("Germany".to_string()),
        dc_city: Some("Berlin".to_string()),
        dc_asn: Some(24940),
        dc_aso: Some("Hetzner".to_string()),
        commission_advertised: Some(7),
        commission_effective: Some(8),
        version: Some("2.0.0".to_string()),
        client_id: Some(3),
        client_id_raw: Some("Agave".to_string()),
        feature_set: Some(123),
        shred_version: Some(456),
        activated_stake: activated_stake.into(),
        credits: Some(100.into()),
        leader_slots: 100.into(),
        blocks_produced: 90.into(),
        skip_rate: Some(0.1),
        uptime_pct: Some(1.0),
        uptime: Some(3600.into()),
        downtime: Some(0.into()),
        updated_at: Some(at("2026-08-03T01:00:00Z")),
        ..common::validator(VOTE_ACCOUNT, EPOCH)
    }
}

#[tokio::test]
async fn the_warehouse_warms_from_the_documents_and_then_answers_304() {
    let Some(store) = common::directory_store("warehouse-warm").await else {
        return;
    };
    let directory = store.client();

    assert!(
        !has_validators(&directory).await.expect("has validators"),
        "an empty store holds no snapshot"
    );

    let mut snapshot = SnapshotDoc::new();
    snapshot.insert(VOTE_ACCOUNT.to_string(), validator(1000));
    directory
        .put(
            &epoch_doc_path(SNAPSHOT_DIR, EPOCH),
            &snapshot,
            Precondition::Create,
        )
        .await
        .expect("write snapshot");
    directory
        .put(
            &epoch_doc_path(EPOCHS_DIR, EPOCH),
            &EpochDoc {
                epoch: EPOCH,
                start_at: at("2026-08-03T00:00:00Z"),
                end_at: at("2026-08-03T01:00:00Z"),
                transaction_count: 300,
                supply: 500_000_000u64.into(),
                inflation: 0.045,
                inflation_taper: 0.15,
                slots_per_year: 78_892_314.984,
            },
            Precondition::Create,
        )
        .await
        .expect("write epoch");
    directory
        .put(
            LIVE_CLUSTER_INFO,
            &ClusterInfoDoc {
                epoch: EPOCH,
                samples: vec![ClusterInfoSample {
                    epoch: EPOCH,
                    epoch_slot: 1,
                    transaction_count: 100,
                    created_at: at("2026-08-03T00:00:00Z"),
                    slots_per_year: 78_892_314.984,
                }],
            },
            Precondition::Create,
        )
        .await
        .expect("write cluster info");

    let mut warehouse = Warehouse::default();
    warehouse
        .warm(&directory, DEFAULT_CACHE_EPOCHS)
        .await
        .expect("warm");

    assert_eq!(warehouse.last_epoch(), EPOCH);
    assert_eq!(warehouse.last_cluster_epoch(), EPOCH);
    assert_eq!(warehouse.snapshots[&EPOCH].len(), 1);
    assert_eq!(warehouse.epochs[&EPOCH].transaction_count, 300);
    assert_eq!(warehouse.live.cluster_info.samples.len(), 1);
    assert!(has_validators(&directory).await.expect("has validators"));

    let stats = load_cluster_stats(&warehouse, DEFAULT_CACHE_EPOCHS).expect("cluster stats");
    assert_eq!(stats.block_production_stats[0].epoch, EPOCH);
    assert_eq!(stats.block_production_stats[0].blocks_produced, 90);
    assert_eq!(stats.block_production_stats[0].leader_slots, 100);
    assert!((stats.block_production_stats[0].avg_skip_rate - 0.1).abs() < 1e-9);
    assert_eq!(stats.dc_concentration_stats[0].total_activated_stake, 1000);
    assert_eq!(
        stats.dc_concentration_stats[0]
            .dc_stake_by_city
            .get("Europe/Germany/Berlin"),
        Some(&1000)
    );
    assert_eq!(
        stats.client_diversity_stats[0].client_stake.get("agave"),
        Some(&1000)
    );

    warehouse
        .warm(&directory, DEFAULT_CACHE_EPOCHS)
        .await
        .expect("second warm");
    assert_eq!(warehouse.snapshots[&EPOCH].len(), 1);
    assert_eq!(warehouse.epochs[&EPOCH].transaction_count, 300);

    let mut snapshot = warehouse.snapshots[&EPOCH].clone();
    snapshot.insert("voteSecond".to_string(), validator(3000));
    let etag = directory
        .get::<SnapshotDoc>(&epoch_doc_path(SNAPSHOT_DIR, EPOCH))
        .await
        .expect("get snapshot")
        .expect("snapshot document")
        .etag;
    directory
        .put(
            &epoch_doc_path(SNAPSHOT_DIR, EPOCH),
            &snapshot,
            Precondition::IfMatch(etag),
        )
        .await
        .expect("replace snapshot");

    warehouse
        .warm(&directory, DEFAULT_CACHE_EPOCHS)
        .await
        .expect("third warm");
    assert_eq!(
        warehouse.snapshots[&EPOCH].len(),
        2,
        "a changed document replaces the cached copy"
    );
}

#[tokio::test]
async fn previous_snapshot_survives_refresh_and_rollover_without_expanding_the_window() {
    let Some(store) = common::directory_store("warehouse-previous-snapshot").await else {
        return;
    };
    let directory = store.client();
    let mut etags = HashMap::new();
    for epoch in EPOCH - 7..=EPOCH {
        let mut validator = validator(epoch * 100);
        validator.epoch = epoch.into();
        let mut snapshot = SnapshotDoc::from([(VOTE_ACCOUNT.to_string(), validator)]);
        if epoch == EPOCH - 7 {
            snapshot.insert(
                "votePreviousOnly".to_string(),
                common::validator("votePreviousOnly", epoch),
            );
        }
        let etag = directory
            .put(
                &epoch_doc_path(SNAPSHOT_DIR, epoch),
                &snapshot,
                Precondition::Create,
            )
            .await
            .expect("write snapshot");
        etags.insert(epoch, etag);
    }
    for (epoch, version, created_at) in [
        (EPOCH - 7, "99.0.0", "2030-01-01T00:00:00Z"),
        (EPOCH, "2.0.0", "2026-08-03T00:00:00Z"),
    ] {
        let versions = SealedVersionsDoc::from([(
            VOTE_ACCOUNT.to_string(),
            vec![VersionSample {
                epoch,
                epoch_slot: 0,
                version: Some(version.to_string()),
                client_id: Some(3),
                client_id_raw: None,
                feature_set: None,
                shred_version: None,
                created_at: at(created_at),
            }],
        )]);
        directory
            .put(
                &epoch_doc_path(VERSIONS_DIR, epoch),
                &versions,
                Precondition::Create,
            )
            .await
            .expect("write versions");
    }
    let mut warehouse = Warehouse::default();
    let mut cluster_etag = None;
    for last_epoch in [EPOCH, EPOCH, EPOCH + 1] {
        if last_epoch > EPOCH {
            let mut validator = validator(last_epoch * 100);
            validator.epoch = last_epoch.into();
            let snapshot = SnapshotDoc::from([(VOTE_ACCOUNT.to_string(), validator)]);
            directory
                .put(
                    &epoch_doc_path(SNAPSHOT_DIR, last_epoch),
                    &snapshot,
                    Precondition::Create,
                )
                .await
                .expect("write advancing snapshot");
        }
        if cluster_etag.is_none() || last_epoch > EPOCH {
            let precondition = match cluster_etag.take() {
                Some(etag) => Precondition::IfMatch(etag),
                None => Precondition::Create,
            };
            cluster_etag = Some(
                directory
                    .put(
                        LIVE_CLUSTER_INFO,
                        &ClusterInfoDoc {
                            epoch: last_epoch,
                            samples: Vec::new(),
                        },
                        precondition,
                    )
                    .await
                    .expect("write cluster info"),
            );
        }
        warehouse.warm(&directory, 7).await.expect("warm window");
        let previous_epoch = last_epoch - 7;
        assert_eq!(
            warehouse.snapshots.keys().copied().collect::<Vec<_>>(),
            (last_epoch - 6..=last_epoch).collect::<Vec<_>>()
        );
        assert_eq!(
            warehouse
                .previous_snapshot
                .keys()
                .copied()
                .collect::<Vec<_>>(),
            vec![previous_epoch]
        );
        assert_eq!(
            warehouse.previous_snapshot[&previous_epoch][VOTE_ACCOUNT].activated_stake,
            (previous_epoch * 100).into()
        );
        assert_eq!(warehouse.last_epoch(), last_epoch);
        assert_eq!(
            warehouse.epochs_window(7),
            (last_epoch - 6..=last_epoch).rev().collect::<Vec<_>>()
        );
        assert_eq!(
            warehouse.versions.keys().copied().collect::<Vec<_>>(),
            vec![EPOCH]
        );
        assert!(matches!(
            directory
                .get_if_none_match::<SnapshotDoc>(
                    &epoch_doc_path(SNAPSHOT_DIR, previous_epoch),
                    &etags[&previous_epoch]
                )
                .await
                .expect("conditional snapshot read"),
            Fetch::NotModified
        ));
        let records = load_validators(&warehouse, 7, 7, &ValidatorOverlays::default())
            .await
            .unwrap();
        assert!(!records.contains_key("votePreviousOnly"));
        assert_eq!(records[VOTE_ACCOUNT].epoch_stats.len(), 7);
        let oldest = records[VOTE_ACCOUNT]
            .epoch_stats
            .iter()
            .min_by_key(|stats| stats.epoch)
            .unwrap();
        assert_eq!(oldest.epoch, last_epoch - 6);
        assert_eq!(oldest.epoch_stake, Some((previous_epoch * 100).into()));
        let flat = load_validators_aggregated_flat(&warehouse, last_epoch, 7).unwrap();
        assert_eq!(flat.len(), 1);
        assert_eq!(flat[0].version, "2.0.0");
        let expected_stake = if last_epoch == EPOCH {
            0.0000997
        } else {
            0.0000998
        };
        assert!((flat[0].avg_stake - expected_stake).abs() < 1e-12);
    }
}
