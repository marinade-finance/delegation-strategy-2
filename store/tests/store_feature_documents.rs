use chrono::{DateTime, Utc};
use clap::Parser;
use collect::releases::{ReleaseEntry, ReleaseSource, ReleasesSnapshot};
use collect::slot_params::baseline_slots_per_year;
use collect::take_rates::{ValidatorEpochRewards, ValidatorRewardsSnapshot};
use collect::validator_version::ValidatorVersion;
use collect::validators::Snapshot;
use collect::validators_jito::{
    JitoAccountType, JitoSnapshot, MevTipDistributionValidatorSnapshot,
    PriorityFeeDistributionValidatorSnapshot,
};
use collect::validators_performance::{
    ClusterInflation, ValidatorRewards, ValidatorsPerformanceSnapshot,
};
use collect::validators_sandwiches::{ValidatorSandwich, ValidatorsSandwichesSnapshot};
use rust_decimal::Decimal;
use std::collections::HashMap;
use store::close_epoch::{close_epoch, CloseEpochParams};
use store::cluster_info::{store_cluster_info, StoreClusterInfoParams};
use store::commissions::{store_commissions, StoreCommissionsParams};
use store::directory::Directory;
use store::docs::{
    epoch_doc_path, EpochDoc, MevEntry, PriorityFeeEntry, ReleasesDoc, SandwichesDoc,
    ValidatorRewardsDoc, ValidatorRewardsEntry, RELEASES_PATH, SANDWICHES_DIR,
    VALIDATOR_REWARDS_DIR,
};
use store::releases::{
    load_feature_gate_floors, load_releases, load_sfdp_floors, store_releases, StoreReleasesParams,
};
use store::take_rates::{
    get_take_rate_series, load_epoch_reward_mix, load_epoch_take_rates, store_take_rates,
    StoreTakeRatesParams,
};
use store::uptime::{store_uptime, StoreUptimeParams};
use store::utils::RewardMixShares;
use store::validators::{store_validators, StoreValidatorsParams};
use store::validators_jito::{store_jito, StoreJitoParams};
use store::validators_sandwiches::{
    load_validator_sandwiches, store_sandwiches, StoreSandwichesParams,
};
use store::versions::{store_versions, StoreVersionsParams};
use store::warehouse::Warehouse;

mod common;

const EPOCH: u64 = 1000;
const VOTE_ACCOUNT: &str = "voteFeature";

fn at(moment: &str) -> DateTime<Utc> {
    moment.parse().expect("timestamp")
}

fn epoch_record(epoch: u64, start_at: &str, end_at: &str) -> EpochDoc {
    EpochDoc {
        epoch,
        start_at: at(start_at),
        end_at: at(end_at),
        transaction_count: 0,
        supply: Decimal::ZERO,
        inflation: 0f64,
        inflation_taper: 0f64,
        slots_per_year: baseline_slots_per_year(),
    }
}

/// Three sealed epochs, two days each from 2026-01-01.
fn warehouse_with_epochs() -> Warehouse {
    let mut warehouse = Warehouse::default();
    for (epoch, start_at, end_at) in [
        (EPOCH, "2026-01-01T00:00:00Z", "2026-01-03T00:00:00Z"),
        (EPOCH + 1, "2026-01-03T00:00:00Z", "2026-01-05T00:00:00Z"),
        (EPOCH + 2, "2026-01-05T00:00:00Z", "2026-01-07T00:00:00Z"),
    ] {
        warehouse
            .epochs
            .insert(epoch, epoch_record(epoch, start_at, end_at));
    }
    warehouse
}

fn release(lineage: &str, version: &str, source: ReleaseSource) -> ReleaseEntry {
    ReleaseEntry {
        client_lineage: lineage.into(),
        client_version: version.parse::<ValidatorVersion>().expect("version"),
        released_at: None,
        sfdp_floor_epoch: None,
        feature_gate_epoch: None,
        release_url: None,
        source,
    }
}

fn shipped(version: &str, released_at: &str) -> ReleaseEntry {
    ReleaseEntry {
        released_at: Some(at(released_at)),
        release_url: Some(format!("https://example.invalid/{version}")),
        ..release("agave", version, ReleaseSource::Github)
    }
}

fn sfdp_floor(version: &str, epoch: u64) -> ReleaseEntry {
    ReleaseEntry {
        sfdp_floor_epoch: Some(epoch),
        ..release("agave", version, ReleaseSource::Sfdp)
    }
}

fn gate_floor(lineage: &str, version: &str, epoch: u64) -> ReleaseEntry {
    ReleaseEntry {
        feature_gate_epoch: Some(epoch),
        ..release(lineage, version, ReleaseSource::FeatureGates)
    }
}

async fn run_store_releases(directory: &Directory, name: &str, releases: Vec<ReleaseEntry>) {
    let snapshot = ReleasesSnapshot {
        version: 1,
        created_at: "2026-01-06T00:00:00Z".into(),
        releases,
    };
    let path = common::write_yaml(name, &snapshot);
    store_releases(
        StoreReleasesParams::parse_from(["store", "--snapshot-file", &path]),
        directory,
    )
    .await
    .expect("store releases");
    std::fs::remove_file(path).expect("remove snapshot");
}

async fn stored_releases(directory: &Directory) -> ReleasesDoc {
    directory
        .get(RELEASES_PATH)
        .await
        .expect("get releases")
        .expect("releases document")
        .body
}

#[tokio::test]
async fn the_three_release_sources_fill_one_entry_without_blanking_each_other() {
    let Some(store) = common::directory_store("releases-sources").await else {
        return;
    };
    let directory = store.client();

    run_store_releases(
        &directory,
        "releases-github",
        vec![shipped("4.2.2", "2026-01-04T12:00:00Z")],
    )
    .await;
    run_store_releases(&directory, "releases-sfdp", vec![sfdp_floor("4.2.2", 1001)]).await;
    run_store_releases(
        &directory,
        "releases-gates",
        vec![gate_floor("agave", "4.2.2", 1002)],
    )
    .await;

    let entry = &stored_releases(&directory).await["agave"]["4.2.2"];
    assert_eq!(entry.released_at, Some(at("2026-01-04T12:00:00Z")));
    assert_eq!(
        entry.release_url.as_deref(),
        Some("https://example.invalid/4.2.2")
    );
    assert_eq!(entry.sfdp_floor_epoch, Some(1001));
    assert_eq!(entry.feature_gate_epoch, Some(1002));

    let mut warehouse = warehouse_with_epochs();
    warehouse.releases = stored_releases(&directory).await;
    let releases = load_releases(&warehouse, None, None);
    let shipped = releases
        .iter()
        .find(|release| release.client_version == "4.2.2")
        .expect("the shipped release is listed");
    assert_eq!(
        shipped.available_epoch,
        Some(EPOCH + 1),
        "a release lands in the sealed epoch its timestamp falls in"
    );
    assert_eq!(
        load_sfdp_floors(&warehouse, Some("agave"), None)
            .iter()
            .map(|floor| (floor.client_version.clone(), floor.effective_epoch))
            .collect::<Vec<_>>(),
        vec![("4.2.2".to_string(), 1001)]
    );
}

#[tokio::test]
async fn the_feature_gate_floor_history_is_seeded_and_a_derived_floor_overwrites_it() {
    let Some(store) = common::directory_store("releases-seed").await else {
        return;
    };
    let directory = store.client();

    run_store_releases(&directory, "releases-empty", vec![]).await;
    let seeded = stored_releases(&directory).await;
    assert_eq!(seeded["agave"]["3.1.0"].feature_gate_epoch, Some(946));
    assert_eq!(
        seeded["firedancer"]["26.8.0"].feature_gate_epoch,
        Some(1026)
    );
    assert_eq!(
        seeded
            .values()
            .map(|versions| versions.len())
            .sum::<usize>(),
        14,
        "every seeded floor and nothing else"
    );

    run_store_releases(
        &directory,
        "releases-derived",
        vec![gate_floor("agave", "3.1.0", 950)],
    )
    .await;
    assert_eq!(
        stored_releases(&directory).await["agave"]["3.1.0"].feature_gate_epoch,
        Some(950),
        "the tracker's own reading replaces the seed, and a rerun does not restore it"
    );

    let mut warehouse = warehouse_with_epochs();
    warehouse.releases = stored_releases(&directory).await;
    let floors = load_feature_gate_floors(&warehouse, Some("frankendancer"), Some(992));
    assert_eq!(
        floors
            .iter()
            .map(|floor| floor.effective_epoch)
            .collect::<Vec<_>>(),
        vec![1019, 999, 992],
        "one lineage, bounded below by since_epoch, newest floor first"
    );
}

#[test]
fn since_epoch_bounds_the_releases_by_the_moment_the_epoch_started() {
    let mut warehouse = warehouse_with_epochs();
    let snapshot = ReleasesSnapshot {
        version: 1,
        created_at: "2026-01-06T00:00:00Z".into(),
        releases: vec![
            shipped("4.2.0", "2026-01-02T00:00:00Z"),
            shipped("4.2.1", "2026-01-04T00:00:00Z"),
            shipped("4.2.2", "2026-01-08T00:00:00Z"),
        ],
    };
    store::releases::apply_releases(
        &mut warehouse.releases,
        &snapshot,
        at("2026-01-06T00:00:00Z"),
    );

    let versions = |since_epoch| {
        load_releases(&warehouse, None, since_epoch)
            .into_iter()
            .map(|release| release.client_version)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        versions(None),
        vec!["4.2.2", "4.2.1", "4.2.0"],
        "newest first"
    );
    assert_eq!(
        versions(Some(EPOCH + 1)),
        vec!["4.2.2", "4.2.1"],
        "from the start of the epoch asked for"
    );
    assert_eq!(
        versions(Some(EPOCH)),
        vec!["4.2.2", "4.2.1", "4.2.0"],
        "an epoch at or under the oldest held takes everything"
    );
    assert_eq!(
        versions(Some(EPOCH + 5)),
        vec!["4.2.2"],
        "an epoch newer than the history, the running one, takes what followed the last close"
    );
    assert_eq!(
        load_releases(&warehouse, None, None)
            .iter()
            .find(|release| release.client_version == "4.2.2")
            .and_then(|release| release.available_epoch),
        None,
        "a release the sealed epochs do not reach is not placed"
    );
}

fn rewards(epoch: u64, vote_account: &str, validator: u64, total: u64) -> ValidatorEpochRewards {
    ValidatorEpochRewards {
        epoch,
        vote_account: vote_account.into(),
        validator_rewards: validator,
        total_rewards: total,
        inflation_rewards: total * 9 / 10,
        mev_rewards: total / 20,
        block_rewards: total - total * 9 / 10 - total / 20,
    }
}

async fn run_store_take_rates(
    directory: &Directory,
    name: &str,
    created_at: &str,
    rewards: Vec<ValidatorEpochRewards>,
) {
    let snapshot = ValidatorRewardsSnapshot {
        version: 1,
        from_epoch: EPOCH,
        loaded_at_epoch: EPOCH + 2,
        loaded_at_slot_index: 10,
        created_at: created_at.into(),
        rewards,
    };
    let path = common::write_yaml(name, &snapshot);
    store_take_rates(
        StoreTakeRatesParams::parse_from(["store", "--snapshot-file", &path]),
        directory,
    )
    .await
    .expect("store take rates");
    std::fs::remove_file(path).expect("remove snapshot");
}

#[tokio::test]
async fn take_rates_land_per_epoch_and_a_rewrite_keeps_when_they_were_first_seen() {
    let Some(store) = common::directory_store("take-rates").await else {
        return;
    };
    let directory = store.client();

    run_store_take_rates(
        &directory,
        "take-rates-first",
        "2026-01-05T00:00:00Z",
        vec![
            rewards(EPOCH, VOTE_ACCOUNT, 10, 100),
            rewards(EPOCH + 1, VOTE_ACCOUNT, 30, 100),
            rewards(EPOCH + 1, "voteIdle", 0, 0),
        ],
    )
    .await;
    run_store_take_rates(
        &directory,
        "take-rates-second",
        "2026-01-07T00:00:00Z",
        vec![rewards(EPOCH + 1, VOTE_ACCOUNT, 40, 100)],
    )
    .await;

    let first: ValidatorRewardsDoc = directory
        .get(&epoch_doc_path(VALIDATOR_REWARDS_DIR, EPOCH))
        .await
        .expect("get rewards")
        .expect("rewards document")
        .body;
    assert_eq!(first[VOTE_ACCOUNT].take_rate, 0.1);
    let second: ValidatorRewardsDoc = directory
        .get(&epoch_doc_path(VALIDATOR_REWARDS_DIR, EPOCH + 1))
        .await
        .expect("get rewards")
        .expect("rewards document")
        .body;
    assert_eq!(second[VOTE_ACCOUNT].take_rate, 0.4, "the rewrite wins");
    assert_eq!(
        (
            second[VOTE_ACCOUNT].created_at,
            second[VOTE_ACCOUNT].updated_at
        ),
        (at("2026-01-05T00:00:00Z"), at("2026-01-07T00:00:00Z"))
    );
    assert!(
        !second.contains_key("voteIdle"),
        "a validator that earned nothing has no rate to store"
    );
}

#[test]
fn the_take_rate_series_weights_each_epoch_by_its_own_commissions() {
    let mut warehouse = warehouse_with_epochs();
    for (epoch, validator_rewards) in [(EPOCH, 10u64), (EPOCH + 1, 30), (EPOCH + 2, 50)] {
        let mut doc = ValidatorRewardsDoc::new();
        let row = rewards(epoch, VOTE_ACCOUNT, validator_rewards, 100);
        doc.insert(
            VOTE_ACCOUNT.into(),
            store::docs::ValidatorRewardsEntry {
                validator_rewards: Decimal::from(row.validator_rewards),
                total_rewards: Decimal::from(row.total_rewards),
                inflation_rewards: Decimal::from(row.inflation_rewards),
                mev_rewards: Decimal::from(row.mev_rewards),
                block_rewards: Decimal::from(row.block_rewards),
                take_rate: validator_rewards as f64 / 100.0,
                created_at: at("2026-01-07T00:00:00Z"),
                updated_at: at("2026-01-07T00:00:00Z"),
            },
        );
        warehouse.validator_rewards.insert(epoch, doc);
    }
    // The epoch still running paid block rewards only.
    let mut accruing = ValidatorRewardsDoc::new();
    accruing.insert(
        VOTE_ACCOUNT.into(),
        store::docs::ValidatorRewardsEntry {
            validator_rewards: Decimal::from(5),
            total_rewards: Decimal::from(5),
            inflation_rewards: Decimal::ZERO,
            mev_rewards: Decimal::ZERO,
            block_rewards: Decimal::from(5),
            take_rate: 1.0,
            created_at: at("2026-01-07T00:00:00Z"),
            updated_at: at("2026-01-07T00:00:00Z"),
        },
    );
    warehouse.validator_rewards.insert(EPOCH + 3, accruing);

    for (epoch, max_observed, advertised) in [
        (EPOCH, Some(5), Some(5)),
        (EPOCH + 1, Some(100), Some(0)),
        (EPOCH + 2, None, Some(10)),
    ] {
        let mut validator = common::validator(VOTE_ACCOUNT, epoch);
        validator.commission_max_observed = max_observed;
        validator.commission_advertised = advertised;
        warehouse
            .snapshots
            .entry(epoch)
            .or_default()
            .insert(VOTE_ACCOUNT.into(), validator);
    }
    warehouse.mev.entry(EPOCH + 2).or_default().insert(
        VOTE_ACCOUNT.into(),
        MevEntry {
            vote_account: VOTE_ACCOUNT.into(),
            mev_commission: 1_000,
            total_epoch_rewards: None,
            claimed_epoch_rewards: None,
            total_epoch_claimants: None,
            epoch_active_claimants: None,
            epoch_slot: Decimal::ONE,
            epoch: Decimal::from(EPOCH + 2),
            created_at: at("2026-01-06T00:00:00Z"),
        },
    );

    let mix = load_epoch_reward_mix(&warehouse);
    assert_eq!(
        mix.keys()
            .copied()
            .collect::<std::collections::BTreeSet<_>>(),
        [EPOCH, EPOCH + 1, EPOCH + 2].into_iter().collect(),
        "the epoch that paid no inflation yet has no mix"
    );
    let shares: RewardMixShares = mix[&EPOCH];
    assert!((shares.inflation - 0.9).abs() < 1e-12);
    assert!((shares.mev - 0.05).abs() < 1e-12);
    assert!((shares.block - 0.05).abs() < 1e-12);

    let rates = load_epoch_take_rates(&warehouse, 90);
    let series = get_take_rate_series(&warehouse, VOTE_ACCOUNT, None, &rates);
    assert_eq!(
        series.iter().map(|record| record.epoch).collect::<Vec<_>>(),
        vec![EPOCH, EPOCH + 1, EPOCH + 2, EPOCH + 3],
        "oldest first, over everything held"
    );
    let approx = |actual: Option<f64>, expected: f64, context: &str| {
        let actual = actual.unwrap_or_else(|| panic!("{context}: expected a rate"));
        assert!(
            (actual - expected).abs() < 1e-12,
            "{context}: expected {expected}, got {actual}"
        );
    };
    let no_jito = shares.inflation + shares.block;
    approx(
        series[0].expected_take_rate,
        (0.05 * shares.inflation + shares.block) / no_jito,
        "a 5% validator with no Jito account",
    );
    approx(
        series[1].expected_take_rate,
        (shares.inflation + shares.block) / no_jito,
        "the epoch's observed ceiling outranks the low rate it advertised",
    );
    approx(
        series[2].expected_take_rate,
        0.1 * shares.inflation + 0.1 * shares.mev + shares.block,
        "the epoch's own MEV commission weighs in",
    );
    assert_eq!(series[3].expected_take_rate, None, "no mix, no expectation");
    assert_eq!(
        (series[3].epoch_start_at, series[3].epoch_end_at),
        (None, None),
        "an unsealed epoch has no boundaries to report"
    );
    assert_eq!(
        get_take_rate_series(&warehouse, VOTE_ACCOUNT, Some(EPOCH + 2), &rates).len(),
        2
    );
}

fn sandwich(
    epoch: u64,
    vote_account: &str,
    blocks_produced: u64,
    rate_30d: f64,
    rate_60d: Option<f64>,
) -> ValidatorSandwich {
    ValidatorSandwich {
        epoch,
        vote_account: vote_account.into(),
        blocks_produced,
        blocks_with_sandwiches: (blocks_produced as f64 * rate_30d / 100.0) as u64,
        sandwich_rate_30d: rate_30d,
        sandwich_rate_60d: rate_60d,
    }
}

async fn run_store_sandwiches(
    directory: &Directory,
    name: &str,
    created_at: &str,
    sandwiches: Vec<ValidatorSandwich>,
) {
    let snapshot = ValidatorsSandwichesSnapshot {
        version: 1,
        from_epoch: EPOCH,
        loaded_at_epoch: EPOCH + 2,
        loaded_at_slot_index: 10,
        created_at: created_at.into(),
        sandwiches,
    };
    let path = common::write_yaml(name, &snapshot);
    store_sandwiches(
        StoreSandwichesParams::parse_from(["store", "--snapshot-file", &path]),
        directory,
    )
    .await
    .expect("store sandwiches");
    std::fs::remove_file(path).expect("remove snapshot");
}

#[tokio::test]
async fn sandwiches_land_per_epoch_and_the_last_row_of_a_pair_wins() {
    let Some(store) = common::directory_store("sandwiches").await else {
        return;
    };
    let directory = store.client();

    run_store_sandwiches(
        &directory,
        "sandwiches-first",
        "2026-01-05T00:00:00Z",
        vec![
            sandwich(EPOCH, VOTE_ACCOUNT, 2_000, 4.5, None),
            sandwich(EPOCH + 1, VOTE_ACCOUNT, 2_000, 5.5, Some(3.0)),
            sandwich(EPOCH + 1, VOTE_ACCOUNT, 2_000, 6.5, Some(3.5)),
        ],
    )
    .await;
    run_store_sandwiches(
        &directory,
        "sandwiches-second",
        "2026-01-07T00:00:00Z",
        vec![sandwich(EPOCH, VOTE_ACCOUNT, 2_100, 4.0, None)],
    )
    .await;

    let first: SandwichesDoc = directory
        .get(&epoch_doc_path(SANDWICHES_DIR, EPOCH))
        .await
        .expect("get sandwiches")
        .expect("sandwiches document")
        .body;
    assert_eq!(
        first[VOTE_ACCOUNT].sandwich_rate_30d, 4.0,
        "the rewrite wins"
    );
    assert_eq!(first[VOTE_ACCOUNT].blocks_produced, 2_100);
    assert_eq!(
        first[VOTE_ACCOUNT].sandwich_rate_60d, None,
        "upstream published only the 30d rate before epoch 820"
    );
    assert_eq!(
        (
            first[VOTE_ACCOUNT].created_at,
            first[VOTE_ACCOUNT].updated_at
        ),
        (at("2026-01-05T00:00:00Z"), at("2026-01-07T00:00:00Z"))
    );
    let second: SandwichesDoc = directory
        .get(&epoch_doc_path(SANDWICHES_DIR, EPOCH + 1))
        .await
        .expect("get sandwiches")
        .expect("sandwiches document")
        .body;
    assert_eq!(
        (
            second[VOTE_ACCOUNT].sandwich_rate_30d,
            second[VOTE_ACCOUNT].sandwich_rate_60d
        ),
        (6.5, Some(3.5)),
        "a pair the snapshot names twice keeps the last"
    );
}

#[test]
fn sandwich_incidents_are_read_with_the_epoch_boundaries_and_the_cluster_median() {
    let mut warehouse = warehouse_with_epochs();
    for (epoch, rows) in [
        (
            EPOCH,
            vec![
                ("voteA", 1.0),
                ("voteB", 2.0),
                ("voteC", 9.0),
                (VOTE_ACCOUNT, 0.5),
            ],
        ),
        (EPOCH + 1, vec![(VOTE_ACCOUNT, 7.0)]),
        (EPOCH + 5, vec![(VOTE_ACCOUNT, 8.0)]),
    ] {
        let mut doc = SandwichesDoc::new();
        for (vote_account, rate) in rows {
            doc.insert(
                vote_account.into(),
                store::docs::SandwichEntry {
                    blocks_produced: 2_000,
                    blocks_with_sandwiches: 20,
                    sandwich_rate_30d: rate,
                    sandwich_rate_60d: None,
                    created_at: at("2026-01-07T00:00:00Z"),
                    updated_at: at("2026-01-07T00:00:00Z"),
                },
            );
        }
        warehouse.sandwiches.insert(epoch, doc);
    }

    let loaded = load_validator_sandwiches(&warehouse, EPOCH..=EPOCH + 5);
    let mine = &loaded[VOTE_ACCOUNT];
    assert_eq!(
        mine.iter().map(|epoch| epoch.epoch).collect::<Vec<_>>(),
        vec![EPOCH, EPOCH + 1],
        "the epoch with no sealed boundaries is left out"
    );
    assert_eq!(mine[0].epoch_start_at, at("2026-01-01T00:00:00Z"));
    assert_eq!(mine[0].epoch_end_at, at("2026-01-03T00:00:00Z"));
    assert_eq!(
        mine[0].cluster_median_rate, 1.5,
        "the median over the four validators of the epoch"
    );
    assert_eq!(loaded["voteC"][0].cluster_median_rate, 1.5);
}

fn warehouse_with_take_rates(epochs: &[u64]) -> Warehouse {
    let mut warehouse = Warehouse::default();
    for &epoch in epochs {
        let mut validator = common::validator(VOTE_ACCOUNT, epoch);
        validator.commission_advertised = Some(10);
        warehouse
            .snapshots
            .entry(epoch)
            .or_default()
            .insert(VOTE_ACCOUNT.into(), validator);
        warehouse
            .validator_rewards
            .entry(epoch)
            .or_default()
            .insert(
                VOTE_ACCOUNT.into(),
                ValidatorRewardsEntry {
                    validator_rewards: 10.into(),
                    total_rewards: 100.into(),
                    inflation_rewards: 90.into(),
                    mev_rewards: 4.into(),
                    block_rewards: 6.into(),
                    take_rate: 0.1,
                    created_at: at("2026-01-07T00:00:00Z"),
                    updated_at: at("2026-01-07T00:00:00Z"),
                },
            );
        warehouse.mev.entry(epoch).or_default().insert(
            VOTE_ACCOUNT.into(),
            MevEntry {
                vote_account: VOTE_ACCOUNT.into(),
                mev_commission: 800,
                total_epoch_rewards: None,
                claimed_epoch_rewards: None,
                total_epoch_claimants: None,
                epoch_active_claimants: None,
                epoch_slot: Decimal::ZERO,
                epoch: epoch.into(),
                created_at: at("2026-01-07T00:00:00Z"),
            },
        );
        warehouse.priority_fees.entry(epoch).or_default().insert(
            VOTE_ACCOUNT.into(),
            PriorityFeeEntry {
                vote_account: VOTE_ACCOUNT.into(),
                priority_commission: 2000,
                total_lamports_transferred: Decimal::ZERO,
                total_epoch_rewards: None,
                claimed_epoch_rewards: None,
                total_epoch_claimants: None,
                epoch_active_claimants: None,
                epoch_slot: Decimal::ZERO,
                epoch: epoch.into(),
                created_at: at("2026-01-07T00:00:00Z"),
            },
        );
    }
    warehouse
}

#[test]
fn the_take_rate_series_keeps_the_oldest_expected_rate_when_cluster_info_is_ahead() {
    let epochs: Vec<_> = (EPOCH - 89..=EPOCH).collect();
    let mut warehouse = warehouse_with_take_rates(&epochs);
    warehouse.live.cluster_info.epoch = EPOCH + 1;
    let mut rates = load_epoch_take_rates(&warehouse, 90);
    assert!(!rates.contains_key(&(VOTE_ACCOUNT.to_string(), EPOCH - 89)));
    rates
        .get_mut(&(VOTE_ACCOUNT.to_string(), EPOCH))
        .unwrap()
        .expected = None;
    let series = get_take_rate_series(&warehouse, VOTE_ACCOUNT, None, &rates);
    assert_eq!(series.len(), 90);
    assert_eq!(series[0].epoch, EPOCH - 89);
    assert!(
        series[0]
            .expected_take_rate
            .is_some_and(|rate| (rate - 0.1052).abs() < 1e-12),
        "oldest expected rate: {:?}",
        series[0].expected_take_rate
    );
    assert_eq!(
        series[89].expected_take_rate, None,
        "a cached unknown rate remains authoritative"
    );
}

#[test]
fn epoch_take_rates_compute_each_rows_commissions_from_documents() {
    let mut warehouse = warehouse_with_take_rates(&[EPOCH]);
    warehouse.live.cluster_info.epoch = EPOCH;
    let rates = load_epoch_take_rates(&warehouse, 3);
    let rate = &rates[&(VOTE_ACCOUNT.to_string(), EPOCH)];
    assert_eq!(rate.realized, 0.1);
    assert!((rate.expected.unwrap() - 0.1052).abs() < 1e-12);
}

#[test]
fn another_validators_rewards_reweight_the_expected_take_rate() {
    let mut warehouse = warehouse_with_take_rates(&[EPOCH]);
    warehouse.live.cluster_info.epoch = EPOCH;
    warehouse.validator_rewards.get_mut(&EPOCH).unwrap().insert(
        "voteOther".into(),
        ValidatorRewardsEntry {
            validator_rewards: 100.into(),
            total_rewards: 100.into(),
            inflation_rewards: Decimal::ZERO,
            mev_rewards: Decimal::ZERO,
            block_rewards: 100.into(),
            take_rate: 1.0,
            created_at: at("2026-01-07T00:00:00Z"),
            updated_at: at("2026-01-07T00:00:00Z"),
        },
    );
    let rates = load_epoch_take_rates(&warehouse, 3);
    assert!((rates[&(VOTE_ACCOUNT.to_string(), EPOCH)].expected.unwrap() - 0.1526).abs() < 1e-12);
}

#[test]
fn epoch_take_rates_cover_three_epochs_anchored_to_cluster_info() {
    let mut warehouse = warehouse_with_take_rates(&[EPOCH - 3, EPOCH - 2, EPOCH - 1, EPOCH]);
    warehouse.live.cluster_info.epoch = EPOCH;
    warehouse.snapshots.remove(&EPOCH);
    let rates = load_epoch_take_rates(&warehouse, 3);
    assert_eq!(warehouse.last_epoch(), EPOCH - 1);
    assert_eq!(rates.len(), 3);
    assert!(!rates.contains_key(&(VOTE_ACCOUNT.to_string(), EPOCH - 3)));
    for epoch in EPOCH - 2..EPOCH {
        let rate = &rates[&(VOTE_ACCOUNT.to_string(), epoch)];
        assert_eq!(rate.realized, 0.1);
        assert!((rate.expected.unwrap() - 0.1052).abs() < 1e-12);
    }
    assert_eq!(rates[&(VOTE_ACCOUNT.to_string(), EPOCH)].expected, None);
}

#[derive(Clone, Copy)]
enum ExpectedRateWriter {
    TakeRates,
    JitoMev,
    JitoPriority,
    CloseEpoch,
}

async fn run_expected_rate_writer(directory: &Directory, writer: ExpectedRateWriter) {
    match writer {
        ExpectedRateWriter::TakeRates => {
            run_store_take_rates(
                directory,
                "expected-rate-rewards",
                "2026-01-07T00:00:00Z",
                vec![ValidatorEpochRewards {
                    epoch: EPOCH,
                    vote_account: VOTE_ACCOUNT.into(),
                    validator_rewards: 10,
                    total_rewards: 100,
                    inflation_rewards: 90,
                    mev_rewards: 4,
                    block_rewards: 6,
                }],
            )
            .await;
        }
        ExpectedRateWriter::JitoMev | ExpectedRateWriter::JitoPriority => {
            let (account_type, validator) = match writer {
                ExpectedRateWriter::JitoMev => (
                    JitoAccountType::MevTipDistribution,
                    collect::validators_jito::ValidatorSnapshot::MevTipDistribution(
                        MevTipDistributionValidatorSnapshot {
                            vote_account: VOTE_ACCOUNT.into(),
                            mev_commission: 800,
                            epoch: EPOCH,
                            total_epoch_rewards: None,
                            claimed_epoch_rewards: None,
                            total_epoch_claimants: None,
                            epoch_active_claimants: None,
                        },
                    ),
                ),
                _ => (
                    JitoAccountType::PriorityFeeDistribution,
                    collect::validators_jito::ValidatorSnapshot::PriorityFeeDistribution(
                        PriorityFeeDistributionValidatorSnapshot {
                            vote_account: VOTE_ACCOUNT.into(),
                            priority_commission: 2000,
                            total_lamports_transferred: 0,
                            merkle_root_upload_authority: "authority".into(),
                            epoch: EPOCH,
                            total_epoch_rewards: None,
                            claimed_epoch_rewards: None,
                            total_epoch_claimants: None,
                            epoch_active_claimants: None,
                        },
                    ),
                ),
            };
            let snapshot = JitoSnapshot {
                epoch: EPOCH,
                version: 1,
                account_type: account_type.clone(),
                loaded_at_epoch: EPOCH + 1,
                loaded_at_slot_index: 0,
                created_at: "2026-01-07T00:00:00Z".into(),
                validators: HashMap::from([(VOTE_ACCOUNT.to_string(), validator)]),
            };
            let path = common::write_yaml("expected-rate-jito", &snapshot);
            store_jito(
                StoreJitoParams::parse_from(["store", "--snapshot-file", &path]),
                directory,
                account_type,
            )
            .await
            .expect("store Jito commissions");
            std::fs::remove_file(path).expect("remove snapshot");
        }
        ExpectedRateWriter::CloseEpoch => {
            let mut performance = common::validator_performance();
            performance.commission = 10;
            let snapshot = ValidatorsPerformanceSnapshot {
                epoch: EPOCH,
                epoch_slot: 432000,
                transaction_count: 200,
                created_at: "2026-01-07T00:02:00Z".into(),
                slots_per_year: baseline_slots_per_year(),
                cluster_inflation: Some(ClusterInflation {
                    sol_total_supply: 500_000_000,
                    inflation: 0.045,
                    inflation_taper: 0.15,
                }),
                validators: HashMap::from([(VOTE_ACCOUNT.to_string(), performance)]),
                rewards: Some(HashMap::from([(
                    VOTE_ACCOUNT.to_string(),
                    ValidatorRewards {
                        commission_effective: Some(10),
                    },
                )])),
                nodes: Default::default(),
            };
            let path = common::write_yaml("expected-rate-close", &snapshot);
            close_epoch(
                CloseEpochParams::parse_from(["store", "--snapshot-file", &path]),
                directory,
            )
            .await
            .expect("close epoch");
            std::fs::remove_file(path).expect("remove snapshot");
        }
    }
}

#[tokio::test]
async fn every_document_writer_order_converges_to_the_same_expected_take_rate() {
    for (name, order) in [
        (
            "expected-rate-rewards-first",
            [
                ExpectedRateWriter::TakeRates,
                ExpectedRateWriter::JitoMev,
                ExpectedRateWriter::JitoPriority,
                ExpectedRateWriter::CloseEpoch,
            ],
        ),
        (
            "expected-rate-rewards-last",
            [
                ExpectedRateWriter::CloseEpoch,
                ExpectedRateWriter::JitoPriority,
                ExpectedRateWriter::JitoMev,
                ExpectedRateWriter::TakeRates,
            ],
        ),
    ] {
        let Some(store) = common::directory_store(name).await else {
            return;
        };
        let directory = store.client();
        let mut performance = common::validator_performance();
        performance.commission = 10;
        let snapshot = Snapshot {
            epoch: EPOCH,
            created_at: "2026-01-07T00:00:00Z".into(),
            validators: vec![common::validator_snapshot(
                "identityExpectedRate",
                VOTE_ACCOUNT,
                performance.clone(),
            )],
        };
        let path = common::write_yaml("expected-rate-validators", &snapshot);
        store_validators(
            StoreValidatorsParams::parse_from(["store", "--snapshot-file", &path]),
            &directory,
        )
        .await
        .expect("store validators");
        std::fs::remove_file(path).expect("remove snapshot");
        for (epoch_slot, created_at) in [(0, "2026-01-07T00:00:00Z"), (1, "2026-01-07T00:01:00Z")] {
            let snapshot = ValidatorsPerformanceSnapshot {
                epoch: EPOCH,
                epoch_slot,
                transaction_count: epoch_slot * 100,
                created_at: created_at.into(),
                slots_per_year: baseline_slots_per_year(),
                cluster_inflation: None,
                validators: HashMap::from([(VOTE_ACCOUNT.to_string(), performance.clone())]),
                rewards: None,
                nodes: Default::default(),
            };
            let path = common::write_yaml("expected-rate-streams", &snapshot);
            store_uptime(
                StoreUptimeParams::parse_from(["store", "--snapshot-file", &path]),
                &directory,
            )
            .await
            .unwrap();
            store_commissions(
                StoreCommissionsParams::parse_from(["store", "--snapshot-file", &path]),
                &directory,
            )
            .await
            .unwrap();
            store_versions(
                StoreVersionsParams::parse_from(["store", "--snapshot-file", &path]),
                &directory,
            )
            .await
            .unwrap();
            store_cluster_info(
                StoreClusterInfoParams::parse_from(["store", "--snapshot-file", &path]),
                &directory,
            )
            .await
            .unwrap();
            std::fs::remove_file(path).expect("remove snapshot");
        }
        for writer in order {
            run_expected_rate_writer(&directory, writer).await;
        }
        let mut warehouse = Warehouse::default();
        warehouse.warm(&directory, 3).await.expect("warm documents");
        let rates = load_epoch_take_rates(&warehouse, 3);
        let rate = &rates[&(VOTE_ACCOUNT.to_string(), EPOCH)];
        assert_eq!(rate.realized, 0.1);
        assert!(
            (rate.expected.unwrap() - 0.1052).abs() < 1e-12,
            "writer order: {name}"
        );
        let series = get_take_rate_series(&warehouse, VOTE_ACCOUNT, None, &rates);
        assert!((series[0].expected_take_rate.unwrap() - 0.1052).abs() < 1e-12);
    }
}
