use crate::validators_block_rewards::VALIDATORS_BLOCK_REWARDS_TABLE;
use rust_decimal::Decimal;
use tokio_postgres::Client;

/// Aggregates rewards by epoch, excluding epochs with too many NULLs in total_epoch_rewards
/// to avoid not fully collected data
async fn get_rewards_by_table(
    psql_client: &Client,
    table_name: &str,
    amount_column_name: &str,
    epochs: u64,
    limit_null_count: u8,
) -> anyhow::Result<Vec<(u64, f64)>> {
    let query = format!(
        r#"
        SELECT SUM(COALESCE({amount_column_name}, 0)) / 1e9 AS amount, epoch
        FROM {table_name}
        GROUP BY epoch
        HAVING COUNT(CASE WHEN {amount_column_name} IS NULL THEN 1 END) < {limit_null_count}
        ORDER BY epoch DESC LIMIT $1
        "#
    );

    // amount_column_name may be NULL as data on commission is loaded at start of epoch
    // when not run snapshot processing that updates data about rewards
    // query ignores whole epoch if there is at least one NULL value
    let rows = psql_client
        .query(&query, &[&i64::try_from(epochs)?])
        .await?;

    Ok(rows
        .into_iter()
        .map(|row| {
            (
                row.get::<_, Decimal>("epoch").try_into().unwrap(),
                row.get::<_, Decimal>("amount").try_into().unwrap(),
            )
        })
        .collect())
}

async fn get_jito_rewards_by_table(
    psql_client: &Client,
    table_name: &str,
    epochs: u64,
    limit_null_count: u8,
) -> anyhow::Result<Vec<(u64, f64)>> {
    get_rewards_by_table(
        psql_client,
        table_name,
        "total_epoch_rewards",
        epochs,
        limit_null_count,
    )
    .await
}

pub async fn get_mev_rewards(psql_client: &Client, epochs: u64) -> anyhow::Result<Vec<(u64, f64)>> {
    // limit_null_count: expecting there are many rows, we want to have at least 10 filled, then considering data is well loaded
    get_jito_rewards_by_table(psql_client, "mev", epochs, 10).await
}

pub async fn get_jito_priority_rewards(
    psql_client: &Client,
    epochs: u64,
) -> anyhow::Result<Vec<(u64, f64)>> {
    // limit_null_count: expecting there are few rows, we want at least one filled to consider data is well loaded
    get_jito_rewards_by_table(psql_client, "jito_priority_fee", epochs, 1).await
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
                row.get::<_, f64>("amount"),
            )
        })
        .collect())
}

pub async fn get_block_rewards(
    psql_client: &Client,
    epochs: u64,
) -> anyhow::Result<Vec<(u64, f64)>> {
    // limit_null_count: at least 10 rows per epoch filled, then considering data is well loaded
    get_rewards_by_table(
        psql_client,
        VALIDATORS_BLOCK_REWARDS_TABLE,
        "amount",
        epochs,
        10,
    )
    .await
}
