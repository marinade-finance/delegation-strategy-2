use clap::Parser;
use collect::solana_service::{
    migration_epoch_credits, parse_epoch_credits, EpochCredits, MAX_TOWER_CREDITS_IN_EPOCH,
};
use log::{info, warn};
use rust_decimal::prelude::*;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::ops::RangeInclusive;
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
    #[arg(long = "rpc-url", env = "RPC_URL")]
    rpc_url: String,
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
    psql_client: &mut Client,
) -> anyhow::Result<()> {
    let vote_accounts = migration_epoch_vote_accounts(psql_client).await?;
    let migration_credits = fetch_migration_epoch_credits(&params.rpc_url, &vote_accounts).await?;
    info!(
        "Read the epochCredits of {} of {} vote accounts of epoch {TESTNET_ALPENGLOW_MIGRATION_EPOCH}",
        migration_credits.len(),
        vote_accounts.len()
    );
    apply_credits_backfill(psql_client, &migration_credits).await
}

/// Fails and changes nothing if one of the epochs has no credits above the tower limit,
/// or if an on-chain reward of the migration epoch is not the stored `credits`.
pub async fn apply_credits_backfill(
    psql_client: &mut Client,
    migration_credits: &HashMap<String, EpochCredits>,
) -> anyhow::Result<()> {
    let epochs: Vec<Decimal> = TESTNET_LAMPORTS_IN_CREDITS_EPOCHS
        .map(Decimal::from)
        .collect();
    let migration_epoch = Decimal::from(TESTNET_ALPENGLOW_MIGRATION_EPOCH);
    let transaction = psql_client.transaction().await?;
    for epoch in &epochs {
        let lamports_rows: i64 = transaction
            .query_one(
                "SELECT COUNT(*) FROM validators WHERE epoch = $1 AND credits > $2",
                &[epoch, &Decimal::from(MAX_TOWER_CREDITS_IN_EPOCH)],
            )
            .await?
            .get(0);
        anyhow::ensure!(
            lamports_rows > 0,
            "Epoch {epoch} has no credits above {MAX_TOWER_CREDITS_IN_EPOCH}: the database is not testnet, or the backfill already ran"
        );
    }

    let stored = transaction
        .query(
            "SELECT vote_account, credits FROM validators WHERE epoch = $1",
            &[&migration_epoch],
        )
        .await?;
    let mismatches: Vec<String> = stored
        .iter()
        .filter_map(|row| {
            let vote_account: String = row.get(0);
            let credits: Option<Decimal> = row.get(1);
            let lamports = migration_credits.get(&vote_account)?.vote_reward_lamports?;
            (credits != Some(Decimal::from(lamports)))
                .then(|| format!("{vote_account}: stored {credits:?}, on chain {lamports}"))
        })
        .collect();
    anyhow::ensure!(
        mismatches.is_empty(),
        "{} rows of epoch {migration_epoch} hold credits that are not the on-chain reward, for example {}",
        mismatches.len(),
        mismatches[..mismatches.len().min(3)].join(", ")
    );

    let moved = transaction
        .execute(
            "UPDATE validators
            SET vote_reward_lamports = CASE WHEN credits = 0 AND activated_stake = 0 THEN NULL ELSE credits END,
                credits = NULL
            WHERE epoch = ANY($1)
              AND credits IS NOT NULL
              AND vote_reward_lamports IS NULL",
            &[&epochs],
        )
        .await?;

    let (tower_vote_accounts, tower_credits): (Vec<String>, Vec<Decimal>) = migration_credits
        .iter()
        .filter_map(|(vote_account, credits)| {
            Some((vote_account.clone(), Decimal::from(credits.tower_credits?)))
        })
        .unzip();
    anyhow::ensure!(
        !tower_credits.is_empty(),
        "The RPC gave no tower credits of epoch {migration_epoch}: epochCredits no longer holds the epoch"
    );
    let restored = transaction
        .execute(
            "UPDATE validators
            SET credits = u.credits
            FROM UNNEST($2::TEXT[], $3::NUMERIC[]) AS u(vote_account, credits)
            WHERE validators.epoch = $1
              AND validators.vote_account = u.vote_account",
            &[&migration_epoch, &tower_vote_accounts, &tower_credits],
        )
        .await?;
    transaction.commit().await?;
    info!(
        "Moved credits to vote_reward_lamports in {moved} rows of epochs {TESTNET_LAMPORTS_IN_CREDITS_EPOCHS:?}"
    );
    info!("Restored the tower credits of {restored} rows of epoch {migration_epoch}");
    Ok(())
}
