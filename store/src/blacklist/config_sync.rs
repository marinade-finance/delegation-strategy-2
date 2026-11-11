use super::config::{BlacklistConfig, ExemptEntry, ManualEntry};
use super::db::max_epoch;
use clap::Parser;
use log::info;
use rust_decimal::prelude::*;
use tokio_postgres::{Client, Transaction};

#[derive(Debug, Parser)]
pub struct ConfigSyncParams {
    #[arg(long = "config", help = "The blacklist YAML config.")]
    config_path: String,
}

/// Syncs the hand-kept adds and the exemptions into the table. One transaction: a failure leaves
/// the table as it was.
pub async fn config_sync(params: ConfigSyncParams, psql_client: &mut Client) -> anyhow::Result<()> {
    let config = BlacklistConfig::load(&params.config_path)?;
    let tx = psql_client.transaction().await?;
    let current_epoch = max_epoch(&tx, "cluster_info")
        .await?
        .ok_or_else(|| anyhow::anyhow!("cluster_info is empty, no current epoch"))?;

    sync_manual(&tx, &config.manual, current_epoch).await?;
    sync_exempt(&tx, &config.exempt).await?;
    apply_exempt(&tx, &config.exempt, current_epoch).await?;

    tx.commit().await?;
    Ok(())
}

async fn sync_manual(
    tx: &Transaction<'_>,
    manual: &[ManualEntry],
    current_epoch: u64,
) -> anyhow::Result<()> {
    let accounts: Vec<&str> = manual.iter().map(|e| e.vote_account.as_str()).collect();
    let codes: Vec<&str> = manual.iter().map(|e| e.code.as_str()).collect();
    let reasons: Vec<&str> = manual.iter().map(|e| e.reason.as_str()).collect();
    let epoch = Decimal::from(current_epoch);

    let added = tx
        .execute(
            "INSERT INTO blacklist (vote_account, source, code, reason, added_epoch, created_at, updated_at)
             SELECT vote_account, 'manual', code, reason, $4, now(), now()
             FROM UNNEST($1::TEXT[], $2::TEXT[], $3::TEXT[]) AS t(vote_account, code, reason)
             ON CONFLICT (vote_account, code) WHERE source = 'manual' AND removed_epoch IS NULL
             DO UPDATE SET reason = EXCLUDED.reason, updated_at = now()
             WHERE blacklist.reason IS DISTINCT FROM EXCLUDED.reason",
            &[&accounts, &codes, &reasons, &epoch],
        )
        .await?;
    let removed = tx
        .execute(
            "UPDATE blacklist
             SET removed_epoch = $3, removed_reason = 'removed from config', updated_at = now()
             WHERE source = 'manual' AND removed_epoch IS NULL
               AND NOT EXISTS (
                 SELECT 1 FROM UNNEST($1::TEXT[], $2::TEXT[]) AS t(vote_account, code)
                 WHERE t.vote_account = blacklist.vote_account AND t.code = blacklist.code
               )",
            &[&accounts, &codes, &epoch],
        )
        .await?;
    info!("Manual entries: {added} added or updated, {removed} removed");
    Ok(())
}

/// Replaces the served exemptions with the config's, without their reasons.
async fn sync_exempt(tx: &Transaction<'_>, exempt: &[ExemptEntry]) -> anyhow::Result<()> {
    let accounts: Vec<&str> = exempt.iter().map(|e| e.vote_account.as_str()).collect();
    let rules: Vec<&str> = exempt.iter().map(|e| e.rule.as_str()).collect();
    let until_epochs: Vec<Option<Decimal>> = exempt
        .iter()
        .map(|e| e.until_epoch.map(Decimal::from))
        .collect();
    tx.execute("DELETE FROM blacklist_exempt", &[]).await?;
    tx.execute(
        "INSERT INTO blacklist_exempt (vote_account, rule, until_epoch)
         SELECT * FROM UNNEST($1::TEXT[], $2::TEXT[], $3::NUMERIC[])",
        &[&accounts, &rules, &until_epochs],
    )
    .await?;
    Ok(())
}

/// Closes the auction and legacy rows an exemption covers. Never touches manual rows. An
/// `until_epoch` is held against the epoch an auction row's code names.
async fn apply_exempt(
    tx: &Transaction<'_>,
    exempt: &[ExemptEntry],
    current_epoch: u64,
) -> anyhow::Result<()> {
    let epoch = Decimal::from(current_epoch);
    for entry in exempt {
        let until_epoch = entry.until_epoch.map(Decimal::from);
        let closed = tx
            .execute(
                "UPDATE blacklist
                 SET removed_epoch = $1, removed_reason = 'exempt: ' || $2::TEXT, updated_at = now()
                 WHERE removed_epoch IS NULL AND vote_account = $3
                   AND (
                     (source = 'auction'
                       AND ($4::TEXT = 'all' OR COALESCE(starts_with(code, $5::TEXT), false))
                       AND ($6::NUMERIC IS NULL
                         OR substring(code FROM '_([0-9]+)$')::NUMERIC <= $6))
                     OR (source = 'legacy' AND $4::TEXT IN ('all', 'legacy'))
                   )",
                &[
                    &epoch,
                    &entry.reason,
                    &entry.vote_account,
                    &entry.rule.as_str(),
                    &entry.rule.code_prefix(),
                    &until_epoch,
                ],
            )
            .await?;
        if closed > 0 {
            info!(
                "Exemption closed {closed} rows of {} ({})",
                entry.vote_account,
                entry.rule.as_str()
            );
        }
    }
    Ok(())
}
