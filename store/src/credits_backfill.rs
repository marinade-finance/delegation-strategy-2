use crate::dto::CreditsColumns;
use crate::utils::UpdateQueryCombiner;
use clap::Parser;
use collect::credits_backfill::CreditsBackfill;
use collect::solana_service::MAX_TOWER_CREDITS_IN_EPOCH;
use log::{info, warn};
use rust_decimal::prelude::*;
use std::collections::HashMap;
use tokio_postgres::{types::ToSql, Client, Transaction};

const DEFAULT_CHUNK_SIZE: usize = 500;

#[derive(Debug, Parser)]
pub struct StoreCreditsBackfillParams {
    #[arg(long = "snapshot-file")]
    snapshot_path: String,
}

pub async fn store_credits_backfill(
    params: StoreCreditsBackfillParams,
    psql_client: &mut Client,
) -> anyhow::Result<()> {
    let backfill: CreditsBackfill =
        serde_yaml::from_reader(std::fs::File::open(params.snapshot_path)?)?;
    backfill_credits(psql_client, &backfill).await
}

/// Moves credits to vote_reward_lamports in the rows of `epoch` whose vote account is not in the backfill (closed, or no epochCredits).
/// Their credits hold the vote reward in lamports or 0 for no entry, except values below the tower limit in an epoch with tower credits.
async fn move_credits_to_reward(
    transaction: &Transaction<'_>,
    epoch: &Decimal,
    vote_accounts: &[&String],
    alpenglow_epoch: bool,
) -> anyhow::Result<u64> {
    Ok(transaction
        .execute(
            "UPDATE validators
            SET vote_reward_lamports = CASE WHEN credits = 0 AND activated_stake = 0 THEN NULL ELSE credits END,
                credits = NULL
            WHERE epoch = $1
              AND vote_account <> ALL($2)
              AND credits IS NOT NULL
              AND vote_reward_lamports IS NULL
              AND ($3 OR credits > $4)",
            &[
                epoch,
                &vote_accounts,
                &alpenglow_epoch,
                &Decimal::from(MAX_TOWER_CREDITS_IN_EPOCH),
            ],
        )
        .await?)
}

/// Sets credits and vote_reward_lamports of each row in the file to the file values, also when both are None.
/// A row whose vote account is not in the file gets its credits moved to vote_reward_lamports, see `move_credits_to_reward`.
pub async fn backfill_credits(
    psql_client: &mut Client,
    backfill: &CreditsBackfill,
) -> anyhow::Result<()> {
    let transaction = psql_client.transaction().await?;
    for (epoch, credits) in &backfill.epochs {
        let epoch = Decimal::from(*epoch);
        let records: Vec<(&String, CreditsColumns)> = credits
            .iter()
            .map(|(vote_account, c)| {
                (
                    vote_account,
                    CreditsColumns {
                        credits: c.tower_credits.map(Decimal::from),
                        vote_reward_lamports: c.vote_reward_lamports.map(Decimal::from),
                    },
                )
            })
            .collect();
        let mut updated = 0;
        for chunk in records.chunks(DEFAULT_CHUNK_SIZE) {
            let mut query = UpdateQueryCombiner::new(
                "validators".to_string(),
                "
                credits = u.credits,
                -- A 0 reward means no epochCredits entry, which counts as 0 only with stake in this epoch.
                -- A reward above 0 stays, because it is paid on the stake of the previous epoch.
                vote_reward_lamports = CASE WHEN validators.activated_stake = 0 AND u.vote_reward_lamports = 0 THEN NULL ELSE u.vote_reward_lamports END
                "
                .to_string(),
                "u(vote_account, epoch, credits, vote_reward_lamports)".to_string(),
                "validators.vote_account = u.vote_account AND validators.epoch = u.epoch"
                    .to_string(),
            );
            for (vote_account, columns) in chunk {
                let mut params: Vec<&(dyn ToSql + Sync)> = vec![
                    vote_account,
                    &epoch,
                    &columns.credits,
                    &columns.vote_reward_lamports,
                ];
                query.add(
                    &mut params,
                    HashMap::from_iter([
                        (1, "NUMERIC".into()),
                        (2, "NUMERIC".into()),
                        (3, "NUMERIC".into()),
                    ]),
                );
            }
            updated += query.execute_in(&transaction).await?.unwrap_or(0);
        }
        let rows: i64 = transaction
            .query_one(
                "SELECT COUNT(*) FROM validators WHERE epoch = $1",
                &[&epoch],
            )
            .await?
            .get(0);
        let vote_accounts: Vec<&String> = credits.keys().collect();
        let alpenglow_epoch = credits.values().all(|c| c.tower_credits.is_none());
        let moved =
            move_credits_to_reward(&transaction, &epoch, &vote_accounts, alpenglow_epoch).await?;
        info!(
            "Epoch {epoch}: updated {updated} of {rows} validator rows from the backfill, moved credits to vote_reward_lamports in {moved} rows not in the backfill"
        );
    }
    let epochs: Vec<Decimal> = backfill.epochs.keys().map(|e| Decimal::from(*e)).collect();
    let above_tower_limit: i64 = transaction
        .query_one(
            "SELECT COUNT(*) FROM validators WHERE epoch = ANY($1) AND credits > $2",
            &[&epochs, &Decimal::from(MAX_TOWER_CREDITS_IN_EPOCH)],
        )
        .await?
        .get(0);
    if above_tower_limit > 0 {
        warn!("{above_tower_limit} rows of the backfilled epochs still hold credits above {MAX_TOWER_CREDITS_IN_EPOCH}");
    }
    transaction.commit().await?;
    Ok(())
}
