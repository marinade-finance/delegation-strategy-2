mod common;

use common::{migrated_client, skip_without_database};
use rust_decimal::Decimal;
use tokio_postgres::Client;

const BACKFILL: &str =
    include_str!("../../migrations/0035-commission-effective-vintage-backfill.sql");
const OPEN_EPOCH: u64 = 1041;

struct Row {
    vote_account: &'static str,
    epoch: u64,
    advertised: i32,
    effective: Option<i32>,
    source: Option<&'static str>,
    sampled_bps: Option<i32>,
}

async fn seed(client: &Client, rows: &[Row]) {
    for epoch in 1029..OPEN_EPOCH {
        client
            .execute(
                "INSERT INTO epochs (epoch, start_at, end_at, transaction_count, supply, inflation, inflation_taper, slots_per_year)
                 VALUES ($1, NOW(), NOW(), 0, 0, 0, 0.15, 0)",
                &[&Decimal::from(epoch)],
            )
            .await
            .unwrap();
    }
    for row in rows {
        client
            .execute(
                "INSERT INTO validators (
                    identity, vote_account, epoch, activated_stake, marinade_stake,
                    marinade_native_stake, superminority, stake_to_become_superminority, credits,
                    leader_slots, blocks_produced, skip_rate, updated_at,
                    commission_advertised, commission_effective, commission_effective_source,
                    inflation_rewards_commission_bps, commission_max_observed, commission_min_observed
                ) VALUES (
                    $1, $1, $2, 100, 0, 0, false, 0, 0, 0, 0, 0, NOW(),
                    $3::INTEGER, $4::INTEGER, $5::TEXT, $6::INTEGER,
                    GREATEST($3::INTEGER, $4::INTEGER), LEAST($3::INTEGER, $4::INTEGER)
                )",
                &[
                    &row.vote_account,
                    &Decimal::from(row.epoch),
                    &row.advertised,
                    &row.effective,
                    &row.source,
                    &row.sampled_bps,
                ],
            )
            .await
            .unwrap();
    }
    client
        .execute(
            "INSERT INTO commissions (vote_account, commission, epoch_slot, epoch, created_at)
             VALUES ('voteSampled', 12, 100, 1040, NOW())",
            &[],
        )
        .await
        .unwrap();
}

type Resolved = (
    String,
    u64,
    Option<i32>,
    Option<i32>,
    Option<String>,
    Option<i32>,
    Option<i32>,
);

async fn read_all(client: &Client) -> Vec<Resolved> {
    client
        .query(
            "SELECT vote_account, epoch, commission_effective, commission_effective_bps,
                    commission_effective_source, commission_max_observed, commission_min_observed
             FROM validators ORDER BY vote_account, epoch",
            &[],
        )
        .await
        .unwrap()
        .iter()
        .map(|row| {
            (
                row.get("vote_account"),
                row.get::<_, Decimal>("epoch").try_into().unwrap(),
                row.get("commission_effective"),
                row.get("commission_effective_bps"),
                row.get("commission_effective_source"),
                row.get("commission_max_observed"),
                row.get("commission_min_observed"),
            )
        })
        .collect()
}

fn row(
    vote_account: &'static str,
    epoch: u64,
    advertised: i32,
    effective: Option<i32>,
    source: Option<&'static str>,
    sampled_bps: Option<i32>,
) -> Row {
    Row {
        vote_account,
        epoch,
        advertised,
        effective,
        source,
        sampled_bps,
    }
}

fn expected(
    vote_account: &str,
    epoch: u64,
    effective: Option<i32>,
    bps: Option<i32>,
    source: Option<&str>,
    max: Option<i32>,
    min: Option<i32>,
) -> Resolved {
    (
        vote_account.to_string(),
        epoch,
        effective,
        bps,
        source.map(str::to_string),
        max,
        min,
    )
}

#[tokio::test]
async fn backfill_re_resolves_closed_epochs_from_1030_at_the_applied_vintage() {
    let schema = "ds_test_commission_vintage_backfill";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    let vote_state = Some("vote_state");
    seed(
        &client,
        &[
            // Before bps sampling: the 0029 backfill copied each epoch's own advertised rate.
            row("voteAdvertised", 1031, 0, Some(0), None, None),
            row("voteAdvertised", 1032, 0, Some(0), None, None),
            row("voteAdvertised", 1033, 5, Some(5), None, None),
            row("voteAdvertised", 1034, 5, Some(5), None, None),
            row("voteAdvertised", 1035, 5, Some(5), None, None),
            // close_epoch read each epoch's own last sample.
            row("voteSampled", 1036, 5, Some(5), vote_state, Some(500)),
            row("voteSampled", 1037, 5, Some(5), vote_state, Some(500)),
            row("voteSampled", 1038, 9, Some(9), vote_state, Some(900)),
            row("voteSampled", 1039, 9, Some(9), vote_state, Some(900)),
            row("voteSampled", 1040, 9, Some(9), vote_state, Some(900)),
            row("voteSampled", OPEN_EPOCH, 9, None, None, Some(900)),
            row(
                "voteRewardRow",
                1037,
                7,
                Some(3),
                Some("reward_row"),
                Some(700),
            ),
            row(
                "voteRewardRow",
                1038,
                7,
                Some(3),
                Some("reward_row"),
                Some(700),
            ),
            row("voteBefore1030", 1028, 9, Some(9), None, None),
            row("voteBefore1030", 1029, 9, Some(7), None, None),
        ],
    )
    .await;

    client.batch_execute(BACKFILL).await.unwrap();
    let resolved = read_all(&client).await;
    assert_eq!(
        resolved,
        vec![
            // The raise to 5 in 1033 applies from 1035, two epochs later.
            expected(
                "voteAdvertised",
                1031,
                Some(0),
                None,
                None,
                Some(0),
                Some(0)
            ),
            expected(
                "voteAdvertised",
                1032,
                Some(0),
                None,
                None,
                Some(0),
                Some(0)
            ),
            expected(
                "voteAdvertised",
                1033,
                Some(0),
                None,
                None,
                Some(5),
                Some(0)
            ),
            expected(
                "voteAdvertised",
                1034,
                Some(0),
                None,
                None,
                Some(5),
                Some(0)
            ),
            expected(
                "voteAdvertised",
                1035,
                Some(5),
                None,
                None,
                Some(5),
                Some(5)
            ),
            expected(
                "voteBefore1030",
                1028,
                Some(9),
                None,
                None,
                Some(9),
                Some(9)
            ),
            expected(
                "voteBefore1030",
                1029,
                Some(7),
                None,
                None,
                Some(9),
                Some(7)
            ),
            expected(
                "voteRewardRow",
                1037,
                Some(3),
                None,
                Some("reward_row"),
                Some(7),
                Some(3)
            ),
            expected(
                "voteRewardRow",
                1038,
                Some(3),
                None,
                Some("reward_row"),
                Some(7),
                Some(3)
            ),
            // 1036 and 1037 have no E-2 bps and fall back like agave; the raise to 9 lands in 1040.
            expected(
                "voteSampled",
                1036,
                Some(5),
                Some(500),
                vote_state,
                Some(5),
                Some(5)
            ),
            expected(
                "voteSampled",
                1037,
                Some(5),
                Some(500),
                vote_state,
                Some(5),
                Some(5)
            ),
            expected(
                "voteSampled",
                1038,
                Some(5),
                Some(500),
                vote_state,
                Some(9),
                Some(5)
            ),
            expected(
                "voteSampled",
                1039,
                Some(5),
                Some(500),
                vote_state,
                Some(9),
                Some(5)
            ),
            expected(
                "voteSampled",
                1040,
                Some(9),
                Some(900),
                vote_state,
                Some(12),
                Some(9)
            ),
            expected(
                "voteSampled",
                OPEN_EPOCH,
                None,
                None,
                None,
                Some(9),
                Some(9)
            ),
        ],
        "only closed epochs from 1030 on move, reward rows and the open epoch stay as they were"
    );

    client.batch_execute(BACKFILL).await.unwrap();
    assert_eq!(
        read_all(&client).await,
        resolved,
        "a second run must change nothing"
    );

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}
