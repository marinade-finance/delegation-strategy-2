mod common;

use chrono::{DateTime, Duration, Utc};
use common::{migrated_client, skip_without_database};
use rust_decimal::Decimal;
use std::collections::HashMap;
use store::take_rates::{get_take_rate_series, load_epoch_reward_mix};
use store::utils::RewardMixShares;
use tokio_postgres::Client;

const LAST_EPOCH: u64 = 1000;
const EPOCHS: u64 = 120;
const VOTE: &str = "voteTakeRates";

const MIX: RewardMixShares = RewardMixShares {
    inflation: 0.90,
    mev: 0.04,
    block: 0.06,
};

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

    let no_mix = HashMap::new();
    let series = get_take_rate_series(&client, VOTE, None, &no_mix)
        .await
        .unwrap();
    let epochs: Vec<u64> = series.iter().map(|record| record.epoch).collect();
    assert_eq!(epochs, (first_epoch..=LAST_EPOCH).collect::<Vec<_>>());
    assert!(
        series
            .iter()
            .all(|record| record.expected_take_rate.is_none()),
        "an unloaded reward mix must null the whole series instead of weighting by zero"
    );

    let epoch_without_boundaries = series.last().unwrap();
    assert_eq!(epoch_without_boundaries.epoch, LAST_EPOCH);
    assert_eq!(epoch_without_boundaries.epoch_start_at, None);
    assert_eq!(epoch_without_boundaries.epoch_end_at, None);

    let closed_epoch = &series[0];
    assert_eq!(closed_epoch.epoch_start_at, Some(epoch_start(first_epoch)));

    let bounded = get_take_rate_series(&client, VOTE, Some(LAST_EPOCH - 2), &no_mix)
        .await
        .unwrap();
    let epochs: Vec<u64> = bounded.iter().map(|record| record.epoch).collect();
    assert_eq!(epochs, vec![LAST_EPOCH - 2, LAST_EPOCH - 1, LAST_EPOCH]);

    let other = get_take_rate_series(&client, "voteOther", None, &no_mix)
        .await
        .unwrap();
    assert_eq!(other.len(), 1);

    let unknown = get_take_rate_series(&client, "voteUnknown", None, &no_mix)
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

    // One epoch per case. Every epoch gets a rewards row, so a null can only come from the joins.
    let epochs = 1001..=1006;
    let mut mix = HashMap::new();
    for epoch in epochs.clone() {
        insert_reward(&client, VOTE, epoch, 0.05).await;
        mix.insert(epoch, MIX);
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

    let series = get_take_rate_series(&client, VOTE, None, &mix)
        .await
        .unwrap();
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

    let mix = load_epoch_reward_mix(&client).await.unwrap();

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
