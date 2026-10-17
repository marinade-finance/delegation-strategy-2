use crate::directory::{Directory, Precondition};
use crate::docs::{
    epoch_doc_path, put_whole, ClusterInfoDoc, CommissionsDoc, EpochDoc, NodeObservationsDoc,
    SealedClusterInfoDoc, SealedCommissionsDoc, SealedNodeObservationsDoc, SealedUptimesDoc,
    SealedVersionsDoc, SnapshotDoc, UptimeInterval, UptimeStatus, UptimesDoc, VersionsDoc,
    CLUSTER_INFO_DIR, COMMISSIONS_DIR, EPOCHS_DIR, LIVE_CLUSTER_INFO, LIVE_COMMISSIONS,
    LIVE_NODE_OBSERVATIONS, LIVE_UPTIMES, LIVE_VERSIONS, NODE_OBSERVATIONS_DIR, SNAPSHOT_DIR,
    UPTIMES_DIR, VERSIONS_DIR,
};
use crate::dto::{
    Validator, COMMISSION_EFFECTIVE_SOURCE_REWARD_ROW, COMMISSION_EFFECTIVE_SOURCE_VOTE_STATE,
};
use chrono::{DateTime, Utc};
use clap::Parser;
use collect::solana_service::bps_to_percent;
use collect::validators_performance::{
    ClusterInflation, ValidatorPerformance, ValidatorsPerformanceSnapshot,
};
use log::{info, warn};
use rust_decimal::prelude::*;
use serde_yaml;
use std::collections::HashMap;

#[cfg(test)]
#[path = "close_epoch_test.rs"]
mod close_epoch_test;

#[derive(Debug, Parser)]
pub struct CloseEpochParams {
    #[arg(long = "snapshot-file")]
    snapshot_path: String,
}

/// The inflation commission a vote state carried when it was sampled: in
/// basis points where the state parsed, else the whole percent it advertised.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SampledCommission {
    Bps(u16),
    Percent(u8),
}

/// `(commission_effective, commission_effective_bps, commission_effective_source)`.
pub type ResolvedCommission = (Option<i32>, Option<i32>, Option<&'static str>);

/// A reward row still wins where one exists, so pre-1030 epochs reprocess to
/// the same values.
pub fn resolve_commission_effective(
    from_reward_row: Option<u8>,
    sampled: Option<SampledCommission>,
) -> ResolvedCommission {
    if let Some(commission) = from_reward_row {
        return (
            Some(i32::from(commission)),
            None,
            Some(COMMISSION_EFFECTIVE_SOURCE_REWARD_ROW),
        );
    }
    match sampled {
        Some(SampledCommission::Bps(bps)) => (
            Some(i32::from(bps_to_percent(bps))),
            Some(i32::from(bps)),
            Some(COMMISSION_EFFECTIVE_SOURCE_VOTE_STATE),
        ),
        Some(SampledCommission::Percent(percent)) => (
            Some(i32::from(percent)),
            None,
            Some(COMMISSION_EFFECTIVE_SOURCE_VOTE_STATE),
        ),
        None => (None, None, None),
    }
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
    // Written by the quick-changes chain since the other four, so an
    // environment without it yet still closes.
    let node_observations = directory
        .get::<NodeObservationsDoc>(LIVE_NODE_OBSERVATIONS)
        .await?;

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
    let sampled = load_sampled_commission(directory, epoch, &validators).await?;
    apply_finalized_performance(&mut validators, &snapshot, &sampled, created_at);
    apply_uptimes(&mut validators, &uptimes.body, epoch, &epoch_record);
    apply_observed_commissions(&mut validators, &commissions.body, epoch);
    warn_on_unresolved_commission(&validators, epoch);
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
    if let Some(node_observations) = &node_observations {
        seal(
            directory,
            &epoch_doc_path(NODE_OBSERVATIONS_DIR, epoch),
            seal_node_observations(&node_observations.body, epoch),
        )
        .await?;
    }
    info!("Sealed the streams of epoch {epoch}");

    // Its presence marks the epoch sealed, so it is written before the trim: a
    // failure before this point leaves `live/` untouched and the re-run identical.
    let path = epoch_doc_path(EPOCHS_DIR, epoch);
    put_whole(directory, &path, &epoch_record).await?;
    info!("Closed epoch {epoch}");

    trim_accumulators(directory, epoch).await
}

/// Drops everything up to and including `epoch` from the accumulators.
///
/// Each document is read immediately before its own write: collector-performance
/// rewrites all of them every minute, and a 412 here lands after the epoch document
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
    if let Some(node_observations) = directory
        .get::<NodeObservationsDoc>(LIVE_NODE_OBSERVATIONS)
        .await?
    {
        directory
            .put(
                LIVE_NODE_OBSERVATIONS,
                &trim_node_observations(node_observations.body, epoch),
                Precondition::IfMatch(node_observations.etag),
            )
            .await?;
    }
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

/// Agave's order for epoch E: epoch_stakes(E) frozen at the close of E-2, then
/// the close of E-1, then live. The E-2 and E-1 snapshots are read for it; E is
/// the one being finalized. An unparsed vote state keeps its row's vintage
/// through the advertised percent, as agave falls back only on absence.
async fn load_sampled_commission(
    directory: &Directory,
    epoch: u64,
    current: &SnapshotDoc,
) -> anyhow::Result<HashMap<String, SampledCommission>> {
    let mut sampled: HashMap<String, SampledCommission> = Default::default();
    for vintage in (epoch.saturating_sub(2)..epoch).chain([epoch]) {
        let fetched;
        let snapshot = if vintage == epoch {
            current
        } else {
            let Some(stored) = directory
                .get::<SnapshotDoc>(&epoch_doc_path(SNAPSHOT_DIR, vintage))
                .await?
            else {
                continue;
            };
            fetched = stored.body;
            &fetched
        };
        for (vote_account, validator) in snapshot.iter() {
            if sampled.contains_key(vote_account) {
                continue;
            }
            if let Some(commission) = sampled_commission(validator)? {
                sampled.insert(vote_account.clone(), commission);
            }
        }
    }
    Ok(sampled)
}

fn sampled_commission(validator: &Validator) -> anyhow::Result<Option<SampledCommission>> {
    Ok(
        match (
            validator.inflation_rewards_commission_bps,
            validator.commission_advertised,
        ) {
            (Some(bps), _) => Some(SampledCommission::Bps(u16::try_from(bps)?)),
            (None, Some(percent)) => Some(SampledCommission::Percent(u8::try_from(percent)?)),
            (None, None) => None,
        },
    )
}

/// The snapshot's validators take their performance and the rate they were
/// paid at. A validator the snapshot never listed still takes the sampled rate,
/// where nothing resolved one yet: a closed epoch is never re-listed.
fn apply_finalized_performance(
    validators: &mut SnapshotDoc,
    snapshot: &ValidatorsPerformanceSnapshot,
    sampled: &HashMap<String, SampledCommission>,
    created_at: DateTime<Utc>,
) {
    let mut from_reward_row = 0;
    let mut from_vote_state = 0;
    let mut unresolved = 0;
    for (vote_account, validator) in validators.iter_mut() {
        let Some(performance) = snapshot.validators.get(vote_account) else {
            if validator.commission_effective.is_none() {
                let (commission, bps, source) =
                    resolve_commission_effective(None, sampled.get(vote_account).copied());
                if commission.is_some() {
                    validator.commission_effective = commission;
                    validator.commission_effective_bps = bps;
                    validator.commission_effective_source = source.map(str::to_string);
                    validator.updated_at = Some(created_at);
                }
            }
            continue;
        };
        let (commission, bps, source) = resolve_commission_effective(
            snapshot
                .rewards
                .as_ref()
                .and_then(|rewards| rewards.get(vote_account))
                .and_then(|reward| reward.commission_effective),
            sampled.get(vote_account).copied(),
        );
        match source {
            Some(COMMISSION_EFFECTIVE_SOURCE_REWARD_ROW) => from_reward_row += 1,
            Some(_) => from_vote_state += 1,
            None => unresolved += 1,
        }
        validator.commission_effective = commission;
        validator.commission_effective_bps = bps;
        validator.commission_effective_source = source.map(str::to_string);
        apply_finalized_credits(validator, performance);
        validator.leader_slots = performance.leader_slots.into();
        validator.blocks_produced = performance.blocks_produced.into();
        validator.skip_rate = performance.skip_rate;
        validator.updated_at = Some(created_at);
    }
    info!(
        "Effective commission for {} validators: {from_reward_row} from a reward row, {from_vote_state} from sampled vote state, {unresolved} unresolved",
        snapshot.validators.len()
    );
}

/// Both unknown means the epoch fell out of the `epochCredits` window, so the
/// stored values stand. The close run reads the stake of the next epoch, so a
/// validator unstaked in the closed epoch can report a reward of 0 where it
/// earned none: its stored reward stands too.
fn apply_finalized_credits(validator: &mut Validator, performance: &ValidatorPerformance) {
    if performance.credits.is_none() && performance.vote_reward_lamports.is_none() {
        return;
    }
    validator.credits = performance.credits.map(Decimal::from);
    if !validator.activated_stake.is_zero() {
        validator.vote_reward_lamports = performance.vote_reward_lamports.map(Decimal::from);
    }
}

fn warn_on_unresolved_commission(validators: &SnapshotDoc, epoch: u64) {
    let unresolved = validators
        .values()
        .filter(|validator| validator.commission_effective.is_none())
        .count();
    if unresolved > 0 {
        warn!("Epoch {epoch} closed with {unresolved} validator records still without commission_effective");
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

fn seal_node_observations(
    observations: &NodeObservationsDoc,
    epoch: u64,
) -> SealedNodeObservationsDoc {
    observations
        .iter()
        .filter_map(|(identity, state)| {
            let changes: Vec<_> = state
                .changes
                .iter()
                .filter(|change| change.epoch == epoch)
                .cloned()
                .collect();
            (!changes.is_empty()).then(|| (identity.clone(), changes))
        })
        .collect()
}

fn trim_node_observations(
    mut observations: NodeObservationsDoc,
    epoch: u64,
) -> NodeObservationsDoc {
    for state in observations.values_mut() {
        state.changes.retain(|change| change.epoch > epoch);
    }
    observations
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
