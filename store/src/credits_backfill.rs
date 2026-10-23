use clap::Parser;
use collect::solana_service::{
    migration_epoch_credits, parse_epoch_credits, EpochCredits, MAX_TOWER_CREDITS_IN_EPOCH,
};
use log::{info, warn};
use rust_decimal::prelude::*;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::ops::RangeInclusive;
use std::path::PathBuf;
use std::time::Duration;
use tokio_postgres::Client;

// Testnet epochs stored before #292 with the vote reward lamports in `credits`.
// In 1042 the collector stored the entry after the migration marker. The tower half is still in the on-chain `epochCredits`.
pub const TESTNET_LAMPORTS_IN_CREDITS_EPOCHS: RangeInclusive<u64> = 1042..=1053;
pub const TESTNET_ALPENGLOW_MIGRATION_EPOCH: u64 = 1042;

// agave MAX_MULTIPLE_ACCOUNTS
const RPC_ACCOUNTS_IN_REQUEST: usize = 100;
const RPC_REQUEST_PAUSE: Duration = Duration::from_millis(500);
const RPC_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Parser)]
pub struct StoreCreditsBackfillParams {
    #[arg(long = "rpc-url")]
    rpc_url: String,

    #[arg(
        long = "output-dir",
        default_value = "scripts/testnet-credits-backfill"
    )]
    output_dir: PathBuf,
}

async fn migration_epoch_vote_accounts(psql_client: &Client) -> anyhow::Result<Vec<String>> {
    Ok(psql_client
        .query(
            "SELECT vote_account FROM validators WHERE epoch = $1",
            &[&Decimal::from(TESTNET_ALPENGLOW_MIGRATION_EPOCH)],
        )
        .await?
        .iter()
        .map(|row| row.get(0))
        .collect())
}

/// One `getMultipleAccounts` request for each 100 vote accounts, one after the other.
async fn fetch_migration_epoch_credits(
    rpc_url: &str,
    vote_accounts: &[String],
) -> anyhow::Result<HashMap<String, EpochCredits>> {
    let client = reqwest::Client::builder().timeout(RPC_TIMEOUT).build()?;
    let mut credits = HashMap::with_capacity(vote_accounts.len());
    for (index, chunk) in vote_accounts.chunks(RPC_ACCOUNTS_IN_REQUEST).enumerate() {
        if index > 0 {
            tokio::time::sleep(RPC_REQUEST_PAUSE).await;
        }
        let response: Value = client
            .post(rpc_url)
            .json(&json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "getMultipleAccounts",
                "params": [chunk, {"encoding": "jsonParsed"}],
            }))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        anyhow::ensure!(
            response["error"].is_null(),
            "getMultipleAccounts failed: {}",
            response["error"]
        );
        let accounts = response["result"]["value"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("getMultipleAccounts gave no value array"))?;
        for (vote_account, account) in chunk.iter().zip(accounts) {
            match parse_epoch_credits(account)
                .and_then(|history| migration_epoch_credits(history, TESTNET_ALPENGLOW_MIGRATION_EPOCH))
            {
                Some(epoch_credits) => {
                    credits.insert(vote_account.clone(), epoch_credits);
                }
                None => warn!("No epochCredits of epoch {TESTNET_ALPENGLOW_MIGRATION_EPOCH} for {vote_account}"),
            }
        }
    }
    Ok(credits)
}

pub async fn store_credits_backfill(
    params: StoreCreditsBackfillParams,
    psql_client: &Client,
) -> anyhow::Result<()> {
    let vote_accounts = migration_epoch_vote_accounts(psql_client).await?;
    let migration_credits = fetch_migration_epoch_credits(&params.rpc_url, &vote_accounts).await?;
    info!(
        "Read the epochCredits of {} of {} vote accounts of epoch {TESTNET_ALPENGLOW_MIGRATION_EPOCH}",
        migration_credits.len(),
        vote_accounts.len()
    );
    let sql = credits_backfill_sql(&migration_credits)?;
    std::fs::create_dir_all(&params.output_dir)?;
    let path = params.output_dir.join("backfill.sql");
    std::fs::write(&path, sql)?;
    info!("Wrote {}", path.display());
    Ok(())
}

/// The SQL fails and changes nothing if one of the epochs has no credits above the tower limit,
/// or if an on-chain reward of the migration epoch is not the stored `credits`.
pub fn credits_backfill_sql(
    migration_credits: &HashMap<String, EpochCredits>,
) -> anyhow::Result<String> {
    anyhow::ensure!(
        migration_credits
            .values()
            .any(|credits| credits.tower_credits.is_some()),
        "The RPC gave no tower credits of epoch {TESTNET_ALPENGLOW_MIGRATION_EPOCH}: epochCredits no longer holds the epoch"
    );
    let mut on_chain: Vec<_> = migration_credits.iter().collect();
    on_chain.sort_by_key(|(vote_account, _)| vote_account.as_str());
    let number = |value: Option<u64>| value.map_or("NULL".to_string(), |value| value.to_string());
    let rows = on_chain
        .iter()
        .map(|(vote_account, credits)| {
            format!(
                "    ('{}', {}, {})",
                vote_account.replace('\'', "''"),
                number(credits.tower_credits),
                number(credits.vote_reward_lamports)
            )
        })
        .collect::<Vec<_>>()
        .join(",\n");
    let first_epoch = TESTNET_LAMPORTS_IN_CREDITS_EPOCHS.start();
    let last_epoch = TESTNET_LAMPORTS_IN_CREDITS_EPOCHS.end();
    let migration_epoch = TESTNET_ALPENGLOW_MIGRATION_EPOCH;
    Ok(format!(
        r#"-- On-chain epochCredits of epoch {migration_epoch}, read on {generated_at}.
BEGIN;

CREATE TEMP TABLE credits_backfill_on_chain (
    vote_account TEXT PRIMARY KEY,
    tower_credits NUMERIC,
    vote_reward_lamports NUMERIC
) ON COMMIT DROP;

INSERT INTO credits_backfill_on_chain (vote_account, tower_credits, vote_reward_lamports) VALUES
{rows};

DO $$
DECLARE
    epoch_without_lamports NUMERIC;
    mismatches BIGINT;
BEGIN
    SELECT e INTO epoch_without_lamports
    FROM generate_series({first_epoch}, {last_epoch}) AS e
    WHERE NOT EXISTS (
        SELECT 1 FROM validators WHERE epoch = e AND credits > {MAX_TOWER_CREDITS_IN_EPOCH}
    )
    LIMIT 1;
    IF epoch_without_lamports IS NOT NULL THEN
        RAISE EXCEPTION 'Epoch % has no credits above {MAX_TOWER_CREDITS_IN_EPOCH}: the database is not testnet, or the backfill already ran', epoch_without_lamports;
    END IF;

    SELECT COUNT(*) INTO mismatches
    FROM validators v
    JOIN credits_backfill_on_chain c USING (vote_account)
    WHERE v.epoch = {migration_epoch}
      AND c.vote_reward_lamports IS NOT NULL
      AND v.credits IS DISTINCT FROM c.vote_reward_lamports;
    IF mismatches > 0 THEN
        RAISE EXCEPTION '% rows of epoch {migration_epoch} hold credits that are not the on-chain reward', mismatches;
    END IF;
END $$;

UPDATE validators
SET vote_reward_lamports = CASE WHEN credits = 0 AND activated_stake = 0 THEN NULL ELSE credits END,
    credits = NULL
WHERE epoch BETWEEN {first_epoch} AND {last_epoch}
  AND credits IS NOT NULL
  AND vote_reward_lamports IS NULL;

UPDATE validators v
SET credits = c.tower_credits
FROM credits_backfill_on_chain c
WHERE v.epoch = {migration_epoch}
  AND v.vote_account = c.vote_account
  AND c.tower_credits IS NOT NULL;

COMMIT;
"#,
        generated_at = chrono::Utc::now().format("%Y-%m-%d %H:%M UTC"),
    ))
}
