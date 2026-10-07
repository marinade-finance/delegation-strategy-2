use chrono::{DateTime, Utc};
use clap::Parser;
use collect::slot_params::baseline_slots_per_year;
use collect::validators::Snapshot;
use collect::validators_performance::{
    ClusterInflation, ValidatorPerformance, ValidatorRewards, ValidatorsPerformanceSnapshot,
};
use rust_decimal::prelude::*;
use std::collections::HashMap;
use store::close_epoch::{close_epoch, CloseEpochParams};
use store::cluster_info::{store_cluster_info, StoreClusterInfoParams};
use store::commissions::{store_commissions, StoreCommissionsParams};
use store::directory::Directory;
use store::directory::Precondition;
use store::docs::{
    epoch_doc_path, ClusterInfoDoc, CommissionsDoc, EpochDoc, SealedClusterInfoDoc,
    SealedCommissionsDoc, SealedUptimesDoc, SealedVersionsDoc, SnapshotDoc, UptimeInterval,
    UptimeStatus, UptimesDoc, VersionsDoc, CLUSTER_INFO_DIR, COMMISSIONS_DIR, EPOCHS_DIR,
    LIVE_CLUSTER_INFO, LIVE_COMMISSIONS, LIVE_UPTIMES, LIVE_VERSIONS, SNAPSHOT_DIR, UPTIMES_DIR,
    VERSIONS_DIR,
};
use store::ls_open_epochs::open_epochs;
use store::uptime::{store_uptime, StoreUptimeParams};
use store::validators::{store_validators, StoreValidatorsParams};
use store::versions::{store_versions, StoreVersionsParams};
use store::warehouse::Warehouse;

mod common;

const EPOCH: u64 = 1000;
const VOTE_ACCOUNT: &str = "voteCloseEpoch";
const IDENTITY: &str = "identityCloseEpoch";
const ADVERTISED: u8 = 7;
const EFFECTIVE: u8 = 8;

fn performance(commission: u8, delinquent: bool) -> ValidatorPerformance {
    ValidatorPerformance {
        commission,
        version: Some("2.0.0".into()),
        client_id: Some(3),
        client_id_raw: Some("Agave".into()),
        feature_set: Some(123),
        shred_version: Some(456),
        credits: Some(10),
        vote_reward_lamports: None,
        last_vote: Some(1),
        credits_total: Some(10),
        leader_slots: 100,
        blocks_produced: 100,
        skip_rate: 0f64,
        delinquent,
    }
}

fn stream_snapshot(
    created_at: &str,
    commission: u8,
    delinquent: bool,
    transaction_count: u64,
) -> ValidatorsPerformanceSnapshot {
    let mut validators = HashMap::new();
    validators.insert(
        VOTE_ACCOUNT.to_string(),
        performance(commission, delinquent),
    );
    ValidatorsPerformanceSnapshot {
        epoch: EPOCH,
        epoch_slot: 1,
        transaction_count,
        created_at: created_at.into(),
        slots_per_year: baseline_slots_per_year(),
        cluster_inflation: None,
        validators,
        rewards: None,
        nodes: Default::default(),
    }
}

fn at(moment: &str) -> DateTime<Utc> {
    moment.parse().expect("timestamp")
}

async fn seed_validators(directory: &Directory) {
    let snapshot = Snapshot {
        epoch: EPOCH,
        created_at: "2026-08-03T00:00:00Z".into(),
        validators: vec![common::validator_snapshot(
            IDENTITY,
            VOTE_ACCOUNT,
            performance(ADVERTISED, false),
        )],
    };
    let path = common::write_yaml("close-epoch-validators", &snapshot);
    store_validators(
        StoreValidatorsParams::parse_from(["store", "--snapshot-file", &path]),
        directory,
    )
    .await
    .expect("store validators");
    std::fs::remove_file(path).expect("remove snapshot");
}

async fn seed_streams(directory: &Directory) {
    // UP, then a 30 second outage, then UP again.
    for (name, created_at, commission, delinquent, transaction_count) in [
        ("close-epoch-s0", "2026-08-03T00:00:00Z", 7u8, false, 100u64),
        ("close-epoch-s1", "2026-08-03T00:00:30Z", 7, true, 200),
        ("close-epoch-s2", "2026-08-03T00:01:00Z", 9, false, 400),
    ] {
        let snapshot = stream_snapshot(created_at, commission, delinquent, transaction_count);
        let path = common::write_yaml(name, &snapshot);
        store_uptime(
            StoreUptimeParams::parse_from(["store", "--snapshot-file", &path]),
            directory,
        )
        .await
        .expect("store uptime");
        store_commissions(
            StoreCommissionsParams::parse_from(["store", "--snapshot-file", &path]),
            directory,
        )
        .await
        .expect("store commissions");
        store_versions(
            StoreVersionsParams::parse_from(["store", "--snapshot-file", &path]),
            directory,
        )
        .await
        .expect("store versions");
        store_cluster_info(
            StoreClusterInfoParams::parse_from(["store", "--snapshot-file", &path]),
            directory,
        )
        .await
        .expect("store cluster info");
        std::fs::remove_file(path).expect("remove snapshot");
    }
}

async fn run_close_epoch(directory: &Directory) {
    let mut validators = HashMap::new();
    validators.insert(
        VOTE_ACCOUNT.to_string(),
        ValidatorPerformance {
            credits: Some(4242),
            leader_slots: 200,
            blocks_produced: 180,
            skip_rate: 0.1,
            ..performance(9, false)
        },
    );
    let mut rewards = HashMap::new();
    rewards.insert(
        VOTE_ACCOUNT.to_string(),
        ValidatorRewards {
            commission_effective: Some(EFFECTIVE),
        },
    );
    let snapshot = ValidatorsPerformanceSnapshot {
        epoch: EPOCH,
        epoch_slot: 432000,
        transaction_count: 400,
        created_at: "2026-08-03T01:00:00Z".into(),
        slots_per_year: baseline_slots_per_year(),
        cluster_inflation: Some(ClusterInflation {
            sol_total_supply: 500_000_000,
            inflation: 0.045,
            inflation_taper: 0.15,
        }),
        validators,
        rewards: Some(rewards),
        nodes: Default::default(),
    };
    let path = common::write_yaml("close-epoch-finalized", &snapshot);
    close_epoch(
        CloseEpochParams::parse_from(["store", "--snapshot-file", &path]),
        directory,
    )
    .await
    .expect("close epoch");
    std::fs::remove_file(path).expect("remove snapshot");
}

#[tokio::test]
async fn close_epoch_seals_derives_and_marks_the_epoch_closed() {
    let Some(store) = common::directory_store("close-epoch").await else {
        return;
    };
    let directory = store.client();

    seed_validators(&directory).await;
    seed_streams(&directory).await;
    assert_eq!(
        open_epochs(&directory).await.expect("open epochs"),
        vec![EPOCH.to_string()],
        "an epoch with a snapshot and no epochs document is open"
    );

    run_close_epoch(&directory).await;

    let epoch_record: EpochDoc = directory
        .get(&epoch_doc_path(EPOCHS_DIR, EPOCH))
        .await
        .expect("get epoch")
        .expect("epoch document")
        .body;
    assert_eq!(epoch_record.start_at, at("2026-08-03T00:00:00Z"));
    assert_eq!(epoch_record.end_at, at("2026-08-03T00:01:00Z"));
    assert_eq!(epoch_record.transaction_count, 300);
    assert_eq!(epoch_record.supply, Decimal::from(500_000_000u64));
    assert_eq!(epoch_record.inflation, 0.045);
    assert_eq!(epoch_record.inflation_taper, 0.15);
    assert_eq!(epoch_record.slots_per_year, baseline_slots_per_year());

    let validators: SnapshotDoc = directory
        .get(&epoch_doc_path(SNAPSHOT_DIR, EPOCH))
        .await
        .expect("get snapshot")
        .expect("snapshot document")
        .body;
    let validator = &validators[VOTE_ACCOUNT];
    assert_eq!(validator.credits, Some(Decimal::from(4242)));
    assert_eq!(validator.leader_slots, Decimal::from(200));
    assert_eq!(validator.blocks_produced, Decimal::from(180));
    assert_eq!(validator.skip_rate, 0.1);
    assert_eq!(validator.commission_effective, Some(EFFECTIVE as i32));
    assert_eq!(validator.commission_min_observed, Some(ADVERTISED as i32));
    assert_eq!(validator.commission_max_observed, Some(9));
    // The epoch ran for 60 seconds and the validator was down for 30 of them.
    assert_eq!(validator.downtime, Some(Decimal::from(30)));
    assert_eq!(validator.uptime, Some(Decimal::from(30)));
    assert_eq!(validator.uptime_pct, Some(0.5));

    let sealed_uptimes: SealedUptimesDoc = directory
        .get(&epoch_doc_path(UPTIMES_DIR, EPOCH))
        .await
        .expect("get uptimes")
        .expect("sealed uptimes")
        .body;
    assert_eq!(sealed_uptimes[VOTE_ACCOUNT].len(), 3);
    let sealed_commissions: SealedCommissionsDoc = directory
        .get(&epoch_doc_path(COMMISSIONS_DIR, EPOCH))
        .await
        .expect("get commissions")
        .expect("sealed commissions")
        .body;
    assert_eq!(sealed_commissions[VOTE_ACCOUNT].len(), 2);
    let sealed_versions: SealedVersionsDoc = directory
        .get(&epoch_doc_path(VERSIONS_DIR, EPOCH))
        .await
        .expect("get versions")
        .expect("sealed versions")
        .body;
    assert_eq!(sealed_versions[VOTE_ACCOUNT].len(), 1);
    let sealed_cluster_info: SealedClusterInfoDoc = directory
        .get(&epoch_doc_path(CLUSTER_INFO_DIR, EPOCH))
        .await
        .expect("get cluster info")
        .expect("sealed cluster info")
        .body;
    assert_eq!(sealed_cluster_info.len(), 3);

    let live_uptimes: UptimesDoc = directory
        .get(LIVE_UPTIMES)
        .await
        .expect("get uptimes")
        .expect("uptimes document")
        .body;
    assert!(
        live_uptimes[VOTE_ACCOUNT].closed.is_empty(),
        "the sealed intervals leave the accumulator"
    );
    let live_commissions: CommissionsDoc = directory
        .get(LIVE_COMMISSIONS)
        .await
        .expect("get commissions")
        .expect("commissions document")
        .body;
    assert!(live_commissions[VOTE_ACCOUNT].changes.is_empty());
    assert_eq!(
        live_commissions[VOTE_ACCOUNT].last.commission, 9,
        "the last commission outlives the seal so the change rule still knows it"
    );
    let live_versions: VersionsDoc = directory
        .get(LIVE_VERSIONS)
        .await
        .expect("get versions")
        .expect("versions document")
        .body;
    assert!(live_versions[VOTE_ACCOUNT].changes.is_empty());
    let live_cluster_info: ClusterInfoDoc = directory
        .get(LIVE_CLUSTER_INFO)
        .await
        .expect("get cluster info")
        .expect("cluster info document")
        .body;
    assert!(live_cluster_info.samples.is_empty());

    assert!(
        open_epochs(&directory)
            .await
            .expect("open epochs")
            .is_empty(),
        "a sealed epoch is no longer open"
    );
}

/// A run that dies between the seal and the trim leaves `live/` holding the
/// epoch's samples. Readers must take the sealed document and ignore them, or
/// the next warm counts the epoch twice.
#[tokio::test]
async fn a_sealed_epoch_is_read_from_its_seal_while_live_still_holds_it() {
    let Some(store) = common::directory_store("close-epoch-leftover").await else {
        return;
    };
    let directory = store.client();

    seed_validators(&directory).await;
    seed_streams(&directory).await;
    run_close_epoch(&directory).await;

    assert!(
        open_epochs(&directory)
            .await
            .expect("open epochs")
            .is_empty(),
        "the epochs document marks the epoch closed"
    );
    let sealed: SealedUptimesDoc = directory
        .get(&epoch_doc_path(UPTIMES_DIR, EPOCH))
        .await
        .expect("get sealed uptimes")
        .expect("sealed uptimes document")
        .body;
    let sealed_count = sealed[VOTE_ACCOUNT].len();

    // What a trim that never ran would have left behind.
    let live = directory
        .get::<UptimesDoc>(LIVE_UPTIMES)
        .await
        .expect("get live uptimes")
        .expect("live uptimes document");
    let mut leftover = live.body;
    leftover
        .get_mut(VOTE_ACCOUNT)
        .expect("validator in live uptimes")
        .closed
        .push(UptimeInterval {
            status: UptimeStatus::Up,
            epoch: EPOCH,
            start_at: at("2026-08-03T00:00:00Z"),
            end_at: at("2026-08-03T00:00:30Z"),
        });
    directory
        .put(LIVE_UPTIMES, &leftover, Precondition::IfMatch(live.etag))
        .await
        .expect("put leftover");

    let mut warehouse = Warehouse::default();
    warehouse.warm(&directory, 80).await.expect("warm");
    let counted = warehouse
        .uptime_intervals(warehouse.window(80))
        .into_iter()
        .filter(|(vote_account, interval)| {
            vote_account.as_str() == VOTE_ACCOUNT && interval.epoch == EPOCH
        })
        .count();
    assert_eq!(
        counted, sealed_count,
        "the leftover live interval is ignored once the epoch has a sealed document"
    );
}

/// A crash between the seal and the trim leaves the accumulators holding the
/// epoch. The rerun must trim, not reseal from accumulators already partly
/// trimmed, which would erase the downtime the seal records.
#[tokio::test]
async fn a_rerun_of_a_sealed_epoch_trims_without_resealing() {
    let Some(store) = common::directory_store("close-epoch-rerun").await else {
        return;
    };
    let directory = store.client();

    seed_validators(&directory).await;
    seed_streams(&directory).await;
    run_close_epoch(&directory).await;

    let sealed: SealedUptimesDoc = directory
        .get(&epoch_doc_path(UPTIMES_DIR, EPOCH))
        .await
        .expect("get uptimes")
        .expect("sealed uptimes")
        .body;
    let intervals = sealed[VOTE_ACCOUNT].len();
    assert!(
        intervals > 0,
        "the first close sealed the epoch's intervals"
    );

    // The accumulators are trimmed by now, so a second close reads exactly what
    // a run that died mid-trim would have left behind.
    run_close_epoch(&directory).await;

    let resealed: SealedUptimesDoc = directory
        .get(&epoch_doc_path(UPTIMES_DIR, EPOCH))
        .await
        .expect("get uptimes")
        .expect("sealed uptimes")
        .body;
    assert_eq!(
        resealed[VOTE_ACCOUNT].len(),
        intervals,
        "a rerun left the sealed intervals alone"
    );
}
