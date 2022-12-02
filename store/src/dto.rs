use chrono::{DateTime, Utc};
use collect::validators::{ValidatorDataCenter, ValidatorSnapshot};
use rust_decimal::prelude::*;
use serde::Serialize;
use std::collections::HashMap;

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

#[derive(Serialize, Debug, Clone)]
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
}

#[derive(Serialize, Debug, Clone)]
pub struct ValidatorRecord {
    pub identity: String,
    pub vote_account: String,
    pub info_name: Option<String>,
    pub info_url: Option<String>,
    pub info_keybase: Option<String>,
    pub node_ip: String,
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
    pub credits: u64,
    pub marinade_score: u64,

    pub epoch_stats: Vec<ValidatorEpochStats>,
}

#[derive(Serialize, Debug)]
pub struct UptimeRecord {
    pub epoch: u64,
    pub status: String,
    pub start_at: DateTime<Utc>,
    pub end_at: DateTime<Utc>,
}

#[derive(Serialize, Debug)]
pub struct VersionRecord {
    pub epoch: u64,
    pub version: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Serialize, Debug)]
pub struct CommissionRecord {
    pub epoch: u64,
    pub commission: u8,
    pub created_at: DateTime<Utc>,
}
