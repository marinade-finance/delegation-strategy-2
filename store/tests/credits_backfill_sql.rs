mod common;

use collect::credits_backfill::CreditsBackfill;
use collect::solana_service::EpochCredits;
use common::{migrated_client, skip_without_database, store_snapshot, validator_snapshot};
use rust_decimal::Decimal;
use std::collections::{BTreeMap, HashMap};
use store::credits_backfill::backfill_credits;
use tokio_postgres::Client;

// Testnet epochs 1042-1053 were stored with the vote reward lamports in `credits`
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

fn credits(tower_credits: Option<u64>, vote_reward_lamports: Option<u64>) -> EpochCredits {
    EpochCredits {
        tower_credits,
        vote_reward_lamports,
    }
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
async fn backfill_rewrites_only_credits_and_vote_reward_lamports() {
    let schema = "credits_backfill_sql";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    seed(&mut client, 1042, "migration", 100, LAMPORTS).await;
    seed(&mut client, 1043, "rewarded", 100, LAMPORTS).await;
    seed(&mut client, 1043, "zeroReward", 100, 0).await;
    seed(&mut client, 1043, "unstaked", 0, 0).await;
    seed(&mut client, 1043, "unstakedRewarded", 0, LAMPORTS).await;
    seed(&mut client, 1043, "noValue", 100, 0).await;
    seed(&mut client, 1043, "closed", 100, LAMPORTS).await;
    seed(&mut client, 1043, "closedNoEntry", 100, 0).await;
    seed(&mut client, 1043, "closedUnstaked", 0, 0).await;
    seed(&mut client, 1042, "closedTower", 100, 65_000).await;
    seed(&mut client, 1042, "closedRewarded", 100, LAMPORTS).await;

    let backfill = CreditsBackfill {
        epochs: BTreeMap::from([
            (
                1042,
                HashMap::from([("migration".into(), credits(Some(65_027), Some(LAMPORTS)))]),
            ),
            (
                1043,
                HashMap::from([
                    ("rewarded".into(), credits(None, Some(LAMPORTS))),
                    ("zeroReward".into(), credits(None, Some(0))),
                    ("unstaked".into(), credits(None, Some(0))),
                    ("unstakedRewarded".into(), credits(None, Some(LAMPORTS))),
                    ("noValue".into(), credits(None, None)),
                ]),
            ),
        ]),
    };
    backfill_credits(&mut client, &backfill).await.unwrap();

    let lamports = Some(Decimal::from(LAMPORTS));
    let leader_slots = Decimal::from(100);
    assert_eq!(
        read(&client, "migration", 1042).await,
        (Some(Decimal::from(65_027)), lamports, leader_slots)
    );
    assert_eq!(
        read(&client, "rewarded", 1043).await,
        (None, lamports, leader_slots)
    );
    assert_eq!(
        read(&client, "zeroReward", 1043).await,
        (None, Some(Decimal::ZERO), leader_slots)
    );
    assert_eq!(
        read(&client, "unstaked", 1043).await,
        (None, None, leader_slots)
    );
    // No stake in 1043, but the reward of 1043 is paid on the stake of 1042, so it stays
    assert_eq!(
        read(&client, "unstakedRewarded", 1043).await,
        (None, lamports, leader_slots)
    );
    assert_eq!(
        read(&client, "noValue", 1043).await,
        (None, None, leader_slots)
    );
    // A vote account that is not in the file gets its credits moved to vote_reward_lamports
    assert_eq!(
        read(&client, "closed", 1043).await,
        (None, lamports, leader_slots)
    );
    assert_eq!(
        read(&client, "closedNoEntry", 1043).await,
        (None, Some(Decimal::ZERO), leader_slots)
    );
    assert_eq!(
        read(&client, "closedUnstaked", 1043).await,
        (None, None, leader_slots)
    );
    // 1042 has tower credits, so only a value above the tower limit moves
    assert_eq!(
        read(&client, "closedTower", 1042).await,
        (Some(Decimal::from(65_000)), None, leader_slots)
    );
    assert_eq!(
        read(&client, "closedRewarded", 1042).await,
        (None, lamports, leader_slots)
    );
}
