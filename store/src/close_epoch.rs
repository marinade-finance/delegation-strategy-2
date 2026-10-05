use crate::directory::{Directory, Precondition};
use crate::docs::{
    epoch_doc_path, put_whole, ClusterInfoDoc, CommissionsDoc, EpochDoc, SealedClusterInfoDoc,
    SealedCommissionsDoc, SealedUptimesDoc, SealedVersionsDoc, SnapshotDoc, UptimeInterval,
    UptimeStatus, UptimesDoc, VersionsDoc, CLUSTER_INFO_DIR, COMMISSIONS_DIR, EPOCHS_DIR,
    LIVE_CLUSTER_INFO, LIVE_COMMISSIONS, LIVE_UPTIMES, LIVE_VERSIONS, SNAPSHOT_DIR, UPTIMES_DIR,
    VERSIONS_DIR,
};
use crate::dto::Validator;
use chrono::{DateTime, Utc};
use collect::validators_performance::{ClusterInflation, ValidatorsPerformanceSnapshot};
use log::info;
use rust_decimal::prelude::*;
use serde_yaml;
use structopt::StructOpt;

#[derive(Debug, StructOpt)]
pub struct CloseEpochParams {
    #[structopt(long = "snapshot-file")]
    snapshot_path: String,
}

pub async fn close_epoch(
    epoch_params: CloseEpochParams,
    directory: &Directory,
) -> anyhow::Result<()> {
    info!("Finalizing validators snapshot...");

    let snapshot_file = std::fs::File::open(epoch_params.snapshot_path)?;
    let snapshot: ValidatorsPerformanceSnapshot = serde_yaml::from_reader(snapshot_file)?;
    let created_at: DateTime<Utc> = snapshot.created_at.parse()?;
    let epoch = snapshot.epoch;
    let inflation = snapshot
        .cluster_inflation
        .clone()
        .ok_or_else(|| anyhow::anyhow!("The snapshot of epoch {epoch} carries no inflation"))?;

    info!("Loaded the snapshot");

    // A run that died between the seal and the trim leaves the accumulators
    // partly trimmed; resealing from them would erase the epoch's downtime.
    if directory
        .get::<EpochDoc>(&epoch_doc_path(EPOCHS_DIR, epoch))
        .await?
        .is_some()
    {
        info!("Epoch {epoch} is already sealed; trimming the accumulators");
        return trim_accumulators(directory, epoch).await;
    }

    let uptimes = read_live::<UptimesDoc>(directory, LIVE_UPTIMES).await?;
    let commissions = read_live::<CommissionsDoc>(directory, LIVE_COMMISSIONS).await?;
    let versions = read_live::<VersionsDoc>(directory, LIVE_VERSIONS).await?;
    let cluster_info = read_live::<ClusterInfoDoc>(directory, LIVE_CLUSTER_INFO).await?;

    let epoch_record = build_epoch_record(
        directory,
        epoch,
        &cluster_info.body,
        inflation,
        snapshot.slots_per_year,
    )
    .await?;

    let snapshot_path = epoch_doc_path(SNAPSHOT_DIR, epoch);
    let stored = directory
        .get::<SnapshotDoc>(&snapshot_path)
        .await?
        .ok_or_else(|| anyhow::anyhow!("{snapshot_path} holds no validators to finalize"))?;
    let mut validators = stored.body;
    apply_finalized_performance(&mut validators, &snapshot, created_at);
    apply_uptimes(&mut validators, &uptimes.body, epoch, &epoch_record);
    apply_observed_commissions(&mut validators, &commissions.body, epoch);
    directory
        .put(
            &snapshot_path,
            &validators,
            Precondition::IfMatch(stored.etag),
        )
        .await?;
    info!("Finalized {} validator records", validators.len());

    seal(
        directory,
        &epoch_doc_path(UPTIMES_DIR, epoch),
        seal_uptimes(&uptimes.body, epoch),
    )
    .await?;
    seal(
        directory,
        &epoch_doc_path(COMMISSIONS_DIR, epoch),
        seal_commissions(&commissions.body, epoch),
    )
    .await?;
    seal(
        directory,
        &epoch_doc_path(VERSIONS_DIR, epoch),
        seal_versions(&versions.body, epoch),
    )
    .await?;
    seal(
        directory,
        &epoch_doc_path(CLUSTER_INFO_DIR, epoch),
        seal_cluster_info(&cluster_info.body, epoch),
    )
    .await?;
    info!("Sealed the streams of epoch {epoch}");

    // Its presence marks the epoch sealed, so it is written before the trim: a
    // failure before this point leaves `live/` untouched and the re-run identical.
    let path = epoch_doc_path(EPOCHS_DIR, epoch);
    put_whole(directory, &path, &epoch_record).await?;
    info!("Closed epoch {epoch}");

    trim_accumulators(directory, epoch).await
}

/// Drops everything up to and including `epoch` from the four accumulators.
///
/// Each document is read immediately before its own write: collector-performance
/// rewrites all four every minute, and a 412 here lands after the epoch document
/// exists, where nothing offers the epoch again.
async fn trim_accumulators(directory: &Directory, epoch: u64) -> anyhow::Result<()> {
    let uptimes = read_live::<UptimesDoc>(directory, LIVE_UPTIMES).await?;
    directory
        .put(
            LIVE_UPTIMES,
            &trim_uptimes(uptimes.body, epoch),
            Precondition::IfMatch(uptimes.etag),
        )
        .await?;
    let commissions = read_live::<CommissionsDoc>(directory, LIVE_COMMISSIONS).await?;
    directory
        .put(
            LIVE_COMMISSIONS,
            &trim_commissions(commissions.body, epoch),
            Precondition::IfMatch(commissions.etag),
        )
        .await?;
    let versions = read_live::<VersionsDoc>(directory, LIVE_VERSIONS).await?;
    directory
        .put(
            LIVE_VERSIONS,
            &trim_versions(versions.body, epoch),
            Precondition::IfMatch(versions.etag),
        )
        .await?;
    let cluster_info = read_live::<ClusterInfoDoc>(directory, LIVE_CLUSTER_INFO).await?;
    directory
        .put(
            LIVE_CLUSTER_INFO,
            &trim_cluster_info(cluster_info.body, epoch),
            Precondition::IfMatch(cluster_info.etag),
        )
        .await?;
    info!("Trimmed the accumulators to epoch {}", epoch + 1);

    Ok(())
}

async fn read_live<T: serde::de::DeserializeOwned>(
    directory: &Directory,
    path: &str,
) -> anyhow::Result<crate::directory::Doc<T>> {
    directory
        .get::<T>(path)
        .await?
        .ok_or_else(|| anyhow::anyhow!("{path} is missing: nothing to seal"))
}

async fn seal<T: serde::Serialize>(
    directory: &Directory,
    path: &str,
    sealed: T,
) -> anyhow::Result<()> {
    put_whole(directory, path, &sealed).await
}

async fn build_epoch_record(
    directory: &Directory,
    epoch: u64,
    cluster_info: &ClusterInfoDoc,
    inflation: ClusterInflation,
    slots_per_year: f64,
) -> anyhow::Result<EpochDoc> {
    let samples: Vec<_> = cluster_info
        .samples
        .iter()
        .filter(|sample| sample.epoch == epoch)
        .collect();
    let first_at = samples
        .iter()
        .map(|sample| sample.created_at)
        .min()
        .ok_or_else(|| anyhow::anyhow!("No cluster info sampled in epoch {epoch}"))?;
    let end_at = samples
        .iter()
        .map(|sample| sample.created_at)
        .max()
        .ok_or_else(|| anyhow::anyhow!("No cluster info sampled in epoch {epoch}"))?;
    let transactions: Vec<u64> = samples
        .iter()
        .map(|sample| sample.transaction_count)
        .collect();
    let transaction_count =
        transactions.iter().max().unwrap_or(&0) - transactions.iter().min().unwrap_or(&0);

    // The epoch starts where the previous one ended; the first epoch ever
    // collected starts at its first sample.
    let previous = match epoch.checked_sub(1) {
        Some(previous) => {
            directory
                .get::<EpochDoc>(&epoch_doc_path(EPOCHS_DIR, previous))
                .await?
        }
        None => None,
    };

    Ok(EpochDoc {
        epoch,
        start_at: previous.map(|doc| doc.body.end_at).unwrap_or(first_at),
        end_at,
        transaction_count,
        supply: Decimal::from(inflation.sol_total_supply),
        inflation: inflation.inflation,
        inflation_taper: inflation.inflation_taper,
        slots_per_year,
    })
}

fn apply_finalized_performance(
    validators: &mut SnapshotDoc,
    snapshot: &ValidatorsPerformanceSnapshot,
    created_at: DateTime<Utc>,
) {
    for (vote_account, performance) in snapshot.validators.iter() {
        let Some(validator) = validators.get_mut(vote_account) else {
            continue;
        };
        validator.commission_effective = snapshot
            .rewards
            .as_ref()
            .and_then(|rewards| rewards.get(vote_account))
            .and_then(|reward| reward.commission_effective.map(|c| c as i32));
        validator.credits = performance.credits.into();
        validator.leader_slots = performance.leader_slots.into();
        validator.blocks_produced = performance.blocks_produced.into();
        validator.skip_rate = performance.skip_rate;
        validator.updated_at = Some(created_at);
    }
}

fn apply_uptimes(
    validators: &mut SnapshotDoc,
    uptimes: &UptimesDoc,
    epoch: u64,
    epoch_record: &EpochDoc,
) {
    let duration = (epoch_record.end_at - epoch_record.start_at).num_seconds() as f64;

    for (vote_account, validator) in validators.iter_mut() {
        let downtime = uptimes
            .get(vote_account)
            .and_then(|state| downtime_seconds(state.closed.iter().chain([&state.open]), epoch));

        validator.uptime_pct = Some(match downtime {
            Some(downtime) => (1f64 - downtime / duration).clamp(0f64, 1f64),
            None => 1f64,
        });
        validator.uptime = Decimal::from_f64((duration - downtime.unwrap_or(0f64)).max(0f64));
        validator.downtime = Decimal::from_f64(downtime.unwrap_or(0f64));
    }
}

/// `None` where the validator was never down in the epoch.
fn downtime_seconds<'a>(
    intervals: impl Iterator<Item = &'a UptimeInterval>,
    epoch: u64,
) -> Option<f64> {
    let seconds: f64 = intervals
        .filter(|interval| interval.epoch == epoch && interval.status == UptimeStatus::Down)
        .map(|interval| (interval.end_at - interval.start_at).num_seconds() as f64)
        .sum::<f64>();
    (seconds > 0f64).then_some(seconds)
}

fn apply_observed_commissions(
    validators: &mut SnapshotDoc,
    commissions: &CommissionsDoc,
    epoch: u64,
) {
    for (vote_account, validator) in validators.iter_mut() {
        let observed: Vec<i32> = commissions
            .get(vote_account)
            .map(|state| {
                state
                    .changes
                    .iter()
                    .filter(|change| change.epoch == epoch)
                    .map(|change| change.commission)
                    .collect()
            })
            .unwrap_or_default();

        validator.commission_max_observed =
            extreme(validator, observed.iter().max().copied(), i32::max);
        validator.commission_min_observed =
            extreme(validator, observed.iter().min().copied(), i32::min);
    }
}

fn extreme(validator: &Validator, observed: Option<i32>, pick: fn(i32, i32) -> i32) -> Option<i32> {
    [
        observed,
        validator.commission_advertised,
        validator.commission_effective,
    ]
    .into_iter()
    .flatten()
    .reduce(pick)
}

fn seal_uptimes(uptimes: &UptimesDoc, epoch: u64) -> SealedUptimesDoc {
    uptimes
        .iter()
        .filter_map(|(vote_account, state)| {
            let intervals: Vec<UptimeInterval> = state
                .closed
                .iter()
                .chain([&state.open])
                .filter(|interval| interval.epoch == epoch)
                .cloned()
                .collect();
            (!intervals.is_empty()).then(|| (vote_account.clone(), intervals))
        })
        .collect()
}

fn trim_uptimes(mut uptimes: UptimesDoc, epoch: u64) -> UptimesDoc {
    for state in uptimes.values_mut() {
        state.closed.retain(|interval| interval.epoch > epoch);
    }
    uptimes
}

fn seal_commissions(commissions: &CommissionsDoc, epoch: u64) -> SealedCommissionsDoc {
    commissions
        .iter()
        .filter_map(|(vote_account, state)| {
            let changes: Vec<_> = state
                .changes
                .iter()
                .filter(|change| change.epoch == epoch)
                .cloned()
                .collect();
            (!changes.is_empty()).then(|| (vote_account.clone(), changes))
        })
        .collect()
}

fn trim_commissions(mut commissions: CommissionsDoc, epoch: u64) -> CommissionsDoc {
    for state in commissions.values_mut() {
        state.changes.retain(|change| change.epoch > epoch);
    }
    commissions
}

fn seal_versions(versions: &VersionsDoc, epoch: u64) -> SealedVersionsDoc {
    versions
        .iter()
        .filter_map(|(vote_account, state)| {
            let changes: Vec<_> = state
                .changes
                .iter()
                .filter(|change| change.epoch == epoch)
                .cloned()
                .collect();
            (!changes.is_empty()).then(|| (vote_account.clone(), changes))
        })
        .collect()
}

fn trim_versions(mut versions: VersionsDoc, epoch: u64) -> VersionsDoc {
    for state in versions.values_mut() {
        state.changes.retain(|change| change.epoch > epoch);
    }
    versions
}

fn seal_cluster_info(cluster_info: &ClusterInfoDoc, epoch: u64) -> SealedClusterInfoDoc {
    cluster_info
        .samples
        .iter()
        .filter(|sample| sample.epoch == epoch)
        .cloned()
        .collect()
}

fn trim_cluster_info(mut cluster_info: ClusterInfoDoc, epoch: u64) -> ClusterInfoDoc {
    cluster_info.samples.retain(|sample| sample.epoch > epoch);
    cluster_info
}
