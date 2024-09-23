use chrono::{DateTime, Utc};
use std::str::FromStr;
use tokio_postgres::Client;
use crate::dto::EpochInfo;

pub async fn get_epochs(psql_client: &Client) -> anyhow::Result<Vec<EpochInfo>> {
    let rows = psql_client
        .query("SELECT epoch::BIGINT, start_at, end_at FROM epochs ORDER BY epoch ASC", &[])
        .await?;

    let epochs: Vec<EpochInfo> = rows.into_iter().map(|row| {
        let epoch: i64 = row.get(0);
        let epoch_as_u64 = epoch as u64;

        EpochInfo {
            epoch: epoch_as_u64,
            start_at: row.get::<_, DateTime<Utc>>(1),
            end_at: row.get::<_, DateTime<Utc>>(2),
        }
    }).collect();

    Ok(epochs)
}