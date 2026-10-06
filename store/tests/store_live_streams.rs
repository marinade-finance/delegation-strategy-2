use chrono::{DateTime, Utc};
use clap::Parser;
use collect::slot_params::baseline_slots_per_year;
use collect::validators_performance::{ValidatorPerformance, ValidatorsPerformanceSnapshot};
use std::collections::HashMap;
use store::cluster_info::{store_cluster_info, StoreClusterInfoParams};
use store::commissions::{store_commissions, StoreCommissionsParams};
use store::directory::Directory;
use store::docs::{
    ClusterInfoDoc, CommissionsDoc, UptimeStatus, UptimesDoc, VersionsDoc, LIVE_CLUSTER_INFO,
    LIVE_COMMISSIONS, LIVE_UPTIMES, LIVE_VERSIONS,
};
use store::uptime::{store_uptime, write_uptimes, StoreUptimeParams};
use store::versions::{store_versions, StoreVersionsParams};

mod common;

const EPOCH: u64 = 1000;
const VOTE_ACCOUNT: &str = "voteLiveStreams";

fn performance(delinquent: bool) -> ValidatorPerformance {
    ValidatorPerformance {
        commission: 7,
        version: Some("2.0.0".into()),
        client_id: Some(3),
        client_id_raw: Some("Agave".into()),
        feature_set: Some(123),
        shred_version: Some(456),
        credits: 10,
        leader_slots: 100,
        blocks_produced: 100,
        skip_rate: 0f64,
        delinquent,
    }
}

fn performance_snapshot(
    created_at: &str,
    delinquent: bool,
    epoch: u64,
) -> ValidatorsPerformanceSnapshot {
    let mut validators = HashMap::new();
    validators.insert(VOTE_ACCOUNT.to_string(), performance(delinquent));
    ValidatorsPerformanceSnapshot {
        epoch,
        epoch_slot: 1,
        transaction_count: 100,
        created_at: created_at.into(),
        slots_per_year: baseline_slots_per_year(),
        cluster_inflation: None,
        validators,
        rewards: None,
        nodes: Default::default(),
    }
}

async fn run_store_uptime(directory: &Directory, name: &str, created_at: &str, delinquent: bool) {
    run_store_uptime_in_epoch(directory, name, created_at, delinquent, EPOCH).await
}

async fn run_store_uptime_in_epoch(
    directory: &Directory,
    name: &str,
    created_at: &str,
    delinquent: bool,
    epoch: u64,
) {
    let snapshot = performance_snapshot(created_at, delinquent, epoch);
    let path = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("{name}.yaml"));
    std::fs::write(&path, serde_yaml::to_string(&snapshot).expect("yaml")).expect("snapshot file");
    store_uptime(
        StoreUptimeParams::parse_from([
            "store",
            "--snapshot-file",
            path.to_str().expect("snapshot path"),
        ]),
        directory,
    )
    .await
    .expect("store uptime");
    std::fs::remove_file(path).expect("remove snapshot");
}

async fn stored_uptimes(directory: &Directory) -> UptimesDoc {
    directory
        .get(LIVE_UPTIMES)
        .await
        .expect("get uptimes")
        .expect("uptimes document")
        .body
}

fn at(moment: &str) -> DateTime<Utc> {
    moment.parse().expect("timestamp")
}

// The cron is the retry: a lost race must surface, not be written over.
#[tokio::test]
async fn a_conflicting_write_is_an_error_and_leaves_the_document_alone() {
    let Some(store) = common::directory_store("uptimes-conflict").await else {
        return;
    };
    let directory = store.client();

    run_store_uptime(&directory, "conflict-seed", "2026-08-03T00:00:00Z", false).await;
    let stale = directory
        .get::<UptimesDoc>(LIVE_UPTIMES)
        .await
        .expect("get uptimes")
        .expect("uptimes document");

    run_store_uptime(&directory, "conflict-winner", "2026-08-03T00:01:00Z", false).await;
    let current = directory
        .get::<UptimesDoc>(LIVE_UPTIMES)
        .await
        .expect("get uptimes")
        .expect("uptimes document");
    assert_ne!(current.etag, stale.etag);

    let snapshot = performance_snapshot("2026-08-03T00:01:30Z", true, EPOCH);
    let error = write_uptimes(&directory, Some(stale), &snapshot)
        .await
        .expect_err("a write against a stale version must fail");
    assert!(
        error.to_string().contains("Conflict"),
        "the 412 must surface: {error}"
    );

    let after = directory
        .get::<UptimesDoc>(LIVE_UPTIMES)
        .await
        .expect("get uptimes")
        .expect("uptimes document");
    assert_eq!(
        after.etag, current.etag,
        "the losing run must not have written"
    );
    assert_eq!(
        after.body[VOTE_ACCOUNT].open.status,
        UptimeStatus::Up,
        "the losing run's sample must not be in the document"
    );
}

#[tokio::test]
async fn samples_extend_close_and_open_intervals() {
    let Some(store) = common::directory_store("uptimes-intervals").await else {
        return;
    };
    let directory = store.client();

    run_store_uptime(&directory, "interval-open", "2026-08-03T00:00:00Z", false).await;
    let opened = stored_uptimes(&directory).await;
    assert!(opened[VOTE_ACCOUNT].closed.is_empty());
    assert_eq!(opened[VOTE_ACCOUNT].open.status, UptimeStatus::Up);
    assert_eq!(
        opened[VOTE_ACCOUNT].open.start_at,
        at("2026-08-03T00:00:00Z")
    );
    assert_eq!(opened[VOTE_ACCOUNT].open.end_at, at("2026-08-03T00:01:00Z"));

    run_store_uptime(&directory, "interval-extend", "2026-08-03T00:01:00Z", false).await;
    let extended = stored_uptimes(&directory).await;
    assert!(
        extended[VOTE_ACCOUNT].closed.is_empty(),
        "an unchanged status extends the open interval"
    );
    assert_eq!(
        extended[VOTE_ACCOUNT].open.start_at,
        at("2026-08-03T00:00:00Z")
    );
    assert_eq!(
        extended[VOTE_ACCOUNT].open.end_at,
        at("2026-08-03T00:02:00Z")
    );

    run_store_uptime(&directory, "interval-down", "2026-08-03T00:02:00Z", true).await;
    let switched = stored_uptimes(&directory).await;
    assert_eq!(switched[VOTE_ACCOUNT].closed.len(), 1);
    assert_eq!(switched[VOTE_ACCOUNT].closed[0].status, UptimeStatus::Up);
    assert_eq!(
        switched[VOTE_ACCOUNT].closed[0].end_at,
        at("2026-08-03T00:02:00Z"),
        "a status change closes the interval at the sample"
    );
    assert_eq!(switched[VOTE_ACCOUNT].open.status, UptimeStatus::Down);
    assert_eq!(
        switched[VOTE_ACCOUNT].open.start_at,
        at("2026-08-03T00:02:00Z")
    );

    run_store_uptime(&directory, "interval-gap", "2026-08-03T00:12:00Z", true).await;
    let after_gap = stored_uptimes(&directory).await;
    assert_eq!(after_gap[VOTE_ACCOUNT].closed.len(), 2);
    assert_eq!(
        after_gap[VOTE_ACCOUNT].closed[1].end_at,
        at("2026-08-03T00:03:00Z"),
        "a gap past the window leaves the closed interval where it stood"
    );
    assert_eq!(
        after_gap[VOTE_ACCOUNT].open.start_at,
        at("2026-08-03T00:12:00Z")
    );

    run_store_uptime_in_epoch(
        &directory,
        "interval-next-epoch",
        "2026-08-03T00:13:00Z",
        true,
        EPOCH + 1,
    )
    .await;
    let next_epoch = stored_uptimes(&directory).await;
    assert_eq!(
        next_epoch[VOTE_ACCOUNT].closed.len(),
        3,
        "an epoch change closes the interval even when the status holds"
    );
    assert_eq!(next_epoch[VOTE_ACCOUNT].closed[2].epoch, EPOCH);
    assert_eq!(next_epoch[VOTE_ACCOUNT].open.epoch, EPOCH + 1);
}

#[tokio::test]
async fn a_sample_from_a_passed_epoch_is_refused() {
    let Some(store) = common::directory_store("uptimes-older-epoch").await else {
        return;
    };
    let directory = store.client();

    run_store_uptime(&directory, "older-seed", "2026-08-03T00:00:00Z", false).await;

    let snapshot = performance_snapshot("2026-08-03T00:01:00Z", false, EPOCH - 1);
    let stored = directory
        .get::<UptimesDoc>(LIVE_UPTIMES)
        .await
        .expect("get uptimes");
    let error = write_uptimes(&directory, stored, &snapshot)
        .await
        .expect_err("a sample from a passed epoch must be refused");
    assert!(error.to_string().contains("older than"), "{error}");
}

fn snapshot_of(
    created_at: &str,
    performance: ValidatorPerformance,
) -> ValidatorsPerformanceSnapshot {
    let mut validators = HashMap::new();
    validators.insert(VOTE_ACCOUNT.to_string(), performance);
    ValidatorsPerformanceSnapshot {
        epoch: EPOCH,
        epoch_slot: 1,
        transaction_count: 100,
        created_at: created_at.into(),
        slots_per_year: baseline_slots_per_year(),
        cluster_inflation: None,
        validators,
        rewards: None,
        nodes: Default::default(),
    }
}

fn write_snapshot(name: &str, snapshot: &ValidatorsPerformanceSnapshot) -> String {
    let path = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("{name}.yaml"));
    std::fs::write(&path, serde_yaml::to_string(snapshot).expect("yaml")).expect("snapshot file");
    path.to_str().expect("snapshot path").to_string()
}

async fn run_store_commissions(
    directory: &Directory,
    name: &str,
    created_at: &str,
    commission: u8,
) {
    let path = write_snapshot(
        name,
        &snapshot_of(
            created_at,
            ValidatorPerformance {
                commission,
                ..performance(false)
            },
        ),
    );
    store_commissions(
        StoreCommissionsParams::parse_from(["store", "--snapshot-file", &path]),
        directory,
    )
    .await
    .expect("store commissions");
    std::fs::remove_file(path).expect("remove snapshot");
}

#[tokio::test]
async fn store_commissions_records_a_change_only_when_the_commission_moves() {
    let Some(store) = common::directory_store("commissions").await else {
        return;
    };
    let directory = store.client();

    run_store_commissions(&directory, "commission-first", "2026-08-03T00:00:00Z", 7).await;
    run_store_commissions(&directory, "commission-same", "2026-08-03T00:01:00Z", 7).await;
    let unchanged: CommissionsDoc = directory
        .get(LIVE_COMMISSIONS)
        .await
        .expect("get commissions")
        .expect("commissions document")
        .body;
    assert_eq!(
        unchanged[VOTE_ACCOUNT].changes.len(),
        1,
        "an unchanged commission is not a change"
    );

    run_store_commissions(&directory, "commission-moved", "2026-08-03T00:02:00Z", 9).await;
    let moved: CommissionsDoc = directory
        .get(LIVE_COMMISSIONS)
        .await
        .expect("get commissions")
        .expect("commissions document")
        .body;
    assert_eq!(moved[VOTE_ACCOUNT].changes.len(), 2);
    assert_eq!(moved[VOTE_ACCOUNT].last.commission, 9);
}

async fn run_store_versions(
    directory: &Directory,
    name: &str,
    created_at: &str,
    client_id: Option<u16>,
    client_id_raw: Option<&str>,
) {
    let path = write_snapshot(
        name,
        &snapshot_of(
            created_at,
            ValidatorPerformance {
                client_id,
                client_id_raw: client_id_raw.map(str::to_string),
                ..performance(false)
            },
        ),
    );
    store_versions(
        StoreVersionsParams::parse_from(["store", "--snapshot-file", &path]),
        directory,
    )
    .await
    .expect("store versions");
    std::fs::remove_file(path).expect("remove snapshot");
}

async fn stored_version_changes(directory: &Directory) -> usize {
    let versions: VersionsDoc = directory
        .get(LIVE_VERSIONS)
        .await
        .expect("get versions")
        .expect("versions document")
        .body;
    versions[VOTE_ACCOUNT].changes.len()
}

#[tokio::test]
async fn store_versions_logs_a_change_only_when_the_resolved_client_changes() {
    let Some(store) = common::directory_store("versions").await else {
        return;
    };
    let directory = store.client();

    run_store_versions(
        &directory,
        "versions-first",
        "2026-08-03T00:00:00Z",
        Some(3),
        Some("Agave"),
    )
    .await;
    assert_eq!(
        stored_version_changes(&directory).await,
        1,
        "the first snapshot must be recorded"
    );

    run_store_versions(
        &directory,
        "versions-unchanged",
        "2026-08-03T00:01:00Z",
        Some(3),
        Some("Agave"),
    )
    .await;
    assert_eq!(
        stored_version_changes(&directory).await,
        1,
        "an unchanged snapshot must not add a change"
    );

    run_store_versions(
        &directory,
        "versions-rerendered",
        "2026-08-03T00:02:00Z",
        Some(3),
        Some("Unknown(3)"),
    )
    .await;
    assert_eq!(
        stored_version_changes(&directory).await,
        1,
        "the answering RPC rendering the same id differently is not a client change"
    );

    run_store_versions(
        &directory,
        "versions-switched",
        "2026-08-03T00:03:00Z",
        Some(1),
        Some("JitoLabs"),
    )
    .await;
    assert_eq!(
        stored_version_changes(&directory).await,
        2,
        "a different resolved client id must be recorded"
    );
}

#[tokio::test]
async fn store_cluster_info_appends_one_sample_per_run() {
    let Some(store) = common::directory_store("cluster-info").await else {
        return;
    };
    let directory = store.client();

    for (name, created_at) in [
        ("cluster-info-first", "2026-08-03T00:00:00Z"),
        ("cluster-info-second", "2026-08-03T00:01:00Z"),
    ] {
        let path = write_snapshot(name, &snapshot_of(created_at, performance(false)));
        store_cluster_info(
            StoreClusterInfoParams::parse_from(["store", "--snapshot-file", &path]),
            &directory,
        )
        .await
        .expect("store cluster info");
        std::fs::remove_file(path).expect("remove snapshot");
    }

    let cluster_info: ClusterInfoDoc = directory
        .get(LIVE_CLUSTER_INFO)
        .await
        .expect("get cluster info")
        .expect("cluster info document")
        .body;
    assert_eq!(cluster_info.epoch, EPOCH);
    assert_eq!(cluster_info.samples.len(), 2);
    assert_eq!(
        cluster_info.samples[1].created_at,
        at("2026-08-03T00:01:00Z")
    );
    assert_eq!(
        cluster_info.samples[0].slots_per_year,
        baseline_slots_per_year()
    );
}
