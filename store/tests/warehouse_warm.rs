use chrono::{DateTime, Utc};
use store::directory::Precondition;
use store::docs::{
    epoch_doc_path, ClusterInfoDoc, ClusterInfoSample, EpochDoc, SnapshotDoc, EPOCHS_DIR,
    LIVE_CLUSTER_INFO, SNAPSHOT_DIR,
};
use store::dto::Validator;
use store::utils::{load_cluster_stats, DEFAULT_CACHE_EPOCHS};
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
        credits: 100.into(),
        leader_slots: 100.into(),
        blocks_produced: 90.into(),
        skip_rate: 0.1,
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
