use crate::directory::Directory;
use crate::docs::{epoch_doc_path, merge_into, merge_snapshot, SnapshotDoc, SNAPSHOT_DIR};
use crate::dto::Validator;
use chrono::{DateTime, Utc};
use collect::validators::Snapshot;
use log::info;
use serde_yaml;
use structopt::StructOpt;

#[derive(Debug, StructOpt)]
pub struct StoreValidatorsParams {
    #[structopt(long = "snapshot-file")]
    snapshot_path: String,
}

pub async fn store_validators(
    params: StoreValidatorsParams,
    directory: &Directory,
) -> anyhow::Result<()> {
    info!("Storing validators snapshot...");

    let snapshot_file = std::fs::File::open(params.snapshot_path)?;
    let snapshot: Snapshot = serde_yaml::from_reader(snapshot_file)?;
    let created_at: DateTime<Utc> = snapshot.created_at.parse()?;

    let validators: SnapshotDoc = snapshot
        .validators
        .iter()
        .map(|v| {
            let mut validator = Validator::new_from_snapshot(v, snapshot.epoch);
            validator.updated_at = Some(created_at);
            (v.vote_account.clone(), validator)
        })
        .collect();

    info!("Loaded the snapshot: {} validators", validators.len());

    let path = epoch_doc_path(SNAPSHOT_DIR, snapshot.epoch);
    merge_into(directory, &path, validators, merge_snapshot).await?;

    info!("Stored the validators snapshot at {path}");

    Ok(())
}
