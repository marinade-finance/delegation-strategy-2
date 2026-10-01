mod common;

use common::{migrated_client, skip_without_database, store_snapshot, validator_snapshot};
use rust_decimal::Decimal;
use tokio_postgres::Client;

const EPOCH: u64 = 1040;

async fn close(client: &Client, epoch: u64) {
    client
        .execute(
            "INSERT INTO epochs (epoch, start_at, end_at, transaction_count, supply, inflation, inflation_taper, slots_per_year)
             VALUES ($1, NOW(), NOW(), 0, 0, 0, 0.15, 0)",
            &[&Decimal::from(epoch)],
        )
        .await
        .unwrap();
}

async fn stored(client: &Client, vote_account: &str, epoch: u64) -> Option<(Decimal, Option<i32>)> {
    client
        .query_opt(
            "SELECT credits, inflation_rewards_commission_bps FROM validators
             WHERE vote_account = $1 AND epoch = $2",
            &[&vote_account, &Decimal::from(epoch)],
        )
        .await
        .unwrap()
        .map(|row| {
            (
                row.get("credits"),
                row.get("inflation_rewards_commission_bps"),
            )
        })
}

#[tokio::test]
async fn a_snapshot_for_a_closed_epoch_is_skipped() {
    let schema = "ds_test_store_validators_closed_epoch";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    let mut snapshot = validator_snapshot(EPOCH, "idKept", "voteKept");
    snapshot.validators[0].inflation_rewards_commission_bps = Some(500);
    snapshot.validators[0].inflation_rewards_commission_bps_is_v4 = Some(true);
    store_snapshot(&mut client, "closed-epoch-open", &snapshot).await;
    close(&client, EPOCH).await;

    snapshot.validators[0].performance.credits = 99;
    snapshot.validators[0].inflation_rewards_commission_bps = Some(900);
    snapshot.validators.push(
        validator_snapshot(EPOCH, "idLate", "voteLate")
            .validators
            .remove(0),
    );
    store_snapshot(&mut client, "closed-epoch-late", &snapshot).await;

    assert_eq!(
        stored(&client, "voteKept", EPOCH).await,
        Some((Decimal::from(10), Some(500))),
        "a late snapshot must not overwrite what the epoch held at close"
    );
    assert_eq!(
        stored(&client, "voteLate", EPOCH).await,
        None,
        "a late snapshot must not add rows close_epoch never resolves"
    );

    snapshot.epoch = EPOCH + 1;
    store_snapshot(&mut client, "closed-epoch-next", &snapshot).await;
    assert_eq!(
        stored(&client, "voteKept", EPOCH + 1).await,
        Some((Decimal::from(99), Some(900))),
        "the next, open epoch still stores"
    );

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}
