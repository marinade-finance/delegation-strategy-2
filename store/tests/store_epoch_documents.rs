use clap::Parser;
use collect::validators::{Snapshot, ValidatorDataCenter, ValidatorSnapshot};
use collect::validators_block_rewards::{ValidatorBlockRewards, ValidatorsBlockRewardsSnapshot};
use collect::validators_events::{ValidatorSettlement, ValidatorsEventsSnapshot};
use collect::validators_jito::{
    JitoAccountType, JitoSnapshot, MevTipDistributionValidatorSnapshot,
    PriorityFeeDistributionValidatorSnapshot,
};
use collect::validators_performance::ValidatorPerformance;
use rust_decimal::prelude::*;
use std::collections::HashMap;
use store::directory::Directory;
use store::docs::{
    block_reward_key, epoch_doc_path, BlockRewardsDoc, EventsDoc, MevDoc, PriorityFeeDoc,
    SnapshotDoc, BLOCK_REWARDS_DIR, EVENTS_DIR, MEV_DIR, PRIORITY_FEE_DIR, SNAPSHOT_DIR,
};
use store::validators::{store_validators, StoreValidatorsParams};
use store::validators_block_rewards::{store_block_rewards, StoreBlockRewardsParams};
use store::validators_events::{store_events, StoreEventsParams};
use store::validators_jito::{store_jito, StoreJitoParams};

mod common;

const EPOCH: u64 = 1000;
const VOTE_ACCOUNT: &str = "voteEpochDocuments";
const IDENTITY: &str = "identityEpochDocuments";
const CREATED_AT: &str = "2026-08-03T00:00:00Z";

// Tokyo. The longitude is outside the range any latitude could hold, so a swap
// cannot pass as a plausible coordinate.
const LON: f64 = 139.6917;
const LAT: f64 = 35.6895;

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

// Unlike `no_client()`, an actual observation: the node reported a client
// absent from client-ids.csv.
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

/// Fixtures are written under one directory and removed by the test that wrote them, so
/// the name carries a counter: two tests naming the same snapshot run in parallel threads
/// and would otherwise delete each other's file.
fn write_yaml<T: serde::Serialize>(name: &str, snapshot: &T) -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nth = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("{name}-{nth}.yaml"));
    std::fs::write(&path, serde_yaml::to_string(snapshot).expect("yaml")).expect("snapshot file");
    path.to_str().expect("snapshot path").to_string()
}

async fn run_store_validators(directory: &Directory, name: &str, client: &ClientFields) {
    let snapshot = Snapshot {
        epoch: EPOCH,
        created_at: CREATED_AT.into(),
        validators: vec![ValidatorSnapshot {
            data_center: Some(ValidatorDataCenter {
                coordinates: Some((LON, LAT)),
                ..Default::default()
            }),
            ..common::validator_snapshot(IDENTITY, VOTE_ACCOUNT, performance(client))
        }],
    };
    let path = write_yaml(name, &snapshot);
    store_validators(
        StoreValidatorsParams::parse_from(["store", "--snapshot-file", &path]),
        directory,
    )
    .await
    .expect("store validators");
    std::fs::remove_file(path).expect("remove snapshot");
}

async fn stored_snapshot(directory: &Directory) -> SnapshotDoc {
    directory
        .get(&epoch_doc_path(SNAPSHOT_DIR, EPOCH))
        .await
        .expect("get snapshot")
        .expect("snapshot document")
        .body
}

fn assert_client(stored: &SnapshotDoc, expected: &ClientFields, context: &str) {
    assert_eq!(stored.len(), 1, "one entry per vote account: {context}");
    let validator = &stored[VOTE_ACCOUNT];
    assert_eq!(
        validator.client_id.map(|id| id as u16),
        expected.client_id,
        "client_id: {context}"
    );
    assert_eq!(
        validator.client_id_raw, expected.client_id_raw,
        "client_id_raw: {context}"
    );
}

#[tokio::test]
async fn store_validators_round_trips_coordinates_without_swapping_them() {
    let Some(store) = common::directory_store("validators-coordinates").await else {
        return;
    };
    let directory = store.client();

    run_store_validators(&directory, "coordinates-create", &agave()).await;
    let created = stored_snapshot(&directory).await;
    assert_eq!(
        (
            created[VOTE_ACCOUNT].dc_coordinates_lat,
            created[VOTE_ACCOUNT].dc_coordinates_lon
        ),
        (Some(LAT), Some(LON)),
        "create path"
    );

    run_store_validators(&directory, "coordinates-merge", &agave()).await;
    let merged = stored_snapshot(&directory).await;
    assert_eq!(merged.len(), 1, "the second run must merge, not duplicate");
    assert_eq!(
        (
            merged[VOTE_ACCOUNT].dc_coordinates_lat,
            merged[VOTE_ACCOUNT].dc_coordinates_lon
        ),
        (Some(LAT), Some(LON)),
        "merge path"
    );
}

#[tokio::test]
async fn store_validators_keeps_the_last_known_client_when_gossip_reports_none() {
    let Some(store) = common::directory_store("validators-gossip-gap").await else {
        return;
    };
    let directory = store.client();

    run_store_validators(&directory, "gossip-gap-seed", &agave()).await;
    run_store_validators(&directory, "gossip-gap-empty", &no_client()).await;

    assert_client(
        &stored_snapshot(&directory).await,
        &agave(),
        "a snapshot with no gossip data must not erase the stored client",
    );
}

// A switch to an unregistered client must not read as a transient gossip gap:
// the old classification has to go.
#[tokio::test]
async fn store_validators_clears_a_stale_classification_when_the_client_becomes_unrecognized() {
    let Some(store) = common::directory_store("validators-unrecognized").await else {
        return;
    };
    let directory = store.client();

    run_store_validators(&directory, "unrecognized-seed", &agave()).await;
    run_store_validators(&directory, "unrecognized-switch", &unrecognized()).await;

    assert_client(
        &stored_snapshot(&directory).await,
        &unrecognized(),
        "a validator switching to an unregistered client keeps no old classification",
    );
}

#[tokio::test]
async fn store_validators_keeps_the_version_a_snapshot_cannot_see() {
    let Some(store) = common::directory_store("validators-version").await else {
        return;
    };
    let directory = store.client();

    run_store_validators(&directory, "version-seed", &agave()).await;
    let mut blind = no_client();
    blind.client_id_raw = None;
    let path = write_yaml(
        "version-blind",
        &Snapshot {
            epoch: EPOCH,
            created_at: CREATED_AT.into(),
            validators: vec![common::validator_snapshot(
                IDENTITY,
                VOTE_ACCOUNT,
                ValidatorPerformance {
                    version: None,
                    ..performance(&blind)
                },
            )],
        },
    );
    store_validators(
        StoreValidatorsParams::parse_from(["store", "--snapshot-file", &path]),
        &directory,
    )
    .await
    .expect("store validators");
    std::fs::remove_file(path).expect("remove snapshot");

    let stored = stored_snapshot(&directory).await;
    assert_eq!(
        stored[VOTE_ACCOUNT].version,
        Some("2.0.0".to_string()),
        "a snapshot without a version must not erase the stored one"
    );
}

async fn run_store_jito(directory: &Directory, name: &str, account_type: JitoAccountType) {
    let mut validators = HashMap::new();
    let commission = if name.ends_with("resample") { 900 } else { 500 };
    match &account_type {
        JitoAccountType::MevTipDistribution => {
            validators.insert(
                VOTE_ACCOUNT.to_string(),
                collect::validators_jito::ValidatorSnapshot::MevTipDistribution(
                    MevTipDistributionValidatorSnapshot {
                        vote_account: VOTE_ACCOUNT.into(),
                        mev_commission: commission,
                        epoch: EPOCH,
                        total_epoch_rewards: Some(10),
                        claimed_epoch_rewards: Some(5),
                        total_epoch_claimants: Some(2),
                        epoch_active_claimants: Some(1),
                    },
                ),
            );
        }
        JitoAccountType::PriorityFeeDistribution => {
            validators.insert(
                VOTE_ACCOUNT.to_string(),
                collect::validators_jito::ValidatorSnapshot::PriorityFeeDistribution(
                    PriorityFeeDistributionValidatorSnapshot {
                        vote_account: VOTE_ACCOUNT.into(),
                        priority_commission: commission,
                        total_lamports_transferred: 42,
                        merkle_root_upload_authority: "authority".into(),
                        epoch: EPOCH,
                        total_epoch_rewards: Some(10),
                        claimed_epoch_rewards: Some(5),
                        total_epoch_claimants: Some(2),
                        epoch_active_claimants: Some(1),
                    },
                ),
            );
        }
    }
    let path = write_yaml(
        name,
        &JitoSnapshot {
            epoch: EPOCH,
            version: 1,
            account_type: account_type.clone(),
            loaded_at_epoch: EPOCH,
            loaded_at_slot_index: 7,
            created_at: CREATED_AT.into(),
            validators,
        },
    );
    store_jito(
        StoreJitoParams::parse_from(["store", "--snapshot-file", &path]),
        directory,
        account_type,
    )
    .await
    .expect("store jito");
    std::fs::remove_file(path).expect("remove snapshot");
}

#[tokio::test]
async fn store_jito_keeps_one_latest_observation_per_vote_account() {
    let Some(store) = common::directory_store("jito").await else {
        return;
    };
    let directory = store.client();

    run_store_jito(
        &directory,
        "mev-create",
        JitoAccountType::MevTipDistribution,
    )
    .await;
    run_store_jito(
        &directory,
        "mev-resample",
        JitoAccountType::MevTipDistribution,
    )
    .await;
    let mev: MevDoc = directory
        .get(&epoch_doc_path(MEV_DIR, EPOCH))
        .await
        .expect("get mev")
        .expect("mev document")
        .body;
    assert_eq!(mev.len(), 1, "a resample must replace, not duplicate");
    assert_eq!(mev[VOTE_ACCOUNT].mev_commission, 900);
    assert_eq!(mev[VOTE_ACCOUNT].epoch_slot, Decimal::from(7));

    run_store_jito(
        &directory,
        "priority-create",
        JitoAccountType::PriorityFeeDistribution,
    )
    .await;
    run_store_jito(
        &directory,
        "priority-resample",
        JitoAccountType::PriorityFeeDistribution,
    )
    .await;
    let priority_fees: PriorityFeeDoc = directory
        .get(&epoch_doc_path(PRIORITY_FEE_DIR, EPOCH))
        .await
        .expect("get priority fees")
        .expect("priority fee document")
        .body;
    assert_eq!(priority_fees.len(), 1);
    assert_eq!(priority_fees[VOTE_ACCOUNT].priority_commission, 900);
    assert_eq!(
        priority_fees[VOTE_ACCOUNT].total_lamports_transferred,
        Decimal::from(42)
    );
}

async fn run_store_events(directory: &Directory, name: &str, amount: i64, epoch: u64) {
    let path = write_yaml(
        name,
        &ValidatorsEventsSnapshot {
            version: 1,
            from_epoch: epoch,
            loaded_at_epoch: epoch,
            loaded_at_slot_index: 7,
            created_at: CREATED_AT.into(),
            events: vec![
                ValidatorSettlement {
                    epoch,
                    vote_account: VOTE_ACCOUNT.into(),
                    reason: "Bidding".into(),
                    meta: "{\"funder\":\"ValidatorBond\"}".into(),
                    amount,
                },
                ValidatorSettlement {
                    epoch,
                    vote_account: VOTE_ACCOUNT.into(),
                    reason: "ProtectedEvent".into(),
                    meta: "{\"funder\":\"Marinade\"}".into(),
                    amount,
                },
            ],
        },
    );
    store_events(
        StoreEventsParams::parse_from(["store", "--snapshot-file", &path]),
        directory,
    )
    .await
    .expect("store events");
    std::fs::remove_file(path).expect("remove snapshot");
}

#[tokio::test]
async fn store_events_merges_on_reason_and_meta() {
    let Some(store) = common::directory_store("events").await else {
        return;
    };
    let directory = store.client();

    run_store_events(&directory, "events-create", 100, EPOCH).await;
    run_store_events(&directory, "events-merge", 250, EPOCH).await;
    run_store_events(&directory, "events-other-epoch", 300, EPOCH - 1).await;

    let events: EventsDoc = directory
        .get(&epoch_doc_path(EVENTS_DIR, EPOCH))
        .await
        .expect("get events")
        .expect("events document")
        .body;
    let stored = &events[VOTE_ACCOUNT];
    assert_eq!(stored.len(), 2, "a re-run must merge on (reason, meta)");
    assert!(stored
        .iter()
        .all(|event| event.amount == Decimal::from(250)));

    let previous: EventsDoc = directory
        .get(&epoch_doc_path(EVENTS_DIR, EPOCH - 1))
        .await
        .expect("get events")
        .expect("events document")
        .body;
    assert_eq!(
        previous[VOTE_ACCOUNT].len(),
        2,
        "each epoch its own document"
    );
}

async fn run_store_block_rewards(directory: &Directory, name: &str, amount: u64) {
    let path = write_yaml(
        name,
        &ValidatorsBlockRewardsSnapshot {
            version: 1,
            epoch: EPOCH,
            loaded_at_epoch: EPOCH,
            loaded_at_slot_index: 7,
            created_at: CREATED_AT.into(),
            block_rewards: vec![ValidatorBlockRewards {
                identity_account: IDENTITY.into(),
                vote_account: VOTE_ACCOUNT.into(),
                node_account: IDENTITY.into(),
                authorized_voter: VOTE_ACCOUNT.into(),
                amount,
            }],
        },
    );
    store_block_rewards(
        StoreBlockRewardsParams::parse_from(["store", "--snapshot-file", &path]),
        directory,
    )
    .await
    .expect("store block rewards");
    std::fs::remove_file(path).expect("remove snapshot");
}

#[tokio::test]
async fn store_block_rewards_merges_on_identity_and_vote_account() {
    let Some(store) = common::directory_store("block-rewards").await else {
        return;
    };
    let directory = store.client();

    run_store_block_rewards(&directory, "block-rewards-create", 100).await;
    run_store_block_rewards(&directory, "block-rewards-merge", 250).await;

    let rewards: BlockRewardsDoc = directory
        .get(&epoch_doc_path(BLOCK_REWARDS_DIR, EPOCH))
        .await
        .expect("get block rewards")
        .expect("block rewards document")
        .body;
    assert_eq!(rewards.len(), 1, "a re-run must merge, not duplicate");
    let stored = &rewards[&block_reward_key(IDENTITY, VOTE_ACCOUNT)];
    assert_eq!(stored.amount, Decimal::from(250));
    assert_eq!(
        stored.created_at, stored.updated_at,
        "both runs carry the same snapshot time"
    );
}
