use collect::vat_debits::VatDebitsSnapshot;
use log::info;
use rust_decimal::Decimal;
use tokio_postgres::Client;

const MAX_UNKNOWN_DEBITED_PERCENT: i64 = 10;
const MAX_STAKED_UNDEBITED_PERCENT: i64 = 10;

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

    let transaction = psql_client.transaction().await?;
    let counts = transaction
        .query_one(
            "WITH listed AS (SELECT DISTINCT UNNEST($2::TEXT[]) AS vote_account),
                  stored AS (SELECT DISTINCT vote_account, activated_stake > 0 AS staked
                             FROM validators WHERE epoch = $1)
             SELECT
                 (SELECT COUNT(*) FROM stored) AS known,
                 (SELECT COUNT(*) FROM listed
                  WHERE vote_account NOT IN (SELECT vote_account FROM stored)) AS unknown_debited,
                 (SELECT COUNT(*) FROM stored WHERE staked) AS staked,
                 (SELECT COUNT(*) FROM stored
                  WHERE staked AND vote_account NOT IN (SELECT vote_account FROM listed)) AS staked_undebited",
            &[&epoch_decimal, debited],
        )
        .await?;
    let known: i64 = counts.get("known");
    let unknown_debited: i64 = counts.get("unknown_debited");
    let staked: i64 = counts.get("staked");
    let staked_undebited: i64 = counts.get("staked_undebited");
    let debited_count = debited.len() as i64;
    anyhow::ensure!(known > 0, "DS2 holds no validators of epoch {epoch}");
    anyhow::ensure!(
        unknown_debited * 100 <= debited_count * MAX_UNKNOWN_DEBITED_PERCENT
            && staked_undebited * 100 <= staked * MAX_STAKED_UNDEBITED_PERCENT,
        "VAT verdict of epoch {epoch} is implausible, nothing written: {unknown_debited} of {debited_count} debited vote accounts are unknown to DS2, {staked_undebited} of {staked} staked vote accounts are not debited"
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
        "Stored the VAT verdict of epoch {epoch} on {updated} validators: {debited_count} debited, {unknown_debited} of them unknown to DS2, {staked_undebited} of {staked} staked not debited"
    );
    Ok(())
}
