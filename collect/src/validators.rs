use crate::common::*;
use crate::marinade_service::*;
use crate::solana_service::solana_client;
use crate::solana_service::*;
use crate::whois_service::*;
use log::info;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use serde_yaml::{self};
use solana_sdk::clock::Epoch;
use solana_sdk::pubkey::Pubkey;
use std::collections::{HashMap, HashSet};
use structopt::StructOpt;

#[derive(Debug, StructOpt)]
pub struct ValidatorsOptions {
    #[structopt(long = "gauge-meister", help = "Gauge meister of the vote gauges.")]
    gauge_meister: Option<Pubkey>,

    #[structopt(long = "escrow-relocker", help = "Escrow relocker program address.")]
    escrow_relocker: Option<Pubkey>,

    #[structopt(
        long = "with-validator-info",
        help = "Whether to get published details."
    )]
    with_validator_info: bool,

    #[structopt(long = "with-rewards", help = "Whether to calculate APY and rewards.")]
    with_rewards: bool,

    #[structopt(long = "whois", help = "Base URL for whois API.")]
    whois: Option<String>,

    #[structopt(
        long = "whois-bearer-token",
        help = "Bearer token to be used to fetch data from whois API"
    )]
    whois_bearer_token: Option<String>,

    #[structopt(long = "epoch", help = "Which epoch to use for epoch-based metrics.")]
    epoch: Option<Epoch>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ValidatorInfo {
    pub name: Option<String>,
    pub url: Option<String>,
    pub details: Option<String>,
    pub keybase: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ValidatorStake {
    pub activated_stake: u64,
    pub marinade_stake: u64,
    pub decentralizer_stake: u64,
    pub superminority: bool,
    pub stake_to_become_superminority: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ValidatorRewards {
    pub commission: Option<u8>,
    pub apy: Option<f64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ValidatorPerformance {
    pub credits: u64,
    pub leader_slots: usize,
    pub blocks_produced: usize,
    pub skip_rate: f64,
    pub delinquent: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ValidatorDataCenter {
    pub ip: String,
    pub coordinates: Option<(f64, f64)>, // lon, lat
    pub continent: Option<String>,
    pub country_iso: Option<String>,
    pub country: Option<String>,
    pub city: Option<String>,
    pub asn: Option<u32>,
    pub aso: Option<String>,
}

impl ValidatorDataCenter {
    fn new(ip: String, ip_info: IpInfo) -> ValidatorDataCenter {
        ValidatorDataCenter {
            ip,
            coordinates: ip_info.coordinates.map_or(None, |c| Some((c.lon, c.lat))),
            continent: ip_info.continent,
            country_iso: ip_info.country_iso,
            country: ip_info.country,
            city: ip_info.city,
            asn: ip_info.asn,
            aso: ip_info.aso,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ValidatorSnapshot {
    pub vote_account: String,
    pub identity: String,
    pub commission: i32,
    pub version: String,
    pub mnde_votes: Option<u64>,
    pub data_center: Option<ValidatorDataCenter>,
    pub info: Option<ValidatorInfo>,
    pub stake: Option<ValidatorStake>,
    pub performance: ValidatorPerformance,
    pub rewards: Option<ValidatorRewards>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub epoch: Epoch,
    pub epoch_slot: u64,
    pub created_at: String,
    pub validators: Vec<ValidatorSnapshot>,
}

pub fn collect_validators_info(
    common_params: CommonParams,
    options: ValidatorsOptions,
) -> anyhow::Result<()> {
    info!("Collecting snaphost of validators: {:?}", &options);
    let client = solana_client(common_params.rpc_url, common_params.commitment);

    let created_at = chrono::Utc::now();
    let current_epoch_info = client.get_epoch_info()?;
    info!("Current epoch: {:?}", current_epoch_info);

    let epoch = options.epoch.unwrap_or(current_epoch_info.epoch);
    info!("Looking at epoch: {}", epoch);

    let mut validators: Vec<ValidatorSnapshot> = vec![];

    let vote_accounts = client.get_vote_accounts()?;
    info!(
        "Total vote accounts found: {}",
        vote_accounts.current.len() + vote_accounts.delinquent.len()
    );
    info!(
        "Delinquent vote accounts found: {}",
        vote_accounts.delinquent.len()
    );

    let (total_activated_live_stake, total_activated_delinquent_stake) =
        get_total_activated_stake(&vote_accounts);
    info!(
        "Total activated stake: {}",
        total_activated_live_stake + total_activated_delinquent_stake
    );
    info!(
        "Delinquent activated stake: {}",
        total_activated_delinquent_stake
    );

    let minimum_superminority_stake = get_minimum_superminority_stake(&vote_accounts);
    let marinade_stake = get_marinade_stakes(&client)?;
    let decentralizer_stake = get_decentralizer_stakes(&client)?;
    let delinquent: HashSet<_> = vote_accounts
        .delinquent
        .iter()
        .map(|v| v.node_pubkey.clone())
        .collect();
    let production_by_validator = get_block_production_by_validator(&client, epoch)?;
    let node_versions = get_cluster_nodes_versions(&client)?;
    let credits = get_credits(&client, epoch)?;
    let validators_info = if options.with_validator_info {
        get_validators_info(&client)?
    } else {
        Default::default()
    };
    let mnde_votes = if let (Some(escrow_relocker), Some(gauge_meister)) =
        (options.escrow_relocker, options.gauge_meister)
    {
        Some(get_mnde_votes(&client, escrow_relocker, gauge_meister)?)
    } else {
        None
    };
    let node_ips = get_cluster_nodes_ips(&client)?;

    let apy = if options.with_rewards {
        get_apy(&client, &vote_accounts, &credits)?
    } else {
        Default::default()
    };

    let data_centers = match options.whois {
        Some(whois) => get_data_centers(
            WhoisClient::new(whois, options.whois_bearer_token),
            node_ips,
        )?,
        _ => Default::default(),
    };

    let commission_from_rewards = if options.with_rewards {
        get_commission_from_inflation_rewards(&client, &vote_accounts, Some(epoch))?
    } else {
        Default::default()
    };

    for vote_account in vote_accounts
        .current
        .iter()
        .chain(vote_accounts.delinquent.iter())
    {
        let vote_pubkey = vote_account.vote_pubkey.clone();
        let identity = vote_account.node_pubkey.clone();
        let (leader_slots, blocks_produced) =
            *production_by_validator.get(&identity).unwrap_or(&(0, 0));

        let rewards = if options.with_rewards {
            Some(ValidatorRewards {
                commission: commission_from_rewards.get(&vote_pubkey).cloned(),
                apy: apy.get(&identity).cloned(),
            })
        } else {
            Default::default()
        };

        validators.push(ValidatorSnapshot {
            vote_account: vote_pubkey.clone(),
            identity: identity.clone(),
            version: node_versions
                .get(&identity)
                .cloned()
                .unwrap_or("unknown".to_string()),
            commission: vote_account.commission as i32,
            mnde_votes: mnde_votes
                .clone()
                .map_or(None, |v| Some(*v.get(&vote_pubkey).unwrap_or(&0))),
            data_center: data_centers
                .get(&identity)
                .map_or(None, |(ip, data_center)| {
                    Some(ValidatorDataCenter::new(ip.clone(), data_center.clone()))
                }),
            info: validators_info.get(&identity).cloned(),

            stake: Some(ValidatorStake {
                activated_stake: vote_account.activated_stake,
                marinade_stake: *marinade_stake.get(&vote_pubkey).unwrap_or(&0),
                decentralizer_stake: *decentralizer_stake.get(&vote_pubkey).unwrap_or(&0),

                superminority: minimum_superminority_stake <= vote_account.activated_stake,
                stake_to_become_superminority: minimum_superminority_stake
                    .saturating_sub(vote_account.activated_stake),
            }),

            performance: ValidatorPerformance {
                credits: *credits.get(&identity).unwrap_or(&0),
                leader_slots,
                blocks_produced,
                skip_rate: if leader_slots == 0 {
                    0f64
                } else {
                    1f64 - (blocks_produced as f64 / leader_slots as f64)
                },
                delinquent: delinquent.contains(&identity),
            },

            rewards,
        });
    }

    serde_yaml::to_writer(
        std::io::stdout(),
        &Snapshot {
            epoch,
            epoch_slot: current_epoch_info.slot_index,
            created_at: created_at.to_string(),
            validators,
        },
    )?;

    Ok(())
}
