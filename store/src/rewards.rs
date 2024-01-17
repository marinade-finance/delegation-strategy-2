use rust_decimal::Decimal;
use tokio_postgres::Client;

pub async fn get_mev_rewards(psql_client: &Client, epochs: u64) -> anyhow::Result<Vec<(u64, f64)>> {
    let rows = psql_client
        .query("SELECT SUM(total_epoch_rewards) / 1e9 AS amount, epoch FROM mev GROUP BY epoch ORDER BY epoch DESC LIMIT $1", &[&i64::try_from(epochs)?])
        .await?;

    Ok(rows
        .into_iter()
        .map(|row| {
            (
                row.get::<_, i32>("epoch").try_into().unwrap(),
                row.get::<_, Decimal>("amount").try_into().unwrap(),
            )
        })
        .collect())
}

pub async fn get_estimated_inflation_rewards(
    psql_client: &Client,
    epochs: u64,
) -> anyhow::Result<Vec<(u64, f64)>> {
    let rows = psql_client
        .query("SELECT epoch, supply * inflation / 1e9 / (365.25 / 2) AS amount FROM epochs ORDER BY epoch DESC LIMIT $1", &[&i64::try_from(epochs)?])
        .await?;

    Ok(rows
        .into_iter()
        .map(|row| {
            (
                row.get::<_, Decimal>("epoch").try_into().unwrap(),
                row.get::<_, f64>("amount").try_into().unwrap(),
            )
        })
        .collect())
}
