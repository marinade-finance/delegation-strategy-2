mod common;

use common::{migrated_client, skip_without_database};
use rust_decimal::Decimal;
use std::collections::HashMap;
use store::dto::{ValidatorRecord, ValidatorWarning};
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

fn warns_high_commission(record: &ValidatorRecord) -> bool {
    record
        .warnings
        .iter()
        .any(|warning| matches!(warning, ValidatorWarning::HighCommission))
}

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

// SIMD-0232 left commission_effective null from epoch 1031 on, so every warning, rug event and
// scoring input that read it alone went quiet. The three tests below pin the sources that replaced it.
#[tokio::test]
async fn high_commission_warning_survives_a_null_effective_commission() {
    let schema = "ds_test_high_commission_warning_null_effective";
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
                leader_slots, blocks_produced, skip_rate, updated_at, uptime_pct,
                commission_advertised, commission_max_observed, commission_min_observed,
                commission_effective
            ) VALUES
                ('identityDear', 'voteDear', $1, 100, 0, 0, false, 0, 0, 0, 0, 0, NOW(), 1, 12, 12, 12, NULL),
                ('identityDear', 'voteDear', $2, 100, 0, 0, false, 0, 0, 0, 0, 0, NOW(), 1, 12, NULL, NULL, NULL),
                ('identityCheap', 'voteCheap', $1, 100, 0, 0, false, 0, 0, 0, 0, 0, NOW(), 1, 5, 5, 5, NULL),
                ('identityCheap', 'voteCheap', $2, 100, 0, 0, false, 0, 0, 0, 0, 0, NOW(), 1, 5, NULL, NULL, NULL),
                ('identityBlank', 'voteBlank', $1, 100, 0, 0, false, 0, 0, 0, 0, 0, NOW(), 1, NULL, NULL, NULL, NULL),
                ('identityBlank', 'voteBlank', $2, 100, 0, 0, false, 0, 0, 0, 0, 0, NOW(), 1, NULL, NULL, NULL, NULL)",
            &[&Decimal::from(EPOCH_CLOSED), &Decimal::from(EPOCH_OPEN)],
        )
        .await
        .unwrap();

    let validators = load(&client, 2).await;

    assert!(
        warns_high_commission(validators.get("voteDear").unwrap()),
        "a 12% ceiling must still warn when the applied rate is null"
    );
    assert!(
        !warns_high_commission(validators.get("voteCheap").unwrap()),
        "a 5% validator must not warn"
    );
    assert!(
        !warns_high_commission(validators.get("voteBlank").unwrap()),
        "an entirely unknown commission is not evidence of a high one"
    );

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}

#[tokio::test]
async fn load_ruggers_detects_a_rug_after_the_effective_commission_went_null() {
    let schema = "ds_test_load_ruggers_null_effective";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    // voteRugger alternates 5/15 across five epochs with commission_effective null throughout;
    // voteSteady never leaves 5. voteCutter ends both its epochs at 5 with a ceiling of 15, the
    // shape an honest mid-epoch cut leaves - and the one a spike-and-revert leaves too.
    client
        .execute(
            "INSERT INTO validators (
                identity, vote_account, epoch, activated_stake, marinade_stake,
                marinade_native_stake, superminority, stake_to_become_superminority, credits,
                leader_slots, blocks_produced, skip_rate, updated_at,
                commission_advertised, commission_max_observed, commission_min_observed,
                commission_effective
            ) VALUES
                ('identityRugger', 'voteRugger', 1031, 100, 0, 0, false, 0, 0, 0, 0, 0, NOW(), 5, 5, 5, NULL),
                ('identityRugger', 'voteRugger', 1032, 100, 0, 0, false, 0, 0, 0, 0, 0, NOW(), 15, 15, 5, NULL),
                ('identityRugger', 'voteRugger', 1033, 100, 0, 0, false, 0, 0, 0, 0, 0, NOW(), 5, 5, 5, NULL),
                ('identityRugger', 'voteRugger', 1034, 100, 0, 0, false, 0, 0, 0, 0, 0, NOW(), 15, 15, 5, NULL),
                ('identityRugger', 'voteRugger', 1035, 100, 0, 0, false, 0, 0, 0, 0, 0, NOW(), 5, 5, 5, NULL),
                ('identitySteady', 'voteSteady', 1031, 100, 0, 0, false, 0, 0, 0, 0, 0, NOW(), 5, 5, 5, NULL),
                ('identitySteady', 'voteSteady', 1032, 100, 0, 0, false, 0, 0, 0, 0, 0, NOW(), 5, 5, 5, NULL),
                ('identitySteady', 'voteSteady', 1033, 100, 0, 0, false, 0, 0, 0, 0, 0, NOW(), 5, 5, 5, NULL),
                ('identityCutter', 'voteCutter', 1031, 100, 0, 0, false, 0, 0, 0, 0, 0, NOW(), 5, 15, 5, NULL),
                ('identityCutter', 'voteCutter', 1032, 100, 0, 0, false, 0, 0, 0, 0, 0, NOW(), 5, 15, 5, NULL)",
            &[],
        )
        .await
        .unwrap();

    let ruggers = store::utils::load_ruggers(&client).await.unwrap();

    let rugger = ruggers.get("voteRugger").expect("the rug must be detected");
    assert_eq!(rugger.occurrences, 3, "1032, 1033 and 1034 each match");
    assert_eq!(rugger.epochs, vec![1032, 1033, 1034]);
    assert_eq!(rugger.observed_commissions, vec![15, 5, 15]);

    assert!(
        !ruggers.contains_key("voteSteady"),
        "a validator that never crossed 10 must not be flagged"
    );
    assert!(
        !ruggers.contains_key("voteCutter"),
        "both epochs ended at 5, so nothing above 10 was ever charged; reading the ceiling instead flagged this twice"
    );

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}

#[tokio::test]
async fn load_ruggers_skips_a_matching_epoch_whose_floor_is_not_yet_known() {
    let schema = "ds_test_load_ruggers_null_floor";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    // commission_min_observed is written only by update_observed_commission, so every epoch still
    // waiting on close_epoch carries NULL while the next epoch's hourly rows already give it a LEAD.
    client
        .execute(
            "INSERT INTO validators (
                identity, vote_account, epoch, activated_stake, marinade_stake,
                marinade_native_stake, superminority, stake_to_become_superminority, credits,
                leader_slots, blocks_produced, skip_rate, updated_at,
                commission_advertised, commission_max_observed, commission_min_observed,
                commission_effective
            ) VALUES
                ('identityLate', 'voteLate', 1031, 100, 0, 0, false, 0, 0, 0, 0, 0, NOW(), 5, 5, 5, NULL),
                ('identityLate', 'voteLate', 1032, 100, 0, 0, false, 0, 0, 0, 0, 0, NOW(), 15, 15, 5, NULL),
                ('identityLate', 'voteLate', 1033, 100, 0, 0, false, 0, 0, 0, 0, 0, NOW(), 5, 5, 5, NULL),
                ('identityLate', 'voteLate', 1034, 100, 0, 0, false, 0, 0, 0, 0, 0, NOW(), 15, 15, NULL, NULL),
                ('identityLate', 'voteLate', 1035, 100, 0, 0, false, 0, 0, 0, 0, 0, NOW(), 5, 5, NULL, NULL)",
            &[],
        )
        .await
        .unwrap();

    let ruggers = store::utils::load_ruggers(&client).await.unwrap();

    let rugger = ruggers.get("voteLate").expect("the rug must be detected");
    assert_eq!(
        rugger.occurrences, 2,
        "1032 and 1033 match on a known floor; 1034 waits for its epoch to close"
    );
    assert_eq!(rugger.epochs, vec![1032, 1033]);
    assert_eq!(rugger.min_commissions, vec![5, 5]);

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}

#[tokio::test]
async fn collector_flags_and_shared_counts_project_per_epoch() {
    let schema = "ds_test_collector_flags_and_shared_counts";
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

    // voteShareA and voteShareB point at one collector; voteHome keeps its own vote account, so its
    // count must stay 1 rather than being pooled with them. votePreV4 has no collector at all, and
    // must not join the null partition's count. voteShareA redirected only in the open epoch.
    client
        .execute(
            "INSERT INTO validators (
                identity, vote_account, epoch, activated_stake, marinade_stake,
                marinade_native_stake, superminority, stake_to_become_superminority, credits,
                leader_slots, blocks_produced, skip_rate, updated_at,
                inflation_rewards_collector, block_revenue_collector,
                inflation_rewards_commission_bps, inflation_rewards_commission_bps_is_v4,
                block_revenue_commission_bps
            ) VALUES
                ('idShareA', 'voteShareA', $1, 100, 0, 0, false, 0, 0, 0, 0, 0, NOW(), 'voteShareA', 'idShareA', 500, true, 10000),
                ('idShareA', 'voteShareA', $2, 100, 0, 0, false, 0, 0, 0, 0, 0, NOW(), 'sharedCollector', 'idOther', 749, true, 10000),
                ('idShareB', 'voteShareB', $2, 100, 0, 0, false, 0, 0, 0, 0, 0, NOW(), 'sharedCollector', 'idShareB', 300, true, 10000),
                ('idHome', 'voteHome', $2, 100, 0, 0, false, 0, 0, 0, 0, 0, NOW(), 'voteHome', 'idHome', 700, false, 10000),
                ('idPreV4', 'votePreV4', $2, 100, 0, 0, false, 0, 0, 0, 0, 0, NOW(), NULL, NULL, 700, false, NULL)",
            &[&Decimal::from(EPOCH_CLOSED), &Decimal::from(EPOCH_OPEN)],
        )
        .await
        .unwrap();

    let validators = load(&client, 2).await;

    let share_a = validators.get("voteShareA").unwrap();
    assert_eq!(
        share_a.inflation_rewards_collector.as_deref(),
        Some("sharedCollector"),
        "the record reads the open epoch, where the collector was sampled last"
    );
    assert_eq!(share_a.inflation_rewards_collector_redirected, Some(true));
    assert_eq!(share_a.inflation_rewards_collector_shared_count, Some(2));
    assert_eq!(
        share_a.block_revenue_collector_is_identity,
        Some(false),
        "a block revenue collector that is not the current identity reads false"
    );
    assert_eq!(share_a.inflation_rewards_commission_bps, Some(749));
    assert_eq!(share_a.inflation_rewards_commission_bps_is_v4, Some(true));

    let share_b = validators.get("voteShareB").unwrap();
    assert_eq!(
        share_b.inflation_rewards_collector_shared_count,
        Some(2),
        "the count shows on every sharer, so a griefed validator can see it"
    );

    let home = validators.get("voteHome").unwrap();
    assert_eq!(home.inflation_rewards_collector_redirected, Some(false));
    assert_eq!(
        home.inflation_rewards_collector_shared_count,
        Some(1),
        "a validator collecting to itself must not be pooled with the shared partition"
    );
    assert_eq!(home.block_revenue_collector_is_identity, Some(true));
    assert_eq!(home.inflation_rewards_commission_bps_is_v4, Some(false));

    let pre_v4 = validators.get("votePreV4").unwrap();
    assert_eq!(pre_v4.inflation_rewards_collector, None);
    assert_eq!(
        pre_v4.inflation_rewards_collector_redirected, None,
        "absence is not 'not redirected'"
    );
    assert_eq!(
        pre_v4.inflation_rewards_collector_shared_count, None,
        "partitioning on a null collector must not count every pre-v4 validator as sharing one"
    );
    assert_eq!(pre_v4.block_revenue_collector_is_identity, None);
    assert_eq!(pre_v4.block_revenue_commission_bps, None);

    // The per-epoch rows keep each epoch's own sample, where the record keeps only the newest.
    let closed = share_a
        .epoch_stats
        .iter()
        .find(|stat| stat.epoch == EPOCH_CLOSED)
        .expect("the closed epoch must be in the stats");
    assert_eq!(
        closed.inflation_rewards_collector.as_deref(),
        Some("voteShareA")
    );
    assert_eq!(closed.inflation_rewards_collector_redirected, Some(false));
    assert_eq!(
        closed.inflation_rewards_collector_shared_count,
        Some(1),
        "the count is scoped per epoch, and in the closed epoch nothing shared this collector"
    );
    assert_eq!(closed.inflation_rewards_commission_bps, Some(500));

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}
