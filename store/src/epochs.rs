use crate::dto::EpochRecord;
use rust_decimal::prelude::*;
use tokio_postgres::Client;

pub async fn load_epochs(psql_client: &Client, epochs: u64) -> anyhow::Result<Vec<EpochRecord>> {
    let rows = psql_client
        .query(
            "SELECT epoch, start_at, end_at FROM epochs ORDER BY epoch DESC LIMIT $1",
            &[&i64::try_from(epochs)?],
        )
        .await?;

    Ok(rows
        .iter()
        .map(|row| EpochRecord {
            epoch: row.get::<_, Decimal>("epoch").to_u64().unwrap(),
            start_at: row.get("start_at"),
            end_at: row.get("end_at"),
        })
        .collect())
}
