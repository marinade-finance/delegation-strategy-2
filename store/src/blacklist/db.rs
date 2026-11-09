use rust_decimal::prelude::*;
use serde::Serialize;
use std::collections::HashSet;
use tokio_postgres::GenericClient;

#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
pub struct BlacklistRow {
    pub vote_account: String,
    /// `manual`, `rule` or `legacy`.
    pub source: String,
    /// The rule that listed the account, for `source = rule`.
    pub rule: Option<String>,
    pub code: String,
    pub reason: Option<String>,
    pub added_epoch: Option<u64>,
    /// Observed values the rule judged.
    #[schema(value_type = Option<Object>)]
    pub evidence: Option<serde_json::Value>,
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

/// The last epoch the rules judged or skipped, and the active rows ordered by vote account, then
/// code. The epoch is read first, so it never names an evaluation newer than the rows.
pub async fn load_blacklist(
    client: &impl GenericClient,
) -> anyhow::Result<(Option<u64>, Vec<BlacklistRow>)> {
    let evaluated_epoch = max_epoch(client, "blacklist_evaluations").await?;
    let rows = client
        .query(
            "SELECT vote_account, source, rule, code, reason, added_epoch, evidence::TEXT AS evidence
             FROM blacklist
             WHERE removed_epoch IS NULL
             ORDER BY vote_account, code, id",
            &[],
        )
        .await?;
    let rows = rows
        .into_iter()
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
        .collect::<anyhow::Result<_>>()?;
    Ok((evaluated_epoch, rows))
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

/// Closed epochs the rules have not reached, and epochs they skipped for missing inputs. The lag
/// is 0 while the rules are off; it is normally 1, the running epoch itself.
pub async fn evaluation_status(
    client: &impl GenericClient,
    start_epoch: Option<u64>,
) -> anyhow::Result<(u64, u64)> {
    let skipped: i64 = client
        .query_one(
            "SELECT COUNT(*) AS skipped FROM blacklist_evaluations WHERE skipped",
            &[],
        )
        .await?
        .get("skipped");
    let Some(start_epoch) = start_epoch else {
        return Ok((0, skipped.try_into()?));
    };
    let current = max_epoch(client, "cluster_info").await?.unwrap_or(0);
    let reached = max_epoch(client, "blacklist_evaluations")
        .await?
        .unwrap_or(start_epoch.saturating_sub(1));
    Ok((current.saturating_sub(reached), skipped.try_into()?))
}
