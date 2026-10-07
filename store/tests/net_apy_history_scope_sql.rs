mod common;

use chrono::{DateTime, Duration, Utc};
use common::{migrated_client, skip_without_database};
use rust_decimal::Decimal;
use store::utils::{load_last_closed_epoch, load_net_apy_history_scope};
use tokio_postgres::Client;

const LAST_EPOCH: u64 = 1000;

fn epoch_start(epoch: u64) -> DateTime<Utc> {
    "2024-01-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap() + Duration::days(2 * epoch as i64)
}

async fn insert_validator(client: &Client, vote_account: &str, epoch: u64) {
    client
        .execute(
            "INSERT INTO validators (
                identity, vote_account, epoch, activated_stake, marinade_stake,
                marinade_native_stake, superminority, stake_to_become_superminority, credits,
                leader_slots, blocks_produced, skip_rate, updated_at
            ) VALUES ($1, $2, $3, 100, 0, 0, false, 0, 0, 0, 0, 0, NOW())",
            &[
                &format!("identity-{vote_account}"),
                &vote_account,
                &Decimal::from(epoch),
            ],
        )
        .await
        .unwrap();
}

async fn insert_epoch(client: &Client, epoch: u64) {
    client
        .execute(
            "INSERT INTO epochs (
                epoch, start_at, end_at, transaction_count, supply, inflation, inflation_taper,
                slots_per_year
            ) VALUES ($1, $2, $3, 0, 0, 0, 0, 0)",
            &[
                &Decimal::from(epoch),
                &epoch_start(epoch),
                &(epoch_start(epoch) + Duration::days(2)),
            ],
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn net_apy_history_scope_covers_the_cached_window() {
    let schema = "ds_test_net_apy_history_scope";
    if skip_without_database(schema) {
        return;
    }
    let client = migrated_client(schema).await.unwrap();

    assert_eq!(
        load_last_closed_epoch(&client).await.unwrap(),
        None,
        "no closed epoch yet is not epoch 0"
    );

    client
        .execute(
            "INSERT INTO cluster_info (epoch_slot, epoch, transaction_count, created_at)
             VALUES (1, $1, 0, NOW())",
            &[&Decimal::from(LAST_EPOCH)],
        )
        .await
        .unwrap();
    for epoch in LAST_EPOCH - 3..LAST_EPOCH {
        insert_epoch(&client, epoch).await;
    }
    insert_validator(&client, "voteOpen", LAST_EPOCH).await;
    insert_validator(&client, "voteOpen", LAST_EPOCH - 1).await;
    insert_validator(&client, "voteEdge", LAST_EPOCH - 2).await;
    insert_validator(&client, "voteOld", LAST_EPOCH - 3).await;

    assert_eq!(
        load_last_closed_epoch(&client).await.unwrap(),
        Some((
            LAST_EPOCH - 1,
            epoch_start(LAST_EPOCH - 1) + Duration::days(2)
        )),
        "the open epoch has no epochs row"
    );

    let (vote_accounts, first_start_at) = load_net_apy_history_scope(&client, 3).await.unwrap();
    assert_eq!(
        vote_accounts,
        vec!["voteEdge".to_string(), "voteOpen".to_string()],
        "each validator of the window once, sorted, and none from before it"
    );
    assert_eq!(first_start_at, Some(epoch_start(LAST_EPOCH - 2)));

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}
