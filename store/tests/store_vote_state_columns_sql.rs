mod common;

use common::{migrated_client, skip_without_database, store_snapshot, validator_snapshot};
use rust_decimal::Decimal;
use tokio_postgres::Client;

const EPOCH: u64 = 1031;
const VOTE_ACCOUNT: &str = "voteVoteStateColumns";
const IDENTITY: &str = "identityVoteStateColumns";

struct Stored {
    inflation_rewards_collector: Option<String>,
    block_revenue_collector: Option<String>,
    inflation_rewards_commission_bps: Option<i32>,
    inflation_rewards_commission_bps_is_v4: Option<bool>,
    block_revenue_commission_bps: Option<i32>,
    pending_delegator_rewards: Option<Decimal>,
}

async fn read_back(client: &Client) -> Stored {
    let row = client
        .query_one(
            "SELECT
                inflation_rewards_collector,
                block_revenue_collector,
                inflation_rewards_commission_bps,
                inflation_rewards_commission_bps_is_v4,
                block_revenue_commission_bps,
                pending_delegator_rewards
             FROM validators WHERE vote_account = $1 AND epoch = $2",
            &[&VOTE_ACCOUNT, &Decimal::from(EPOCH)],
        )
        .await
        .unwrap();
    Stored {
        inflation_rewards_collector: row.get("inflation_rewards_collector"),
        block_revenue_collector: row.get("block_revenue_collector"),
        inflation_rewards_commission_bps: row.get("inflation_rewards_commission_bps"),
        inflation_rewards_commission_bps_is_v4: row.get("inflation_rewards_commission_bps_is_v4"),
        block_revenue_commission_bps: row.get("block_revenue_commission_bps"),
        pending_delegator_rewards: row.get("pending_delegator_rewards"),
    }
}

// Both write paths matter: the first store of an epoch inserts, every later one that epoch updates,
// and the two carry their own positional column lists.
#[tokio::test]
async fn vote_state_columns_survive_both_write_paths_at_full_width() {
    let schema = "ds_test_store_vote_state_columns";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    let mut snapshot = validator_snapshot(EPOCH, IDENTITY, VOTE_ACCOUNT);
    let validator = &mut snapshot.validators[0];
    validator.inflation_rewards_collector = Some("collectorInflation".into());
    validator.block_revenue_collector = Some("collectorBlockRevenue".into());
    // 749 is not a whole percent and 10_000 is the top of the u16 range the column has to hold.
    validator.inflation_rewards_commission_bps = Some(749);
    validator.inflation_rewards_commission_bps_is_v4 = Some(true);
    validator.block_revenue_commission_bps = Some(10_000);
    validator.pending_delegator_rewards = Some(u64::MAX);

    store_snapshot(&mut client, "vote-state-insert", &snapshot).await;
    let inserted = read_back(&client).await;
    assert_eq!(
        inserted.inflation_rewards_collector.as_deref(),
        Some("collectorInflation")
    );
    assert_eq!(
        inserted.block_revenue_collector.as_deref(),
        Some("collectorBlockRevenue")
    );
    assert_eq!(inserted.inflation_rewards_commission_bps, Some(749));
    assert_eq!(inserted.inflation_rewards_commission_bps_is_v4, Some(true));
    assert_eq!(inserted.block_revenue_commission_bps, Some(10_000));
    assert_eq!(
        inserted.pending_delegator_rewards,
        Some(Decimal::from(u64::MAX)),
        "a u64 must not be narrowed to a signed column"
    );

    // Second store of the same epoch takes the UPDATE branch; the validator redirected meanwhile.
    let validator = &mut snapshot.validators[0];
    validator.inflation_rewards_collector = Some("collectorRedirected".into());
    validator.inflation_rewards_commission_bps = Some(1_001);
    store_snapshot(&mut client, "vote-state-update", &snapshot).await;
    let updated = read_back(&client).await;
    assert_eq!(
        updated.inflation_rewards_collector.as_deref(),
        Some("collectorRedirected"),
        "the update path has to carry the new collector, not keep the inserted one"
    );
    assert_eq!(updated.inflation_rewards_commission_bps, Some(1_001));
    assert_eq!(
        updated.block_revenue_commission_bps,
        Some(10_000),
        "the untouched fields must survive the update"
    );

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}

#[tokio::test]
async fn a_pre_v4_validator_stores_no_collector_and_still_stores_a_rate() {
    let schema = "ds_test_store_vote_state_columns_pre_v4";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    let mut snapshot = validator_snapshot(EPOCH, IDENTITY, VOTE_ACCOUNT);
    let validator = &mut snapshot.validators[0];
    validator.inflation_rewards_commission_bps = Some(700);
    validator.inflation_rewards_commission_bps_is_v4 = Some(false);

    store_snapshot(&mut client, "vote-state-pre-v4", &snapshot).await;
    let stored = read_back(&client).await;

    assert_eq!(
        stored.inflation_rewards_collector, None,
        "a pre-v4 state has no collector to record, and 11111... would read as one"
    );
    assert_eq!(stored.block_revenue_collector, None);
    assert_eq!(stored.block_revenue_commission_bps, None);
    assert_eq!(stored.pending_delegator_rewards, None);
    assert_eq!(
        stored.inflation_rewards_commission_bps,
        Some(700),
        "the projection agave applies is still the rate, so it is stored"
    );
    assert_eq!(stored.inflation_rewards_commission_bps_is_v4, Some(false));

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}

// store runs against snapshots written by the previous release for as long as it takes to redeploy.
#[tokio::test]
async fn a_snapshot_written_before_these_fields_still_stores() {
    let schema = "ds_test_store_vote_state_columns_old_snapshot";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    let snapshot = validator_snapshot(EPOCH, IDENTITY, VOTE_ACCOUNT);
    let mut yaml = serde_yaml::to_value(&snapshot).unwrap();
    let validator = yaml
        .get_mut("validators")
        .and_then(|validators| validators.get_mut(0))
        .and_then(serde_yaml::Value::as_mapping_mut)
        .unwrap();
    for field in [
        "inflation_rewards_collector",
        "block_revenue_collector",
        "inflation_rewards_commission_bps",
        "inflation_rewards_commission_bps_is_v4",
        "block_revenue_commission_bps",
        "pending_delegator_rewards",
    ] {
        assert!(
            validator
                .remove(&serde_yaml::Value::String(field.into()))
                .is_some(),
            "{field} must be present to be worth removing"
        );
    }

    let stripped: collect::validators::Snapshot = serde_yaml::from_value(yaml).unwrap();
    store_snapshot(&mut client, "vote-state-old-snapshot", &stripped).await;
    let stored = read_back(&client).await;

    assert_eq!(stored.inflation_rewards_collector, None);
    assert_eq!(stored.inflation_rewards_commission_bps, None);
    assert_eq!(
        stored.inflation_rewards_commission_bps_is_v4, None,
        "an old snapshot recorded no version, which is not the same as recording pre-v4"
    );

    client
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}
