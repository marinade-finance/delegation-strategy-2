mod common;

use collect::validators::Snapshot;
use collect::validators_performance::{
    ValidatorPerformance, ValidatorRewards, ValidatorsPerformanceSnapshot,
};
use common::{
    migrated_client, run_close_epoch, skip_without_database, store_snapshot, validator_performance,
    validator_snapshot,
};
use rust_decimal::Decimal;
use std::collections::HashMap;
use tokio_postgres::Client;

const EPOCH: u64 = 1045;
const CHANGED_LATE: &str = "voteChangedAfterTheVintage";
const NEW_LAST_EPOCH: &str = "voteFirstSampledLastEpoch";
const NEW_THIS_EPOCH: &str = "voteFirstSampledThisEpoch";
const UNPARSED_AT_VINTAGE: &str = "voteUnparsedAtTheVintage";
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

fn performance_snapshot(
    listed: &[&str],
    reward_rows: &[(&str, u8)],
) -> ValidatorsPerformanceSnapshot {
    let validators: HashMap<String, ValidatorPerformance> = listed
        .iter()
        .map(|vote_account| (vote_account.to_string(), validator_performance()))
        .collect();
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
    common::performance_snapshot(EPOCH, validators, rewards)
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
            (UNPARSED_AT_VINTAGE, None),
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
            (UNPARSED_AT_VINTAGE, Some(800)),
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
            (UNPARSED_AT_VINTAGE, Some(900)),
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
        UNPARSED_AT_VINTAGE,
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
        read_effective(&client, UNPARSED_AT_VINTAGE).await,
        (Some(7), None, vote_state.clone()),
        "an unparsed E-2 vote state keeps its vintage through the advertised percent, not E-1's bps"
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
