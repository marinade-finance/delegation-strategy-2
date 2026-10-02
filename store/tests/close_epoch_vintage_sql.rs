mod common;

use clap::Parser;
use collect::slot_params::baseline_slots_per_year;
use collect::validators::Snapshot;
use collect::validators_performance::{
    ClusterInflation, ValidatorPerformance, ValidatorRewards, ValidatorsPerformanceSnapshot,
};
use common::{
    migrated_client, skip_without_database, store_snapshot, validator_performance,
    validator_snapshot, write_yaml,
};
use rust_decimal::Decimal;
use std::collections::HashMap;
use store::close_epoch::{close_epoch, CloseEpochParams};
use tokio_postgres::Client;

const EPOCH: u64 = 1045;
const CHANGED_LATE: &str = "voteChangedAfterTheVintage";
const NEW_LAST_EPOCH: &str = "voteFirstSampledLastEpoch";
const NEW_THIS_EPOCH: &str = "voteFirstSampledThisEpoch";
const UNSAMPLED: &str = "voteNeverSampled";
const OUTSIDE: &str = "voteOutsideTheSnapshot";
const REWARD_ROW: &str = "voteWithARewardRow";

async fn store_samples(client: &mut Client, epoch: u64, samples: &[(&str, Option<u16>)]) {
    let mut snapshot = Snapshot {
        validators: vec![],
        ..validator_snapshot(epoch, "unused", "unused")
    };
    for (vote_account, bps) in samples {
        let mut validator =
            validator_snapshot(epoch, &format!("identity-{vote_account}"), vote_account)
                .validators
                .remove(0);
        validator.inflation_rewards_commission_bps = *bps;
        validator.inflation_rewards_commission_bps_is_v4 = bps.map(|_| true);
        snapshot.validators.push(validator);
    }
    store_snapshot(client, &format!("vintage-{epoch}"), &snapshot).await;
}

fn performance_snapshot(listed: &[&str], reward_rows: &[(&str, u8)]) -> String {
    let validators: HashMap<_, _> = listed
        .iter()
        .map(|vote_account| (vote_account.to_string(), validator_performance()))
        .collect::<HashMap<String, ValidatorPerformance>>();
    let rewards = listed
        .iter()
        .map(|vote_account| {
            let commission_effective = reward_rows
                .iter()
                .find(|(reward_vote, _)| reward_vote == vote_account)
                .map(|(_, commission)| *commission);
            (
                vote_account.to_string(),
                ValidatorRewards {
                    commission_effective,
                },
            )
        })
        .collect();
    serde_yaml::to_string(&ValidatorsPerformanceSnapshot {
        epoch: EPOCH,
        epoch_slot: 432_000,
        transaction_count: 0,
        created_at: "2026-10-01T14:00:00Z".into(),
        slots_per_year: baseline_slots_per_year(),
        cluster_inflation: Some(ClusterInflation {
            sol_total_supply: 0,
            inflation: 0f64,
            inflation_taper: 0f64,
        }),
        validators,
        nodes: Default::default(),
        rewards: Some(rewards),
    })
    .unwrap()
}

async fn run_close_epoch(client: &mut Client, schema_tag: &str, snapshot: &str) {
    client
        .execute(
            "INSERT INTO cluster_info (epoch_slot, epoch, transaction_count, created_at)
             VALUES (0, $1, 0, NOW()), (432000, $1, 0, NOW())",
            &[&Decimal::from(EPOCH)],
        )
        .await
        .unwrap();
    let path = write_yaml(schema_tag, snapshot);
    close_epoch(
        CloseEpochParams::parse_from(["store", "--snapshot-file", &path]),
        client,
    )
    .await
    .unwrap();
    std::fs::remove_file(path).unwrap();
}

async fn read_effective(
    client: &Client,
    vote_account: &str,
) -> (Option<i32>, Option<i32>, Option<String>) {
    let row = client
        .query_one(
            "SELECT commission_effective, commission_effective_bps, commission_effective_source
             FROM validators WHERE vote_account = $1 AND epoch = $2",
            &[&vote_account, &Decimal::from(EPOCH)],
        )
        .await
        .unwrap();
    (
        row.get("commission_effective"),
        row.get("commission_effective_bps"),
        row.get("commission_effective_source"),
    )
}

async fn seed_three_epochs(client: &mut Client) {
    store_samples(
        client,
        EPOCH - 2,
        &[
            (CHANGED_LATE, Some(500)),
            (UNSAMPLED, None),
            (OUTSIDE, Some(300)),
            (REWARD_ROW, Some(500)),
        ],
    )
    .await;
    store_samples(
        client,
        EPOCH - 1,
        &[
            (CHANGED_LATE, Some(800)),
            (NEW_LAST_EPOCH, Some(800)),
            (UNSAMPLED, None),
            (OUTSIDE, Some(500)),
            (REWARD_ROW, Some(800)),
        ],
    )
    .await;
    store_samples(
        client,
        EPOCH,
        &[
            (CHANGED_LATE, Some(900)),
            (NEW_LAST_EPOCH, Some(900)),
            (NEW_THIS_EPOCH, Some(900)),
            (UNSAMPLED, None),
            (OUTSIDE, Some(700)),
            (REWARD_ROW, Some(900)),
        ],
    )
    .await;
}

#[tokio::test]
async fn close_epoch_prices_the_vote_state_agave_applied_to_the_epoch() {
    let schema = "ds_test_close_epoch_vintage";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    seed_three_epochs(&mut client).await;
    let listed = [
        CHANGED_LATE,
        NEW_LAST_EPOCH,
        NEW_THIS_EPOCH,
        UNSAMPLED,
        REWARD_ROW,
    ];
    run_close_epoch(
        &mut client,
        schema,
        &performance_snapshot(&listed, &[(REWARD_ROW, 6)]),
    )
    .await;

    let vote_state = Some("vote_state".to_string());
    assert_eq!(
        read_effective(&client, CHANGED_LATE).await,
        (Some(5), Some(500), vote_state.clone()),
        "epoch_stakes(E) froze at the close of E-2, so a change during E-1 or E must not reach E"
    );
    assert_eq!(
        read_effective(&client, NEW_LAST_EPOCH).await,
        (Some(8), Some(800), vote_state.clone()),
        "without an E-2 sample agave falls back to epoch_stakes(E+1), the close of E-1"
    );
    assert_eq!(
        read_effective(&client, NEW_THIS_EPOCH).await,
        (Some(9), Some(900), vote_state.clone()),
        "with neither snapshot agave reads the live state, the epoch's own last sample"
    );
    assert_eq!(
        read_effective(&client, UNSAMPLED).await,
        (None, None, None),
        "no sample in any of the three epochs leaves the rate unknown"
    );
    assert_eq!(
        read_effective(&client, OUTSIDE).await,
        (Some(3), Some(300), vote_state.clone()),
        "a row the snapshot never listed resolves at the same vintage, bps included"
    );
    assert_eq!(
        read_effective(&client, REWARD_ROW).await,
        (Some(6), None, Some("reward_row".to_string())),
        "a reward row still wins, and its whole percent carries no bps"
    );

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}
