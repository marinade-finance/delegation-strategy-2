mod common;

use common::{migrated_client, skip_without_database, store_snapshot, validator_snapshot};
use rust_decimal::Decimal;
use store::utils::{load_validators, ValidatorOverlays};
use tokio_postgres::Client;

const EPOCH: u64 = 1000;
const VOTE_ACCOUNT: &str = "voteDirectStake";
const IDENTITY: &str = "identityDirectStake";
const MARINADE_STAKE: u64 = 7;
const INSTITUTIONAL_STAKE: u64 = 11;

async fn run_store_validators(client: &mut Client, name: &str, direct_stake: u64) {
    let mut snapshot = validator_snapshot(EPOCH, IDENTITY, VOTE_ACCOUNT);
    snapshot.validators[0].marinade_stake = MARINADE_STAKE;
    snapshot.validators[0].institutional_stake = INSTITUTIONAL_STAKE;
    snapshot.validators[0].direct_stake = direct_stake;
    store_snapshot(client, name, &snapshot).await;
}

async fn stored_stakes(client: &Client) -> Vec<(Decimal, Decimal, Decimal)> {
    client
        .query(
            "SELECT marinade_stake, institutional_stake, direct_stake
             FROM validators WHERE vote_account = $1",
            &[&VOTE_ACCOUNT],
        )
        .await
        .unwrap()
        .iter()
        .map(|row| {
            (
                row.get("marinade_stake"),
                row.get("institutional_stake"),
                row.get("direct_stake"),
            )
        })
        .collect()
}

// The UPDATE path casts parameters via a positional index map the compiler cannot check.
#[tokio::test]
async fn store_validators_round_trips_direct_stake_on_insert_and_update() {
    let schema = "ds_test_store_validators_direct_stake";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();
    let expected = |direct_stake: u64| {
        vec![(
            Decimal::from(MARINADE_STAKE),
            Decimal::from(INSTITUTIONAL_STAKE),
            Decimal::from(direct_stake),
        )]
    };

    run_store_validators(&mut client, "direct-stake-insert", 101_208).await;
    assert_eq!(
        stored_stakes(&client).await,
        expected(101_208),
        "INSERT path"
    );

    run_store_validators(&mut client, "direct-stake-update", 42).await;
    assert_eq!(
        stored_stakes(&client).await,
        expected(42),
        "UPDATE path must overwrite direct_stake and leave its neighbours alone"
    );

    client
        .execute(
            "INSERT INTO cluster_info (epoch_slot, epoch, transaction_count, created_at)
             VALUES (1, $1, 0, NOW())",
            &[&Decimal::from(EPOCH)],
        )
        .await
        .unwrap();
    let unreachable_scoring_url = "http://127.0.0.1:1".to_string();
    let validators = load_validators(
        &client,
        unreachable_scoring_url,
        1,
        1,
        &ValidatorOverlays::default(),
    )
    .await
    .unwrap();
    let record = validators
        .get(VOTE_ACCOUNT)
        .expect("the stored row must load");
    assert_eq!(record.direct_stake, Decimal::from(42), "record");
    assert_eq!(
        record.marinade_stake,
        Decimal::from(MARINADE_STAKE),
        "direct stake must not be folded into marinade_stake"
    );
    assert_eq!(record.epoch_stats.len(), 1);
    assert_eq!(
        record.epoch_stats[0].direct_stake,
        Decimal::from(42),
        "epoch stats"
    );

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}
