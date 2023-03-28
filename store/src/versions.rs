use crate::utils::*;
use chrono::{DateTime, Utc};
use collect::validators_performance::ValidatorsPerformanceSnapshot;
use log::info;
use rust_decimal::prelude::*;
use serde_yaml;
use std::collections::HashSet;
use structopt::StructOpt;
use tokio_postgres::{types::ToSql, Client};

#[derive(Debug, StructOpt)]
pub struct StoreVersionsOptions {
    #[structopt(long = "snapshot-file")]
    snapshot_path: String,
}

pub async fn store_versions(
    options: StoreVersionsOptions,
    mut psql_client: &mut Client,
) -> anyhow::Result<()> {
    info!("Storing versions...");

    let snapshot_file = std::fs::File::open(options.snapshot_path)?;
    let snapshot: ValidatorsPerformanceSnapshot = serde_yaml::from_reader(snapshot_file)?;
    let snapshot_epoch_slot: Decimal = snapshot.epoch_slot.into();
    let snapshot_epoch: Decimal = snapshot.epoch.into();
    let snapshot_created_at = snapshot.created_at.parse::<DateTime<Utc>>().unwrap();

    info!("Loaded the snapshot");

    let mut skipped_vote_accounts: HashSet<String> = Default::default();

    for row in psql_client
        .query(
            "
        SELECT DISTINCT ON (vote_account)
            vote_account,
            version,
            epoch
        FROM versions
        ORDER BY vote_account, created_at DESC
    ",
            &[],
        )
        .await?
    {
        let vote_account: &str = row.get("vote_account");
        let version: Option<String> = row.get("version");
        let epoch: Decimal = row.get("epoch");

        if let Some(validator_snapshot) = snapshot.validators.get(vote_account) {
            if epoch == snapshot_epoch && version == validator_snapshot.version {
                skipped_vote_accounts.insert(vote_account.to_string());
            }
        }
    }

    let mut query = InsertQueryCombiner::new(
        "versions".to_string(),
        "vote_account, version, epoch_slot, epoch, created_at".to_string(),
    );

    for (vote_account, v) in snapshot.validators.iter() {
        if !skipped_vote_accounts.contains(vote_account) {
            let mut params: Vec<&(dyn ToSql + Sync)> = vec![
                vote_account,
                &v.version,
                &snapshot_epoch_slot,
                &snapshot_epoch,
                &snapshot_created_at,
            ];
            query.add(&mut params);
        }
    }
    let insertions = query.execute(&mut psql_client).await?;

    info!("Stored {} version changes", insertions.unwrap_or(0));

    Ok(())
}
