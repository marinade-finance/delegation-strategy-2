use rust_decimal::prelude::*;
use serde::Serialize;
use std::collections::HashSet;
use tokio_postgres::GenericClient;

#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
pub struct BlacklistRow {
    pub vote_account: String,
    /// `legacy`, `manual` or `auction`.
    pub source: String,
    pub code: String,
    /// The SAM auction that listed the account, for `source = auction`.
    pub auction_epoch: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
pub struct BlacklistExempt {
    pub vote_account: String,
    /// `all`, `sandwich`, `slow_slots` or `legacy`.
    pub rule: String,
    /// Listings on evidence up to this epoch are exempt; absent for as long as the entry exists.
    pub until_epoch: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
pub struct Blacklist {
    pub rows: Vec<BlacklistRow>,
    pub exempt: Vec<BlacklistExempt>,
}

/// `MAX(epoch)` of a table; `None` while it is empty.
pub async fn max_epoch(client: &impl GenericClient, table: &str) -> anyhow::Result<Option<u64>> {
    Ok(client
        .query_one(&format!("SELECT MAX(epoch) AS epoch FROM {table}"), &[])
        .await?
        .get::<_, Option<Decimal>>("epoch")
        .map(u64::try_from)
        .transpose()?)
}

/// Whether the legacy list was imported. Before that the table holds only part of the list, and
/// serving it would unlist everyone else.
pub async fn bootstrapped(client: &impl GenericClient) -> anyhow::Result<bool> {
    Ok(client
        .query_one(
            "SELECT EXISTS (SELECT 1 FROM blacklist WHERE source = 'legacy') AS bootstrapped",
            &[],
        )
        .await?
        .get("bootstrapped"))
}

fn optional_epoch(value: Option<Decimal>) -> anyhow::Result<Option<u64>> {
    Ok(value.map(u64::try_from).transpose()?)
}

/// The active rows ordered by vote account, then code, and the exemptions; `None` before the
/// legacy import.
pub async fn load_blacklist(client: &impl GenericClient) -> anyhow::Result<Option<Blacklist>> {
    if !bootstrapped(client).await? {
        return Ok(None);
    }
    let rows = client
        .query(
            "SELECT vote_account, source, code, auction_epoch FROM blacklist
             WHERE removed_epoch IS NULL
             ORDER BY vote_account, code, id",
            &[],
        )
        .await?
        .into_iter()
        .map(|row| {
            Ok(BlacklistRow {
                vote_account: row.get("vote_account"),
                source: row.get("source"),
                code: row.get("code"),
                auction_epoch: optional_epoch(row.get("auction_epoch"))?,
            })
        })
        .collect::<anyhow::Result<_>>()?;
    let exempt = client
        .query(
            "SELECT vote_account, rule, until_epoch FROM blacklist_exempt ORDER BY vote_account, rule",
            &[],
        )
        .await?
        .into_iter()
        .map(|row| {
            Ok(BlacklistExempt {
                vote_account: row.get("vote_account"),
                rule: row.get("rule"),
                until_epoch: optional_epoch(row.get("until_epoch"))?,
            })
        })
        .collect::<anyhow::Result<_>>()?;
    Ok(Some(Blacklist { rows, exempt }))
}

/// Epochs from the last imported SAM auction to the current one; 0 before the first import, so
/// nothing pages until the auction decides the list.
pub async fn import_lag_epochs(client: &impl GenericClient) -> anyhow::Result<u64> {
    let Some(imported) = max_epoch_of(client, "auction_epoch", "blacklist_auction_imports").await?
    else {
        return Ok(0);
    };
    let current = max_epoch(client, "cluster_info").await?.unwrap_or(imported);
    Ok(current.saturating_sub(imported))
}

async fn max_epoch_of(
    client: &impl GenericClient,
    column: &str,
    table: &str,
) -> anyhow::Result<Option<u64>> {
    optional_epoch(
        client
            .query_one(&format!("SELECT MAX({column}) AS epoch FROM {table}"), &[])
            .await?
            .get("epoch"),
    )
}

/// Accounts with an active row. Fails before the legacy import rather than return a partial set.
pub async fn load_active_accounts(client: &impl GenericClient) -> anyhow::Result<HashSet<String>> {
    anyhow::ensure!(
        bootstrapped(client).await?,
        "The blacklist is not bootstrapped yet: run the legacy import first"
    );
    Ok(client
        .query(
            "SELECT DISTINCT vote_account FROM blacklist WHERE removed_epoch IS NULL",
            &[],
        )
        .await?
        .into_iter()
        .map(|row| row.get("vote_account"))
        .collect())
}
