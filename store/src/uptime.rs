use crate::directory::{Directory, Doc, Precondition};
use crate::docs::{UptimeInterval, UptimeState, UptimeStatus, UptimesDoc, LIVE_UPTIMES};
use chrono::{DateTime, Duration, Utc};
use clap::Parser;
use collect::validators_performance::{ValidatorPerformance, ValidatorsPerformanceSnapshot};
use log::{info, warn};
use serde_yaml;

#[cfg(test)]
#[path = "uptime_test.rs"]
mod uptime_test;

#[derive(Debug, Parser)]
pub struct StoreUptimeParams {
    #[arg(long = "snapshot-file")]
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

pub fn apply_uptime_samples(
    uptimes: &mut UptimesDoc,
    snapshot: &ValidatorsPerformanceSnapshot,
    created_at: DateTime<Utc>,
) {
    let default_end_at = created_at + Duration::minutes(1);

    for (vote_account, validator) in snapshot.validators.iter() {
        let opened = |status| UptimeInterval {
            status,
            epoch: snapshot.epoch,
            start_at: created_at,
            end_at: default_end_at,
        };

        let Some(state) = uptimes.get_mut(vote_account) else {
            let status = UptimeStatus::from_delinquency(is_down(validator, None));
            uptimes.insert(
                vote_account.clone(),
                UptimeState {
                    open: opened(status),
                    closed: Vec::new(),
                    last_credits: validator.credits_total,
                },
            );
            warn_on_status(vote_account, status);
            continue;
        };

        let status = UptimeStatus::from_delinquency(is_down(validator, state.last_credits));
        state.last_credits = validator.credits_total.or(state.last_credits);
        let within_window = state.open.end_at + status_max_delay_to_extend() > created_at;
        if within_window && state.open.status == status && state.open.epoch == snapshot.epoch {
            state.open.end_at = default_end_at;
            continue;
        }
        if within_window {
            state.open.end_at = created_at;
        }
        let closed = std::mem::replace(&mut state.open, opened(status));
        state.closed.push(closed);
        warn_on_status(vote_account, status);
    }
}

/// RPC delinquency reads the last vote, and `lastVote` is 0 once the vote
/// state records no votes, which SIMD-0357 makes the case under Alpenglow.
/// Such a validator is down when its cumulative credits stopped growing since
/// the previous sample; with no previous sample it counts as up until the next
/// one compares.
fn is_down(performance: &ValidatorPerformance, last_credits: Option<u64>) -> bool {
    if performance.last_vote.is_some_and(|last_vote| last_vote > 0) {
        return performance.delinquent;
    }
    match (performance.credits_total, last_credits) {
        (Some(credits_total), Some(last_credits)) => credits_total <= last_credits,
        (Some(_), None) => false,
        (None, _) => performance.delinquent,
    }
}

fn warn_on_status(vote_account: &str, status: UptimeStatus) {
    match status {
        UptimeStatus::Down => warn!("Validator {vote_account} is now DOWN"),
        UptimeStatus::Up => info!("Validator {vote_account} is now UP"),
    }
}
