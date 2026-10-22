mod common;

use collect::solana_service::EpochCredits;
use common::{migrated_client, skip_without_database, store_snapshot, validator_snapshot};
use rust_decimal::Decimal;
use std::collections::HashMap;
use store::credits_backfill::{apply_credits_backfill, TESTNET_LAMPORTS_IN_CREDITS_EPOCHS};
use tokio_postgres::Client;

const LAMPORTS: u64 = 300_000_000_000;

async fn seed(client: &mut Client, epoch: u64, vote_account: &str, stake: u64, credits: u64) {
    let mut snapshot = validator_snapshot(epoch, &format!("id-{vote_account}"), vote_account);
    snapshot.validators[0].activated_stake = stake;
    snapshot.validators[0].performance.credits = Some(credits);
    store_snapshot(
        client,
        &format!("credits_backfill_{vote_account}_{epoch}"),
        &snapshot,
    )
    .await;
}

async fn read(
    client: &Client,
    vote_account: &str,
    epoch: u64,
) -> (Option<Decimal>, Option<Decimal>, Decimal) {
    let row = client
        .query_one(
            "SELECT credits, vote_reward_lamports, leader_slots FROM validators WHERE vote_account = $1 AND epoch = $2",
            &[&vote_account, &Decimal::from(epoch)],
        )
        .await
        .unwrap();
    (row.get(0), row.get(1), row.get(2))
}

fn migration_credits(
    entries: &[(&str, Option<u64>, Option<u64>)],
) -> HashMap<String, EpochCredits> {
    entries
        .iter()
        .map(|(vote_account, tower_credits, vote_reward_lamports)| {
            (
                vote_account.to_string(),
                EpochCredits {
                    tower_credits: *tower_credits,
                    vote_reward_lamports: *vote_reward_lamports,
                },
            )
        })
        .collect()
}

#[tokio::test]
async fn backfill_moves_credits_to_vote_reward_lamports() {
    let schema = "credits_backfill_sql";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    for epoch in TESTNET_LAMPORTS_IN_CREDITS_EPOCHS {
        seed(&mut client, epoch, "rewarded", 100, LAMPORTS).await;
    }
    // Testnet 1050 holds rewards of 48 lamports, below the tower limit
    seed(&mut client, 1050, "smallReward", 100, 48).await;
    seed(&mut client, 1043, "zeroReward", 100, 0).await;
    seed(&mut client, 1043, "unstaked", 0, 0).await;
    seed(&mut client, 1043, "unstakedRewarded", 0, LAMPORTS).await;
    seed(&mut client, 1041, "tower", 100, 65_000).await;
    // Testnet 1042: CRXYbE3W has no marker and no entry of 1042
    seed(&mut client, 1042, "noVote", 100, 0).await;
    seed(&mut client, 1042, "closed", 100, LAMPORTS).await;
    let on_chain = migration_credits(&[
        ("rewarded", Some(64_638), Some(LAMPORTS)),
        ("noVote", Some(0), None),
    ]);

    apply_credits_backfill(&mut client, &on_chain)
        .await
        .unwrap();

    let lamports = Some(Decimal::from(LAMPORTS));
    let leader_slots = Decimal::from(100);
    assert_eq!(
        read(&client, "rewarded", 1042).await,
        (Some(Decimal::from(64_638)), lamports, leader_slots)
    );
    for epoch in 1043..=*TESTNET_LAMPORTS_IN_CREDITS_EPOCHS.end() {
        assert_eq!(
            read(&client, "rewarded", epoch).await,
            (None, lamports, leader_slots)
        );
    }
    assert_eq!(
        read(&client, "noVote", 1042).await,
        (Some(Decimal::ZERO), Some(Decimal::ZERO), leader_slots)
    );
    assert_eq!(
        read(&client, "closed", 1042).await,
        (None, lamports, leader_slots)
    );
    assert_eq!(
        read(&client, "smallReward", 1050).await,
        (None, Some(Decimal::from(48)), leader_slots)
    );
    assert_eq!(
        read(&client, "zeroReward", 1043).await,
        (None, Some(Decimal::ZERO), leader_slots)
    );
    assert_eq!(
        read(&client, "unstaked", 1043).await,
        (None, None, leader_slots)
    );
    // No stake in 1043, but the reward of 1043 is paid on the stake of 1042
    assert_eq!(
        read(&client, "unstakedRewarded", 1043).await,
        (None, lamports, leader_slots)
    );
    assert_eq!(
        read(&client, "tower", 1041).await,
        (Some(Decimal::from(65_000)), None, leader_slots)
    );

    assert!(apply_credits_backfill(&mut client, &on_chain)
        .await
        .is_err());
}

#[tokio::test]
async fn backfill_fails_without_lamports_in_credits() {
    let schema = "credits_backfill_tower_sql";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    for epoch in TESTNET_LAMPORTS_IN_CREDITS_EPOCHS {
        seed(&mut client, epoch, "notTestnet", 100, LAMPORTS).await;
    }
    client
        .execute(
            "UPDATE validators SET credits = 65000 WHERE epoch = 1053",
            &[],
        )
        .await
        .unwrap();

    let on_chain = migration_credits(&[("notTestnet", Some(64_638), Some(LAMPORTS))]);
    assert!(apply_credits_backfill(&mut client, &on_chain)
        .await
        .is_err());
    assert_eq!(
        read(&client, "notTestnet", 1042).await,
        (Some(Decimal::from(LAMPORTS)), None, Decimal::from(100))
    );
}

#[tokio::test]
async fn backfill_fails_when_the_stored_reward_is_not_on_chain() {
    let schema = "credits_backfill_mismatch_sql";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    for epoch in TESTNET_LAMPORTS_IN_CREDITS_EPOCHS {
        seed(&mut client, epoch, "mismatch", 100, LAMPORTS).await;
    }
    let on_chain = migration_credits(&[("mismatch", Some(64_638), Some(LAMPORTS + 1))]);

    assert!(apply_credits_backfill(&mut client, &on_chain)
        .await
        .is_err());
    assert_eq!(
        read(&client, "mismatch", 1042).await,
        (Some(Decimal::from(LAMPORTS)), None, Decimal::from(100))
    );
}

#[tokio::test]
async fn backfill_fails_without_tower_credits_on_chain() {
    let schema = "credits_backfill_no_tower_sql";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    for epoch in TESTNET_LAMPORTS_IN_CREDITS_EPOCHS {
        seed(&mut client, epoch, "noTower", 100, LAMPORTS).await;
    }
    let on_chain = migration_credits(&[("noTower", None, Some(LAMPORTS))]);

    assert!(apply_credits_backfill(&mut client, &on_chain)
        .await
        .is_err());
    assert_eq!(
        read(&client, "noTower", 1042).await,
        (Some(Decimal::from(LAMPORTS)), None, Decimal::from(100))
    );
}
