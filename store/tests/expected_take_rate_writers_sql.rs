mod common;

use clap::Parser;
use collect::take_rates::{ValidatorEpochRewards, ValidatorRewardsSnapshot};
use collect::validators_jito::{
    JitoAccountType, JitoSnapshot, MevTipDistributionValidatorSnapshot,
    PriorityFeeDistributionValidatorSnapshot, ValidatorSnapshot,
};
use collect::validators_performance::ValidatorRewards;
use common::{
    migrated_client, performance_snapshot, run_close_epoch, skip_without_database, store_snapshot,
    validator_performance, validator_snapshot, write_yaml,
};
use rust_decimal::Decimal;
use std::collections::HashMap;
use store::take_rates::{store_take_rates, StoreTakeRatesParams};
use store::utils::{expected_take_rate, worst_known_commission, RewardMixShares};
use store::validators_jito::{store_jito, StoreJitoParams};
use tokio_postgres::Client;

const EPOCH: u64 = 1050;
const VOTE: &str = "voteExpectedWriters";

#[derive(Clone, Copy, Debug)]
enum Writer {
    TakeRates,
    JitoMev,
    JitoPriority,
    CloseEpoch,
}

async fn run(client: &mut Client, schema: &str, writer: Writer) {
    let name = format!("{schema}-{writer:?}");
    match writer {
        Writer::TakeRates => {
            let snapshot = ValidatorRewardsSnapshot {
                version: 1,
                from_epoch: EPOCH,
                loaded_at_epoch: EPOCH + 1,
                loaded_at_slot_index: 0,
                created_at: "2026-10-12T00:00:00Z".into(),
                rewards: vec![ValidatorEpochRewards {
                    epoch: EPOCH,
                    vote_account: VOTE.into(),
                    validator_rewards: 10,
                    total_rewards: 100,
                    inflation_rewards: 90,
                    mev_rewards: 4,
                    block_rewards: 6,
                }],
            };
            let path = write_yaml(&name, &serde_yaml::to_string(&snapshot).unwrap());
            store_take_rates(
                StoreTakeRatesParams::parse_from(["store", "--snapshot-file", &path]),
                client,
            )
            .await
            .unwrap();
        }
        Writer::JitoMev | Writer::JitoPriority => {
            let (account_type, validator) = match writer {
                Writer::JitoMev => (
                    JitoAccountType::MevTipDistribution,
                    ValidatorSnapshot::MevTipDistribution(MevTipDistributionValidatorSnapshot {
                        vote_account: VOTE.into(),
                        mev_commission: 800,
                        epoch: EPOCH,
                        total_epoch_rewards: None,
                        claimed_epoch_rewards: None,
                        total_epoch_claimants: None,
                        epoch_active_claimants: None,
                    }),
                ),
                _ => (
                    JitoAccountType::PriorityFeeDistribution,
                    ValidatorSnapshot::PriorityFeeDistribution(
                        PriorityFeeDistributionValidatorSnapshot {
                            vote_account: VOTE.into(),
                            priority_commission: 2000,
                            total_lamports_transferred: 0,
                            merkle_root_upload_authority: "authority".into(),
                            epoch: EPOCH,
                            total_epoch_rewards: None,
                            claimed_epoch_rewards: None,
                            total_epoch_claimants: None,
                            epoch_active_claimants: None,
                        },
                    ),
                ),
            };
            let snapshot = JitoSnapshot {
                epoch: EPOCH,
                version: 1,
                account_type: account_type.clone(),
                loaded_at_epoch: EPOCH + 1,
                loaded_at_slot_index: 0,
                created_at: "2026-10-12T00:00:00Z".into(),
                validators: HashMap::from([(VOTE.to_string(), validator)]),
            };
            let path = write_yaml(&name, &serde_yaml::to_string(&snapshot).unwrap());
            store_jito(
                StoreJitoParams::parse_from(["store", "--snapshot-file", &path]),
                client,
                account_type,
            )
            .await
            .unwrap();
        }
        Writer::CloseEpoch => {
            let snapshot = performance_snapshot(
                EPOCH,
                HashMap::from([(VOTE.to_string(), validator_performance())]),
                HashMap::from([(
                    VOTE.to_string(),
                    ValidatorRewards {
                        commission_effective: None,
                    },
                )]),
            );
            run_close_epoch(client, &name, &snapshot).await;
        }
    }
}

async fn stored_rate(client: &Client) -> Option<f64> {
    client
        .query_one(
            "SELECT expected_take_rate FROM validators_rewards WHERE vote_account = $1 AND epoch = $2",
            &[&VOTE, &Decimal::from(EPOCH)],
        )
        .await
        .unwrap()
        .get(0)
}

async fn run_in_order(schema: &str, order: [Writer; 4]) -> (Option<f64>, Option<f64>) {
    let mut client = migrated_client(schema).await.unwrap();
    store_snapshot(
        &mut client,
        schema,
        &validator_snapshot(EPOCH, "identityExpectedWriters", VOTE),
    )
    .await;
    for writer in order {
        run(&mut client, schema, writer).await;
    }

    let row = client
        .query_one(
            "SELECT commission_max_observed, commission_advertised FROM validators
             WHERE vote_account = $1 AND epoch = $2",
            &[&VOTE, &Decimal::from(EPOCH)],
        )
        .await
        .unwrap();
    let formula = expected_take_rate(
        RewardMixShares {
            inflation: 0.90,
            mev: 0.04,
            block: 0.06,
        },
        worst_known_commission(row.get(0), row.get(1)),
        Some(800),
        Some(2000),
    );
    let stored = stored_rate(&client).await;

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
    (stored, formula)
}

#[tokio::test]
async fn every_writer_order_converges_to_the_same_expected_take_rate() {
    let schema = "ds_test_expected_writers";
    if skip_without_database(schema) {
        return;
    }

    let (rewards_first, formula) = run_in_order(
        "ds_test_expected_writers_rewards_first",
        [
            Writer::TakeRates,
            Writer::JitoMev,
            Writer::JitoPriority,
            Writer::CloseEpoch,
        ],
    )
    .await;
    let (rewards_last, _) = run_in_order(
        "ds_test_expected_writers_rewards_last",
        [
            Writer::CloseEpoch,
            Writer::JitoPriority,
            Writer::JitoMev,
            Writer::TakeRates,
        ],
    )
    .await;

    assert!(formula.is_some(), "the seeded inputs must yield a rate");
    assert_eq!(
        rewards_first, formula,
        "the last writer refreshes the row, whichever it is"
    );
    assert_eq!(
        rewards_last, formula,
        "the take-rates writer refreshes a row whose other inputs are already stored"
    );
}
