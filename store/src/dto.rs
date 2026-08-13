use chrono::{DateTime, Utc};
use collect::solana_service::{resolve_client_id, ClientId};
use collect::validators::{ValidatorDataCenter, ValidatorSnapshot};
use collect::validators_block_rewards::ValidatorBlockRewards;
use collect::validators_jito::{
    MevTipDistributionValidatorSnapshot, PriorityFeeDistributionValidatorSnapshot,
};
use rust_decimal::prelude::*;
use serde::de::{self, Unexpected};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashMap;

/// Served instead of null so every consumer has a client name to render.
pub const UNKNOWN_CLIENT_NAME: &str = "Unknown";

// Re-resolved because the stored id is registry-only: without this a client-ids.csv row added later reclassifies nothing already collected.
pub fn effective_client_id(client_id: Option<u16>, client_id_raw: Option<&str>) -> Option<u16> {
    client_id.or_else(|| resolve_client_id(client_id_raw).number())
}

// Gated on both halves of the registry — the CSV names and the vendored groupings — so an id in one but not the other cannot read as half-classified.
fn classified(client_id: Option<u16>) -> Option<ClientId> {
    let client = ClientId::Registered(client_id?);
    (client.name().is_some() && client.label().is_some()).then_some(client)
}

pub fn client_name(client_id: Option<u16>) -> String {
    classified(client_id)
        .and_then(|client| client.name())
        .unwrap_or(UNKNOWN_CLIENT_NAME)
        .to_string()
}

pub fn client_label(client_id: Option<u16>) -> String {
    classified(client_id)
        .and_then(|client| client.label())
        .unwrap_or(UNKNOWN_CLIENT_NAME)
        .to_string()
}

pub fn client_vendor(client_id: Option<u16>) -> Option<String> {
    classified(client_id).and_then(|client| client.vendor().map(str::to_string))
}

pub fn client_lineage(client_id: Option<u16>) -> Option<String> {
    classified(client_id).and_then(|client| client.lineage().map(str::to_string))
}

pub struct ValidatorJitoMEVInfo {
    pub vote_account: String,
    pub mev_commission: i32,
    pub epoch: Decimal,
    pub total_epoch_rewards: Option<Decimal>,
    pub claimed_epoch_rewards: Option<Decimal>,
    pub total_epoch_claimants: Option<i32>,
    pub epoch_active_claimants: Option<i32>,
}

impl ValidatorJitoMEVInfo {
    pub fn from_snapshot(v: &MevTipDistributionValidatorSnapshot) -> Self {
        Self {
            vote_account: v.vote_account.clone(),
            mev_commission: v.mev_commission as i32,
            epoch: v.epoch.into(),
            total_epoch_rewards: v.total_epoch_rewards.map(Into::into),
            claimed_epoch_rewards: v.claimed_epoch_rewards.map(Into::into),
            total_epoch_claimants: v.total_epoch_claimants.map(|v| v as i32),
            epoch_active_claimants: v.epoch_active_claimants.map(|v| v as i32),
        }
    }
}

pub struct ValidatorJitoPriorityFeeInfo {
    pub vote_account: String,
    pub priority_commission: i32,
    pub total_lamports_transferred: Decimal,
    pub epoch: Decimal,
    pub total_epoch_rewards: Option<Decimal>,
    pub claimed_epoch_rewards: Option<Decimal>,
    pub total_epoch_claimants: Option<i32>,
    pub epoch_active_claimants: Option<i32>,
}

impl ValidatorJitoPriorityFeeInfo {
    pub fn from_snapshot(v: &PriorityFeeDistributionValidatorSnapshot) -> Self {
        Self {
            vote_account: v.vote_account.clone(),
            priority_commission: v.priority_commission as i32,
            total_lamports_transferred: v.total_lamports_transferred.into(),
            epoch: v.epoch.into(),
            total_epoch_rewards: v.total_epoch_rewards.map(Into::into),
            claimed_epoch_rewards: v.claimed_epoch_rewards.map(Into::into),
            total_epoch_claimants: v.total_epoch_claimants.map(|v| v as i32),
            epoch_active_claimants: v.epoch_active_claimants.map(|v| v as i32),
        }
    }
}

pub struct ValidatorBlockReward {
    pub epoch: Decimal,
    pub identity_account: String,
    pub vote_account: String,
    pub authorized_voter: String,
    pub amount: Decimal,
}

impl ValidatorBlockReward {
    pub fn from_snapshot(reward: &ValidatorBlockRewards, epoch: u64) -> Self {
        Self {
            epoch: epoch.into(),
            identity_account: reward.identity_account.clone(),
            vote_account: reward.vote_account.clone(),
            authorized_voter: reward.authorized_voter.clone(),
            amount: Decimal::from(reward.amount),
        }
    }
}

pub struct Validator {
    pub identity: String,
    pub vote_account: String,
    pub epoch: Decimal,
    pub info_name: Option<String>,
    pub info_url: Option<String>,
    pub info_keybase: Option<String>,
    pub info_icon_url: Option<String>,
    pub node_ip: Option<String>,
    pub dc_coordinates_lat: Option<f64>,
    pub dc_coordinates_lon: Option<f64>,
    pub dc_continent: Option<String>,
    pub dc_country_iso: Option<String>,
    pub dc_country: Option<String>,
    pub dc_city: Option<String>,
    pub dc_asn: Option<i32>,
    pub dc_aso: Option<String>,
    pub commission_max_observed: Option<i32>,
    pub commission_min_observed: Option<i32>,
    pub commission_advertised: Option<i32>,
    pub commission_effective: Option<i32>,
    pub version: Option<String>,
    pub client_id: Option<i32>,
    pub client_id_raw: Option<String>,
    pub feature_set: Option<i64>,
    pub shred_version: Option<i32>,
    pub gossip_port: Option<i32>,
    pub rpc_public: Option<bool>,
    pub pubsub_public: Option<bool>,
    pub activated_stake: Decimal,
    pub marinade_stake: Decimal,
    pub foundation_stake: Decimal,
    pub marinade_native_stake: Decimal,
    pub institutional_stake: Decimal,
    pub self_stake: Decimal,
    pub superminority: bool,
    pub stake_to_become_superminority: Decimal,
    pub credits: Decimal,
    pub leader_slots: Decimal,
    pub blocks_produced: Decimal,
    pub skip_rate: f64,
    pub uptime_pct: Option<f64>,
    pub uptime: Option<Decimal>,
    pub downtime: Option<Decimal>,
    pub updated_at: Option<DateTime<Utc>>,
}

impl Validator {
    pub fn new_from_snapshot(v: &ValidatorSnapshot, epoch: u64) -> Self {
        let ValidatorDataCenter {
            coordinates,
            continent,
            country_iso,
            country,
            city,
            asn,
            aso,
        } = v.data_center.clone().unwrap_or_default();

        Self {
            identity: v.identity.clone(),
            vote_account: v.vote_account.clone(),
            epoch: epoch.into(),
            info_name: v.info_name.clone(),
            info_url: v.info_url.clone(),
            info_keybase: v.info_keybase.clone(),
            info_icon_url: v.info_icon_url.clone(),

            node_ip: v.node_ip.clone(),
            dc_coordinates_lon: coordinates.map(|(lon, _)| lon),
            dc_coordinates_lat: coordinates.map(|(_, lat)| lat),
            dc_continent: continent,
            dc_country_iso: country_iso,
            dc_country: country,
            dc_city: city,
            dc_asn: asn.map(|asn| asn as i32),
            dc_aso: aso,

            commission_max_observed: None,
            commission_min_observed: None,
            commission_advertised: Some(v.performance.commission as i32),
            commission_effective: None,
            version: v.performance.version.clone(),
            client_id: v.performance.client_id.map(|id| id as i32),
            client_id_raw: v.performance.client_id_raw.clone(),
            feature_set: v.performance.feature_set.map(|f| f as i64),
            shred_version: v.performance.shred_version.map(|s| s as i32),
            gossip_port: v.gossip_port.map(|p| p as i32),
            rpc_public: v.rpc_public,
            pubsub_public: v.pubsub_public,
            activated_stake: v.activated_stake.into(),
            marinade_stake: v.marinade_stake.into(),
            foundation_stake: v.foundation_stake.into(),
            marinade_native_stake: v.marinade_native_stake.into(),
            institutional_stake: v.institutional_stake.into(),
            self_stake: v.self_stake.into(),
            superminority: v.superminority,
            stake_to_become_superminority: v.stake_to_become_superminority.into(),
            credits: v.performance.credits.into(),
            leader_slots: v.performance.leader_slots.into(),
            blocks_produced: v.performance.blocks_produced.into(),
            skip_rate: v.performance.skip_rate,
            uptime_pct: None,
            uptime: None,
            downtime: None,

            updated_at: None,
        }
    }
}

#[derive(Deserialize, Serialize, Debug, Clone, utoipa::ToSchema)]
pub struct ValidatorEpochStats {
    pub epoch: u64,
    pub epoch_start_at: Option<DateTime<Utc>>,
    pub epoch_end_at: Option<DateTime<Utc>>,
    pub commission_max_observed: Option<u8>,
    pub commission_min_observed: Option<u8>,
    pub commission_advertised: Option<u8>,
    pub commission_effective: Option<u8>,
    pub version: Option<String>,
    pub mev_commission_bps: Option<i32>,
    pub priority_commission_bps: Option<i32>,
    pub dc_asn: Option<i32>,
    pub dc_aso: Option<String>,
    pub dc_city: Option<String>,
    pub dc_country: Option<String>,
    /// Numeric Solana Foundation client id decoded from `client_id_raw`; null when the answering RPC rendered a name absent from our registry. `client_vendor` and `client_lineage` are derived from it, but stay null for an id the registry does not know.
    pub client_id: Option<u16>,
    /// Client name to display, never null: the Solana Foundation registry name of `client_id`, e.g. `Rakurai` or `Agave Bam`, and `Unknown` for a client the registry does not know. See `client_id_raw` for what the node actually reported.
    pub client_name: String,
    /// Client label to display, never null: the lineage, plus the vendor's modification of it when there is one, e.g. `Agave + JitoBAM` or `Frankendancer`. `Unknown` for a client the registry does not know.
    pub client_label: String,
    /// Who ships the binary, derived from `client_id`, collapsing a vendor's per-lineage ids (`HarmonicAgave` + `HarmonicFiredancer` -> `harmonic`).
    pub client_vendor: Option<String>,
    /// Which upstream codebase `client_id` is built from: `agave`, `firedancer`, `frankendancer` or `sig`.
    pub client_lineage: Option<String>,
    /// Literal `getClusterNodes.clientId`, rendered by the answering RPC's own table so the same node may report `Rakurai` or `Unknown(8)`; use `client_id` instead.
    pub client_id_raw: Option<String>,
    pub feature_set: Option<u32>,
    pub shred_version: Option<u16>,
    pub gossip_port: Option<u16>,
    pub rpc_public: Option<bool>,
    pub pubsub_public: Option<bool>,
    pub activated_stake: Decimal,
    pub marinade_stake: Decimal,
    pub foundation_stake: Decimal,
    pub marinade_native_stake: Decimal,
    pub institutional_stake: Decimal,
    pub self_stake: Decimal,
    pub superminority: bool,
    pub stake_to_become_superminority: Decimal,
    pub credits: u64,
    pub leader_slots: u64,
    pub blocks_produced: u64,
    pub skip_rate: f64,
    pub uptime_pct: Option<f64>,
    pub uptime: Option<u64>,
    pub downtime: Option<u64>,
    pub apr: Option<f64>,
    pub apy: Option<f64>,
    pub score: Option<f64>,
    pub rank_score: Option<usize>,
    pub rank_activated_stake: Option<usize>,
    pub rank_apy: Option<usize>,
}

#[derive(Deserialize, Serialize, Debug, Clone, utoipa::ToSchema)]
pub struct ValidatorRecord {
    pub identity: String,
    pub vote_account: String,
    pub start_epoch: u64,
    pub start_date: Option<DateTime<Utc>>,
    pub info_name: Option<String>,
    pub info_url: Option<String>,
    pub info_keybase: Option<String>,
    pub info_icon_url: Option<String>,
    pub node_ip: Option<String>,
    pub dc_coordinates_lat: Option<f64>,
    pub dc_coordinates_lon: Option<f64>,
    pub dc_continent: Option<String>,
    pub dc_country_iso: Option<String>,
    pub dc_country: Option<String>,
    pub dc_city: Option<String>,
    pub dc_full_city: Option<String>,
    pub dc_asn: Option<i32>,
    pub dc_aso: Option<String>,
    pub dcc_full_city: Option<f64>,
    pub dcc_asn: Option<f64>,
    pub dcc_aso: Option<f64>,
    pub dcc_country: Option<f64>,
    pub commission_max_observed: Option<i32>,
    pub commission_min_observed: Option<i32>,
    pub commission_advertised: Option<i32>,
    pub commission_effective: Option<i32>,
    pub commission_aggregated: Option<i32>,
    pub rugged_commission_occurrences: u64,
    pub rugged_commission: bool,
    pub rugged_commission_info: Vec<RugInfo>,
    pub version: Option<String>,
    /// Numeric Solana Foundation client id decoded from `client_id_raw`; null when the answering RPC rendered a name absent from our registry. `client_vendor` and `client_lineage` are derived from it, but stay null for an id the registry does not know.
    pub client_id: Option<u16>,
    /// Client name to display, never null: the Solana Foundation registry name of `client_id`, e.g. `Rakurai` or `Agave Bam`, and `Unknown` for a client the registry does not know. See `client_id_raw` for what the node actually reported.
    pub client_name: String,
    /// Client label to display, never null: the lineage, plus the vendor's modification of it when there is one, e.g. `Agave + JitoBAM` or `Frankendancer`. `Unknown` for a client the registry does not know.
    pub client_label: String,
    /// Who ships the binary, derived from `client_id`, collapsing a vendor's per-lineage ids (`HarmonicAgave` + `HarmonicFiredancer` -> `harmonic`).
    pub client_vendor: Option<String>,
    /// Which upstream codebase `client_id` is built from: `agave`, `firedancer`, `frankendancer` or `sig`.
    pub client_lineage: Option<String>,
    /// Literal `getClusterNodes.clientId`, rendered by the answering RPC's own table so the same node may report `Rakurai` or `Unknown(8)`; use `client_id` instead.
    pub client_id_raw: Option<String>,
    pub feature_set: Option<u32>,
    pub shred_version: Option<u16>,
    pub gossip_port: Option<u16>,
    pub rpc_public: Option<bool>,
    pub pubsub_public: Option<bool>,
    pub activated_stake: Decimal,
    pub marinade_stake: Decimal,
    pub foundation_stake: Decimal,
    pub marinade_native_stake: Decimal,
    pub institutional_stake: Decimal,
    pub self_stake: Decimal,
    pub superminority: bool,
    pub credits: u64,
    pub score: Option<f64>,
    pub warnings: Vec<ValidatorWarning>,
    pub epoch_stats: Vec<ValidatorEpochStats>,
    pub epochs_count: u64,
    pub has_last_epoch_stats: bool,
    pub avg_uptime_pct: Option<f64>,
    pub avg_apy: Option<f64>,
    pub unique_delegators: Option<u64>,
    pub avg_take_rate: Option<f64>,
    /// What the validator's current fee settings imply it keeps, as a fraction: its inflation, MEV and block-reward commissions weighted by the cluster reward mix. Forward-looking where `avg_take_rate` measures realized rewards, so two validators on the same fee settings read the same here regardless of size or block-production luck. Taking no commission anywhere floors it at the cluster block share, not at 0, because block rewards go wholly to the producer unless it also shares them through Jito's PriorityFeeDistribution. Null for a validator whose advertised commission is unknown.
    pub expected_take_rate: Option<f64>,
    /// Latest point of the apy-api 14-day rolling staker APY, a fraction like `avg_apy` but MEV-inclusive where `avg_apy` is inflation-only. Null for a validator apy-api has no rewards data for.
    pub net_apy: Option<f64>,
    pub incidents: Vec<IncidentRecord>,
    #[serde(default)]
    pub verified: bool,
    /// As listed by the validator-bonds `/validators/protected` endpoint, which owns the rule.
    #[serde(default)]
    pub protected: bool,
}

#[derive(Deserialize, Serialize, Debug, Clone, utoipa::ToSchema)]
pub struct UptimeRecord {
    pub epoch: u64,
    pub epoch_start_at: DateTime<Utc>,
    pub epoch_end_at: DateTime<Utc>,
    pub status: String,
    pub start_at: DateTime<Utc>,
    pub end_at: DateTime<Utc>,
}

/// A single downtime incident (one DOWN interval from the uptimes table).
#[derive(Deserialize, Serialize, Debug, Clone, utoipa::ToSchema)]
pub struct IncidentRecord {
    pub epoch: u64,
    pub start_at: DateTime<Utc>,
    pub end_at: DateTime<Utc>,
    pub downtime_seconds: u64,
}

#[derive(Deserialize, Serialize, Debug, Clone, utoipa::ToSchema)]
pub struct JitoMevRecord {
    pub epoch: Decimal,
    pub vote_account: String,
    pub mev_commission_bps: i32,
}

#[derive(Deserialize, Serialize, Debug, Clone, utoipa::ToSchema)]
pub struct JitoPriorityFeeRecord {
    pub epoch: Decimal,
    pub priority_commission_bps: i32,
    pub vote_account: String,
    pub total_lamports_transferred: u64,
}

#[derive(Deserialize, Serialize, Debug, Clone, utoipa::ToSchema)]
pub struct JitoRecord {
    pub epoch: Decimal,
    pub vote_account: String,
    pub mev_commission_bps: Option<i32>,
    pub priority_commission_bps: Option<i32>,
    pub priority_total_lamports_transferred: Option<u64>,
}

#[derive(Deserialize, Serialize, Debug, Clone, utoipa::ToSchema)]
pub struct ValidatorBlockRewardsRecord {
    pub epoch: u64,
    pub identity_account: String,
    pub vote_account: String,
    pub authorized_voter: String,
    pub amount: Decimal,
}

#[derive(Deserialize, Serialize, Debug, Clone, utoipa::ToSchema)]
pub struct VersionRecord {
    pub epoch: u64,
    pub version: Option<String>,
    /// Numeric Solana Foundation client id decoded from `client_id_raw`; null when the answering RPC rendered a name absent from our registry. `client_vendor` and `client_lineage` are derived from it, but stay null for an id the registry does not know.
    pub client_id: Option<u16>,
    /// Client name to display, never null: the Solana Foundation registry name of `client_id`, e.g. `Rakurai` or `Agave Bam`, and `Unknown` for a client the registry does not know. See `client_id_raw` for what the node actually reported.
    pub client_name: String,
    /// Client label to display, never null: the lineage, plus the vendor's modification of it when there is one, e.g. `Agave + JitoBAM` or `Frankendancer`. `Unknown` for a client the registry does not know.
    pub client_label: String,
    /// Who ships the binary, derived from `client_id`, collapsing a vendor's per-lineage ids (`HarmonicAgave` + `HarmonicFiredancer` -> `harmonic`).
    pub client_vendor: Option<String>,
    /// Which upstream codebase `client_id` is built from: `agave`, `firedancer`, `frankendancer` or `sig`.
    pub client_lineage: Option<String>,
    /// Literal `getClusterNodes.clientId`, rendered by the answering RPC's own table so the same node may report `Rakurai` or `Unknown(8)`; use `client_id` instead.
    pub client_id_raw: Option<String>,
    pub feature_set: Option<u32>,
    pub shred_version: Option<u16>,
    pub created_at: DateTime<Utc>,
}

#[derive(Deserialize, Serialize, Debug, Clone, utoipa::ToSchema)]
pub struct SettlementRecord {
    /// Raw upstream JSON tagged enum, e.g. `"Bidding"` or `{"ProtectedEvent":{...}}`.
    pub reason: String,
    /// Raw upstream JSON, e.g. `{"funder":"ValidatorBond"}`.
    pub meta: String,
    /// Settlement amount in lamports.
    pub amount: Decimal,
}

#[derive(Deserialize, Serialize, Debug, Clone, utoipa::ToSchema)]
pub struct PerformanceRecord {
    pub blocks_produced: u64,
    pub leader_slots: u64,
    pub skip_rate: f64,
    pub credits: u64,
}

#[derive(Deserialize, Serialize, Debug, Clone, utoipa::ToSchema)]
pub struct EventEpochRecord {
    pub epoch: u64,
    pub epoch_end_at: Option<DateTime<Utc>>,
    pub performance: Option<PerformanceRecord>,
    pub uptime_pct: Option<f64>,
    pub downtime: Option<u64>,
    pub settlements: Vec<SettlementRecord>,
}

#[derive(Deserialize, Serialize, Debug, Clone, utoipa::ToSchema)]
pub struct CommissionRecord {
    pub epoch: u64,
    pub epoch_start_at: DateTime<Utc>,
    pub epoch_end_at: DateTime<Utc>,
    pub epoch_slot: u64,
    pub commission: u8,
    pub created_at: DateTime<Utc>,
}

#[derive(Deserialize, Serialize, Debug, Clone, utoipa::ToSchema)]
pub struct RuggerRecord {
    pub epochs: Vec<u64>,
    pub occurrences: u64,
    pub observed_commissions: Vec<u64>,
    pub min_commissions: Vec<u64>,
    pub created_at: DateTime<Utc>,
}

#[derive(Deserialize, Serialize, Debug, Clone, utoipa::ToSchema)]
pub struct RugInfo {
    pub epoch: u64,
    pub after: u64,
    pub before: u64,
}

#[derive(Deserialize, Serialize, Debug, Clone, utoipa::ToSchema)]
pub enum ValidatorWarning {
    HighCommission,
    Superminority,
    LowUptime,
}

#[derive(Deserialize, Serialize, Debug, Clone, utoipa::ToSchema)]
pub struct DCConcentrationStats {
    pub epoch: u64,
    pub total_activated_stake: u64,
    pub dc_concentration_by_aso: HashMap<String, f64>,
    pub dc_stake_by_aso: HashMap<String, u64>,
    pub dc_concentration_by_asn: HashMap<String, f64>,
    pub dc_stake_by_asn: HashMap<String, u64>,
    pub dc_concentration_by_city: HashMap<String, f64>,
    pub dc_stake_by_city: HashMap<String, u64>,
    pub dc_concentration_by_country: HashMap<String, f64>,
    pub dc_stake_by_country: HashMap<String, u64>,
}

#[derive(Deserialize, Serialize, Debug, Clone, utoipa::ToSchema)]
pub struct BlockProductionStats {
    pub epoch: u64,
    pub blocks_produced: u64,
    pub leader_slots: u64,
    pub avg_skip_rate: f64,
}

#[derive(Deserialize, Serialize, Debug, Clone, utoipa::ToSchema)]
pub struct ClientDiversityStats {
    pub epoch: u64,
    pub total_activated_stake: u64,
    pub client_stake: HashMap<String, u64>,
    pub client_share: HashMap<String, f64>,
    pub client_validator_count: HashMap<String, u64>,
}

#[derive(Deserialize, Serialize, Debug, Clone, utoipa::ToSchema)]
pub struct ClientLineageStats {
    pub epoch: u64,
    pub total_activated_stake: u64,
    pub lineage_stake: HashMap<String, u64>,
    pub lineage_share: HashMap<String, f64>,
    pub lineage_validator_count: HashMap<String, u64>,
}

#[derive(Deserialize, Serialize, Debug, Clone, utoipa::ToSchema)]
pub struct FeatureSetStats {
    pub epoch: u64,
    pub total_activated_stake: u64,
    pub feature_set_stake: HashMap<String, u64>,
    pub feature_set_share: HashMap<String, f64>,
    pub feature_set_validator_count: HashMap<String, u64>,
}

#[derive(Deserialize, Serialize, Debug, Clone, utoipa::ToSchema)]
pub struct ClusterStats {
    pub block_production_stats: Vec<BlockProductionStats>,
    pub dc_concentration_stats: Vec<DCConcentrationStats>,
    pub client_diversity_stats: Vec<ClientDiversityStats>,
    pub client_lineage_stats: Vec<ClientLineageStats>,
    pub feature_set_stats: Vec<FeatureSetStats>,
}

#[derive(Deserialize, Serialize, Debug, Clone, utoipa::ToSchema)]
pub struct ValidatorsAggregated {
    pub epoch: u64,
    pub epoch_start_date: Option<DateTime<Utc>>,
    pub avg_marinade_score: Option<f64>,
    pub avg_apy: Option<f64>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct ValidatorAggregatedFlat {
    pub vote_account: String,
    pub minimum_stake: f64,
    pub avg_stake: f64,
    pub avg_dc_concentration: f64,
    pub avg_skip_rate: f64,
    pub avg_grace_skip_rate: f64,
    pub max_commission: u8,
    pub avg_adjusted_credits: f64,
    pub dc_aso: String,
    pub marinade_stake: f64,
    pub version: String,
    pub client_vendor: String,
    pub client_lineage: String,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct ValidatorScoringCsvRow {
    pub vote_account: String,
    pub score: f64,
    pub rank: i32,
    pub vemnde_votes: Decimal,
    pub msol_votes: Decimal,
    pub ui_hints: String,
    #[serde(deserialize_with = "bool_from_int")]
    pub eligible_stake_algo: bool,
    #[serde(deserialize_with = "bool_from_int")]
    pub eligible_stake_vemnde: bool,
    #[serde(deserialize_with = "bool_from_int")]
    pub eligible_stake_msol: bool,
    pub normalized_dc_concentration: f64,
    pub normalized_grace_skip_rate: f64,
    pub normalized_adjusted_credits: f64,
    pub avg_dc_concentration: f64,
    pub avg_grace_skip_rate: f64,
    pub avg_adjusted_credits: f64,
    pub rank_dc_concentration: i32,
    pub rank_grace_skip_rate: i32,
    pub rank_adjusted_credits: i32,
    pub target_stake_algo: Decimal,
    pub target_stake_vemnde: Decimal,
    pub target_stake_msol: Decimal,
}

#[derive(Deserialize, Serialize, Debug, Clone, utoipa::ToSchema)]
pub struct ValidatorScoreRecord {
    pub vote_account: String,
    pub score: f64,
    pub rank: i32,
    pub vemnde_votes: u64,
    pub msol_votes: u64,
    pub ui_hints: Vec<String>,
    pub component_scores: Vec<f64>,
    pub component_ranks: Vec<i32>,
    pub component_values: Vec<Option<String>>,
    pub eligible_stake_algo: bool,
    pub eligible_stake_vemnde: bool,
    pub eligible_stake_msol: bool,
    pub target_stake_algo: u64,
    pub target_stake_vemnde: u64,
    pub target_stake_msol: u64,
    pub scoring_run_id: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Deserialize, Serialize, Debug, Clone, utoipa::ToSchema)]
pub struct ValidatorScoreV2Record {
    pub vote_account: String,
    pub score: f64,
    pub rank: i32,
    pub vemnde_votes: f64,
    pub msol_votes: f64,
    pub ui_hints: Vec<String>,
    pub component_scores: Vec<f64>,
    pub eligible_stake_algo: bool,
    pub eligible_stake_vemnde: bool,
    pub eligible_stake_msol: bool,
    pub target_stake_algo: f64,
    pub target_stake_vemnde: f64,
    pub target_stake_msol: f64,
    pub scoring_run_id: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct ScoringRunRecord {
    pub scoring_run_id: Decimal,
    pub created_at: DateTime<Utc>,
    pub epoch: i32,
    pub components: Vec<String>,
    pub component_weights: Vec<f64>,
    pub ui_id: String,
}

#[derive(Deserialize, Serialize, Debug, Clone, Hash, Eq, PartialEq, utoipa::ToSchema)]
pub enum UnstakeHint {
    HighCommission,
    HighCommissionInPreviousEpoch,
    Blacklist,
    LowCredits,
}

#[derive(Deserialize, Serialize, Debug, Clone, utoipa::ToSchema)]
pub struct UnstakeHintRecord {
    pub vote_account: String,
    pub marinade_stake: f64,
    pub hints: Vec<UnstakeHint>,
}

#[derive(Deserialize, Serialize, Debug, Clone, utoipa::ToSchema)]
pub struct GlobalUnstakeHintRecord {
    pub vote_account: String,
    pub hints: Vec<UnstakeHint>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct BlacklistRecord {
    pub vote_account: String,
    pub code: String,
}

fn bool_from_int<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    match u8::deserialize(deserializer)? {
        0 => Ok(false),
        1 => Ok(true),
        other => Err(de::Error::invalid_value(
            Unexpected::Unsigned(other as u64),
            &"zero or one",
        )),
    }
}
