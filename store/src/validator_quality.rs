use rust_decimal::prelude::*;
use serde::Serialize;
use tokio_postgres::Client;

/// One validator's quality inputs at an epoch. A field is `null` when its source has no row for it.
#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
pub struct ValidatorQualityRecord {
    pub vote_account: String,
    /// Over the 30-day window the sandwich rates are measured on, not over the epoch.
    pub blocks_produced: Option<u64>,
    pub blocks_with_sandwiches: Option<u64>,
    /// Percent.
    pub sandwich_rate_30d: Option<f64>,
    /// Percent.
    pub wide_sandwich_rate_30d: Option<f64>,
    pub slot_duration_median_ms: Option<f64>,
    pub ibrl_score: Option<f64>,
}

pub async fn load_validator_quality(
    psql_client: &Client,
    epoch: u64,
) -> anyhow::Result<Vec<ValidatorQualityRecord>> {
    let rows = psql_client
        .query(
            "SELECT
                COALESCE(s.vote_account, t.vote_account) AS vote_account,
                s.blocks_produced,
                s.blocks_with_sandwiches,
                s.sandwich_rate_30d,
                s.wide_sandwich_rate_30d,
                t.slot_duration_median_ms,
                t.ibrl_score
             FROM (SELECT * FROM validators_sandwiches WHERE epoch = $1) s
             FULL OUTER JOIN (
                SELECT * FROM validators_slot_times WHERE epoch = $1 AND source = 'trillium'
             ) t ON t.vote_account = s.vote_account
             ORDER BY vote_account",
            &[&Decimal::from(epoch)],
        )
        .await?;

    rows.into_iter()
        .map(|row| {
            let count = |column: &str| -> anyhow::Result<Option<u64>> {
                Ok(row
                    .get::<_, Option<Decimal>>(column)
                    .map(u64::try_from)
                    .transpose()?)
            };
            Ok(ValidatorQualityRecord {
                blocks_produced: count("blocks_produced")?,
                blocks_with_sandwiches: count("blocks_with_sandwiches")?,
                vote_account: row.get("vote_account"),
                sandwich_rate_30d: row.get("sandwich_rate_30d"),
                wide_sandwich_rate_30d: row.get("wide_sandwich_rate_30d"),
                slot_duration_median_ms: row.get("slot_duration_median_ms"),
                ibrl_score: row.get("ibrl_score"),
            })
        })
        .collect()
}
