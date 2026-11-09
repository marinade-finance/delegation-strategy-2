use rust_decimal::prelude::*;
use std::collections::HashSet;
use tokio_postgres::GenericClient;

#[derive(Debug, Clone, PartialEq)]
pub struct BlacklistRow {
    pub vote_account: String,
    pub source: String,
    pub rule: Option<String>,
    pub code: String,
    pub reason: Option<String>,
    pub added_epoch: Option<u64>,
    pub evidence: Option<serde_json::Value>,
}

/// Active rows ordered by vote account, then code.
pub async fn load_active(client: &impl GenericClient) -> anyhow::Result<Vec<BlacklistRow>> {
    let rows = client
        .query(
            "SELECT vote_account, source, rule, code, reason, added_epoch, evidence::TEXT AS evidence
             FROM blacklist
             WHERE removed_epoch IS NULL
             ORDER BY vote_account, code, id",
            &[],
        )
        .await?;
    rows.into_iter()
        .map(|row| {
            Ok(BlacklistRow {
                vote_account: row.get("vote_account"),
                source: row.get("source"),
                rule: row.get("rule"),
                code: row.get("code"),
                reason: row.get("reason"),
                added_epoch: row
                    .get::<_, Option<Decimal>>("added_epoch")
                    .map(u64::try_from)
                    .transpose()?,
                evidence: row
                    .get::<_, Option<String>>("evidence")
                    .map(|text| serde_json::from_str(&text))
                    .transpose()?,
            })
        })
        .collect()
}

pub async fn load_active_accounts(client: &impl GenericClient) -> anyhow::Result<HashSet<String>> {
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

pub async fn last_evaluated_epoch(client: &impl GenericClient) -> anyhow::Result<Option<u64>> {
    Ok(client
        .query_one("SELECT MAX(epoch) AS epoch FROM blacklist_evaluations", &[])
        .await?
        .get::<_, Option<Decimal>>("epoch")
        .map(u64::try_from)
        .transpose()?)
}
