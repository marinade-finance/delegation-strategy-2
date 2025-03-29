use rust_decimal::Decimal;
use tokio_postgres::Client;

pub async fn get_mev_rewards(psql_client: &Client, epochs: u64) -> anyhow::Result<Vec<(u64, f64)>> {
    // total_epoch_rewards may be NULL as data on commission is loaded at start of epoch
    // when JITO has not run own snapshot processing that updates data about rewards
    // query ignores whole epoch if there is at least one NULL value
    let rows = psql_client
        .query(
            r#"
             SELECT SUM(total_epoch_rewards) / 1e9 AS amount, epoch
             FROM mev
             GROUP BY epoch
             HAVING COUNT(CASE WHEN total_epoch_rewards IS NULL THEN 1 END) < 10
             ORDER BY epoch
             DESC LIMIT $1"#,
            &[&i64::try_from(epochs)?],
        )
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
