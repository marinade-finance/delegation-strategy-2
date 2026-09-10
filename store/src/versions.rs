use crate::directory::{Directory, Doc, Precondition};
use crate::docs::{VersionSample, VersionState, VersionsDoc, LIVE_VERSIONS};
use chrono::{DateTime, Utc};
use collect::validators_performance::ValidatorsPerformanceSnapshot;
use log::info;
use serde_yaml;
use structopt::StructOpt;

#[derive(Debug, StructOpt)]
pub struct StoreVersionsParams {
    #[structopt(long = "snapshot-file")]
    snapshot_path: String,
}

pub async fn store_versions(
    params: StoreVersionsParams,
    directory: &Directory,
) -> anyhow::Result<()> {
    info!("Storing versions...");

    let snapshot_file = std::fs::File::open(params.snapshot_path)?;
    let snapshot: ValidatorsPerformanceSnapshot = serde_yaml::from_reader(snapshot_file)?;

    info!("Loaded the snapshot");

    let stored = directory.get::<VersionsDoc>(LIVE_VERSIONS).await?;
    write_versions(directory, stored, &snapshot).await
}

pub async fn write_versions(
    directory: &Directory,
    stored: Option<Doc<VersionsDoc>>,
    snapshot: &ValidatorsPerformanceSnapshot,
) -> anyhow::Result<()> {
    let created_at: DateTime<Utc> = snapshot.created_at.parse()?;
    let (mut versions, precondition) = match stored {
        Some(stored) => (stored.body, Precondition::IfMatch(stored.etag)),
        None => (VersionsDoc::new(), Precondition::Create),
    };

    let changes = apply_version_samples(&mut versions, snapshot, created_at);

    // No retry on a conflict: the cron is the retry.
    directory
        .put(LIVE_VERSIONS, &versions, precondition)
        .await?;

    info!("Stored {changes} version changes");

    Ok(())
}

pub fn apply_version_samples(
    versions: &mut VersionsDoc,
    snapshot: &ValidatorsPerformanceSnapshot,
    created_at: DateTime<Utc>,
) -> usize {
    let mut changes = 0;

    for (vote_account, validator) in snapshot.validators.iter() {
        let sample = VersionSample {
            epoch: snapshot.epoch,
            epoch_slot: snapshot.epoch_slot,
            version: validator.version.clone(),
            client_id: validator.client_id.map(|id| id as i32),
            client_id_raw: validator.client_id_raw.clone(),
            feature_set: validator.feature_set.map(|set| set as i64),
            shred_version: validator.shred_version.map(|version| version as i32),
            created_at,
        };

        match versions.get_mut(vote_account) {
            Some(state) => {
                if state.last.epoch == sample.epoch && !is_version_change(&state.last, &sample) {
                    continue;
                }
                state.changes.push(sample.clone());
                state.last = sample;
            }
            None => {
                versions.insert(
                    vote_account.clone(),
                    VersionState {
                        last: sample.clone(),
                        changes: vec![sample],
                    },
                );
            }
        }
        changes += 1;
    }

    changes
}

// client_id_raw tracks the answering RPC's rendering, not the node, so it stays out of the key.
fn is_version_change(last: &VersionSample, sample: &VersionSample) -> bool {
    last.version != sample.version
        || last.client_id != sample.client_id
        || last.feature_set != sample.feature_set
        || last.shred_version != sample.shred_version
}
