//! Every document the API serves from, refreshed by conditional GETs: a
//! sealed epoch answers 304 forever, so only the open epoch costs bytes.

use crate::directory::{Directory, Fetch};
use crate::docs::{
    epoch_doc_path, scoring_breakdowns_path, BlockRewardsDoc, ClusterInfoDoc, CommissionSample,
    CommissionsDoc, EpochDoc, EventsDoc, MevDoc, PriorityFeeDoc, ScoringBreakdownsDoc,
    SealedClusterInfoDoc, SealedCommissionsDoc, SealedUptimesDoc, SealedVersionsDoc, SnapshotDoc,
    UptimeInterval, UptimesDoc, VersionSample, VersionsDoc, BLOCK_REWARDS_DIR, CLUSTER_INFO_DIR,
    COMMISSIONS_DIR, EPOCHS_DIR, EVENTS_DIR, LIVE_CLUSTER_INFO, LIVE_COMMISSIONS, LIVE_UPTIMES,
    LIVE_VERSIONS, MEV_DIR, PRIORITY_FEE_DIR, SNAPSHOT_DIR, UPTIMES_DIR, VERSIONS_DIR,
};
use anyhow::Context;
use log::info;
use serde::de::DeserializeOwned;
use std::collections::{BTreeMap, HashMap};
use std::ops::RangeInclusive;

/// The accumulators, as they stood at the last warm.
#[derive(Default)]
pub struct Live {
    pub uptimes: UptimesDoc,
    pub commissions: CommissionsDoc,
    pub versions: VersionsDoc,
    pub cluster_info: ClusterInfoDoc,
}

#[derive(Default)]
pub struct Warehouse {
    pub snapshots: BTreeMap<u64, SnapshotDoc>,
    pub mev: BTreeMap<u64, MevDoc>,
    pub priority_fees: BTreeMap<u64, PriorityFeeDoc>,
    pub events: BTreeMap<u64, EventsDoc>,
    pub block_rewards: BTreeMap<u64, BlockRewardsDoc>,
    pub epochs: BTreeMap<u64, EpochDoc>,
    pub uptimes: BTreeMap<u64, SealedUptimesDoc>,
    pub commissions: BTreeMap<u64, SealedCommissionsDoc>,
    pub versions: BTreeMap<u64, SealedVersionsDoc>,
    pub cluster_info: BTreeMap<u64, SealedClusterInfoDoc>,
    pub scoring: BTreeMap<u64, ScoringBreakdownsDoc>,
    pub live: Live,
    etags: HashMap<String, String>,
}

impl Warehouse {
    /// Refreshes every document of the last `epochs` epochs.
    pub async fn warm(&mut self, directory: &Directory, epochs: u64) -> anyhow::Result<()> {
        let Some(last_epoch) = last_epoch(directory).await? else {
            info!("No epoch has a snapshot yet");
            return Ok(());
        };
        let window = last_epoch.saturating_sub(epochs.saturating_sub(1))..=last_epoch;
        info!("Warming documents of epochs {window:?}");

        self.warm_kind(directory, SNAPSHOT_DIR, window.clone(), |w| {
            &mut w.snapshots
        })
        .await?;
        self.warm_kind(directory, MEV_DIR, window.clone(), |w| &mut w.mev)
            .await?;
        self.warm_kind(directory, PRIORITY_FEE_DIR, window.clone(), |w| {
            &mut w.priority_fees
        })
        .await?;
        self.warm_kind(directory, EVENTS_DIR, window.clone(), |w| &mut w.events)
            .await?;
        self.warm_kind(directory, BLOCK_REWARDS_DIR, window.clone(), |w| {
            &mut w.block_rewards
        })
        .await?;
        self.warm_kind(directory, EPOCHS_DIR, window.clone(), |w| &mut w.epochs)
            .await?;
        self.warm_kind(directory, UPTIMES_DIR, window.clone(), |w| &mut w.uptimes)
            .await?;
        self.warm_kind(directory, COMMISSIONS_DIR, window.clone(), |w| {
            &mut w.commissions
        })
        .await?;
        self.warm_kind(directory, VERSIONS_DIR, window.clone(), |w| &mut w.versions)
            .await?;
        self.warm_kind(directory, CLUSTER_INFO_DIR, window.clone(), |w| {
            &mut w.cluster_info
        })
        .await?;

        self.warm_scoring(directory, window).await?;

        self.warm_live(directory, LIVE_UPTIMES, |w| &mut w.live.uptimes)
            .await?;
        self.warm_live(directory, LIVE_COMMISSIONS, |w| &mut w.live.commissions)
            .await?;
        self.warm_live(directory, LIVE_VERSIONS, |w| &mut w.live.versions)
            .await?;
        self.warm_live(directory, LIVE_CLUSTER_INFO, |w| &mut w.live.cluster_info)
            .await?;

        Ok(())
    }

    /// The last epoch cluster info was sampled in, which is what the SQL path
    /// read off the `cluster_info` table.
    pub fn last_cluster_epoch(&self) -> u64 {
        let sealed = self.cluster_info.keys().max().copied().unwrap_or(0);
        sealed.max(self.live.cluster_info.epoch)
    }

    /// The last epoch with a snapshot.
    pub fn last_epoch(&self) -> u64 {
        self.snapshots.keys().max().copied().unwrap_or(0)
    }

    /// The last `epochs` epochs ending at the last one with a snapshot, newest
    /// first. Every epoch of the window is listed, including the ones no
    /// snapshot landed for: the per-epoch series the API pairs must line up.
    pub fn epochs_window(&self, epochs: u64) -> Vec<u64> {
        let last_epoch = self.last_epoch();
        let first_epoch = last_epoch - epochs.min(last_epoch) + 1;
        (first_epoch..=last_epoch).rev().collect()
    }

    /// The first epoch of a window of `epochs` epochs ending at the last one
    /// cluster info was sampled in.
    pub fn window_start(&self, epochs: u64) -> u64 {
        let last = self.last_cluster_epoch();
        last.saturating_sub(epochs.saturating_sub(1))
    }

    /// Every uptime interval of the window: the sealed ones, and the
    /// accumulator's for the epochs no seal covers yet.
    pub fn uptime_intervals(&self, epochs: u64) -> Vec<(&String, &UptimeInterval)> {
        let first = self.window_start(epochs);
        let mut intervals: Vec<_> = self
            .uptimes
            .range(first..)
            .flat_map(|(_, sealed)| sealed.iter())
            .flat_map(|(vote_account, intervals)| {
                intervals
                    .iter()
                    .map(move |interval| (vote_account, interval))
            })
            .collect();

        for (vote_account, state) in self.live.uptimes.iter() {
            for interval in state.closed.iter().chain([&state.open]) {
                if interval.epoch >= first && !self.uptimes.contains_key(&interval.epoch) {
                    intervals.push((vote_account, interval));
                }
            }
        }

        intervals
    }

    /// The commission changes recorded in one epoch, sealed or still live.
    pub fn commissions_of(&self, epoch: u64) -> HashMap<&String, Vec<&CommissionSample>> {
        if let Some(sealed) = self.commissions.get(&epoch) {
            return sealed
                .iter()
                .map(|(vote_account, changes)| (vote_account, changes.iter().collect()))
                .collect();
        }

        self.live
            .commissions
            .iter()
            .filter_map(|(vote_account, state)| {
                let changes: Vec<_> = state
                    .changes
                    .iter()
                    .filter(|change| change.epoch == epoch)
                    .collect();
                (!changes.is_empty()).then_some((vote_account, changes))
            })
            .collect()
    }

    pub fn commission_changes(&self, epochs: u64) -> Vec<(&String, &CommissionSample)> {
        let first = self.window_start(epochs);
        let mut changes: Vec<_> = self
            .commissions
            .range(first..)
            .flat_map(|(_, sealed)| sealed.iter())
            .flat_map(|(vote_account, changes)| {
                changes.iter().map(move |change| (vote_account, change))
            })
            .collect();

        for (vote_account, state) in self.live.commissions.iter() {
            for change in state.changes.iter() {
                if change.epoch >= first && !self.commissions.contains_key(&change.epoch) {
                    changes.push((vote_account, change));
                }
            }
        }

        changes
    }

    pub fn version_changes(&self, epochs: u64) -> Vec<(&String, &VersionSample)> {
        let first = self.window_start(epochs);
        let mut changes: Vec<_> = self
            .versions
            .range(first..)
            .flat_map(|(_, sealed)| sealed.iter())
            .flat_map(|(vote_account, changes)| {
                changes.iter().map(move |change| (vote_account, change))
            })
            .collect();

        for (vote_account, state) in self.live.versions.iter() {
            for change in state.changes.iter() {
                if change.epoch >= first && !self.versions.contains_key(&change.epoch) {
                    changes.push((vote_account, change));
                }
            }
        }

        changes
    }

    async fn warm_kind<T: DeserializeOwned>(
        &mut self,
        directory: &Directory,
        dir: &str,
        window: RangeInclusive<u64>,
        pick: fn(&mut Self) -> &mut BTreeMap<u64, T>,
    ) -> anyhow::Result<()> {
        for epoch in window.clone() {
            let path = epoch_doc_path(dir, epoch);
            match self.fetch::<T>(directory, &path).await? {
                Some(Some(doc)) => {
                    pick(self).insert(epoch, doc);
                }
                Some(None) => {
                    pick(self).remove(&epoch);
                }
                None => {}
            }
        }
        pick(self).retain(|epoch, _| window.contains(epoch));
        Ok(())
    }

    async fn warm_scoring(
        &mut self,
        directory: &Directory,
        window: RangeInclusive<u64>,
    ) -> anyhow::Result<()> {
        for epoch in window.clone() {
            let path = scoring_breakdowns_path(epoch);
            // The only document here written by another service. A field
            // ds-scoring adds or retypes would otherwise fail this warm and
            // with it every other one, leaving validators, rewards and uptimes
            // indefinitely stale behind two green probes. Its own routes go
            // stale instead.
            let fetched = match self.fetch::<ScoringBreakdownsDoc>(directory, &path).await {
                Ok(fetched) => fetched,
                Err(err) => {
                    log::error!("Scoring breakdowns at {path} were not readable: {err:#}");
                    continue;
                }
            };
            match fetched {
                Some(Some(doc)) => {
                    self.scoring.insert(epoch, doc);
                }
                Some(None) => {
                    self.scoring.remove(&epoch);
                }
                None => {}
            }
        }
        self.scoring.retain(|epoch, _| window.contains(epoch));
        Ok(())
    }

    async fn warm_live<T: DeserializeOwned + Default>(
        &mut self,
        directory: &Directory,
        path: &str,
        pick: fn(&mut Self) -> &mut T,
    ) -> anyhow::Result<()> {
        match self.fetch::<T>(directory, path).await? {
            Some(Some(doc)) => *pick(self) = doc,
            Some(None) => *pick(self) = T::default(),
            None => {}
        }
        Ok(())
    }

    /// `None` where the stored version is the one already held, `Some(None)`
    /// where the document is gone.
    async fn fetch<T: DeserializeOwned>(
        &mut self,
        directory: &Directory,
        path: &str,
    ) -> anyhow::Result<Option<Option<T>>> {
        let Some(etag) = self.etags.get(path) else {
            let Some(doc) = directory.get::<T>(path).await? else {
                return Ok(Some(None));
            };
            self.etags.insert(path.to_string(), doc.etag);
            return Ok(Some(Some(doc.body)));
        };

        match directory.get_if_none_match::<T>(path, etag).await? {
            Fetch::Modified(doc) => {
                self.etags.insert(path.to_string(), doc.etag);
                Ok(Some(Some(doc.body)))
            }
            Fetch::NotModified => Ok(None),
            Fetch::Missing => {
                self.etags.remove(path);
                Ok(Some(None))
            }
        }
    }
}

/// The newest epoch under `/validators/snapshot`, in the store's natural
/// order, or `None` before the first snapshot is written.
async fn last_epoch(directory: &Directory) -> anyhow::Result<Option<u64>> {
    let Some(path) = directory.resolve(&format!("{SNAPSHOT_DIR}/@last")).await? else {
        return Ok(None);
    };
    let name = path.rsplit('/').next().unwrap_or_default();
    let epoch = name
        .parse::<u64>()
        .with_context(|| format!("{path} is not an epoch, so no window can be built on it"))?;
    Ok(Some(epoch))
}

/// Whether the store holds a snapshot at all, which a cold cache cannot tell
/// from lost data on its own.
pub async fn has_validators(directory: &Directory) -> anyhow::Result<bool> {
    Ok(last_epoch(directory).await?.is_some())
}
