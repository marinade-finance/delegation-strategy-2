mod common;

use common::{migrated_client, skip_without_database};
use rust_decimal::Decimal;
use store::dto::UnstakeHint;
use store::scoring::load_unstake_hints;
use store::utils::{load_validators, load_validators_aggregated_flat, ValidatorOverlays};
use store::vote_performance::load_vote_performance;
use tokio_postgres::Client;

const LAST_EPOCH: u64 = 1000;
const EPOCHS: u64 = 7;
const TOWER_EPOCHS: [u64; 3] = [994, 995, 996];
const MIGRATION_EPOCH: u64 = 997;
const ALPENGLOW_EPOCHS: [u64; 3] = [998, 999, 1000];

struct Row {
    vote_account: &'static str,
    stake: u64,
    leader_slots: u64,
    commission: i32,
    tower_credits: u64,
    migration_credits: u64,
    migration_reward: u64,
    reward: u64,
}

// S = 1000, N = 100, so w is 0.2 for "up" and "down" and 1.6 for "large"
const ROWS: [Row; 3] = [
    Row {
        vote_account: "up",
        stake: 100,
        leader_slots: 10,
        commission: 0,
        tower_credits: 1000,
        migration_credits: 500,
        migration_reward: 500,
        reward: 1000,
    },
    Row {
        vote_account: "down",
        stake: 100,
        leader_slots: 10,
        commission: 0,
        tower_credits: 600,
        migration_credits: 0,
        migration_reward: 0,
        reward: 250,
    },
    Row {
        vote_account: "large",
        stake: 800,
        leader_slots: 80,
        commission: 10,
        tower_credits: 1000,
        migration_credits: 500,
        migration_reward: 4000,
        reward: 8000,
    },
];

// (100·1000 + 100·600 + 800·1000) / 1000
const TOWER_MEAN_CREDITS: f64 = 960.0;
// Σreward / Σw = 9250 / 2
const MEAN_REWARD_PER_WEIGHT: f64 = 4625.0;

#[allow(clippy::too_many_arguments)]
async fn insert_validator(
    client: &Client,
    vote_account: &str,
    epoch: u64,
    stake: u64,
    leader_slots: u64,
    commission: i32,
    credits: Option<u64>,
    vote_reward_lamports: Option<u64>,
) {
    client
        .execute(
            "INSERT INTO validators (
                identity, vote_account, epoch, activated_stake, marinade_stake,
                marinade_native_stake, superminority, stake_to_become_superminority, credits,
                vote_reward_lamports, leader_slots, blocks_produced, skip_rate,
                commission_advertised, commission_effective, updated_at
            ) VALUES ($1, $2, $3, $4, 0, 0, false, 0, $5, $6, $7, $7, 0, $8, $8, NOW())",
            &[
                &format!("identity-{vote_account}"),
                &vote_account,
                &Decimal::from(epoch),
                &Decimal::from(stake),
                &credits.map(Decimal::from),
                &vote_reward_lamports.map(Decimal::from),
                &Decimal::from(leader_slots),
                &commission,
            ],
        )
        .await
        .unwrap();
}

async fn seed(client: &Client) {
    for row in ROWS.iter() {
        let insert = |epoch, credits, reward| {
            insert_validator(
                client,
                row.vote_account,
                epoch,
                row.stake,
                row.leader_slots,
                row.commission,
                credits,
                reward,
            )
        };
        for epoch in TOWER_EPOCHS {
            insert(epoch, Some(row.tower_credits), None).await;
        }
        insert(
            MIGRATION_EPOCH,
            Some(row.migration_credits),
            Some(row.migration_reward),
        )
        .await;
        for epoch in ALPENGLOW_EPOCHS {
            insert(epoch, None, Some(row.reward)).await;
        }
    }
    client
        .execute(
            "INSERT INTO cluster_info (epoch_slot, epoch, transaction_count, created_at)
             VALUES (0, $1, 0, NOW())",
            &[&Decimal::from(LAST_EPOCH)],
        )
        .await
        .unwrap();
}

fn tower_performance(row: &Row) -> f64 {
    row.tower_credits as f64 / TOWER_MEAN_CREDITS
}

fn alpenglow_performance(row: &Row) -> f64 {
    let weight = row.stake as f64 / 1000.0 + row.leader_slots as f64 / 100.0;
    row.reward as f64 / weight / MEAN_REWARD_PER_WEIGHT
}

fn row(vote_account: &str) -> &'static Row {
    ROWS.iter()
        .find(|row| row.vote_account == vote_account)
        .unwrap()
}

fn assert_close(actual: f64, expected: f64) {
    assert!((actual - expected).abs() < 1e-9, "{actual} != {expected}");
}

async fn hints(client: &Client, epoch: u64) -> Vec<(String, Vec<UnstakeHint>)> {
    let blacklist = std::env::temp_dir().join(format!("ds-test-blacklist-{epoch}.csv"));
    std::fs::write(&blacklist, "vote_account,code\n").unwrap();
    let mut hints: Vec<_> = load_unstake_hints(client, &blacklist.display().to_string(), epoch)
        .await
        .unwrap()
        .into_iter()
        .map(|(vote_account, hints)| (vote_account, hints.into_iter().collect()))
        .collect();
    hints.sort_by(|(a, _), (b, _)| a.cmp(b));
    hints
}

#[tokio::test]
async fn low_credits_fires_on_the_alpenglow_ratio_only() {
    let schema = "ds_test_vote_performance_hints";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();
    seed(&client).await;

    // down: 600 / 960 in a tower epoch, 0 credits in the migration epoch, (250 / 0.2) / 4625 after
    assert_eq!(hints(&client, TOWER_EPOCHS[0]).await, vec![]);
    assert_eq!(hints(&client, MIGRATION_EPOCH).await, vec![]);
    assert_eq!(
        hints(&client, ALPENGLOW_EPOCHS[0]).await,
        vec![("down".to_string(), vec![UnstakeHint::LowCredits])]
    );
}

#[tokio::test]
async fn the_flat_aggregate_adds_the_adjusted_vote_performance() {
    let schema = "ds_test_vote_performance_flat";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();
    seed(&client).await;

    let validators = load_validators_aggregated_flat(&client, LAST_EPOCH, EPOCHS)
        .await
        .unwrap();
    for validator in validators.iter() {
        let row = row(&validator.vote_account);
        let staker_share = (100 - row.commission) as f64 / 100.0;
        // The migration epoch has no value, so 3 tower and 3 Alpenglow epochs count
        assert_close(
            validator.avg_adjusted_vote_performance.unwrap(),
            (tower_performance(row) + alpenglow_performance(row)) / 2.0 * staker_share,
        );
        assert_close(
            validator.avg_adjusted_credits,
            (3.0 * row.tower_credits as f64 + row.migration_credits as f64) / 4.0 * staker_share,
        );
    }
    assert_eq!(
        validators.len(),
        2,
        "down has 0 credits in the migration epoch"
    );
}

#[tokio::test]
async fn validators_and_epochs_get_the_vote_performance() {
    let schema = "ds_test_vote_performance_records";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();
    seed(&client).await;

    let records = load_validators(
        &client,
        "http://127.0.0.1:1".to_string(),
        EPOCHS,
        EPOCHS,
        &ValidatorOverlays::default(),
        &Default::default(),
    )
    .await
    .unwrap();
    let performance = load_vote_performance(&client, TOWER_EPOCHS[0], LAST_EPOCH)
        .await
        .unwrap();
    for row in ROWS.iter() {
        assert_close(
            records[row.vote_account].vote_performance.unwrap(),
            alpenglow_performance(row),
        );
        let by_epoch = |epoch| {
            performance
                .get(&(epoch, row.vote_account.to_string()))
                .copied()
        };
        assert_close(by_epoch(TOWER_EPOCHS[0]).unwrap(), tower_performance(row));
        assert_eq!(by_epoch(MIGRATION_EPOCH), None);
        assert_close(by_epoch(LAST_EPOCH).unwrap(), alpenglow_performance(row));
    }
}

#[tokio::test]
async fn alpenglow_reads_the_stake_of_the_previous_epoch() {
    let schema = "ds_test_vote_performance_epoch_stake";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();
    let [previous, epoch] = [ALPENGLOW_EPOCHS[0], ALPENGLOW_EPOCHS[1]];
    // "grown" doubles its stake in `epoch`, its reward is on the 100 of `previous`
    for (vote_account, stake) in [("grown", 200), ("steady", 100)] {
        insert_validator(
            &client,
            vote_account,
            previous,
            100,
            10,
            0,
            None,
            Some(1000),
        )
        .await;
        insert_validator(&client, vote_account, epoch, stake, 10, 0, None, Some(1000)).await;
    }
    // Without a row in the previous epoch
    insert_validator(&client, "new", epoch, 100, 10, 0, None, Some(1000)).await;

    let performance = load_vote_performance(&client, epoch, epoch).await.unwrap();
    assert_close(performance[&(epoch, "grown".to_string())], 1.0);
    assert_close(performance[&(epoch, "steady".to_string())], 1.0);
    assert_eq!(performance.get(&(epoch, "new".to_string())), None);
}
