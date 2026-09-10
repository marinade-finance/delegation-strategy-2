//! The documents stored in marinade-directory and the rules for merging a
//! fresh snapshot into one. Every writer and every reader agree here.

use crate::directory::{Directory, Precondition};
use crate::dto::{
    Validator, ValidatorBlockReward, ValidatorJitoMEVInfo, ValidatorJitoPriorityFeeInfo,
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

/// The accumulators. A separate parent from the sealed epochs so that `@last`
/// under `/validators/uptimes/` is always the newest sealed epoch.
pub const LIVE_UPTIMES: &str = "/validators/live/uptimes";
pub const LIVE_COMMISSIONS: &str = "/validators/live/commissions";
pub const LIVE_VERSIONS: &str = "/validators/live/versions";
pub const LIVE_CLUSTER_INFO: &str = "/validators/live/cluster-info";

pub const SCORING_DIR: &str = "/scoring";

pub fn epoch_doc_path(dir: &str, epoch: u64) -> String {
    format!("{dir}/{epoch}")
}

pub fn scoring_breakdowns_path(epoch: u64) -> String {
    format!("{SCORING_DIR}/{epoch}/breakdowns")
}

/// The snapshot of one epoch, keyed by vote account.
pub type SnapshotDoc = BTreeMap<String, Validator>;

/// What `store validators` cannot see stays: the snapshot's `version` and
/// client columns are absent whenever the answering RPC did not report the
/// node, and the close-epoch derivations are written by close-epoch alone.
pub fn merge_validator(existing: Option<&Validator>, mut incoming: Validator) -> Validator {
    let Some(old) = existing else {
        return incoming;
    };
    incoming.version = incoming.version.or_else(|| old.version.clone());
    if incoming.client_id_raw.is_none() {
        incoming.client_id = old.client_id;
        incoming.client_id_raw = old.client_id_raw.clone();
    }
    incoming.commission_max_observed = old.commission_max_observed;
    incoming.commission_min_observed = old.commission_min_observed;
    incoming.commission_effective = old.commission_effective;
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

/// The latest MEV observation of one validator in one epoch.
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

/// The latest priority-fee observation of one validator in one epoch.
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

/// One settlement of one validator in one epoch, keyed by `(reason, meta)`.
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

/// One validator's uptime: the interval still being extended, and the ones
/// closed since the last seal.
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
    pub slots_per_year: Option<f64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ClusterInfoDoc {
    pub epoch: u64,
    #[serde(default)]
    pub samples: Vec<ClusterInfoSample>,
}
