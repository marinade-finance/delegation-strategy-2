mod common;

use chrono::{DateTime, Duration, Utc};
use clap::Parser;
use common::{migrated_client, skip_without_database};
use rust_decimal::Decimal;
use std::collections::HashMap;
use store::take_rates::{
    get_take_rate_series, load_epoch_reward_mix, load_epoch_take_rates,
    refresh_expected_take_rates, refresh_stored_expected_take_rates, EpochTakeRates,
    RefreshExpectedTakeRatesParams,
};
use tokio_postgres::Client;

const LAST_EPOCH: u64 = 1000;
const EPOCHS: u64 = 120;
const VOTE: &str = "voteTakeRates";

fn approx(actual: Option<f64>, expected: f64, context: &str) {
    let actual = actual.unwrap_or_else(|| panic!("{context} must not be null"));
    assert!(
        (actual - expected).abs() < 1e-12,
        "{context}: {actual} != {expected}"
    );
}

fn epoch_start(epoch: u64) -> DateTime<Utc> {
    "2024-01-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap() + Duration::days(2 * epoch as i64)
}

async fn insert_reward(client: &Client, vote_account: &str, epoch: u64, take_rate: f64) {
    client
        .execute(
            "INSERT INTO validators_rewards (
                vote_account, epoch, validator_rewards, total_rewards, inflation_rewards,
                mev_rewards, block_rewards, take_rate, created_at, updated_at
            ) VALUES ($1, $2, 10, 100, 100, 0, 0, $3, NOW(), NOW())",
            &[&vote_account, &Decimal::from(epoch), &take_rate],
        )
        .await
        .unwrap();
}

async fn insert_reward_components(
    client: &Client,
    vote_account: &str,
    epoch: u64,
    inflation: i64,
    mev: i64,
    block: i64,
) {
    client
        .execute(
            "INSERT INTO validators_rewards (
                vote_account, epoch, validator_rewards, total_rewards, inflation_rewards,
                mev_rewards, block_rewards, take_rate, created_at, updated_at
            ) VALUES (
                $1, $2, 0,
                $3::NUMERIC + $4::NUMERIC + $5::NUMERIC,
                $3::NUMERIC, $4::NUMERIC, $5::NUMERIC,
                0, NOW(), NOW()
            )",
            &[
                &vote_account,
                &Decimal::from(epoch),
                &Decimal::from(inflation),
                &Decimal::from(mev),
                &Decimal::from(block),
            ],
        )
        .await
        .unwrap();
}

async fn insert_validator(
    client: &Client,
    epoch: u64,
    commission_advertised: Option<i32>,
    commission_max_observed: Option<i32>,
) {
    client
        .execute(
            "INSERT INTO validators (
                identity, vote_account, epoch, activated_stake, marinade_stake,
                marinade_native_stake, superminority, stake_to_become_superminority, credits,
                leader_slots, blocks_produced, skip_rate, updated_at,
                commission_advertised, commission_max_observed
            ) VALUES ('identityTakeRates', $1, $2, 100, 0, 0, false, 0, 0, 0, 0, 0, NOW(), $3, $4)",
            &[
                &VOTE,
                &Decimal::from(epoch),
                &commission_advertised,
                &commission_max_observed,
            ],
        )
        .await
        .unwrap();
}

async fn insert_mev(
    client: &Client,
    epoch: u64,
    mev_commission_bps: i32,
    created_at: DateTime<Utc>,
) {
    client
        .execute(
            "INSERT INTO mev (vote_account, mev_commission, epoch_slot, epoch, created_at)
             VALUES ($1, $2, 1, $3, $4)",
            &[
                &VOTE,
                &mev_commission_bps,
                &Decimal::from(epoch),
                &created_at,
            ],
        )
        .await
        .unwrap();
}

async fn insert_priority_fee(client: &Client, epoch: u64, priority_commission_bps: i32) {
    client
        .execute(
            "INSERT INTO jito_priority_fee (
                vote_account, validator_commission, total_lamports_transferred, epoch_slot, epoch,
                created_at
            ) VALUES ($1, $2, 0, 1, $3, NOW())",
            &[&VOTE, &priority_commission_bps, &Decimal::from(epoch)],
        )
        .await
        .unwrap();
}

async fn insert_epoch(client: &Client, epoch: u64) {
    client
        .execute(
            "INSERT INTO epochs (
                epoch, start_at, end_at, transaction_count, supply, inflation, inflation_taper,
                slots_per_year
            ) VALUES ($1, $2, $3, 0, 0, 0, 0, 0)",
            &[
                &Decimal::from(epoch),
                &epoch_start(epoch),
                &(epoch_start(epoch) + Duration::days(2)),
            ],
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn take_rate_series_spans_the_whole_stored_history() {
    let schema = "ds_test_take_rate_series";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    let first_epoch = LAST_EPOCH - EPOCHS + 1;
    for epoch in first_epoch..=LAST_EPOCH {
        insert_reward(&client, VOTE, epoch, 0.05).await;
        // No `epochs` row for the last one, so its boundaries come back null.
        if epoch < LAST_EPOCH {
            insert_epoch(&client, epoch).await;
        }
    }
    insert_reward(&client, "voteOther", LAST_EPOCH, 0.1).await;

    let series = get_take_rate_series(&client, VOTE, None).await.unwrap();
    let epochs: Vec<u64> = series.iter().map(|record| record.epoch).collect();
    assert_eq!(epochs, (first_epoch..=LAST_EPOCH).collect::<Vec<_>>());
    assert!(
        series
            .iter()
            .all(|record| record.expected_take_rate.is_none()),
        "a row no writer has refreshed reads null instead of a rate computed on read"
    );

    let epoch_without_boundaries = series.last().unwrap();
    assert_eq!(epoch_without_boundaries.epoch, LAST_EPOCH);
    assert_eq!(epoch_without_boundaries.epoch_start_at, None);
    assert_eq!(epoch_without_boundaries.epoch_end_at, None);

    let closed_epoch = &series[0];
    assert_eq!(closed_epoch.epoch_start_at, Some(epoch_start(first_epoch)));

    let bounded = get_take_rate_series(&client, VOTE, Some(LAST_EPOCH - 2))
        .await
        .unwrap();
    let epochs: Vec<u64> = bounded.iter().map(|record| record.epoch).collect();
    assert_eq!(epochs, vec![LAST_EPOCH - 2, LAST_EPOCH - 1, LAST_EPOCH]);

    let other = get_take_rate_series(&client, "voteOther", None)
        .await
        .unwrap();
    assert_eq!(other.len(), 1);

    let unknown = get_take_rate_series(&client, "voteUnknown", None)
        .await
        .unwrap();
    assert!(unknown.is_empty());

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}

#[tokio::test]
async fn expected_take_rate_weights_each_epoch_by_its_own_commissions() {
    let schema = "ds_test_take_rate_series_expected";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    // One epoch per case. Every epoch gets a rewards row with a 90/4/6 mix, so a null can only come
    // from the joins.
    let epochs = 1001..=1006;
    for epoch in epochs.clone() {
        insert_reward_components(&client, VOTE, epoch, 90, 4, 6).await;
    }

    // 1001: all three commissions known.
    insert_validator(&client, 1001, Some(10), Some(10)).await;
    insert_mev(&client, 1001, 800, Utc::now()).await;
    insert_priority_fee(&client, 1001, 2000).await;

    // 1002: no `mev` row, so the MEV share leaves the denominator.
    insert_validator(&client, 1002, Some(10), Some(10)).await;
    insert_priority_fee(&client, 1002, 2000).await;

    // 1003: no `jito_priority_fee` row, so every block reward counts as kept.
    insert_validator(&client, 1003, Some(10), Some(10)).await;
    insert_mev(&client, 1003, 800, Utc::now()).await;

    // 1004: no `validators` row at all.
    insert_mev(&client, 1004, 800, Utc::now()).await;
    insert_priority_fee(&client, 1004, 2000).await;

    // 1005: advertised 0 but the epoch was caught at 100.
    insert_validator(&client, 1005, Some(0), Some(100)).await;
    insert_mev(&client, 1005, 0, Utc::now()).await;

    // 1006: two snapshots for the same epoch.
    insert_validator(&client, 1006, Some(10), Some(10)).await;
    insert_mev(&client, 1006, 800, Utc::now() - Duration::hours(1)).await;
    insert_mev(&client, 1006, 300, Utc::now()).await;
    insert_priority_fee(&client, 1006, 2000).await;

    refresh_expected_take_rates(&client, &Vec::from_iter(epochs.clone()))
        .await
        .unwrap();
    let series = get_take_rate_series(&client, VOTE, None).await.unwrap();
    let rates: HashMap<u64, Option<f64>> = series
        .iter()
        .map(|record| (record.epoch, record.expected_take_rate))
        .collect();
    assert_eq!(series.len(), epochs.count());

    approx(rates[&1001], 0.1052, "all three commissions known");
    approx(
        rates[&1002],
        0.102 / 0.96,
        "a validator with no MEV account must not be credited a zero MEV commission",
    );
    approx(
        rates[&1003],
        0.1532,
        "block rewards are wholly kept without a PriorityFeeDistribution account",
    );
    assert_eq!(
        rates[&1004], None,
        "an epoch with no commission recorded must read null, not zero"
    );
    approx(
        rates[&1005],
        0.96,
        "the epoch's ceiling must win over what it advertised",
    );
    approx(
        rates[&1006],
        0.1032,
        "the newest snapshot of the epoch wins",
    );

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}

async fn stored_expected_take_rate(client: &Client, vote_account: &str, epoch: u64) -> Option<f64> {
    client
        .query_one(
            "SELECT expected_take_rate FROM validators_rewards WHERE vote_account = $1 AND epoch = $2",
            &[&vote_account, &Decimal::from(epoch)],
        )
        .await
        .unwrap()
        .get(0)
}

#[tokio::test]
async fn refresh_stores_each_rows_expected_take_rate_under_its_epochs_mix() {
    let schema = "ds_test_refresh_expected_take_rates";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    // Rewards components 90/4/6 make the epoch mix 0.90/0.04/0.06.
    for epoch in [3001, 3002, 3003] {
        insert_reward_components(&client, VOTE, epoch, 90, 4, 6).await;
    }
    insert_validator(&client, 3001, Some(10), Some(10)).await;
    insert_mev(&client, 3001, 800, Utc::now()).await;
    insert_priority_fee(&client, 3001, 2000).await;
    insert_validator(&client, 3002, Some(10), Some(10)).await;
    insert_priority_fee(&client, 3002, 2000).await;

    assert_eq!(
        stored_expected_take_rate(&client, VOTE, 3001).await,
        None,
        "nothing is stored before a refresh"
    );
    assert_eq!(
        refresh_expected_take_rates(&client, &[3001, 3002, 3003])
            .await
            .unwrap(),
        3
    );

    approx(
        stored_expected_take_rate(&client, VOTE, 3001).await,
        0.1052,
        "all three commissions known",
    );
    approx(
        stored_expected_take_rate(&client, VOTE, 3002).await,
        0.102 / 0.96,
        "no MEV account leaves the MEV share out",
    );
    assert_eq!(
        stored_expected_take_rate(&client, VOTE, 3003).await,
        None,
        "no commission recorded stores null, not zero"
    );

    // Another validator's rewards change the epoch mix to 90/4/106, so VOTE's rate moves too.
    insert_reward_components(&client, "voteOther", 3001, 0, 0, 100).await;
    refresh_expected_take_rates(&client, &[3001]).await.unwrap();
    approx(
        stored_expected_take_rate(&client, VOTE, 3001).await,
        (0.1 * 90.0 + 0.08 * 4.0 + 0.2 * 106.0) / 200.0,
        "a row added to the epoch reweights every row of it",
    );

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}

#[tokio::test]
async fn backfill_refreshes_every_stored_epoch_from_the_given_one() {
    let schema = "ds_test_backfill_expected_take_rates";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    for epoch in [3001, 3002, 3003] {
        insert_reward_components(&client, VOTE, epoch, 90, 4, 6).await;
        insert_validator(&client, epoch, Some(10), Some(10)).await;
        insert_mev(&client, epoch, 800, Utc::now()).await;
        insert_priority_fee(&client, epoch, 2000).await;
    }

    refresh_stored_expected_take_rates(
        RefreshExpectedTakeRatesParams::parse_from(["refresh", "--from-epoch", "3002"]),
        &client,
    )
    .await
    .unwrap();

    assert_eq!(
        stored_expected_take_rate(&client, VOTE, 3001).await,
        None,
        "an epoch before --from-epoch is left alone"
    );
    for epoch in [3002, 3003] {
        approx(
            stored_expected_take_rate(&client, VOTE, epoch).await,
            0.1052,
            &format!("epoch {epoch} is filled"),
        );
    }

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}

#[tokio::test]
async fn epoch_reward_mix_splits_each_epoch_across_its_components() {
    let schema = "ds_test_epoch_reward_mix";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    insert_reward_components(&client, VOTE, 2001, 60, 20, 20).await;
    insert_reward_components(&client, "voteOther", 2001, 40, 0, 0).await;
    insert_reward_components(&client, VOTE, 2002, 50, 25, 25).await;
    insert_reward_components(&client, VOTE, 2003, 0, 0, 0).await;
    insert_reward_components(&client, VOTE, 2004, 0, 0, 50).await;

    let mix = load_epoch_reward_mix(&client, &[2001, 2002, 2003, 2004].map(Decimal::from))
        .await
        .unwrap();

    let shared = mix[&2001];
    approx(
        Some(shared.inflation),
        100.0 / 140.0,
        "2001 inflation share",
    );
    approx(Some(shared.mev), 20.0 / 140.0, "2001 mev share");
    approx(Some(shared.block), 20.0 / 140.0, "2001 block share");

    let single = mix[&2002];
    approx(Some(single.inflation), 0.5, "2002 inflation share");
    approx(Some(single.mev), 0.25, "2002 mev share");
    approx(Some(single.block), 0.25, "2002 block share");

    assert!(
        !mix.contains_key(&2003),
        "an epoch that paid nothing has no mix to weight by"
    );

    assert!(
        !mix.contains_key(&2004),
        "an in-progress epoch has only accruing block rewards, which would weight out every commission"
    );

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}

#[tokio::test]
async fn epoch_take_rates_cover_the_cached_window_only() {
    let schema = "ds_test_epoch_take_rates";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    client
        .execute(
            "INSERT INTO cluster_info (epoch_slot, epoch, transaction_count, created_at)
             VALUES (1, $1, 0, NOW())",
            &[&Decimal::from(LAST_EPOCH)],
        )
        .await
        .unwrap();
    insert_reward(&client, VOTE, LAST_EPOCH, 0.05).await;
    insert_reward(&client, VOTE, LAST_EPOCH - 2, 0.07).await;
    insert_reward(&client, VOTE, LAST_EPOCH - 3, 0.09).await;
    insert_reward(&client, "voteOther", LAST_EPOCH - 1, 1.0).await;
    client
        .execute(
            "UPDATE validators_rewards SET expected_take_rate = 0.06 WHERE epoch = $1",
            &[&Decimal::from(LAST_EPOCH - 2)],
        )
        .await
        .unwrap();

    let rates = |realized, expected| EpochTakeRates { realized, expected };
    assert_eq!(
        load_epoch_take_rates(&client, 3).await.unwrap(),
        HashMap::from([
            ((VOTE.to_string(), LAST_EPOCH), rates(0.05, None)),
            ((VOTE.to_string(), LAST_EPOCH - 2), rates(0.07, Some(0.06))),
            (("voteOther".to_string(), LAST_EPOCH - 1), rates(1.0, None)),
        ]),
        "the window ends three epochs back from cluster_info, and the stored expected rate rides along"
    );

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}
