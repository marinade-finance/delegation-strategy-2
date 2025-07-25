use chrono::{DateTime, Utc};
use collect::validators::{ValidatorDataCenter, ValidatorSnapshot};
use collect::validators_jito::{
    MevTipDistributionValidatorSnapshot, PriorityFeeDistributionValidatorSnapshot,
};
use rust_decimal::prelude::*;
use serde::de::{self, Unexpected};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashMap;

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
    pub validator_commission: i32,
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
            validator_commission: v.validator_commission as i32,
            total_lamports_transferred: v.total_lamports_transferred.into(),
            epoch: v.epoch.into(),
            total_epoch_rewards: v.total_epoch_rewards.map(Into::into),
            claimed_epoch_rewards: v.claimed_epoch_rewards.map(Into::into),
            total_epoch_claimants: v.total_epoch_claimants.map(|v| v as i32),
            epoch_active_claimants: v.epoch_active_claimants.map(|v| v as i32),
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
        } = match v.data_center.clone() {
            Some(dc) => dc.clone(),
            _ => Default::default(),
        };

        Self {
            identity: v.identity.clone(),
            vote_account: v.vote_account.clone(),
            epoch: epoch.into(),
            info_name: v.info_name.clone(),
            info_url: v.info_url.clone(),
            info_keybase: v.info_keybase.clone(),

            node_ip: v.node_ip.clone(),
            dc_coordinates_lon: coordinates.map(|(_, lon)| lon),
            dc_coordinates_lat: coordinates.map(|(lat, _)| lat),
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
    pub commission_max_observed: Option<i32>,
    pub commission_min_observed: Option<i32>,
    pub commission_advertised: Option<i32>,
    pub commission_effective: Option<i32>,
    pub commission_aggregated: Option<i32>,
    pub rugged_commission_occurrences: u64,
    pub rugged_commission: bool,
    pub rugged_commission_info: Vec<RugInfo>,
    pub version: Option<String>,
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

#[derive(Deserialize, Serialize, Debug, Clone, utoipa::ToSchema)]
pub struct JitoMevRecord {
    pub vote_account: String,
    pub mev_commission_bps: i32,
    pub epoch: Decimal,
}

#[derive(Deserialize, Serialize, Debug, Clone, utoipa::ToSchema)]
pub struct JitoPriorityFeeRecord {
    pub vote_account: String,
    pub epoch: Decimal,
    pub validator_commission_bps: i32,
    pub total_lamports_transferred: u64,
}

#[derive(Deserialize, Serialize, Debug, Clone, utoipa::ToSchema)]
pub struct JitoRecord {
    pub vote_account: String,
    pub epoch: Decimal,
    pub mev_commission_bps: Option<i32>,
    pub validator_commission_bps: Option<i32>,
    pub total_lamports_transferred: Option<u64>,
}

#[derive(Deserialize, Serialize, Debug, Clone, utoipa::ToSchema)]
pub struct VersionRecord {
    pub epoch: u64,
    pub version: Option<String>,
    pub created_at: DateTime<Utc>,
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
}

#[derive(Deserialize, Serialize, Debug, Clone, utoipa::ToSchema)]
pub struct BlockProductionStats {
    pub epoch: u64,
    pub blocks_produced: u64,
    pub leader_slots: u64,
    pub avg_skip_rate: f64,
}

#[derive(Deserialize, Serialize, Debug, Clone, utoipa::ToSchema)]
pub struct ClusterStats {
    pub block_production_stats: Vec<BlockProductionStats>,
    pub dc_concentration_stats: Vec<DCConcentrationStats>,
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
