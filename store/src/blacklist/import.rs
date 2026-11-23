use super::config_sync::EXEMPT_COVERS;
use super::db::{bootstrapped, max_epoch};
use crate::dto::BlacklistRecord;
use clap::Parser;
use csv::{required, Column};
use log::info;
use rust_decimal::prelude::*;
use std::path::Path;
use tokio_postgres::Client;

const BLACKLIST_COLUMNS: [Column; 2] = [required("vote_account"), required("code")];

#[derive(Debug, Parser)]
pub struct ImportParams {
    #[arg(
        long = "csv",
        help = "The blacklist.csv to import (vote_account,code)."
    )]
    csv_path: String,

    #[arg(
        long = "source-ref",
        help = "Where the file comes from, e.g. the delegation-strategy-2 commit. Kept in every row's reason."
    )]
    source_ref: String,
}

/// One-off: today's hand-kept list becomes `legacy` rows. Refuses to run twice.
pub async fn import_legacy(params: ImportParams, psql_client: &mut Client) -> anyhow::Result<()> {
    let records: Vec<BlacklistRecord> =
        csv::load_path(Path::new(&params.csv_path), &BLACKLIST_COLUMNS)?;
    info!(
        "Importing {} blacklist rows from {}",
        records.len(),
        params.csv_path
    );

    let tx = psql_client.transaction().await?;
    tx.batch_execute("SELECT pg_advisory_xact_lock(hashtext('blacklist'))")
        .await?;
    anyhow::ensure!(
        !bootstrapped(&tx).await?,
        "legacy blacklist rows exist already, refusing to import again"
    );

    let vote_accounts: Vec<&str> = records.iter().map(|r| r.vote_account.as_str()).collect();
    let codes: Vec<&str> = records.iter().map(|r| r.code.as_str()).collect();
    let reason = format!("imported from ds2 blacklist.csv @ {}", params.source_ref);
    let inserted = tx
        .execute(
            "INSERT INTO blacklist (vote_account, source, code, reason, created_at, updated_at)
             SELECT vote_account, 'legacy', code, $3, now(), now()
             FROM UNNEST($1::TEXT[], $2::TEXT[]) AS t(vote_account, code)",
            &[&vote_accounts, &codes, &reason],
        )
        .await?;
    let mut exempted = 0;
    if let Some(current_epoch) = max_epoch(&tx, "cluster_info").await? {
        exempted = tx
            .execute(
                &format!(
                    "UPDATE blacklist SET removed_epoch = $1, removed_reason = 'exempt', updated_at = now()
                     FROM blacklist_exempt AS x
                     WHERE blacklist.removed_epoch IS NULL AND blacklist.source = 'legacy' AND {EXEMPT_COVERS}"
                ),
                &[&Decimal::from(current_epoch)],
            )
            .await?;
    }
    tx.commit().await?;

    info!("Imported {inserted} legacy blacklist rows, {exempted} of them exempt");
    Ok(())
}
