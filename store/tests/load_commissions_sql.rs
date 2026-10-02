mod common;

use common::{migrated_client, skip_without_database};
use rust_decimal::Decimal;
use store::utils::load_commissions;

const EPOCH: u64 = 1000;

#[tokio::test]
async fn load_commissions_carries_bps_only_on_a_vote_state_applied_rate() {
    let schema = "ds_test_load_commissions_bps";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    client
        .execute(
            "INSERT INTO cluster_info (epoch_slot, epoch, transaction_count, created_at)
             VALUES (1, $1, 0, NOW())",
            &[&Decimal::from(EPOCH)],
        )
        .await
        .unwrap();
    client
        .execute(
            "INSERT INTO commissions (vote_account, commission, epoch_slot, epoch, created_at)
             VALUES ('voteSampled', 7, 100, $1, NOW())",
            &[&Decimal::from(EPOCH)],
        )
        .await
        .unwrap();
    client
        .execute(
            "INSERT INTO validators (
                identity, vote_account, epoch, activated_stake, marinade_stake,
                marinade_native_stake, superminority, stake_to_become_superminority, credits,
                leader_slots, blocks_produced, skip_rate, updated_at,
                commission_effective, commission_effective_source, inflation_rewards_commission_bps
            ) VALUES
                ('idSampled', 'voteSampled', $1, 100, 0, 0, false, 0, 0, 0, 0, 0, NOW(), 7, 'vote_state', 650),
                ('idReward', 'voteReward', $1, 100, 0, 0, false, 0, 0, 0, 0, 0, NOW(), 7, 'reward_row', 650)",
            &[&Decimal::from(EPOCH)],
        )
        .await
        .unwrap();

    let commissions = load_commissions(&client, 1).await.unwrap();

    let mut sampled: Vec<_> = commissions["voteSampled"]
        .iter()
        .map(|record| (record.epoch_slot, record.commission, record.commission_bps))
        .collect();
    sampled.sort();
    assert_eq!(
        sampled,
        vec![(100, 7, None), (432000, 7, Some(650))],
        "the advertised sample has no bps; the effective vote-state row carries its own"
    );

    let reward: Vec<_> = commissions["voteReward"]
        .iter()
        .map(|record| (record.commission, record.commission_bps))
        .collect();
    assert_eq!(
        reward,
        vec![(7, None)],
        "a reward row applied the whole percent, not the sampled bps"
    );

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}
