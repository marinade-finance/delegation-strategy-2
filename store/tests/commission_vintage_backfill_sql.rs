mod common;

use common::{migrated_client, skip_without_database};
use rust_decimal::Decimal;
use tokio_postgres::Client;

const BACKFILL: &str =
    include_str!("../../migrations/0035-commission-effective-vintage-backfill.sql");
const OPEN_EPOCH: u64 = 1041;

type Row = (
    &'static str,
    u64,
    i32,
    Option<i32>,
    Option<&'static str>,
    Option<i32>,
);

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
    for (vote_account, epoch, advertised, effective, source, sampled_bps) in rows {
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
                    vote_account,
                    &Decimal::from(*epoch),
                    advertised,
                    effective,
                    source,
                    sampled_bps,
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

// One line per row, "vote epoch effective bps source max min", so the expected table stays readable.
async fn read_all(client: &Client) -> Vec<String> {
    let show = |value: Option<i32>| value.map_or("-".to_string(), |v| v.to_string());
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
            format!(
                "{} {} {} {} {} {} {}",
                row.get::<_, String>("vote_account"),
                row.get::<_, Decimal>("epoch"),
                show(row.get("commission_effective")),
                show(row.get("commission_effective_bps")),
                row.get::<_, Option<String>>("commission_effective_source")
                    .unwrap_or("-".to_string()),
                show(row.get("commission_max_observed")),
                show(row.get("commission_min_observed")),
            )
        })
        .collect()
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
            ("voteAdvertised", 1031, 0, Some(0), None, None),
            ("voteAdvertised", 1032, 0, Some(0), None, None),
            ("voteAdvertised", 1033, 5, Some(5), None, None),
            ("voteAdvertised", 1034, 5, Some(5), None, None),
            ("voteAdvertised", 1035, 5, Some(5), None, None),
            // close_epoch read each epoch's own last sample.
            ("voteSampled", 1036, 5, Some(5), vote_state, Some(500)),
            ("voteSampled", 1037, 5, Some(5), vote_state, Some(500)),
            ("voteSampled", 1038, 9, Some(9), vote_state, Some(900)),
            ("voteSampled", 1039, 9, Some(9), vote_state, Some(900)),
            ("voteSampled", 1040, 9, Some(9), vote_state, Some(900)),
            ("voteSampled", OPEN_EPOCH, 9, None, None, Some(900)),
            (
                "voteRewardRow",
                1037,
                7,
                Some(3),
                Some("reward_row"),
                Some(700),
            ),
            (
                "voteRewardRow",
                1038,
                7,
                Some(3),
                Some("reward_row"),
                Some(700),
            ),
            ("voteBefore1030", 1028, 9, Some(9), None, None),
            ("voteBefore1030", 1029, 9, Some(7), None, None),
        ],
    )
    .await;

    client.batch_execute(BACKFILL).await.unwrap();
    let resolved = read_all(&client).await;
    assert_eq!(
        resolved,
        vec![
            // The raise to 5 in 1033 applies from 1035, two epochs later.
            "voteAdvertised 1031 0 - - 0 0",
            "voteAdvertised 1032 0 - - 0 0",
            "voteAdvertised 1033 0 - - 5 0",
            "voteAdvertised 1034 0 - - 5 0",
            "voteAdvertised 1035 5 - - 5 5",
            "voteBefore1030 1028 9 - - 9 9",
            "voteBefore1030 1029 7 - - 9 7",
            "voteRewardRow 1037 3 - reward_row 7 3",
            "voteRewardRow 1038 3 - reward_row 7 3",
            // 1036 and 1037 have no E-2 bps and fall back like agave; the raise to 9 lands in 1040.
            "voteSampled 1036 5 500 vote_state 5 5",
            "voteSampled 1037 5 500 vote_state 5 5",
            "voteSampled 1038 5 500 vote_state 9 5",
            "voteSampled 1039 5 500 vote_state 9 5",
            "voteSampled 1040 9 900 vote_state 12 9",
            "voteSampled 1041 - - - 9 9",
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
