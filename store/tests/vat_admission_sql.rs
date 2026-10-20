mod common;

use collect::vat_admission::VatAdmissionReason;
use common::{migrated_client, skip_without_database, store_snapshot, validator_snapshot};
use rust_decimal::Decimal;
use store::dto::ValidatorEpochStats;
use store::utils::{load_validators, ValidatorOverlays};
use tokio_postgres::Client;

const EPOCH: u64 = 1058;
const VOTE_ACCOUNT: &str = "VatAdmissionVote111111111111111111111111111";
const BLS_PUBKEY: &str = "BLSkey";

async fn seed(
    client: &mut Client,
    schema: &str,
    is_v4: Option<bool>,
    vat_admitted: Option<bool>,
    reason: Option<VatAdmissionReason>,
) {
    let mut snapshot = validator_snapshot(EPOCH, "identityVatAdmission", VOTE_ACCOUNT);
    let validator = &mut snapshot.validators[0];
    validator.inflation_rewards_commission_bps_is_v4 = is_v4;
    validator.bls_pubkey = is_v4.map(|_| BLS_PUBKEY.to_string());
    validator.vote_account_lamports = is_v4.map(|_| 27_000_000);
    validator.vat_admitted = vat_admitted;
    validator.vat_admission_reason = reason;
    store_snapshot(client, schema, &snapshot).await;
}

async fn epoch_stats(client: &Client) -> ValidatorEpochStats {
    client
        .execute(
            "INSERT INTO cluster_info (epoch_slot, epoch, transaction_count, created_at)
             VALUES (0, $1, 0, NOW()) ON CONFLICT DO NOTHING",
            &[&Decimal::from(EPOCH)],
        )
        .await
        .unwrap();
    let unreachable_scoring_url = "http://127.0.0.1:1".to_string();
    load_validators(
        client,
        unreachable_scoring_url,
        3,
        3,
        &ValidatorOverlays::default(),
    )
    .await
    .unwrap()
    .remove(VOTE_ACCOUNT)
    .expect("the validator must load")
    .epoch_stats
    .into_iter()
    .find(|stats| stats.epoch == EPOCH)
    .expect("the epoch must load")
}

async fn stored_lamports(client: &Client) -> Option<Decimal> {
    client
        .query_one(
            "SELECT vote_account_lamports FROM validators WHERE vote_account = $1 AND epoch = $2",
            &[&VOTE_ACCOUNT, &Decimal::from(EPOCH)],
        )
        .await
        .unwrap()
        .get(0)
}

#[tokio::test]
async fn a_rejected_verdict_is_served_with_its_reason() {
    let schema = "ds_test_vat_admission_rejected";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    seed(
        &mut client,
        schema,
        Some(true),
        Some(false),
        Some(VatAdmissionReason::LowBalance),
    )
    .await;

    let stats = epoch_stats(&client).await;
    assert_eq!(stats.vat_admitted, Some(false));
    assert_eq!(stats.vat_admission_reason.as_deref(), Some("low_balance"));
    assert_eq!(stats.bls_pubkey.as_deref(), Some(BLS_PUBKEY));
    assert_eq!(
        stored_lamports(&client).await,
        Some(Decimal::from(27_000_000))
    );
}

#[tokio::test]
async fn an_admitted_verdict_has_no_reason() {
    let schema = "ds_test_vat_admission_admitted";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    seed(&mut client, schema, Some(true), Some(true), None).await;

    let stats = epoch_stats(&client).await;
    assert_eq!(stats.vat_admitted, Some(true));
    assert_eq!(stats.vat_admission_reason, None);
}

#[tokio::test]
async fn a_later_snapshot_that_could_not_parse_the_vote_state_keeps_the_stored_verdict() {
    let schema = "ds_test_vat_admission_unparsed";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    seed(
        &mut client,
        schema,
        Some(true),
        Some(false),
        Some(VatAdmissionReason::NoStake),
    )
    .await;
    seed(&mut client, schema, None, None, None).await;

    let stats = epoch_stats(&client).await;
    assert_eq!(stats.vat_admitted, Some(false));
    assert_eq!(stats.vat_admission_reason.as_deref(), Some("no_stake"));
    assert_eq!(stats.bls_pubkey.as_deref(), Some(BLS_PUBKEY));
    assert_eq!(
        stored_lamports(&client).await,
        Some(Decimal::from(27_000_000))
    );
}

#[tokio::test]
async fn a_later_admitted_verdict_clears_the_stored_reason() {
    let schema = "ds_test_vat_admission_recovered";
    if skip_without_database(schema) {
        return;
    }
    let mut client = migrated_client(schema).await.unwrap();

    seed(
        &mut client,
        schema,
        Some(true),
        Some(false),
        Some(VatAdmissionReason::LowBalance),
    )
    .await;
    seed(&mut client, schema, Some(true), Some(true), None).await;

    let stats = epoch_stats(&client).await;
    assert_eq!(stats.vat_admitted, Some(true));
    assert_eq!(stats.vat_admission_reason, None);
}
