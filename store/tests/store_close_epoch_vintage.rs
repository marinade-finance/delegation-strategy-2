use clap::Parser;
use collect::slot_params::baseline_slots_per_year;
use collect::validators::Snapshot;
use collect::validators_performance::{
    ClusterInflation, ValidatorPerformance, ValidatorRewards, ValidatorsPerformanceSnapshot,
};
use std::collections::HashMap;
use store::close_epoch::{close_epoch, CloseEpochParams};
use store::directory::{Directory, Precondition};
use store::docs::{
    epoch_doc_path, ClusterInfoDoc, ClusterInfoSample, CommissionsDoc, SnapshotDoc, UptimesDoc,
    VersionsDoc, LIVE_CLUSTER_INFO, LIVE_COMMISSIONS, LIVE_UPTIMES, LIVE_VERSIONS, SNAPSHOT_DIR,
};
use store::dto::{COMMISSION_EFFECTIVE_SOURCE_REWARD_ROW, COMMISSION_EFFECTIVE_SOURCE_VOTE_STATE};
use store::validators::{store_validators, StoreValidatorsParams};

mod common;

const EPOCH: u64 = 1045;
const CHANGED_LATE: &str = "voteChangedAfterTheVintage";
const NEW_LAST_EPOCH: &str = "voteFirstSampledLastEpoch";
const NEW_THIS_EPOCH: &str = "voteFirstSampledThisEpoch";
const UNPARSED_AT_VINTAGE: &str = "voteUnparsedAtTheVintage";
const OUTSIDE: &str = "voteOutsideTheSnapshot";
const REWARD_ROW: &str = "voteWithARewardRow";

async fn store_samples(directory: &Directory, epoch: u64, samples: &[(&str, Option<u16>)]) {
    let snapshot = Snapshot {
        epoch,
        created_at: "2026-07-31T00:00:00Z".into(),
        validators: samples
            .iter()
            .map(|(vote_account, bps)| {
                let mut validator = common::validator_snapshot(
                    &format!("identity-{vote_account}"),
                    vote_account,
                    common::validator_performance(),
                );
                validator.inflation_rewards_commission_bps = *bps;
                validator.inflation_rewards_commission_bps_is_v4 = bps.map(|_| true);
                validator
            })
            .collect(),
    };
    let path = common::write_yaml(&format!("vintage-{epoch}"), &snapshot);
    store_validators(
        StoreValidatorsParams::parse_from(["store", "--snapshot-file", &path]),
        directory,
    )
    .await
    .expect("store validators");
    std::fs::remove_file(path).expect("remove snapshot");
}

/// The accumulators close-epoch seals, holding only the two cluster-info samples
/// that bound the epoch.
async fn seed_live_streams(directory: &Directory) {
    let sample = |epoch_slot, created_at: &str| ClusterInfoSample {
        epoch: EPOCH,
        epoch_slot,
        transaction_count: 0,
        created_at: created_at.parse().expect("timestamp"),
        slots_per_year: baseline_slots_per_year(),
    };
    directory
        .put(
            LIVE_CLUSTER_INFO,
            &ClusterInfoDoc {
                epoch: EPOCH,
                samples: vec![
                    sample(0, "2026-09-16T00:00:00Z"),
                    sample(432_000, "2026-09-16T23:00:00Z"),
                ],
            },
            Precondition::Create,
        )
        .await
        .expect("put cluster info");
    directory
        .put(LIVE_UPTIMES, &UptimesDoc::new(), Precondition::Create)
        .await
        .expect("put uptimes");
    directory
        .put(
            LIVE_COMMISSIONS,
            &CommissionsDoc::new(),
            Precondition::Create,
        )
        .await
        .expect("put commissions");
    directory
        .put(LIVE_VERSIONS, &VersionsDoc::new(), Precondition::Create)
        .await
        .expect("put versions");
}

fn performance_snapshot(
    listed: &[&str],
    reward_rows: &[(&str, u8)],
) -> ValidatorsPerformanceSnapshot {
    let validators: HashMap<String, ValidatorPerformance> = listed
        .iter()
        .map(|vote_account| (vote_account.to_string(), common::validator_performance()))
        .collect();
    let rewards = listed
        .iter()
        .map(|vote_account| {
            let commission_effective = reward_rows
                .iter()
                .find(|(reward_vote, _)| reward_vote == vote_account)
                .map(|(_, commission)| *commission);
            (
                vote_account.to_string(),
                ValidatorRewards {
                    commission_effective,
                },
            )
        })
        .collect();
    ValidatorsPerformanceSnapshot {
        epoch: EPOCH,
        epoch_slot: 432_000,
        transaction_count: 0,
        created_at: "2026-09-16T23:00:00Z".into(),
        slots_per_year: baseline_slots_per_year(),
        cluster_inflation: Some(ClusterInflation {
            sol_total_supply: 0,
            inflation: 0f64,
            inflation_taper: 0f64,
        }),
        validators,
        nodes: Default::default(),
        rewards: Some(rewards),
    }
}

async fn read_effective(
    directory: &Directory,
    vote_account: &str,
) -> (Option<i32>, Option<i32>, Option<String>) {
    let snapshot: SnapshotDoc = directory
        .get(&epoch_doc_path(SNAPSHOT_DIR, EPOCH))
        .await
        .expect("get snapshot")
        .expect("snapshot document")
        .body;
    let validator = &snapshot[vote_account];
    (
        validator.commission_effective,
        validator.commission_effective_bps,
        validator.commission_effective_source.clone(),
    )
}

#[tokio::test]
async fn close_epoch_prices_the_vote_state_agave_applied_to_the_epoch() {
    let Some(store) = common::directory_store("close-epoch-vintage").await else {
        return;
    };
    let directory = store.client();

    store_samples(
        &directory,
        EPOCH - 2,
        &[
            (CHANGED_LATE, Some(500)),
            (UNPARSED_AT_VINTAGE, None),
            (OUTSIDE, Some(300)),
            (REWARD_ROW, Some(500)),
        ],
    )
    .await;
    store_samples(
        &directory,
        EPOCH - 1,
        &[
            (CHANGED_LATE, Some(800)),
            (NEW_LAST_EPOCH, Some(800)),
            (UNPARSED_AT_VINTAGE, Some(800)),
            (OUTSIDE, Some(500)),
            (REWARD_ROW, Some(800)),
        ],
    )
    .await;
    store_samples(
        &directory,
        EPOCH,
        &[
            (CHANGED_LATE, Some(900)),
            (NEW_LAST_EPOCH, Some(900)),
            (NEW_THIS_EPOCH, Some(900)),
            (UNPARSED_AT_VINTAGE, Some(900)),
            (OUTSIDE, Some(700)),
            (REWARD_ROW, Some(900)),
        ],
    )
    .await;
    seed_live_streams(&directory).await;

    let listed = [
        CHANGED_LATE,
        NEW_LAST_EPOCH,
        NEW_THIS_EPOCH,
        UNPARSED_AT_VINTAGE,
        REWARD_ROW,
    ];
    let path = common::write_yaml(
        "close-epoch-vintage",
        &performance_snapshot(&listed, &[(REWARD_ROW, 6)]),
    );
    close_epoch(
        CloseEpochParams::parse_from(["store", "--snapshot-file", &path]),
        &directory,
    )
    .await
    .expect("close epoch");
    std::fs::remove_file(path).expect("remove snapshot");

    let vote_state = Some(COMMISSION_EFFECTIVE_SOURCE_VOTE_STATE.to_string());
    assert_eq!(
        read_effective(&directory, CHANGED_LATE).await,
        (Some(5), Some(500), vote_state.clone()),
        "epoch_stakes(E) froze at the close of E-2, so a change during E-1 or E must not reach E"
    );
    assert_eq!(
        read_effective(&directory, NEW_LAST_EPOCH).await,
        (Some(8), Some(800), vote_state.clone()),
        "without an E-2 sample agave falls back to epoch_stakes(E+1), the close of E-1"
    );
    assert_eq!(
        read_effective(&directory, NEW_THIS_EPOCH).await,
        (Some(9), Some(900), vote_state.clone()),
        "with neither snapshot agave reads the live state, the epoch's own last sample"
    );
    assert_eq!(
        read_effective(&directory, UNPARSED_AT_VINTAGE).await,
        (Some(7), None, vote_state.clone()),
        "an unparsed E-2 vote state keeps its vintage through the advertised percent, not E-1's bps"
    );
    assert_eq!(
        read_effective(&directory, OUTSIDE).await,
        (Some(3), Some(300), vote_state.clone()),
        "a validator the snapshot never listed resolves at the same vintage, bps included"
    );
    assert_eq!(
        read_effective(&directory, REWARD_ROW).await,
        (
            Some(6),
            None,
            Some(COMMISSION_EFFECTIVE_SOURCE_REWARD_ROW.to_string())
        ),
        "a reward row still wins, and its whole percent carries no bps"
    );
}
