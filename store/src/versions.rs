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
pub struct StoreVersionsParams {
    #[structopt(long = "snapshot-file")]
    snapshot_path: String,
}

pub async fn store_versions(
    params: StoreVersionsParams,
    psql_client: &mut Client,
) -> anyhow::Result<()> {
    info!("Storing versions...");

    let snapshot_file = std::fs::File::open(params.snapshot_path)?;
    let snapshot: ValidatorsPerformanceSnapshot = serde_yaml::from_reader(snapshot_file)?;
    let snapshot_epoch_slot: Decimal = snapshot.epoch_slot.into();
    let snapshot_epoch: Decimal = snapshot.epoch.into();
    let snapshot_created_at: DateTime<Utc> = snapshot.created_at.parse().unwrap();

    info!("Loaded the snapshot");

    let mut skipped_vote_accounts: HashSet<String> = Default::default();

    for row in psql_client
        .query(
            "
        SELECT DISTINCT ON (vote_account)
            vote_account,
            version,
            client_id,
            feature_set,
            shred_version,
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
        let client_id: Option<String> = row.get("client_id");
        let feature_set: Option<i64> = row.get("feature_set");
        let shred_version: Option<i32> = row.get("shred_version");
        let epoch: Decimal = row.get("epoch");

        if let Some(validator_snapshot) = snapshot.validators.get(vote_account) {
            let snapshot_feature_set = validator_snapshot.feature_set.map(|f| f as i64);
            let snapshot_shred_version = validator_snapshot.shred_version.map(|s| s as i32);
            if epoch == snapshot_epoch
                && version == validator_snapshot.version
                && client_id == validator_snapshot.client_id
                && feature_set == snapshot_feature_set
                && shred_version == snapshot_shred_version
            {
                skipped_vote_accounts.insert(vote_account.to_string());
            }
        }
    }

    let mut query = InsertQueryCombiner::new(
        "versions".to_string(),
        "vote_account, version, client_id, client_type, feature_set, shred_version, epoch_slot, epoch, created_at".to_string(),
    );

    let rows_to_insert: Vec<(&String, &_, Option<i64>, Option<i32>)> = snapshot
        .validators
        .iter()
        .filter(|(va, _)| !skipped_vote_accounts.contains(*va))
        .map(|(va, v)| {
            (
                va,
                v,
                v.feature_set.map(|f| f as i64),
                v.shred_version.map(|s| s as i32),
            )
        })
        .collect();

    for (vote_account, v, feature_set, shred_version) in rows_to_insert.iter() {
        let mut params: Vec<&(dyn ToSql + Sync)> = vec![
            *vote_account,
            &v.version,
            &v.client_id,
            &v.client_type,
            feature_set,
            shred_version,
            &snapshot_epoch_slot,
            &snapshot_epoch,
            &snapshot_created_at,
        ];
        query.add(&mut params);
    }
    let insertions = query.execute(psql_client).await?;

    info!("Stored {} version changes", insertions.unwrap_or(0));

    Ok(())
}
