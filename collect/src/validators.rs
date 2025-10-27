use crate::common::*;
use crate::marinade_service::*;
use crate::solana_service::solana_client;
use crate::solana_service::*;
use crate::validators_performance::{validators_performance, ValidatorPerformance};
use crate::whois_service::*;
use chrono::DateTime;
use chrono::Utc;
use log::info;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use solana_sdk::clock::Epoch;
use structopt::StructOpt;

#[derive(Debug, StructOpt)]
pub struct ValidatorsOptions {
    #[structopt(long = "whois", help = "Base URL for whois API.")]
    whois: Option<String>,

    #[structopt(
        long = "whois-bearer-token",
        help = "Bearer token to be used to fetch data from whois API"
    )]
    whois_bearer_token: Option<String>,

    #[structopt(
        long = "bonds-url",
        default_value = "https://validator-bonds-api.marinade.finance/bonds/bidding"
    )]
    pub bonds_url: String,

    #[structopt(
        long = "rpc-attempts",
        help = "How many times to retry the operation.",
        default_value = "10"
    )]
    rpc_attempts: usize,

    #[structopt(long = "epoch", help = "Which epoch to use for epoch-based metrics.")]
    epoch: Option<Epoch>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct ValidatorInfo {
    pub name: Option<String>,
    pub url: Option<String>,
    pub details: Option<String>,
    pub keybase: Option<String>,
    pub icon_url: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ValidatorBond {
    pub pubkey: String,
    pub vote_account: String,
    pub authority: String,
    pub cpmpe: Decimal,
    pub max_stake_wanted: Decimal,
    pub updated_at: DateTime<Utc>,
    pub epoch: u64,
    pub funded_amount: Decimal,
    pub effective_amount: Decimal,
    pub remaining_witdraw_request_amount: Decimal,
    pub remainining_settlement_claim_amount: Decimal,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct BondsResponse {
    pub bonds: Vec<ValidatorBond>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct ValidatorDataCenter {
    pub coordinates: Option<(f64, f64)>, // lon, lat
    pub continent: Option<String>,
    pub country_iso: Option<String>,
    pub country: Option<String>,
    pub city: Option<String>,
    pub asn: Option<u32>,
    pub aso: Option<String>,
}

impl ValidatorDataCenter {
    fn new(ip_info: IpInfo) -> ValidatorDataCenter {
        ValidatorDataCenter {
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
    pub identity: String,
    pub vote_account: String,
    pub node_ip: Option<String>,
    pub info_name: Option<String>,
    pub info_url: Option<String>,
    pub info_details: Option<String>,
    pub info_keybase: Option<String>,
    pub info_icon_url: Option<String>,
    pub data_center: Option<ValidatorDataCenter>,
    pub activated_stake: u64,
    pub foundation_stake: u64,
    pub self_stake: u64,
    pub marinade_stake: u64,
    pub marinade_native_stake: u64,
    pub institutional_stake: u64,
    pub superminority: bool,
    pub stake_to_become_superminority: u64,
    pub performance: ValidatorPerformance,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub epoch: Epoch,
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
    let stake_history = get_stake_history(&client)?;
    let minimum_superminority_stake = get_minimum_superminority_stake(&vote_accounts);
    let marinade_stake = get_marinade_stakes(&client, epoch, &stake_history)?;
    let foundation_stake = get_foundation_stakes(&client, epoch, &stake_history)?;
    let institutional_stake = get_institutional_stakes(&client, epoch, &stake_history)?;
    let marinade_native_stake = get_marinade_native_stakes(&client, epoch, &stake_history)?;
    let self_stake = get_self_stake(
        &client,
        epoch,
        &stake_history,
        &options.bonds_url,
        options.rpc_attempts,
    )?;
    let validators_info = get_validators_info(&client)?;
    let node_ips = get_cluster_nodes_ips(&client)?;

    info!("Self stake: {}", self_stake.values().sum::<u64>());
    info!(
        "Foundation stake: {}",
        foundation_stake.values().sum::<u64>()
    );

    let data_centers = match options.whois {
        Some(whois) => get_data_centers(
            WhoisClient::new(whois, options.whois_bearer_token),
            node_ips.clone(),
        )?,
        _ => Default::default(),
    };

    let performance = validators_performance(&client, epoch, &vote_accounts)?;

    for vote_account in vote_accounts
        .current
        .iter()
        .chain(vote_accounts.delinquent.iter())
    {
        let vote_pubkey = vote_account.vote_pubkey.clone();
        let identity = vote_account.node_pubkey.clone();

        let ValidatorInfo {
            name,
            url,
            keybase,
            details,
            icon_url,
        } = validators_info
            .get(&identity)
            .cloned()
            .unwrap_or_else(|| Default::default());

        validators.push(ValidatorSnapshot {
            vote_account: vote_pubkey.clone(),
            identity: identity.clone(),
            node_ip: data_centers.get(&identity).map(|(ip, _)| ip.clone()),
            data_center: data_centers
                .get(&identity)
                .map_or(None, |(_ip, data_center)| {
                    Some(ValidatorDataCenter::new(data_center.clone()))
                }),

            info_url: url,
            info_name: name,
            info_keybase: keybase,
            info_details: details,
            info_icon_url: icon_url,

            activated_stake: vote_account.activated_stake,
            marinade_stake: *marinade_stake.get(&vote_pubkey).unwrap_or(&0),
            foundation_stake: *foundation_stake.get(&vote_pubkey).unwrap_or(&0),
            self_stake: *self_stake.get(&vote_pubkey).unwrap_or(&0),
            marinade_native_stake: *marinade_native_stake.get(&vote_pubkey).unwrap_or(&0),
            institutional_stake: *institutional_stake.get(&vote_pubkey).unwrap_or(&0),
            superminority: minimum_superminority_stake <= vote_account.activated_stake,
            stake_to_become_superminority: minimum_superminority_stake
                .saturating_sub(vote_account.activated_stake),

            performance: performance.get(&vote_pubkey).unwrap().clone(),
        });
    }

    serde_yaml::to_writer(
        std::io::stdout(),
        &Snapshot {
            epoch,
            created_at: created_at.to_string(),
            validators,
        },
    )?;

    Ok(())
}
