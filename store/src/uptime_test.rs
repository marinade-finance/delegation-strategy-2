use crate::docs::{UptimeStatus, UptimesDoc};
use crate::uptime::{apply_uptime_samples, is_down};
use chrono::{DateTime, Duration, Utc};
use collect::validators_performance::{ValidatorPerformance, ValidatorsPerformanceSnapshot};
use std::collections::HashMap;

const VOTE_ACCOUNT: &str = "KmCRTozzcAXvFEH2xakNMV7GeWyftyVFKS32XGV4spW";

fn performance(
    delinquent: bool,
    last_vote: Option<u64>,
    credits_total: Option<u64>,
) -> ValidatorPerformance {
    ValidatorPerformance {
        commission: 0,
        version: None,
        client_id: None,
        client_id_raw: None,
        feature_set: None,
        shred_version: None,
        credits: None,
        vote_reward_lamports: None,
        last_vote,
        credits_total,
        leader_slots: 0,
        blocks_produced: 0,
        skip_rate: 0.0,
        delinquent,
    }
}

#[test]
fn a_voting_validator_keeps_the_rpc_delinquency() {
    assert!(is_down(
        &performance(true, Some(446897992), Some(10)),
        Some(5)
    ));
    assert!(!is_down(
        &performance(false, Some(446897992), Some(10)),
        Some(10)
    ));
}

#[test]
fn without_votes_growing_credits_is_up() {
    assert!(!is_down(&performance(true, Some(0), Some(11)), Some(10)));
    assert!(!is_down(&performance(true, None, Some(11)), Some(10)));
}

#[test]
fn without_votes_flat_credits_is_down() {
    assert!(is_down(&performance(false, Some(0), Some(10)), Some(10)));
}

#[test]
fn without_votes_and_no_previous_credits_is_up() {
    assert!(!is_down(&performance(true, Some(0), Some(10)), None));
    assert!(!is_down(&performance(false, Some(0), Some(10)), None));
}

#[test]
fn without_votes_and_no_credits_keeps_the_rpc_delinquency() {
    assert!(is_down(&performance(true, Some(0), None), Some(10)));
}

fn alpenglow_snapshot(credits_total: Option<u64>) -> ValidatorsPerformanceSnapshot {
    ValidatorsPerformanceSnapshot {
        epoch: 1043,
        epoch_slot: 0,
        transaction_count: 0,
        created_at: String::new(),
        slots_per_year: 0.0,
        cluster_inflation: None,
        validators: HashMap::from([(
            VOTE_ACCOUNT.to_string(),
            performance(true, Some(0), credits_total),
        )]),
        nodes: Default::default(),
        rewards: None,
    }
}

// One sample a minute, as collector-performance writes them. The third
// repeats the second's credits, so the validator stopped earning in between;
// the fourth reads no credits and keeps the RPC delinquency; the fifth
// compares against the credits of the third.
#[test]
fn without_votes_flat_credits_turn_a_validator_down_and_growth_brings_it_back() {
    let start: DateTime<Utc> = "2026-09-30T00:00:00Z".parse().unwrap();
    let mut uptimes = UptimesDoc::new();
    let samples = [Some(100), Some(200), Some(200), None, Some(300)];
    for (minute, credits_total) in samples.into_iter().enumerate() {
        apply_uptime_samples(
            &mut uptimes,
            &alpenglow_snapshot(credits_total),
            start + Duration::minutes(minute as i64),
        );
    }

    let state = &uptimes[VOTE_ACCOUNT];
    let statuses: Vec<UptimeStatus> = state
        .closed
        .iter()
        .chain([&state.open])
        .map(|interval| interval.status)
        .collect();
    assert_eq!(
        statuses,
        vec![UptimeStatus::Up, UptimeStatus::Down, UptimeStatus::Up]
    );
    assert_eq!(state.last_credits, Some(300));
}
