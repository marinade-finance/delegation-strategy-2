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

/// Whether exemption `x` (vote_account, rule, code_prefix, until_epoch) covers a `blacklist` row:
/// auction and legacy rows only, never manual ones. An `until_epoch` is held against the epoch an
/// auction row's code names.
pub(super) const EXEMPT_COVERS: &str = "blacklist.vote_account = x.vote_account
    AND ((blacklist.source = 'auction'
          AND (x.rule = 'all' OR COALESCE(starts_with(blacklist.code, x.code_prefix), false))
          AND (x.until_epoch IS NULL
            OR substring(blacklist.code FROM '_([0-9]+)$')::NUMERIC <= x.until_epoch))
      OR (blacklist.source = 'legacy' AND x.rule IN ('all', 'legacy')))";

/// Replaces the served exemptions with the config's, without their reasons.
async fn sync_exempt(tx: &Transaction<'_>, exempt: &[ExemptEntry]) -> anyhow::Result<()> {
    let accounts: Vec<&str> = exempt.iter().map(|e| e.vote_account.as_str()).collect();
    let rules: Vec<&str> = exempt.iter().map(|e| e.rule.as_str()).collect();
    let prefixes: Vec<Option<&str>> = exempt.iter().map(|e| e.rule.code_prefix()).collect();
    let until_epochs: Vec<Option<Decimal>> = exempt
        .iter()
        .map(|e| e.until_epoch.map(Decimal::from))
        .collect();
    tx.execute("DELETE FROM blacklist_exempt", &[]).await?;
    tx.execute(
        "INSERT INTO blacklist_exempt (vote_account, rule, code_prefix, until_epoch)
         SELECT * FROM UNNEST($1::TEXT[], $2::TEXT[], $3::TEXT[], $4::NUMERIC[])",
        &[&accounts, &rules, &prefixes, &until_epochs],
    )
    .await?;
    Ok(())
}

/// Closes the rows the exemptions cover.
async fn apply_exempt(
    tx: &Transaction<'_>,
    exempt: &[ExemptEntry],
    current_epoch: u64,
) -> anyhow::Result<()> {
    let accounts: Vec<&str> = exempt.iter().map(|e| e.vote_account.as_str()).collect();
    let rules: Vec<&str> = exempt.iter().map(|e| e.rule.as_str()).collect();
    let prefixes: Vec<Option<&str>> = exempt.iter().map(|e| e.rule.code_prefix()).collect();
    let until_epochs: Vec<Option<Decimal>> = exempt
        .iter()
        .map(|e| e.until_epoch.map(Decimal::from))
        .collect();
    let reasons: Vec<&str> = exempt.iter().map(|e| e.reason.as_str()).collect();
    let closed = tx
        .execute(
            &format!(
                "UPDATE blacklist
                 SET removed_epoch = $1, removed_reason = 'exempt: ' || x.reason, updated_at = now()
                 FROM UNNEST($2::TEXT[], $3::TEXT[], $4::TEXT[], $5::NUMERIC[], $6::TEXT[])
                   AS x(vote_account, rule, code_prefix, until_epoch, reason)
                 WHERE blacklist.removed_epoch IS NULL AND {EXEMPT_COVERS}"
            ),
            &[
                &Decimal::from(current_epoch),
                &accounts,
                &rules,
                &prefixes,
                &until_epochs,
                &reasons,
            ],
        )
        .await?;
    info!("Exemptions closed {closed} rows");
    Ok(())
}
