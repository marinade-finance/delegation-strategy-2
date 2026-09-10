use crate::directory::{Directory, Doc, Precondition};
use crate::docs::{UptimeInterval, UptimeState, UptimeStatus, UptimesDoc, LIVE_UPTIMES};
use chrono::{DateTime, Duration, Utc};
use collect::validators_performance::ValidatorsPerformanceSnapshot;
use log::{info, warn};
use serde_yaml;
use structopt::StructOpt;

#[derive(Debug, StructOpt)]
pub struct StoreUptimeParams {
    #[structopt(long = "snapshot-file")]
    snapshot_path: String,
}

/// A sample this far past the open interval's end still extends it; a longer
/// gap closes the interval where it stood and opens a new one.
fn status_max_delay_to_extend() -> Duration {
    Duration::minutes(5)
}

pub async fn store_uptime(params: StoreUptimeParams, directory: &Directory) -> anyhow::Result<()> {
    info!("Storing uptime...");

    let snapshot_file = std::fs::File::open(params.snapshot_path)?;
    let snapshot: ValidatorsPerformanceSnapshot = serde_yaml::from_reader(snapshot_file)?;

    info!("Loaded the snapshot");

    let stored = directory.get::<UptimesDoc>(LIVE_UPTIMES).await?;
    write_uptimes(directory, stored, &snapshot).await
}

/// Applies one sample to the accumulator and writes it back under the version
/// it was read at.
pub async fn write_uptimes(
    directory: &Directory,
    stored: Option<Doc<UptimesDoc>>,
    snapshot: &ValidatorsPerformanceSnapshot,
) -> anyhow::Result<()> {
    let created_at: DateTime<Utc> = snapshot.created_at.parse()?;
    let (mut uptimes, precondition) = match stored {
        Some(stored) => (stored.body, Precondition::IfMatch(stored.etag)),
        None => (UptimesDoc::new(), Precondition::Create),
    };
    refuse_older_epoch(&uptimes, snapshot.epoch)?;

    apply_uptime_samples(&mut uptimes, snapshot, created_at);

    // No retry on a conflict: two overlapping runs must not both write, and
    // the next minute's sample carries what this one loses.
    directory.put(LIVE_UPTIMES, &uptimes, precondition).await?;

    info!("Stored uptimes of {} validators", uptimes.len());

    Ok(())
}

/// The accumulator moves forward only: a sample from an epoch already passed
/// would reopen it.
fn refuse_older_epoch(uptimes: &UptimesDoc, epoch: u64) -> anyhow::Result<()> {
    let stored_epoch = uptimes
        .values()
        .map(|state| state.open.epoch)
        .max()
        .unwrap_or(epoch);
    if epoch < stored_epoch {
        anyhow::bail!("Sample of epoch {epoch} is older than the stored epoch {stored_epoch}");
    }
    Ok(())
}

/// Extends the open interval while the status and the epoch hold and the
/// sample is inside the extension window; otherwise closes it and opens a new
/// one at the sample.
pub fn apply_uptime_samples(
    uptimes: &mut UptimesDoc,
    snapshot: &ValidatorsPerformanceSnapshot,
    created_at: DateTime<Utc>,
) {
    let default_end_at = created_at + Duration::minutes(1);

    for (vote_account, validator) in snapshot.validators.iter() {
        let status = UptimeStatus::from_delinquency(validator.delinquent);
        let opened = UptimeInterval {
            status,
            epoch: snapshot.epoch,
            start_at: created_at,
            end_at: default_end_at,
        };

        let Some(state) = uptimes.get_mut(vote_account) else {
            uptimes.insert(
                vote_account.clone(),
                UptimeState {
                    open: opened,
                    closed: Vec::new(),
                },
            );
            warn_on_status(vote_account, status);
            continue;
        };

        let within_window = state.open.end_at + status_max_delay_to_extend() > created_at;
        if within_window && state.open.status == status && state.open.epoch == snapshot.epoch {
            state.open.end_at = default_end_at;
            continue;
        }
        if within_window {
            // A status or epoch change ends the interval where the sample found it.
            state.open.end_at = created_at;
        }
        let closed = std::mem::replace(&mut state.open, opened);
        state.closed.push(closed);
        warn_on_status(vote_account, status);
    }
}

fn warn_on_status(vote_account: &str, status: UptimeStatus) {
    match status {
        UptimeStatus::Down => warn!("Validator {vote_account} is now DOWN"),
        UptimeStatus::Up => info!("Validator {vote_account} is now UP"),
    }
}
