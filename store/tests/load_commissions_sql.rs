mod common;

use common::{migrated_client, skip_without_database};
use rust_decimal::Decimal;
use store::utils::load_commissions;

const EPOCH: u64 = 1000;

#[tokio::test]
async fn load_commissions_serves_only_the_advertised_samples() {
    let schema = "ds_test_load_commissions_advertised";
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
                commission_effective, commission_effective_source, commission_effective_bps
            ) VALUES
                ('idSampled', 'voteSampled', $1, 100, 0, 0, false, 0, 0, 0, 0, 0, NOW(), 9, 'vote_state', 850),
                ('idApplied', 'voteApplied', $1, 100, 0, 0, false, 0, 0, 0, 0, 0, NOW(), 9, 'vote_state', 850)",
            &[&Decimal::from(EPOCH)],
        )
        .await
        .unwrap();

    let commissions = load_commissions(&client, 1).await.unwrap();

    let sampled: Vec<_> = commissions["voteSampled"]
        .iter()
        .map(|record| (record.epoch, record.epoch_slot, record.commission))
        .collect();
    assert_eq!(
        sampled,
        vec![(EPOCH, 100, 7)],
        "the applied rate lags the advertised timeline, so it is not a point on it"
    );
    assert!(
        !commissions.contains_key("voteApplied"),
        "a validator with only an applied rate has no advertised samples"
    );

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}
