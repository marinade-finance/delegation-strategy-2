use clap::Parser;
use collect::solana_service::MAX_TOWER_CREDITS_IN_EPOCH;
use log::info;
use rust_decimal::prelude::*;
use std::ops::RangeInclusive;
use tokio_postgres::Client;

// Testnet epochs stored before #292 with the vote reward lamports in `credits`.
// In 1042 the collector kept the entry after the migration marker, so the tower credits of 1042 are lost.
pub const TESTNET_LAMPORTS_IN_CREDITS_EPOCHS: RangeInclusive<u64> = 1042..=1053;

#[derive(Debug, Parser)]
pub struct StoreCreditsBackfillParams {}

/// Fails and changes nothing if one of the epochs has no credits above the tower limit.
pub async fn store_credits_backfill(psql_client: &mut Client) -> anyhow::Result<()> {
    let epochs: Vec<Decimal> = TESTNET_LAMPORTS_IN_CREDITS_EPOCHS
        .map(Decimal::from)
        .collect();
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
    transaction.commit().await?;
    info!(
        "Moved credits to vote_reward_lamports in {moved} rows of epochs {TESTNET_LAMPORTS_IN_CREDITS_EPOCHS:?}"
    );
    Ok(())
}
