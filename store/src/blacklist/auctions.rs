use super::db::max_epoch;
use clap::Parser;
use collect::blacklist_auctions::{AuctionListings, BlacklistAuctionsSnapshot};
use log::info;
use rust_decimal::prelude::*;
use tokio_postgres::Client;

#[derive(Debug, Parser)]
pub struct StoreAuctionsParams {
    #[arg(long = "snapshot-file", help = "YAML of `collect blacklist-auctions`.")]
    snapshot_file: String,
}

pub async fn store_auctions(
    params: StoreAuctionsParams,
    psql_client: &mut Client,
) -> anyhow::Result<()> {
    let snapshot: BlacklistAuctionsSnapshot =
        serde_yaml::from_reader(std::fs::File::open(&params.snapshot_file)?)?;
    for run in &snapshot.runs {
        import_run(psql_client, run).await?;
    }
    Ok(())
}

/// Replaces an auction epoch's listings with those of `run` unless that run is imported already.
/// One transaction per run: a failure leaves the epoch as it was.
pub async fn import_run(psql_client: &mut Client, run: &AuctionListings) -> anyhow::Result<()> {
    let tx = psql_client.transaction().await?;
    let auction_epoch = Decimal::from(run.auction_epoch);
    let imported: Option<String> = tx
        .query_opt(
            "SELECT run_id FROM blacklist_auction_imports WHERE auction_epoch = $1",
            &[&auction_epoch],
        )
        .await?
        .map(|row| row.get("run_id"));
    if imported.as_deref() == Some(run.run_id.as_str()) {
        return Ok(());
    }
    let current_epoch = max_epoch(&tx, "cluster_info")
        .await?
        .map_or(auction_epoch, Decimal::from);

    let replaced = tx
        .execute(
            "UPDATE blacklist
             SET removed_epoch = $2, removed_reason = 'replaced by run ' || $3::TEXT, updated_at = now()
             WHERE source = 'auction' AND auction_epoch = $1 AND removed_epoch IS NULL",
            &[&auction_epoch, &current_epoch, &run.run_id],
        )
        .await?;
    let accounts: Vec<&str> = run.rows.iter().map(|r| r.vote_account.as_str()).collect();
    let codes: Vec<&str> = run.rows.iter().map(|r| r.code.as_str()).collect();
    let added = tx
        .execute(
            "INSERT INTO blacklist (vote_account, source, code, auction_epoch, run_id, added_epoch,
                created_at, updated_at)
             SELECT vote_account, 'auction', code, $3, $4, $3, now(), now()
             FROM UNNEST($1::TEXT[], $2::TEXT[]) AS t(vote_account, code)
             ON CONFLICT (vote_account, code) WHERE source = 'auction' AND removed_epoch IS NULL
             DO NOTHING",
            &[&accounts, &codes, &auction_epoch, &run.run_id],
        )
        .await?;
    tx.execute(
        "INSERT INTO blacklist_auction_imports (auction_epoch, run_id, rows, imported_at)
         VALUES ($1, $2, $3, now())
         ON CONFLICT (auction_epoch) DO UPDATE
         SET run_id = EXCLUDED.run_id, rows = EXCLUDED.rows, imported_at = EXCLUDED.imported_at",
        &[&auction_epoch, &run.run_id, &i32::try_from(added)?],
    )
    .await?;
    tx.commit().await?;
    info!(
        "Auction {} run {}: {added} listings imported, {replaced} of an earlier run closed",
        run.auction_epoch, run.run_id
    );
    Ok(())
}
