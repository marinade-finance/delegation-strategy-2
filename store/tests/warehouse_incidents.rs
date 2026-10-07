use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use std::collections::HashMap;
use store::docs::{
    CommissionSample, CommissionState, EpochDoc, UptimeInterval, UptimeState, UptimeStatus,
};
use store::dto::{IncidentRecord, ValidatorRecord};
use store::incidents::{IncidentFilters, ValidatorIncidents};
use store::utils::{
    load_commissions, load_ruggers, load_validator_incidents, load_validators,
    load_validators_aggregated_flat, worst_known_commission, TakeRates, ValidatorOverlays,
};
use store::warehouse::Warehouse;

mod common;

const EPOCH: u64 = 100;

fn at(moment: &str) -> DateTime<Utc> {
    moment.parse().expect("timestamp")
}

fn no_records() -> HashMap<String, ValidatorRecord> {
    HashMap::new()
}

/// A commission sample in the live stream, as the minute writer records it.
fn commission(
    warehouse: &mut Warehouse,
    vote_account: &str,
    epoch: u64,
    epoch_slot: u64,
    commission: i32,
    created_at: &str,
) {
    let sample = CommissionSample {
        epoch,
        epoch_slot,
        commission,
        created_at: at(created_at),
    };
    match warehouse.live.commissions.get_mut(vote_account) {
        Some(state) => {
            state.changes.push(sample.clone());
            state.last = sample;
        }
        None => {
            warehouse.live.commissions.insert(
                vote_account.to_string(),
                CommissionState {
                    last: sample.clone(),
                    changes: vec![sample],
                },
            );
        }
    }
}

fn raises(incidents: &ValidatorIncidents, vote_account: &str) -> Vec<(u64, u8, u8)> {
    incidents
        .get(vote_account)
        .map(|records| {
            records
                .commission_raises
                .iter()
                .map(|raise| (raise.epoch, raise.commission_before, raise.commission_after))
                .collect()
        })
        .unwrap_or_default()
}

fn incidents(warehouse: &Warehouse, from_epoch: u64, last_epoch: u64) -> ValidatorIncidents {
    load_validator_incidents(warehouse, from_epoch..=last_epoch, &no_records()).expect("incidents")
}

#[test]
fn a_raise_over_the_bar_is_loaded_with_the_rate_it_came_from() {
    let mut warehouse = Warehouse::default();
    commission(&mut warehouse, "voteA", 99, 100, 5, "2026-01-01T00:00:00Z");
    commission(
        &mut warehouse,
        "voteA",
        100,
        200,
        100,
        "2026-02-01T00:00:00Z",
    );

    let loaded = incidents(&warehouse, 100, 100);

    assert_eq!(raises(&loaded, "voteA"), vec![(100, 5, 100)]);
    let raise = &loaded.get("voteA").unwrap().commission_raises[0];
    assert_eq!(raise.epoch_slot, 200);
    assert_eq!(raise.changed_at, at("2026-02-01T00:00:00Z"));
}

// The epoch under the window is read for the rate the window's first epoch moved from, and for
// nothing else.
#[test]
fn a_raise_inside_the_baseline_epoch_is_not_served() {
    let mut warehouse = Warehouse::default();
    commission(&mut warehouse, "voteA", 99, 100, 5, "2026-01-01T00:00:00Z");
    commission(
        &mut warehouse,
        "voteA",
        99,
        200,
        100,
        "2026-01-01T01:00:00Z",
    );
    commission(
        &mut warehouse,
        "voteA",
        100,
        100,
        100,
        "2026-02-01T00:00:00Z",
    );

    assert!(incidents(&warehouse, 100, 100).is_empty());
}

#[test]
fn a_validator_first_sampled_over_the_bar_opens_nothing() {
    let mut warehouse = Warehouse::default();
    commission(
        &mut warehouse,
        "voteA",
        100,
        100,
        100,
        "2026-02-01T00:00:00Z",
    );
    commission(
        &mut warehouse,
        "voteA",
        100,
        200,
        100,
        "2026-02-01T01:00:00Z",
    );

    assert!(incidents(&warehouse, 100, 100).is_empty());
}

// The stream carries a sample per vote account per epoch, so every validator would reach the cache
// if the loader filed them before judging their samples.
#[test]
fn a_validator_that_never_raised_is_not_filed_at_all() {
    let mut warehouse = Warehouse::default();
    commission(&mut warehouse, "voteA", 99, 100, 5, "2026-01-01T00:00:00Z");
    commission(&mut warehouse, "voteA", 100, 100, 5, "2026-02-01T00:00:00Z");

    assert!(incidents(&warehouse, 100, 100).is_empty());
}

// Read in the order the samples arrive, the 5 at slot 300 would open a second raise.
#[test]
fn samples_are_read_in_slot_order_whatever_order_they_were_written_in() {
    let mut warehouse = Warehouse::default();
    commission(&mut warehouse, "voteA", 100, 300, 5, "2026-02-01T02:00:00Z");
    commission(&mut warehouse, "voteA", 100, 100, 5, "2026-02-01T00:00:00Z");
    commission(
        &mut warehouse,
        "voteA",
        100,
        200,
        100,
        "2026-02-01T01:00:00Z",
    );

    assert_eq!(
        raises(&incidents(&warehouse, 100, 100), "voteA"),
        vec![(100, 5, 100)]
    );
}

#[test]
fn the_bar_is_read_at_ninety() {
    let mut warehouse = Warehouse::default();
    commission(&mut warehouse, "voteA", 99, 100, 89, "2026-01-01T00:00:00Z");
    commission(
        &mut warehouse,
        "voteA",
        100,
        100,
        90,
        "2026-02-01T00:00:00Z",
    );
    commission(&mut warehouse, "voteB", 99, 100, 88, "2026-01-01T00:00:00Z");
    commission(
        &mut warehouse,
        "voteB",
        100,
        100,
        89,
        "2026-02-01T00:00:00Z",
    );

    let loaded = incidents(&warehouse, 100, 100);
    assert_eq!(raises(&loaded, "voteA"), vec![(100, 89, 90)]);
    assert!(loaded.get("voteB").is_none());
}

#[test]
fn a_drop_back_under_the_bar_opens_a_second_raise() {
    let mut warehouse = Warehouse::default();
    commission(&mut warehouse, "voteA", 100, 100, 5, "2026-02-01T00:00:00Z");
    commission(
        &mut warehouse,
        "voteA",
        100,
        200,
        100,
        "2026-02-01T01:00:00Z",
    );
    commission(&mut warehouse, "voteA", 100, 300, 5, "2026-02-01T02:00:00Z");
    commission(
        &mut warehouse,
        "voteA",
        100,
        400,
        100,
        "2026-02-01T03:00:00Z",
    );

    assert_eq!(
        raises(&incidents(&warehouse, 100, 100), "voteA"),
        vec![(100, 5, 100), (100, 5, 100)]
    );
}

#[test]
fn a_raise_carries_the_epoch_peak_rather_than_the_crossing_sample() {
    let mut warehouse = Warehouse::default();
    commission(&mut warehouse, "voteA", 100, 100, 1, "2026-02-01T00:00:00Z");
    commission(
        &mut warehouse,
        "voteA",
        100,
        200,
        95,
        "2026-02-01T01:00:00Z",
    );
    commission(
        &mut warehouse,
        "voteA",
        100,
        300,
        100,
        "2026-02-01T02:00:00Z",
    );

    assert_eq!(
        raises(&incidents(&warehouse, 100, 100), "voteA"),
        vec![(100, 1, 100)]
    );
}

// A later epoch's higher rate is its own epoch's business, not this raise's peak.
#[test]
fn the_peak_does_not_reach_past_the_epoch_it_was_raised_in() {
    let mut warehouse = Warehouse::default();
    commission(&mut warehouse, "voteA", 99, 100, 1, "2026-01-01T00:00:00Z");
    commission(
        &mut warehouse,
        "voteA",
        100,
        100,
        95,
        "2026-02-01T00:00:00Z",
    );
    commission(
        &mut warehouse,
        "voteA",
        101,
        100,
        100,
        "2026-03-01T00:00:00Z",
    );

    assert_eq!(
        raises(&incidents(&warehouse, 100, 101), "voteA"),
        vec![(100, 1, 95)]
    );
}

// A sealed epoch's samples are read from its seal; a raise there is judged the same way.
#[test]
fn a_raise_sealed_by_close_epoch_is_loaded_like_a_live_one() {
    let mut warehouse = Warehouse::default();
    warehouse.commissions.entry(99).or_default().insert(
        "voteA".into(),
        vec![CommissionSample {
            epoch: 99,
            epoch_slot: 100,
            commission: 5,
            created_at: at("2026-01-01T00:00:00Z"),
        }],
    );
    warehouse.commissions.entry(100).or_default().insert(
        "voteA".into(),
        vec![CommissionSample {
            epoch: 100,
            epoch_slot: 200,
            commission: 100,
            created_at: at("2026-02-01T00:00:00Z"),
        }],
    );

    assert_eq!(
        raises(&incidents(&warehouse, 100, 100), "voteA"),
        vec![(100, 5, 100)]
    );
}

fn down(warehouse: &mut Warehouse, vote_account: &str, epoch: u64, start_at: &str, end_at: &str) {
    let interval = UptimeInterval {
        status: UptimeStatus::Down,
        epoch,
        start_at: at(start_at),
        end_at: at(end_at),
    };
    warehouse
        .live
        .uptimes
        .entry(vote_account.to_string())
        .or_insert_with(|| UptimeState {
            open: UptimeInterval {
                status: UptimeStatus::Up,
                epoch: 200,
                start_at: at("2026-12-01T00:00:00Z"),
                end_at: at("2026-12-01T00:01:00Z"),
            },
            closed: Vec::new(),
            last_credits: None,
        })
        .closed
        .push(interval);
}

#[test]
fn downtime_is_loaded_oldest_first_inside_a_window_closed_on_both_ends() {
    let mut warehouse = Warehouse::default();
    down(
        &mut warehouse,
        "voteA",
        101,
        "2026-03-01T00:00:00Z",
        "2026-03-01T00:10:00Z",
    );
    down(
        &mut warehouse,
        "voteA",
        100,
        "2026-02-01T00:00:00Z",
        "2026-02-01T00:05:00Z",
    );
    down(
        &mut warehouse,
        "voteA",
        99,
        "2026-01-01T00:00:00Z",
        "2026-01-01T00:10:00Z",
    );
    down(
        &mut warehouse,
        "voteA",
        102,
        "2026-04-01T00:00:00Z",
        "2026-04-01T00:10:00Z",
    );

    let loaded = incidents(&warehouse, 100, 101);
    let downtimes = &loaded.get("voteA").unwrap().downtimes;
    assert_eq!(
        downtimes
            .iter()
            .map(|downtime| (downtime.epoch, downtime.downtime_seconds))
            .collect::<Vec<_>>(),
        vec![(100, 300), (101, 600)]
    );

    let served = loaded.into_response_incidents(
        "voteA",
        &IncidentFilters {
            types: None,
            ..Default::default()
        },
    );
    assert_eq!(served.len(), 2);
    assert!(matches!(
        served[0],
        IncidentRecord::Downtime { epoch: 100, .. }
    ));
}

#[test]
fn a_spike_is_served_beside_the_epochs_downtime() {
    let mut warehouse = Warehouse::default();
    down(
        &mut warehouse,
        "voteA",
        100,
        "2026-02-01T00:00:00Z",
        "2026-02-01T00:10:00Z",
    );
    commission(&mut warehouse, "voteA", 99, 100, 5, "2026-01-01T00:00:00Z");
    commission(
        &mut warehouse,
        "voteA",
        100,
        200,
        100,
        "2026-02-01T01:00:00Z",
    );

    let served = incidents(&warehouse, 100, 100).into_response_incidents(
        "voteA",
        &IncidentFilters {
            types: None,
            ..Default::default()
        },
    );
    assert_eq!(served.len(), 2);
    assert!(served
        .iter()
        .any(|incident| matches!(incident, IncidentRecord::CommissionSpike { .. })));
}

fn epoch_record(epoch: u64) -> EpochDoc {
    EpochDoc {
        epoch,
        start_at: at("2026-01-01T00:00:00Z") + chrono::Duration::days(2 * (epoch - EPOCH) as i64),
        end_at: at("2026-01-03T00:00:00Z") + chrono::Duration::days(2 * (epoch - EPOCH) as i64),
        transaction_count: 0,
        supply: Decimal::from(500_000_000_000_000_000u64),
        inflation: 0.045,
        inflation_taper: 0.15,
        slots_per_year: 78_892_314.984,
    }
}

const MIX: RewardMixShares = RewardMixShares {
    inflation: 0.90,
    mev: 0.044,
    block: 0.056,
};

fn approx(actual: Option<f64>, expected: f64, context: &str) {
    let actual = actual.unwrap_or_else(|| panic!("{context}: expected a rate"));
    assert!(
        (actual - expected).abs() < 1e-12,
        "{context}: expected {expected}, got {actual}"
    );
}

use store::utils::RewardMixShares;

async fn load(warehouse: &Warehouse, display_epochs: u64) -> HashMap<String, ValidatorRecord> {
    let overlays = ValidatorOverlays {
        take_rates: TakeRates {
            measured: Default::default(),
            shares: Some(MIX),
        },
        ..Default::default()
    };
    load_validators(warehouse, display_epochs, 2, &overlays)
        .await
        .expect("load validators")
}

/// `(epoch, advertised, max_observed, min_observed, effective)`.
type CommissionEpoch = (u64, Option<i32>, Option<i32>, Option<i32>, Option<i32>);

fn store_commission_series(
    warehouse: &mut Warehouse,
    vote_account: &str,
    identity: &str,
    series: &[CommissionEpoch],
) {
    for (epoch, advertised, max_observed, min_observed, effective) in series {
        let mut validator = common::validator(vote_account, *epoch);
        validator.identity = identity.to_string();
        validator.activated_stake = Decimal::from(100);
        validator.commission_advertised = *advertised;
        validator.commission_max_observed = *max_observed;
        validator.commission_min_observed = *min_observed;
        validator.commission_effective = *effective;
        validator.updated_at = Some(Utc::now());
        warehouse
            .snapshots
            .entry(*epoch)
            .or_default()
            .insert(vote_account.to_string(), validator);
    }
}

// One record per validator, newest epoch first, and that one is open: its close fields are null.
#[tokio::test]
async fn load_validators_projects_commissions_from_the_newest_closed_epoch() {
    let mut warehouse = Warehouse::default();
    warehouse.live.cluster_info.epoch = EPOCH + 1;
    warehouse.epochs.insert(EPOCH, epoch_record(EPOCH));
    // voteGamer advertises 0 in the open epoch but the closed epoch caught it at 100.
    store_commission_series(
        &mut warehouse,
        "voteGamer",
        "identityGamer",
        &[
            (EPOCH, Some(100), Some(100), Some(0), Some(100)),
            (EPOCH + 1, Some(0), None, None, None),
        ],
    );
    store_commission_series(
        &mut warehouse,
        "voteHonest",
        "identityHonest",
        &[
            (EPOCH, Some(0), Some(0), Some(0), Some(0)),
            (EPOCH + 1, Some(0), None, None, None),
        ],
    );
    store_commission_series(
        &mut warehouse,
        "voteNew",
        "identityNew",
        &[(EPOCH + 1, Some(5), None, None, None)],
    );
    store_commission_series(
        &mut warehouse,
        "voteRaised",
        "identityRaised",
        &[
            (EPOCH, Some(5), Some(5), Some(5), Some(5)),
            (EPOCH + 1, Some(10), None, None, None),
        ],
    );

    let validators = load(&warehouse, 2).await;

    let gamer = &validators["voteGamer"];
    assert_eq!(
        gamer.commission_advertised,
        Some(0),
        "commission_advertised keeps meaning the open epoch's snapshot"
    );
    assert_eq!(
        (
            gamer.commission_max_observed,
            gamer.commission_min_observed,
            gamer.commission_effective
        ),
        (Some(100), Some(0), Some(100)),
        "all three must come from the newest closed epoch instead of staying null"
    );
    let honest = &validators["voteHonest"];
    assert_eq!(
        (
            honest.commission_max_observed,
            honest.commission_min_observed,
            honest.commission_effective
        ),
        (Some(0), Some(0), Some(0)),
        "a genuine zero must project as a zero, not as unknown"
    );
    let new = &validators["voteNew"];
    assert_eq!(
        (
            new.commission_max_observed,
            new.commission_min_observed,
            new.commission_effective
        ),
        (None, None, None),
        "a validator with no closed epoch yet has nothing to project"
    );

    // No Jito entries, so the MEV weight renormalizes out and only inflation and block remain.
    let weight = MIX.inflation + MIX.block;
    approx(
        gamer.expected_take_rate,
        1.0,
        "a validator observed at 100% keeps everything",
    );
    approx(
        honest.expected_take_rate,
        MIX.block / weight,
        "a genuinely free validator floors at the block share",
    );
    approx(
        validators["voteRaised"].expected_take_rate,
        (0.10 * MIX.inflation + MIX.block) / weight,
        "a rise advertised in the open epoch counts before the epoch closes",
    );
}

// Rugged two epochs ago, 5 since: with close-epoch pending, only the superseded epoch is populated.
#[tokio::test]
async fn load_validators_does_not_reach_past_the_newest_closed_epoch_for_commission() {
    let mut warehouse = Warehouse::default();
    warehouse.live.cluster_info.epoch = EPOCH + 2;
    warehouse.epochs.insert(EPOCH, epoch_record(EPOCH));
    store_commission_series(
        &mut warehouse,
        "voteReformed",
        "identityReformed",
        &[
            (EPOCH, Some(100), Some(100), Some(0), Some(100)),
            (EPOCH + 1, Some(5), None, None, None),
            (EPOCH + 2, Some(5), None, None, None),
        ],
    );
    store_commission_series(
        &mut warehouse,
        "voteDeparted",
        "identityDeparted",
        &[
            (EPOCH, Some(7), Some(7), Some(7), Some(7)),
            (EPOCH + 1, Some(7), None, None, None),
        ],
    );

    let validators = load(&warehouse, 3).await;

    let reformed = &validators["voteReformed"];
    assert_eq!(
        (
            reformed.commission_max_observed,
            reformed.commission_min_observed,
            reformed.commission_effective
        ),
        (None, None, None),
        "the walk must stop one epoch below the record's own instead of reaching two epochs back"
    );
    assert_eq!(reformed.commission_advertised, Some(5));
    assert_eq!(
        worst_known_commission(
            reformed.commission_max_observed,
            reformed.commission_advertised
        ),
        Some(5)
    );
    approx(
        reformed.expected_take_rate,
        (0.05 * MIX.inflation + MIX.block) / (MIX.inflation + MIX.block),
        "bounding the walk must not cost the validator its take rate",
    );

    // voteDeparted's newest closed epoch is two below the tip, so a tip-measured bound misses it.
    let departed = &validators["voteDeparted"];
    assert_eq!(
        (
            departed.commission_max_observed,
            departed.commission_min_observed,
            departed.commission_effective
        ),
        (Some(7), Some(7), Some(7)),
        "the bound is one epoch below the record's own seeding epoch, not below the cluster's tip"
    );
}

#[tokio::test]
async fn node_metadata_projects_from_the_newest_epoch_that_reported_any() {
    let mut warehouse = Warehouse::default();
    warehouse.live.cluster_info.epoch = EPOCH + 1;
    let mut reported = common::validator("voteNode", EPOCH);
    reported.version = Some("2.3.0".into());
    reported.client_id = Some(3);
    reported.client_id_raw = Some("Agave".into());
    reported.gossip_port = Some(8001);
    reported.rpc_public = Some(true);
    let mut silent = common::validator("voteNode", EPOCH + 1);
    silent.activated_stake = Decimal::from(5);
    warehouse
        .snapshots
        .entry(EPOCH)
        .or_default()
        .insert("voteNode".into(), reported);
    warehouse
        .snapshots
        .entry(EPOCH + 1)
        .or_default()
        .insert("voteNode".into(), silent);

    let record = &load(&warehouse, 2).await["voteNode"];
    assert_eq!(
        record.activated_stake,
        Decimal::from(5),
        "the record is seeded from the newest epoch"
    );
    assert_eq!(
        (
            record.version.as_deref(),
            record.client_id,
            record.gossip_port,
            record.rpc_public
        ),
        (Some("2.3.0"), Some(3), Some(8001), Some(true)),
        "but its node metadata comes from the newest epoch that reported the node at all"
    );
    assert_eq!(record.client_lineage.as_deref(), Some("agave"));
}

#[tokio::test]
async fn collector_flags_and_shared_counts_project_per_epoch() {
    let mut warehouse = Warehouse::default();
    warehouse.live.cluster_info.epoch = EPOCH;
    for (vote_account, inflation, block) in [
        ("voteA", Some("sharedCollector"), Some("identity-voteA")),
        ("voteB", Some("sharedCollector"), Some("elsewhere")),
        ("voteC", Some("voteC"), None),
        ("voteD", None, None),
    ] {
        let mut validator = common::validator(vote_account, EPOCH);
        validator.inflation_rewards_collector = inflation.map(str::to_string);
        validator.block_revenue_collector = block.map(str::to_string);
        validator.inflation_rewards_collector_healthy = inflation.map(|_| false);
        warehouse
            .snapshots
            .entry(EPOCH)
            .or_default()
            .insert(vote_account.into(), validator);
    }

    let validators = load(&warehouse, 1).await;
    let fields = |vote_account: &str| {
        let record = &validators[vote_account];
        (
            record.inflation_rewards_collector_redirected,
            record.inflation_rewards_collector_shared_count,
            record.block_revenue_collector_is_identity,
            record.block_revenue_collector_shared_count,
        )
    };
    assert_eq!(fields("voteA"), (Some(true), Some(2), Some(true), Some(1)));
    assert_eq!(fields("voteB"), (Some(true), Some(2), Some(false), Some(1)));
    assert_eq!(
        fields("voteC"),
        (Some(false), Some(1), None, None),
        "a collector that is the vote account itself is not a redirect"
    );
    assert_eq!(
        fields("voteD"),
        (None, None, None, None),
        "a pre-v4 vote state is counted nowhere rather than pooled under null"
    );
    assert_eq!(
        validators["voteA"].epoch_stats[0].inflation_rewards_collector_shared_count,
        Some(2),
        "the epoch stats carry the same derivation"
    );
    assert_eq!(
        validators["voteA"].inflation_rewards_collector_healthy,
        Some(false)
    );
}

// From 1030 on commission_effective is close-epoch's vote-state sample at the epoch_stakes vintage,
// so a cut it still lags must not read as a rug.
#[test]
fn load_ruggers_does_not_flag_an_honest_cut_while_the_applied_rate_lags_it() {
    let mut warehouse = Warehouse::default();
    // Advertised 100 through 1030, cut to 0 at 1031; the applied rate follows two epochs later.
    store_commission_series(
        &mut warehouse,
        "voteHonest",
        "identityHonest",
        &[
            (1030, Some(100), Some(100), Some(100), Some(100)),
            (1031, Some(0), Some(100), Some(0), Some(100)),
            (1032, Some(0), Some(100), Some(0), Some(100)),
            (1033, Some(0), Some(0), Some(0), Some(0)),
        ],
    );
    // A genuine rug: 0 in every sample the epoch saw, paid at 100.
    store_commission_series(
        &mut warehouse,
        "voteRugger",
        "identityRugger",
        &[
            (1030, Some(0), Some(0), Some(0), Some(0)),
            (1031, Some(0), Some(100), Some(0), Some(100)),
            (1032, Some(0), Some(100), Some(0), Some(100)),
            (1033, Some(0), Some(0), Some(0), Some(0)),
        ],
    );
    for epoch in 1030..=1033 {
        for vote_account in ["voteHonest", "voteRugger"] {
            let advertised = warehouse.snapshots[&epoch][vote_account]
                .commission_advertised
                .unwrap();
            commission(
                &mut warehouse,
                vote_account,
                epoch,
                1,
                advertised,
                "2026-02-01T00:00:00Z",
            );
        }
    }

    let ruggers = load_ruggers(&warehouse);
    assert!(
        !ruggers.contains_key("voteHonest"),
        "the applied rate at 1031 and 1032 is the 100 it carried in at 1029 and 1030"
    );
    let rugger = ruggers.get("voteRugger").expect("the rug is still found");
    assert_eq!(rugger.epochs, vec![1031, 1032]);
}

#[test]
fn load_commissions_serves_only_the_sampled_changes() {
    let mut warehouse = Warehouse::default();
    warehouse.live.cluster_info.epoch = EPOCH;
    store_commission_series(
        &mut warehouse,
        "voteA",
        "identityA",
        &[(EPOCH, Some(5), Some(5), Some(5), Some(5))],
    );
    commission(
        &mut warehouse,
        "voteA",
        EPOCH,
        10,
        5,
        "2026-02-01T00:00:00Z",
    );

    let commissions = load_commissions(&warehouse, 1).expect("commissions");
    assert_eq!(
        commissions["voteA"]
            .iter()
            .map(|record| (record.epoch_slot, record.commission))
            .collect::<Vec<_>>(),
        vec![(10, 5)],
        "the applied rate of a closed epoch is not a sample of its own"
    );
}

#[test]
fn validators_flat_reports_the_window_max_inflation_commission_bps_only_when_every_epoch_has_one() {
    // Seven epochs with credits, the floor the flat loader serves a validator from.
    let epochs = 7;
    let mut warehouse = Warehouse::default();
    for (vote_account, bps_at_last) in [("voteFull", Some(749)), ("votePartial", None)] {
        for offset in 0..epochs {
            let epoch = EPOCH + offset;
            let mut validator = common::validator(vote_account, epoch);
            validator.activated_stake = Decimal::from(100);
            validator.credits = Some(Decimal::from(10));
            validator.inflation_rewards_commission_bps = if offset + 1 == epochs {
                bps_at_last
            } else {
                Some(500)
            };
            validator.commission_advertised = Some(5);
            warehouse
                .snapshots
                .entry(epoch)
                .or_default()
                .insert(vote_account.into(), validator);
        }
    }

    let flat =
        load_validators_aggregated_flat(&warehouse, EPOCH + epochs - 1, epochs).expect("flat");
    let bps_of = |vote_account: &str| {
        flat.iter()
            .find(|row| row.vote_account == vote_account)
            .map(|row| row.max_inflation_rewards_commission_bps)
    };
    assert_eq!(bps_of("voteFull"), Some(Some(749)));
    assert_eq!(
        bps_of("votePartial"),
        Some(None),
        "a maximum over a partial series would understate the rate"
    );
}
