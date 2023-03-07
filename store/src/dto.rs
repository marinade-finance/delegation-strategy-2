use chrono::{DateTime, Utc};
use collect::validators::{ValidatorDataCenter, ValidatorSnapshot};
use collect::validators_mev::ValidatorMEVSnapshot;
use rust_decimal::prelude::*;
use serde::de::{Unexpected, self};
use serde::{Deserialize, Serialize, Deserializer};
use std::collections::HashMap;

pub struct ValidatorMEVInfo {
    pub vote_account: String,
    pub mev_commission: i32,
    pub epoch: i32,
    pub total_epoch_rewards: Decimal,
    pub claimed_epoch_rewards: Decimal,
    pub total_epoch_claimants: i32,
    pub epoch_active_claimants: i32,
}

impl ValidatorMEVInfo {
    pub fn new_from_snapshot(v: &ValidatorMEVSnapshot) -> Self {
        Self {
            vote_account: v.vote_account.clone(),
            mev_commission: (v.mev_commission as i32),
            epoch: (v.epoch as i32),
            total_epoch_rewards: v.total_epoch_rewards.into(),
            claimed_epoch_rewards: v.claimed_epoch_rewards.into(),
            total_epoch_claimants: (v.total_epoch_claimants as i32),
            epoch_active_claimants: (v.epoch_active_claimants as i32),
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
    pub mnde_votes: Option<Decimal>,
    pub activated_stake: Decimal,
    pub marinade_stake: Decimal,
    pub decentralizer_stake: Decimal,
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
            mnde_votes: v.mnde_votes.map(|mnde_votes| mnde_votes.into()),
            activated_stake: v.activated_stake.into(),
            marinade_stake: v.marinade_stake.into(),
            decentralizer_stake: v.decentralizer_stake.into(),
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

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct ValidatorEpochStats {
    pub epoch: u64,
    pub commission_max_observed: Option<u8>,
    pub commission_min_observed: Option<u8>,
    pub commission_advertised: Option<u8>,
    pub commission_effective: Option<u8>,
    pub version: Option<String>,
    pub mnde_votes: Option<u64>,
    pub activated_stake: u64,
    pub marinade_stake: u64,
    pub decentralizer_stake: u64,
    pub superminority: bool,
    pub stake_to_become_superminority: u64,
    pub credits: u64,
    pub leader_slots: u64,
    pub blocks_produced: u64,
    pub skip_rate: f64,
    pub uptime_pct: Option<f64>,
    pub uptime: Option<u64>,
    pub downtime: Option<u64>,
    pub apr: Option<f64>,
    pub apy: Option<f64>,
    pub marinade_score: u64,
    pub rank_marinade_score: Option<usize>,
    pub rank_activated_stake: Option<usize>,
    pub rank_apy: Option<usize>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct ValidatorRecord {
    pub identity: String,
    pub vote_account: String,
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
    pub version: Option<String>,
    pub mnde_votes: Option<Decimal>,
    pub activated_stake: Decimal,
    pub marinade_stake: Decimal,
    pub decentralizer_stake: Decimal,
    pub superminority: bool,
    pub credits: u64,
    pub marinade_score: u64,
    pub warnings: Vec<WarningRecord>,

    pub epoch_stats: Vec<ValidatorEpochStats>,

    pub epochs_count: u64,

    pub avg_uptime_pct: Option<f64>,
    pub avg_apy: Option<f64>,
}

#[derive(Serialize, Debug, Clone)]
pub struct UptimeRecord {
    pub epoch: u64,
    pub status: String,
    pub start_at: DateTime<Utc>,
    pub end_at: DateTime<Utc>,
}

#[derive(Serialize, Debug, Clone)]
pub struct VersionRecord {
    pub epoch: u64,
    pub version: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Serialize, Debug, Clone)]
pub struct CommissionRecord {
    pub epoch: u64,
    pub epoch_slot: u64,
    pub commission: u8,
    pub created_at: DateTime<Utc>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct WarningRecord {
    pub code: String,
    pub message: String,
    pub details: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
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

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct BlockProductionStats {
    pub epoch: u64,
    pub blocks_produced: u64,
    pub leader_slots: u64,
    pub avg_skip_rate: f64,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct ClusterStats {
    pub block_production_stats: Vec<BlockProductionStats>,
    pub dc_concentration_stats: Vec<DCConcentrationStats>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct ValidatorsAggregated {
    pub epoch: u64,
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
    pub mnde_votes: u64,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct ValidatorScoringCsvRow {
    pub vote_account: String,
    pub score: f64,
    pub rank: i32,
    pub ui_hints: String,
    #[serde(deserialize_with = "bool_from_int")]
    pub eligible_stake_algo: bool,
    #[serde(deserialize_with = "bool_from_int")]
    pub eligible_stake_mnde: bool,
    #[serde(deserialize_with = "bool_from_int")]
    pub eligible_stake_msol: bool,
    pub normalized_dc_concentration: f64,
    pub normalized_grace_skip_rate: f64,
    pub normalized_adjusted_credits: f64,
    pub target_stake_algo: Decimal,
    pub target_stake_mnde: Decimal,
    pub target_stake_msol: Decimal,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct ValidatorScoreRecord {
    pub vote_account: String,
    pub score: f64,
    pub rank: i32,
    pub ui_hints: Vec<String>,
    pub eligible_stake_algo: bool,
    pub eligible_stake_mnde: bool,
    pub eligible_stake_msol: bool,
    pub target_stake_algo: u64,
    pub target_stake_mnde: u64,
    pub target_stake_msol: u64,
    pub scoring_run_id: i64,
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
