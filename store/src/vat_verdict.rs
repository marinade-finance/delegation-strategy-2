use crate::utils::{
    load_vat_minimum_balances, vat_forecasts, BLS_PUBKEY_COLLECTED_SQL, DEFAULT_CACHE_EPOCHS,
};
use collect::vat_debits::VatDebitsSnapshot;
use log::info;
use rust_decimal::Decimal;
use std::collections::HashSet;
use tokio_postgres::Client;

const MAX_UNKNOWN_DEBITED_PERCENT: i64 = 10;
const MAX_ADMITTED_UNDEBITED_PERCENT: i64 = 10;

#[derive(Debug, clap::Parser)]
pub struct StoreVatVerdictParams {
    #[arg(long = "snapshot-file")]
    snapshot_path: String,
}

pub async fn store_vat_verdict(
    params: StoreVatVerdictParams,
    psql_client: &mut Client,
) -> anyhow::Result<()> {
    let path = params.snapshot_path;
    let snapshot_file = std::fs::File::open(&path)
        .map_err(|e| anyhow::anyhow!("Failed to open VAT debits file '{path}': {e}"))?;
    let snapshot: VatDebitsSnapshot = serde_yaml::from_reader(snapshot_file)
        .map_err(|e| anyhow::anyhow!("Failed to parse VAT debits file '{path}': {e}"))?;
    let epoch = snapshot.epoch;
    let debited = &snapshot.vote_accounts;
    anyhow::ensure!(
        !debited.is_empty(),
        "VAT debits file '{path}' of epoch {epoch} lists no vote account"
    );
    let epoch_decimal = Decimal::from(epoch);
    let minimums = load_vat_minimum_balances(psql_client, DEFAULT_CACHE_EPOCHS).await?;

    let transaction = psql_client.transaction().await?;
    let counts = transaction
        .query_one(
            "WITH listed AS (SELECT DISTINCT UNNEST($2::TEXT[]) AS vote_account),
                  stored AS (SELECT DISTINCT vote_account FROM validators WHERE epoch = $1)
             SELECT
                 (SELECT COUNT(*) FROM stored) AS known,
                 (SELECT COUNT(*) FROM listed) AS debited,
                 (SELECT COUNT(*) FROM listed
                  WHERE vote_account NOT IN (SELECT vote_account FROM stored)) AS unknown_debited",
            &[&epoch_decimal, debited],
        )
        .await?;
    let known: i64 = counts.get("known");
    let unknown_debited: i64 = counts.get("unknown_debited");
    let debited_count: i64 = counts.get("debited");
    anyhow::ensure!(known > 0, "DS2 holds no validators of epoch {epoch}");
    let rows = transaction
        .query(
            &format!(
                "SELECT vote_account, epoch, vote_account_lamports, bls_pubkey,
                     inflation_rewards_commission_bps_is_v4, {BLS_PUBKEY_COLLECTED_SQL},
                     activated_stake, activating_stake, deactivating_stake, pending_delegator_rewards
                 FROM validators WHERE epoch = $1"
            ),
            &[&epoch_decimal],
        )
        .await?;
    let listed: HashSet<&str> = debited.iter().map(String::as_str).collect();
    let forecasts = vat_forecasts(&rows, &minimums);
    let admitted: Vec<&str> = forecasts
        .iter()
        .filter(|(_, rejection)| rejection.is_none())
        .map(|((_, vote_account), _)| vote_account.as_str())
        .collect();
    let admitted_count = admitted.len() as i64;
    let admitted_undebited = admitted.iter().filter(|v| !listed.contains(*v)).count() as i64;
    anyhow::ensure!(
        admitted_count > 0,
        "DS2 holds no VAT forecast of epoch {epoch}, nothing written"
    );
    anyhow::ensure!(
        unknown_debited * 100 <= debited_count * MAX_UNKNOWN_DEBITED_PERCENT
            && admitted_undebited * 100 <= admitted_count * MAX_ADMITTED_UNDEBITED_PERCENT,
        "VAT verdict of epoch {epoch} is implausible, nothing written: {unknown_debited} of {debited_count} debited vote accounts are unknown to DS2, {admitted_undebited} of {admitted_count} forecast-admitted vote accounts are not debited"
    );

    let updated = transaction
        .execute(
            "UPDATE validators
             SET vat_admitted = CASE
                 WHEN vote_account = ANY($2) THEN TRUE
                 WHEN activated_stake > 0 THEN FALSE
                 ELSE NULL
             END
             WHERE epoch = $1",
            &[&epoch_decimal, debited],
        )
        .await?;
    transaction.commit().await?;

    info!(
        "Stored the VAT verdict of epoch {epoch} on {updated} validators: {debited_count} debited, {unknown_debited} of them unknown to DS2, {admitted_undebited} of {admitted_count} forecast-admitted not debited"
    );
    Ok(())
}
