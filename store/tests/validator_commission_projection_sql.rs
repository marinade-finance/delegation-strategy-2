mod common;

use common::{migrated_client, skip_without_database};
use rust_decimal::Decimal;
use std::collections::HashMap;
use store::dto::ValidatorRecord;
use store::utils::{
    load_validators, worst_known_commission, RewardMixShares, TakeRates, ValidatorOverlays,
};

const EPOCH_STALE: u64 = 999;
const EPOCH_CLOSED: u64 = 1000;
const EPOCH_OPEN: u64 = 1001;

const MIX: RewardMixShares = RewardMixShares {
    inflation: 0.90,
    mev: 0.044,
    block: 0.056,
};

fn approx(actual: Option<f64>, expected: f64, context: &str) {
    let actual = actual.unwrap_or_else(|| panic!("expected a rate: {context}"));
    assert!(
        (actual - expected).abs() < 1e-12,
        "expected {expected}, got {actual}: {context}"
    );
}

async fn load(
    client: &tokio_postgres::Client,
    display_epochs: u64,
) -> HashMap<String, ValidatorRecord> {
    let overlays = ValidatorOverlays {
        take_rates: TakeRates {
            measured: Default::default(),
            shares: Some(MIX),
        },
        ..Default::default()
    };
    load_validators(
        client,
        "http://127.0.0.1:1".to_string(),
        display_epochs,
        2,
        &overlays,
    )
    .await
    .unwrap()
}

// The record projects one row per validator and the newest epoch is always still open, where the
// three epoch-close commission columns are null.
#[tokio::test]
async fn load_validators_projects_commissions_from_the_newest_closed_epoch() {
    let schema = "ds_test_load_validators_commission_projection";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    client
        .execute(
            "INSERT INTO cluster_info (epoch_slot, epoch, transaction_count, created_at)
             VALUES (1, $1, 0, NOW()), (1, $2, 0, NOW())",
            &[&Decimal::from(EPOCH_CLOSED), &Decimal::from(EPOCH_OPEN)],
        )
        .await
        .unwrap();

    // voteGamer advertises 0 in the open epoch but the closed epoch caught it at 100.
    client
        .execute(
            "INSERT INTO validators (
                identity, vote_account, epoch, activated_stake, marinade_stake,
                marinade_native_stake, superminority, stake_to_become_superminority, credits,
                leader_slots, blocks_produced, skip_rate, updated_at,
                commission_advertised, commission_max_observed, commission_min_observed,
                commission_effective
            ) VALUES
                ('identityGamer', 'voteGamer', $1, 100, 0, 0, false, 0, 0, 0, 0, 0, NOW(), 100, 100, 0, 100),
                ('identityGamer', 'voteGamer', $2, 100, 0, 0, false, 0, 0, 0, 0, 0, NOW(), 0, NULL, NULL, NULL),
                ('identityHonest', 'voteHonest', $1, 100, 0, 0, false, 0, 0, 0, 0, 0, NOW(), 0, 0, 0, 0),
                ('identityHonest', 'voteHonest', $2, 100, 0, 0, false, 0, 0, 0, 0, 0, NOW(), 0, NULL, NULL, NULL),
                ('identityNew', 'voteNew', $2, 100, 0, 0, false, 0, 0, 0, 0, 0, NOW(), 5, NULL, NULL, NULL)",
            &[&Decimal::from(EPOCH_CLOSED), &Decimal::from(EPOCH_OPEN)],
        )
        .await
        .unwrap();

    let validators = load(&client, 2).await;

    let gamer = validators.get("voteGamer").expect("voteGamer must load");
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

    let honest = validators.get("voteHonest").expect("voteHonest must load");
    assert_eq!(
        (
            honest.commission_max_observed,
            honest.commission_min_observed,
            honest.commission_effective
        ),
        (Some(0), Some(0), Some(0)),
        "a genuine zero must project as a zero, not as unknown"
    );

    let new = validators.get("voteNew").expect("voteNew must load");
    assert_eq!(
        (
            new.commission_max_observed,
            new.commission_min_observed,
            new.commission_effective
        ),
        (None, None, None),
        "a validator with no closed epoch yet has nothing to project"
    );

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}

#[tokio::test]
async fn expected_take_rate_reads_the_rate_a_commission_gamer_actually_charges() {
    let schema = "ds_test_expected_take_rate_commission_gamer";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    client
        .execute(
            "INSERT INTO cluster_info (epoch_slot, epoch, transaction_count, created_at)
             VALUES (1, $1, 0, NOW()), (1, $2, 0, NOW())",
            &[&Decimal::from(EPOCH_CLOSED), &Decimal::from(EPOCH_OPEN)],
        )
        .await
        .unwrap();
    client
        .execute(
            "INSERT INTO validators (
                identity, vote_account, epoch, activated_stake, marinade_stake,
                marinade_native_stake, superminority, stake_to_become_superminority, credits,
                leader_slots, blocks_produced, skip_rate, updated_at,
                commission_advertised, commission_max_observed, commission_min_observed,
                commission_effective
            ) VALUES
                ('identityGamer', 'voteGamer', $1, 100, 0, 0, false, 0, 0, 0, 0, 0, NOW(), 100, 100, 0, 100),
                ('identityGamer', 'voteGamer', $2, 100, 0, 0, false, 0, 0, 0, 0, 0, NOW(), 0, NULL, NULL, NULL),
                ('identityFree', 'voteFree', $1, 100, 0, 0, false, 0, 0, 0, 0, 0, NOW(), 0, 0, 0, 0),
                ('identityFree', 'voteFree', $2, 100, 0, 0, false, 0, 0, 0, 0, 0, NOW(), 0, NULL, NULL, NULL),
                ('identityRaised', 'voteRaised', $1, 100, 0, 0, false, 0, 0, 0, 0, 0, NOW(), 5, 5, 5, 5),
                ('identityRaised', 'voteRaised', $2, 100, 0, 0, false, 0, 0, 0, 0, 0, NOW(), 10, NULL, NULL, NULL)",
            &[&Decimal::from(EPOCH_CLOSED), &Decimal::from(EPOCH_OPEN)],
        )
        .await
        .unwrap();

    let validators = load(&client, 2).await;

    // No Jito rows, so the MEV weight renormalizes out and only inflation and block remain.
    let weight = MIX.inflation + MIX.block;
    let gamer = validators.get("voteGamer").unwrap().expected_take_rate;
    let free = validators.get("voteFree").unwrap().expected_take_rate;

    approx(gamer, 1.0, "a validator observed at 100% keeps everything");
    approx(
        free,
        MIX.block / weight,
        "a genuinely free validator floors at the block share",
    );
    assert_ne!(
        gamer, free,
        "reading commission_advertised is what used to tie a gamer to the honest floor"
    );

    approx(
        validators.get("voteRaised").unwrap().expected_take_rate,
        (0.10 * MIX.inflation + MIX.block) / weight,
        "a rise advertised in the open epoch counts before the epoch closes",
    );

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}

// A validator that rugged two epochs ago and has charged 5 since: with close_epoch yet to run for
// the epoch below the open one, the only populated row left is the superseded one.
#[tokio::test]
async fn load_validators_does_not_reach_past_the_newest_closed_epoch_for_commission() {
    let schema = "ds_test_load_validators_commission_projection_bound";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    client
        .execute(
            "INSERT INTO cluster_info (epoch_slot, epoch, transaction_count, created_at)
             VALUES (1, $1, 0, NOW()), (1, $2, 0, NOW()), (1, $3, 0, NOW())",
            &[
                &Decimal::from(EPOCH_STALE),
                &Decimal::from(EPOCH_CLOSED),
                &Decimal::from(EPOCH_OPEN),
            ],
        )
        .await
        .unwrap();

    client
        .execute(
            "INSERT INTO validators (
                identity, vote_account, epoch, activated_stake, marinade_stake,
                marinade_native_stake, superminority, stake_to_become_superminority, credits,
                leader_slots, blocks_produced, skip_rate, updated_at,
                commission_advertised, commission_max_observed, commission_min_observed,
                commission_effective
            ) VALUES
                ('identityReformed', 'voteReformed', $1, 100, 0, 0, false, 0, 0, 0, 0, 0, NOW(), 100, 100, 0, 100),
                ('identityReformed', 'voteReformed', $2, 100, 0, 0, false, 0, 0, 0, 0, 0, NOW(), 5, NULL, NULL, NULL),
                ('identityReformed', 'voteReformed', $3, 100, 0, 0, false, 0, 0, 0, 0, 0, NOW(), 5, NULL, NULL, NULL),
                ('identityDeparted', 'voteDeparted', $1, 100, 0, 0, false, 0, 0, 0, 0, 0, NOW(), 7, 7, 7, 7),
                ('identityDeparted', 'voteDeparted', $2, 100, 0, 0, false, 0, 0, 0, 0, 0, NOW(), 7, NULL, NULL, NULL)",
            &[
                &Decimal::from(EPOCH_STALE),
                &Decimal::from(EPOCH_CLOSED),
                &Decimal::from(EPOCH_OPEN),
            ],
        )
        .await
        .unwrap();

    let validators = load(&client, 3).await;
    let reformed = validators
        .get("voteReformed")
        .expect("voteReformed must load");

    assert_eq!(
        (
            reformed.commission_max_observed,
            reformed.commission_min_observed,
            reformed.commission_effective
        ),
        (None, None, None),
        "the walk must stop one epoch below the record's own instead of reaching two epochs back"
    );
    assert_eq!(
        reformed.commission_advertised,
        Some(5),
        "commission_advertised still comes from the open epoch that seeded the record"
    );
    assert_eq!(
        worst_known_commission(
            reformed.commission_max_observed,
            reformed.commission_advertised
        ),
        Some(5),
        "an unknown ceiling leaves the fresh advertised rate in charge rather than blanking it"
    );
    approx(
        reformed.expected_take_rate,
        (0.05 * MIX.inflation + MIX.block) / (MIX.inflation + MIX.block),
        "bounding the walk must not cost the validator its take rate",
    );

    // voteDeparted left the set an epoch ago, so its newest closed epoch is two below the cluster's
    // tip: reachable from its own seeding row, but not from a bound measured against the tip.
    let departed = validators
        .get("voteDeparted")
        .expect("voteDeparted must load");
    assert_eq!(
        (
            departed.commission_max_observed,
            departed.commission_min_observed,
            departed.commission_effective
        ),
        (Some(7), Some(7), Some(7)),
        "the bound is one epoch below the record's own seeding epoch, not below the cluster's tip"
    );

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}
