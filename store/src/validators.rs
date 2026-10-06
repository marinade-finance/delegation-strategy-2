use crate::directory::Directory;
use crate::docs::{
    epoch_doc_path, merge_into, merge_snapshot, EpochDoc, SnapshotDoc, EPOCHS_DIR, SNAPSHOT_DIR,
};
use crate::dto::Validator;
use chrono::{DateTime, Utc};
use clap::Parser;
use collect::validators::Snapshot;
use log::{info, warn};
use serde_yaml;

#[derive(Debug, Parser)]
pub struct StoreValidatorsParams {
    #[arg(long = "snapshot-file")]
    snapshot_path: String,
}

/// How far back a data center is still good evidence of where a node is now.
const DATA_CENTER_CARRY_EPOCHS: u64 = 10;

pub async fn store_validators(
    params: StoreValidatorsParams,
    directory: &Directory,
) -> anyhow::Result<()> {
    info!("Storing validators snapshot...");

    let snapshot_file = std::fs::File::open(params.snapshot_path)?;
    let snapshot: Snapshot = serde_yaml::from_reader(snapshot_file)?;
    let created_at: DateTime<Utc> = snapshot.created_at.parse()?;

    // close-epoch runs once per epoch, so a snapshot landing after it would
    // leave mid-epoch values.
    let sealed = directory
        .get::<EpochDoc>(&epoch_doc_path(EPOCHS_DIR, snapshot.epoch))
        .await?;
    if sealed.is_some() {
        warn!(
            "Epoch {} is already closed, skipping the snapshot taken at {}",
            snapshot.epoch, snapshot.created_at
        );
        return Ok(());
    }

    let mut validators: SnapshotDoc = snapshot
        .validators
        .iter()
        .map(|v| {
            let mut validator = Validator::new_from_snapshot(v, snapshot.epoch);
            validator.updated_at = Some(created_at);
            (v.vote_account.clone(), validator)
        })
        .collect();

    info!("Loaded the snapshot: {} validators", validators.len());

    carry_previous_data_centers(directory, snapshot.epoch, &mut validators).await?;

    let path = epoch_doc_path(SNAPSHOT_DIR, snapshot.epoch);
    merge_into(directory, &path, validators, merge_snapshot).await?;

    info!("Stored the validators snapshot at {path}");

    Ok(())
}

/// Fills the data center of every validator whose whois lookup failed from
/// the newest previous epoch that knew one for the same address, so a
/// boundary-wide lookup failure does not open a gap the whole epoch long.
/// Matching the address is what stops a node that moved from inheriting the
/// old address's data center. The merge keeps whatever the epoch already
/// holds for an unchanged address, so this only reaches the entries that
/// would otherwise hold nothing.
async fn carry_previous_data_centers(
    directory: &Directory,
    epoch: u64,
    validators: &mut SnapshotDoc,
) -> anyhow::Result<()> {
    let mut unresolved: Vec<String> = validators
        .iter()
        .filter(|(_, validator)| !validator.dc_resolved && !validator.has_data_center())
        .map(|(vote_account, _)| vote_account.clone())
        .collect();
    if unresolved.is_empty() {
        return Ok(());
    }

    let oldest = epoch.saturating_sub(DATA_CENTER_CARRY_EPOCHS);
    let mut carried = 0;
    for previous in (oldest..epoch).rev() {
        if unresolved.is_empty() {
            break;
        }
        let Some(stored) = directory
            .get::<SnapshotDoc>(&epoch_doc_path(SNAPSHOT_DIR, previous))
            .await?
        else {
            continue;
        };
        unresolved.retain(|vote_account| {
            let Some(known) = stored.body.get(vote_account) else {
                return true;
            };
            let validator = validators
                .get_mut(vote_account)
                .expect("the unresolved list was built from these validators");
            if known.node_ip != validator.node_ip || !known.has_data_center() {
                return true;
            }
            validator.copy_data_center_from(known);
            carried += 1;
            false
        });
    }

    info!("Carried a previously known data center for {carried} validators");

    Ok(())
}
