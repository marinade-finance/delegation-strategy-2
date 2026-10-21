mod common;

use common::{migrated_client, skip_without_database, store_snapshot, validator_snapshot};
use rust_decimal::Decimal;
use store::credits_backfill::{store_credits_backfill, TESTNET_LAMPORTS_IN_CREDITS_EPOCHS};
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

    store_credits_backfill(&mut client).await.unwrap();

    let lamports = Some(Decimal::from(LAMPORTS));
    let leader_slots = Decimal::from(100);
    for epoch in TESTNET_LAMPORTS_IN_CREDITS_EPOCHS {
        assert_eq!(
            read(&client, "rewarded", epoch).await,
            (None, lamports, leader_slots)
        );
    }
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

    assert!(store_credits_backfill(&mut client).await.is_err());
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

    assert!(store_credits_backfill(&mut client).await.is_err());
    assert_eq!(
        read(&client, "notTestnet", 1042).await,
        (Some(Decimal::from(LAMPORTS)), None, Decimal::from(100))
    );
}
