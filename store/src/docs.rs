//! The documents stored in marinade-directory and the rules for merging a
//! fresh snapshot into one.

use crate::directory::{Directory, Precondition};
use crate::dto::{
    ScoringRunRecord, Validator, ValidatorBlockReward, ValidatorJitoMEVInfo,
    ValidatorJitoPriorityFeeInfo, ValidatorScoreRecord,
};
use chrono::{DateTime, Utc};
use rust_decimal::prelude::*;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const SNAPSHOT_DIR: &str = "/validators/snapshot";
pub const MEV_DIR: &str = "/validators/mev";
pub const PRIORITY_FEE_DIR: &str = "/validators/priority-fee";
pub const EVENTS_DIR: &str = "/validators/events";
pub const BLOCK_REWARDS_DIR: &str = "/validators/block-rewards";
pub const EPOCHS_DIR: &str = "/validators/epochs";
pub const UPTIMES_DIR: &str = "/validators/uptimes";
pub const COMMISSIONS_DIR: &str = "/validators/commissions";
pub const VERSIONS_DIR: &str = "/validators/versions";
pub const CLUSTER_INFO_DIR: &str = "/validators/cluster-info";
pub const VALIDATOR_REWARDS_DIR: &str = "/validators/validator-rewards";
pub const SANDWICHES_DIR: &str = "/validators/sandwiches";

/// The accumulators. A separate parent from the sealed epochs so that `@last`
/// under `/validators/uptimes/` is always the newest sealed epoch.
pub const LIVE_UPTIMES: &str = "/validators/live/uptimes";
pub const LIVE_COMMISSIONS: &str = "/validators/live/commissions";
pub const LIVE_VERSIONS: &str = "/validators/live/versions";
pub const LIVE_CLUSTER_INFO: &str = "/validators/live/cluster-info";

/// Documents with no epoch of their own: keyed by what they describe, rewritten
/// in place.
pub const RELEASES_PATH: &str = "/validators/releases";

pub const SCORING_DIR: &str = "/scoring";

pub fn epoch_doc_path(dir: &str, epoch: u64) -> String {
    format!("{dir}/{epoch}")
}

pub fn scoring_breakdowns_path(epoch: u64) -> String {
    format!("{SCORING_DIR}/{epoch}/breakdowns")
}

pub type SnapshotDoc = BTreeMap<String, Validator>;

/// What `store validators` cannot see stays: the snapshot's `version` and
/// client columns are absent whenever the answering RPC did not report the
/// node, the vote-state fields whenever its state did not parse, the pending
/// and direct stakes whenever their accounts were not read, and the data
/// center whenever whois did not answer for an unchanged address. The
/// close-epoch derivations are written by close-epoch alone.
pub fn merge_validator(existing: Option<&Validator>, mut incoming: Validator) -> Validator {
    let Some(old) = existing else {
        return incoming;
    };
    incoming.version = incoming.version.or_else(|| old.version.clone());
    if incoming.client_id_raw.is_none() {
        incoming.client_id = old.client_id;
        incoming.client_id_raw = old.client_id_raw.clone();
    }
    // A resolved answer replaces all eight together: mixing its nulls with the
    // previous data center would invent a location nothing observed.
    if !incoming.dc_resolved && incoming.node_ip == old.node_ip && old.has_data_center() {
        incoming.copy_data_center_from(old);
    }
    if incoming.inflation_rewards_commission_bps_is_v4.is_none() {
        incoming.inflation_rewards_collector = old.inflation_rewards_collector.clone();
        incoming.block_revenue_collector = old.block_revenue_collector.clone();
        incoming.inflation_rewards_commission_bps = old.inflation_rewards_commission_bps;
        incoming.inflation_rewards_commission_bps_is_v4 =
            old.inflation_rewards_commission_bps_is_v4;
        incoming.block_revenue_commission_bps = old.block_revenue_commission_bps;
        incoming.pending_delegator_rewards = old.pending_delegator_rewards;
        incoming.inflation_rewards_collector_owner = old.inflation_rewards_collector_owner.clone();
        incoming.inflation_rewards_collector_lamports = old.inflation_rewards_collector_lamports;
        incoming.inflation_rewards_collector_healthy = old.inflation_rewards_collector_healthy;
        incoming.block_revenue_collector_owner = old.block_revenue_collector_owner.clone();
        incoming.block_revenue_collector_lamports = old.block_revenue_collector_lamports;
        incoming.block_revenue_collector_healthy = old.block_revenue_collector_healthy;
    }
    incoming.activating_stake = incoming.activating_stake.or(old.activating_stake);
    incoming.deactivating_stake = incoming.deactivating_stake.or(old.deactivating_stake);
    incoming.direct_stake = incoming.direct_stake.or(old.direct_stake);
    incoming.direct_activating_stake = incoming
        .direct_activating_stake
        .or(old.direct_activating_stake);
    incoming.direct_deactivating_stake = incoming
        .direct_deactivating_stake
        .or(old.direct_deactivating_stake);
    incoming.commission_max_observed = old.commission_max_observed;
    incoming.commission_min_observed = old.commission_min_observed;
    incoming.commission_effective = old.commission_effective;
    incoming.commission_effective_source = old.commission_effective_source.clone();
    incoming.commission_effective_bps = old.commission_effective_bps;
    incoming.uptime_pct = old.uptime_pct;
    incoming.uptime = old.uptime;
    incoming.downtime = old.downtime;
    incoming
}

pub fn merge_snapshot(doc: &mut SnapshotDoc, incoming: SnapshotDoc) {
    for (vote_account, validator) in incoming {
        let merged = merge_validator(doc.get(&vote_account), validator);
        doc.insert(vote_account, merged);
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MevEntry {
    pub vote_account: String,
    pub mev_commission: i32,
    pub total_epoch_rewards: Option<Decimal>,
    pub claimed_epoch_rewards: Option<Decimal>,
    pub total_epoch_claimants: Option<i32>,
    pub epoch_active_claimants: Option<i32>,
    pub epoch_slot: Decimal,
    pub epoch: Decimal,
    pub created_at: DateTime<Utc>,
}

impl MevEntry {
    pub fn new(
        info: &ValidatorJitoMEVInfo,
        epoch_slot: Decimal,
        created_at: DateTime<Utc>,
    ) -> Self {
        Self {
            vote_account: info.vote_account.clone(),
            mev_commission: info.mev_commission,
            total_epoch_rewards: info.total_epoch_rewards,
            claimed_epoch_rewards: info.claimed_epoch_rewards,
            total_epoch_claimants: info.total_epoch_claimants,
            epoch_active_claimants: info.epoch_active_claimants,
            epoch_slot,
            epoch: info.epoch,
            created_at,
        }
    }
}

pub type MevDoc = BTreeMap<String, MevEntry>;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PriorityFeeEntry {
    pub vote_account: String,
    pub priority_commission: i32,
    pub total_lamports_transferred: Decimal,
    pub total_epoch_rewards: Option<Decimal>,
    pub claimed_epoch_rewards: Option<Decimal>,
    pub total_epoch_claimants: Option<i32>,
    pub epoch_active_claimants: Option<i32>,
    pub epoch_slot: Decimal,
    pub epoch: Decimal,
    pub created_at: DateTime<Utc>,
}

impl PriorityFeeEntry {
    pub fn new(
        info: &ValidatorJitoPriorityFeeInfo,
        epoch_slot: Decimal,
        created_at: DateTime<Utc>,
    ) -> Self {
        Self {
            vote_account: info.vote_account.clone(),
            priority_commission: info.priority_commission,
            total_lamports_transferred: info.total_lamports_transferred,
            total_epoch_rewards: info.total_epoch_rewards,
            claimed_epoch_rewards: info.claimed_epoch_rewards,
            total_epoch_claimants: info.total_epoch_claimants,
            epoch_active_claimants: info.epoch_active_claimants,
            epoch_slot,
            epoch: info.epoch,
            created_at,
        }
    }
}

pub type PriorityFeeDoc = BTreeMap<String, PriorityFeeEntry>;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BlockRewardEntry {
    pub epoch: u64,
    pub identity_account: String,
    pub vote_account: String,
    pub authorized_voter: String,
    pub amount: Decimal,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl BlockRewardEntry {
    pub fn new(reward: &ValidatorBlockReward, created_at: DateTime<Utc>) -> anyhow::Result<Self> {
        Ok(Self {
            epoch: reward.epoch.try_into()?,
            identity_account: reward.identity_account.clone(),
            vote_account: reward.vote_account.clone(),
            authorized_voter: reward.authorized_voter.clone(),
            amount: reward.amount,
            created_at,
            updated_at: created_at,
        })
    }
}

pub type BlockRewardsDoc = BTreeMap<String, BlockRewardEntry>;

pub fn block_reward_key(identity_account: &str, vote_account: &str) -> String {
    format!("{identity_account}/{vote_account}")
}

/// A rewrite of an existing reward keeps the moment it was first seen.
pub fn merge_block_rewards(doc: &mut BlockRewardsDoc, incoming: BlockRewardsDoc) {
    for (key, mut reward) in incoming {
        if let Some(old) = doc.get(&key) {
            reward.created_at = old.created_at;
        }
        doc.insert(key, reward);
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct EventEntry {
    pub reason: String,
    pub meta: String,
    pub amount: Decimal,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub type EventsDoc = BTreeMap<String, Vec<EventEntry>>;

pub fn merge_events(doc: &mut EventsDoc, incoming: EventsDoc) {
    for (vote_account, events) in incoming {
        let stored = doc.entry(vote_account).or_default();
        for event in events {
            upsert_event(stored, event);
        }
    }
}

/// A settlement is unique per `(reason, meta)`; a rewrite keeps the moment it
/// was first seen.
pub fn upsert_event(events: &mut Vec<EventEntry>, mut event: EventEntry) {
    match events
        .iter()
        .position(|e| e.reason == event.reason && e.meta == event.meta)
    {
        Some(index) => {
            event.created_at = events[index].created_at;
            events[index] = event;
        }
        None => events.push(event),
    }
}

/// mev and priority-fee hold the latest observation of a validator, so a
/// fresh entry simply replaces the stored one.
pub fn replace_entries<T>(doc: &mut BTreeMap<String, T>, incoming: BTreeMap<String, T>) {
    doc.extend(incoming);
}

/// One validator's rewards in one epoch, as the take-rates collector sums them
/// from BigQuery: both sides of every component, in lamports.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ValidatorRewardsEntry {
    pub validator_rewards: Decimal,
    pub total_rewards: Decimal,
    pub inflation_rewards: Decimal,
    pub mev_rewards: Decimal,
    pub block_rewards: Decimal,
    /// `validator_rewards / total_rewards`.
    pub take_rate: f64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub type ValidatorRewardsDoc = BTreeMap<String, ValidatorRewardsEntry>;

/// A rewrite keeps the moment the rewards were first seen.
pub fn merge_validator_rewards(doc: &mut ValidatorRewardsDoc, incoming: ValidatorRewardsDoc) {
    for (vote_account, mut entry) in incoming {
        if let Some(old) = doc.get(&vote_account) {
            entry.created_at = old.created_at;
        }
        doc.insert(vote_account, entry);
    }
}

/// One validator's sandwich figures in one epoch, as solana-sandwich-report
/// published them. The counts cover the 30-day window the rate is measured
/// over, not the epoch.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SandwichEntry {
    pub blocks_produced: u64,
    pub blocks_with_sandwiches: u64,
    /// Percent, one decimal.
    pub sandwich_rate_30d: f64,
    /// Absent before epoch 820: upstream published only the 30d rate then.
    pub sandwich_rate_60d: Option<f64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub type SandwichesDoc = BTreeMap<String, SandwichEntry>;

/// A rewrite keeps the moment the figures were first seen.
pub fn merge_sandwiches(doc: &mut SandwichesDoc, incoming: SandwichesDoc) {
    for (vote_account, mut entry) in incoming {
        if let Some(old) = doc.get(&vote_account) {
            entry.created_at = old.created_at;
        }
        doc.insert(vote_account, entry);
    }
}

/// One release of a client, filled by three sources that write disjoint
/// fields: GitHub gives the publish time, the SFDP endpoint and the feature
/// gate tracker each give a floor.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ReleaseEntry {
    pub released_at: Option<DateTime<Utc>>,
    pub release_url: Option<String>,
    /// First epoch the Solana Foundation Delegation Program required this version.
    pub sfdp_floor_epoch: Option<u64>,
    /// First epoch the cluster's feature gates required this version.
    pub feature_gate_epoch: Option<u64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Keyed by client lineage, then by the version as the client reports it in
/// gossip.
pub type ReleasesDoc = BTreeMap<String, BTreeMap<String, ReleaseEntry>>;

/// Writes a document its writer owns outright: creates it, or replaces the
/// version that is there. The read discards the body it does not need.
pub async fn put_whole<T: Serialize>(
    directory: &Directory,
    path: &str,
    body: &T,
) -> anyhow::Result<()> {
    let precondition = match directory.get::<serde::de::IgnoredAny>(path).await? {
        Some(stored) => Precondition::IfMatch(stored.etag),
        None => Precondition::Create,
    };
    directory.put(path, body, precondition).await?;
    Ok(())
}

/// GET-merge-PUT of a per-epoch document. The one repeat covers a concurrent
/// first write of the epoch; a lost `IfMatch` race is an error.
pub async fn merge_into<T>(
    directory: &Directory,
    path: &str,
    incoming: T,
    merge: fn(&mut T, T),
) -> anyhow::Result<()>
where
    T: Default + Clone + Serialize + DeserializeOwned,
{
    let stored = directory.get::<T>(path).await?;
    let created = stored.is_none();
    let (mut doc, precondition) = match stored {
        Some(stored) => (stored.body, Precondition::IfMatch(stored.etag)),
        None => (T::default(), Precondition::Create),
    };
    merge(&mut doc, incoming.clone());

    match directory.put(path, &doc, precondition).await {
        Ok(_) => Ok(()),
        Err(err) if created && err.is_conflict() => {
            let stored = directory
                .get::<T>(path)
                .await?
                .ok_or_else(|| anyhow::anyhow!("{path} vanished between two writes"))?;
            let mut doc = stored.body;
            merge(&mut doc, incoming);
            directory
                .put(path, &doc, Precondition::IfMatch(stored.etag))
                .await?;
            Ok(())
        }
        Err(err) => Err(err.into()),
    }
}

/// Written last, once an epoch is sealed; its presence is the sealed signal.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct EpochDoc {
    pub epoch: u64,
    pub start_at: DateTime<Utc>,
    pub end_at: DateTime<Utc>,
    pub transaction_count: u64,
    pub supply: Decimal,
    pub inflation: f64,
    pub inflation_taper: f64,
    pub slots_per_year: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum UptimeStatus {
    Up,
    Down,
}

impl UptimeStatus {
    pub fn from_delinquency(delinquent: bool) -> Self {
        if delinquent {
            Self::Down
        } else {
            Self::Up
        }
    }
}

impl std::fmt::Display for UptimeStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Up => write!(f, "UP"),
            Self::Down => write!(f, "DOWN"),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct UptimeInterval {
    pub status: UptimeStatus,
    pub epoch: u64,
    pub start_at: DateTime<Utc>,
    pub end_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct UptimeState {
    pub open: UptimeInterval,
    #[serde(default)]
    pub closed: Vec<UptimeInterval>,
}

pub type UptimesDoc = BTreeMap<String, UptimeState>;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CommissionSample {
    pub epoch: u64,
    pub epoch_slot: u64,
    pub commission: i32,
    pub created_at: DateTime<Utc>,
}

/// `last` outlives a seal so the change rule still knows the previous value.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CommissionState {
    pub last: CommissionSample,
    #[serde(default)]
    pub changes: Vec<CommissionSample>,
}

pub type CommissionsDoc = BTreeMap<String, CommissionState>;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct VersionSample {
    pub epoch: u64,
    pub epoch_slot: u64,
    pub version: Option<String>,
    pub client_id: Option<i32>,
    pub client_id_raw: Option<String>,
    pub feature_set: Option<i64>,
    pub shred_version: Option<i32>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct VersionState {
    pub last: VersionSample,
    #[serde(default)]
    pub changes: Vec<VersionSample>,
}

pub type VersionsDoc = BTreeMap<String, VersionState>;

/// Each sample carries its own epoch: the hour between a rollover and
/// close-epoch samples both epochs, and the closing one is still needed.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ClusterInfoSample {
    pub epoch: u64,
    pub epoch_slot: u64,
    pub transaction_count: u64,
    pub created_at: DateTime<Utc>,
    pub slots_per_year: f64,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ClusterInfoDoc {
    pub epoch: u64,
    #[serde(default)]
    pub samples: Vec<ClusterInfoSample>,
}

/// ds-scoring's document for one epoch: the run that produced the scores and
/// the scores themselves. DS2 reads it; it never writes one.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ScoringBreakdownsDoc {
    pub scoring_run_id: i64,
    pub epoch: i32,
    pub created_at: DateTime<Utc>,
    pub ui_id: String,
    pub components: Vec<String>,
    pub component_weights: Vec<f64>,
    pub scores: Vec<ValidatorScoreRecord>,
}

impl ScoringBreakdownsDoc {
    pub fn scoring_run(&self) -> ScoringRunRecord {
        ScoringRunRecord {
            scoring_run_id: self.scoring_run_id.into(),
            created_at: self.created_at,
            epoch: self.epoch,
            components: self.components.clone(),
            component_weights: self.component_weights.clone(),
            ui_id: self.ui_id.clone(),
        }
    }
}

/// What close-epoch seals under `/validators/{stream}/{epoch}`.
pub type SealedUptimesDoc = BTreeMap<String, Vec<UptimeInterval>>;
pub type SealedCommissionsDoc = BTreeMap<String, Vec<CommissionSample>>;
pub type SealedVersionsDoc = BTreeMap<String, Vec<VersionSample>>;
pub type SealedClusterInfoDoc = Vec<ClusterInfoSample>;
