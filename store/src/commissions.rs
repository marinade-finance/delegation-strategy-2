use crate::directory::{Directory, Doc, Precondition};
use crate::docs::{CommissionSample, CommissionState, CommissionsDoc, LIVE_COMMISSIONS};
use chrono::{DateTime, Utc};
use collect::validators_performance::ValidatorsPerformanceSnapshot;
use log::info;
use serde_yaml;
use structopt::StructOpt;

#[derive(Debug, StructOpt)]
pub struct StoreCommissionsParams {
    #[structopt(long = "snapshot-file")]
    snapshot_path: String,
}

pub async fn store_commissions(
    params: StoreCommissionsParams,
    directory: &Directory,
) -> anyhow::Result<()> {
    info!("Storing commission...");

    let snapshot_file = std::fs::File::open(params.snapshot_path)?;
    let snapshot: ValidatorsPerformanceSnapshot = serde_yaml::from_reader(snapshot_file)?;

    info!("Loaded the snapshot");

    let stored = directory.get::<CommissionsDoc>(LIVE_COMMISSIONS).await?;
    write_commissions(directory, stored, &snapshot).await
}

pub async fn write_commissions(
    directory: &Directory,
    stored: Option<Doc<CommissionsDoc>>,
    snapshot: &ValidatorsPerformanceSnapshot,
) -> anyhow::Result<()> {
    let created_at: DateTime<Utc> = snapshot.created_at.parse()?;
    let (mut commissions, precondition) = match stored {
        Some(stored) => (stored.body, Precondition::IfMatch(stored.etag)),
        None => (CommissionsDoc::new(), Precondition::Create),
    };

    let changes = apply_commission_samples(&mut commissions, snapshot, created_at);

    // No retry on a conflict: the cron is the retry.
    directory
        .put(LIVE_COMMISSIONS, &commissions, precondition)
        .await?;

    info!("Stored {changes} commission changes");

    Ok(())
}

/// Records a change when the commission differs from the last one seen, and
/// once per epoch so every epoch carries the commission it started at.
pub fn apply_commission_samples(
    commissions: &mut CommissionsDoc,
    snapshot: &ValidatorsPerformanceSnapshot,
    created_at: DateTime<Utc>,
) -> usize {
    let mut changes = 0;

    for (vote_account, validator) in snapshot.validators.iter() {
        let sample = CommissionSample {
            epoch: snapshot.epoch,
            epoch_slot: snapshot.epoch_slot,
            commission: validator.commission as i32,
            created_at,
        };

        match commissions.get_mut(vote_account) {
            Some(state) => {
                if state.last.epoch == sample.epoch && state.last.commission == sample.commission {
                    continue;
                }
                state.changes.push(sample.clone());
                state.last = sample;
            }
            None => {
                commissions.insert(
                    vote_account.clone(),
                    CommissionState {
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
