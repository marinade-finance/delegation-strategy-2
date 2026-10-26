use crate::common::*;
use crate::marinade_service::*;
use crate::solana_service::solana_client_with_timeout;
use crate::solana_service::*;
use crate::validators_performance::{validators_performance, ValidatorPerformance};
use crate::vat_admission::{
    compute_vat_admission, get_vat_params, VatAdmissionInput, VatAdmissionReason,
};
use crate::whois_service::*;
use chrono::DateTime;
use chrono::Utc;
use clap::Parser;
use log::{info, warn};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use solana_rpc_client_api::response::RpcVoteAccountStatus;
use solana_sdk::clock::Epoch;
use std::collections::HashMap;
use std::time::Duration;

#[derive(Debug, Parser)]
pub struct ValidatorsParams {
    #[arg(long = "whois", help = "Base URL for whois API.")]
    whois: Option<String>,

    #[arg(
        long = "whois-bearer-token",
        help = "Bearer token to be used to fetch data from whois API"
    )]
    whois_bearer_token: Option<BearerToken>,

    #[arg(
        long = "bonds-url",
        default_value = "https://validator-bonds-api.marinade.finance/bonds/bidding"
    )]
    pub bonds_url: String,

    #[arg(
        long = "allow-zero-funded-bonds",
        help = "When set (or ALLOW_ZERO_FUNDED_BONDS=true), if all bonds have funded_amount == 0, log a warning instead of failing."
    )]
    pub allow_zero_funded_bonds: bool,

    #[arg(
        long = "allow-skipped-stake-pages",
        help = "When set, a stake account page still too large for one RPC response at the finest split is left out of the totals with a warning instead of failing."
    )]
    pub allow_skipped_stake_pages: bool,

    #[arg(
        long = "rpc-attempts",
        help = "How many times to retry the operation.",
        default_value = "10"
    )]
    rpc_attempts: usize,

    #[arg(
        long = "rpc-timeout",
        help = "How long to wait for RPC response (seconds).",
        default_value = "300"
    )]
    rpc_timeout: u64,

    #[arg(long = "epoch", help = "Which epoch to use for epoch-based metrics.")]
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
            coordinates: ip_info.coordinates.map(|c| (c.lon, c.lat)),
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
    #[serde(default)]
    pub gossip_port: Option<u16>,
    #[serde(default)]
    pub rpc_public: Option<bool>,
    #[serde(default)]
    pub pubsub_public: Option<bool>,
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
    // Absent in a snapshot an older binary wrote.
    #[serde(default)]
    pub activating_stake: Option<u64>,
    #[serde(default)]
    pub deactivating_stake: Option<u64>,
    #[serde(default)]
    pub direct_stake: Option<u64>,
    #[serde(default)]
    pub direct_activating_stake: Option<u64>,
    #[serde(default)]
    pub direct_deactivating_stake: Option<u64>,
    pub superminority: bool,
    pub stake_to_become_superminority: u64,
    pub performance: ValidatorPerformance,
    // Absent both in a pre-deploy snapshot and per validator on a pre-v4 state; store reads both.
    #[serde(default)]
    pub inflation_rewards_collector: Option<String>,
    #[serde(default)]
    pub block_revenue_collector: Option<String>,
    #[serde(default)]
    pub inflation_rewards_commission_bps: Option<u16>,
    #[serde(default)]
    pub inflation_rewards_commission_bps_is_v4: Option<bool>,
    #[serde(default)]
    pub block_revenue_commission_bps: Option<u16>,
    #[serde(default)]
    pub pending_delegator_rewards: Option<u64>,
    #[serde(default)]
    pub inflation_rewards_collector_owner: Option<String>,
    #[serde(default)]
    pub inflation_rewards_collector_lamports: Option<u64>,
    #[serde(default)]
    pub inflation_rewards_collector_healthy: Option<bool>,
    #[serde(default)]
    pub block_revenue_collector_owner: Option<String>,
    #[serde(default)]
    pub block_revenue_collector_lamports: Option<u64>,
    #[serde(default)]
    pub block_revenue_collector_healthy: Option<bool>,
    #[serde(default)]
    pub bls_pubkey: Option<String>,
    #[serde(default)]
    pub vote_account_lamports: Option<u64>,
    // None where the vote state did not parse.
    #[serde(default)]
    pub vat_admitted: Option<bool>,
    #[serde(default)]
    pub vat_admission_reason: Option<VatAdmissionReason>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub epoch: Epoch,
    pub created_at: String,
    pub validators: Vec<ValidatorSnapshot>,
}

// Only accounts with a known vote state version and balance take part, so the rest get no verdict.
fn vat_admission_inputs(
    vote_accounts: &RpcVoteAccountStatus,
    vote_account_states: &HashMap<String, VoteStateFields>,
) -> HashMap<String, VatAdmissionInput> {
    vote_accounts
        .current
        .iter()
        .chain(vote_accounts.delinquent.iter())
        .filter_map(|vote_account| {
            let state = vote_account_states.get(&vote_account.vote_pubkey)?;
            state.inflation_rewards_commission_bps_is_v4?;
            Some((
                vote_account.vote_pubkey.clone(),
                VatAdmissionInput {
                    stake: vote_account.activated_stake,
                    lamports: state.lamports?,
                    pending_delegator_rewards: state.pending_delegator_rewards.unwrap_or(0),
                    has_bls_pubkey: state.bls_pubkey.is_some(),
                },
            ))
        })
        .collect()
}

pub fn collect_validators_info(
    common_params: CommonParams,
    validator_params: ValidatorsParams,
) -> anyhow::Result<()> {
    info!("Collecting snaphost of validators: {:?}", &validator_params);
    let client = solana_client_with_timeout(
        common_params.rpc_url,
        Duration::from_secs(validator_params.rpc_timeout),
        common_params.commitment,
    );

    let created_at = chrono::Utc::now();
    let current_epoch_info = client.get_epoch_info()?;
    info!("Current epoch: {current_epoch_info:?}");

    let epoch = validator_params.epoch.unwrap_or(current_epoch_info.epoch);
    info!("Looking at epoch: {epoch}");

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
    info!("Delinquent activated stake: {total_activated_delinquent_stake}");
    let stake_history = get_stake_history(&client)?;
    let minimum_superminority_stake = get_minimum_superminority_stake(&vote_accounts);
    let marinade_stake = get_marinade_stakes(&client, epoch, &stake_history)?;
    let foundation_stake = get_foundation_stakes(&client, epoch, &stake_history)?;
    let institutional_stake = get_institutional_stakes(&client, epoch, &stake_history)?;
    let direct_stake = get_direct_stakes(&client, epoch, &stake_history)?;
    let marinade_native_stake = get_marinade_native_stakes(&client, epoch, &stake_history)?;
    let allow_zero_funded_bonds = validator_params.allow_zero_funded_bonds
        || std::env::var("ALLOW_ZERO_FUNDED_BONDS")
            .ok()
            .and_then(|v| v.parse::<bool>().ok())
            .unwrap_or(false);
    // One vote-program scan feeds both the withdraw authorities and the vote state below.
    let vote_account_states = get_vote_account_states(&client)?;
    let collector_health = get_collector_health(&client, &vote_account_states)?;
    let vat_admission = compute_vat_admission(
        &vat_admission_inputs(&vote_accounts, &vote_account_states),
        get_vat_params(&client, epoch)?,
    );
    let stake_account_totals = get_stake_account_totals(
        &client,
        epoch,
        &stake_history,
        &validator_params.bonds_url,
        allow_zero_funded_bonds,
        validator_params.rpc_attempts,
        Duration::from_secs(validator_params.rpc_timeout),
        validator_params.allow_skipped_stake_pages,
        &vote_account_states,
    )?;
    let validators_info = get_validators_info(&client)?;
    let node_info = get_cluster_nodes_info(&client)?;

    info!(
        "Self stake: {}",
        stake_account_totals
            .values()
            .map(|t| t.self_stake)
            .sum::<u64>()
    );
    info!(
        "Pending stake: {} activating, {} deactivating",
        stake_account_totals
            .values()
            .map(|t| t.activating)
            .sum::<u64>(),
        stake_account_totals
            .values()
            .map(|t| t.deactivating)
            .sum::<u64>()
    );
    info!(
        "Foundation stake: {}",
        foundation_stake.values().sum::<u64>()
    );
    info!(
        "Direct stake: {} effective, {} activating, {} deactivating over {} validators",
        direct_stake.values().map(|s| s.effective).sum::<u64>(),
        direct_stake.values().map(|s| s.activating).sum::<u64>(),
        direct_stake.values().map(|s| s.deactivating).sum::<u64>(),
        direct_stake.len()
    );
    if direct_stake.is_empty() {
        warn!("Direct stake scan found nothing, every validator stores 0 direct stake");
    }

    let data_centers = match validator_params.whois {
        Some(whois) => {
            let node_ips: HashMap<String, String> = node_info
                .iter()
                .filter_map(|(identity, n)| n.ip.clone().map(|ip| (identity.clone(), ip)))
                .collect();
            get_data_centers(
                WhoisClient::new(whois, validator_params.whois_bearer_token)?,
                node_ips,
            )?
        }
        _ => Default::default(),
    };

    let performance = validators_performance(
        &client,
        epoch,
        current_epoch_info.epoch,
        &vote_accounts,
        validator_params.rpc_attempts,
        &node_info,
    )?;

    let rpc_epoch = client.get_epoch_info()?.epoch;
    if rpc_epoch != epoch {
        anyhow::bail!(
            "Snapshot is labelled epoch {epoch} but the RPC was at epoch {rpc_epoch} after reading the accounts, their state may belong to either epoch"
        );
    }

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
            .unwrap_or_else(Default::default);

        let node = node_info.get(&identity);
        // An account the scan could not parse leaves the v4 fields null, never a made-up default.
        let vote_state = vote_account_states.get(&vote_pubkey);
        let stake_totals = stake_account_totals
            .get(&vote_pubkey)
            .copied()
            .unwrap_or_default();
        let direct = direct_stake.get(&vote_pubkey).copied().unwrap_or_default();
        let (inflation_health, block_revenue_health) =
            collector_health_of(&vote_pubkey, vote_state, &collector_health);

        validators.push(ValidatorSnapshot {
            vote_account: vote_pubkey.clone(),
            identity: identity.clone(),
            node_ip: data_centers
                .get(&identity)
                .map(|(ip, _)| ip.clone())
                .or_else(|| node.and_then(|n| n.ip.clone())),
            gossip_port: node.and_then(|n| n.gossip_port),
            rpc_public: node.map(|n| n.rpc_public),
            pubsub_public: node.map(|n| n.pubsub_public),
            data_center: data_centers
                .get(&identity)
                .map(|(_ip, data_center)| ValidatorDataCenter::new(data_center.clone())),

            info_url: url,
            info_name: name,
            info_keybase: keybase,
            info_details: details,
            info_icon_url: icon_url,

            activated_stake: vote_account.activated_stake,
            marinade_stake: *marinade_stake.get(&vote_pubkey).unwrap_or(&0),
            foundation_stake: *foundation_stake.get(&vote_pubkey).unwrap_or(&0),
            self_stake: stake_totals.self_stake,
            marinade_native_stake: *marinade_native_stake.get(&vote_pubkey).unwrap_or(&0),
            institutional_stake: *institutional_stake.get(&vote_pubkey).unwrap_or(&0),
            activating_stake: Some(stake_totals.activating),
            deactivating_stake: Some(stake_totals.deactivating),
            direct_stake: Some(direct.effective),
            direct_activating_stake: Some(direct.activating),
            direct_deactivating_stake: Some(direct.deactivating),
            superminority: minimum_superminority_stake <= vote_account.activated_stake,
            stake_to_become_superminority: minimum_superminority_stake
                .saturating_sub(vote_account.activated_stake),

            performance: performance.get(&vote_pubkey).unwrap().clone(),

            inflation_rewards_collector: vote_state
                .and_then(|state| state.inflation_rewards_collector)
                .map(|collector| collector.to_string()),
            block_revenue_collector: vote_state
                .and_then(|state| state.block_revenue_collector)
                .map(|collector| collector.to_string()),
            inflation_rewards_commission_bps: vote_state
                .and_then(|state| state.inflation_rewards_commission_bps),
            inflation_rewards_commission_bps_is_v4: vote_state
                .and_then(|state| state.inflation_rewards_commission_bps_is_v4),
            block_revenue_commission_bps: vote_state
                .and_then(|state| state.block_revenue_commission_bps),
            pending_delegator_rewards: vote_state.and_then(|state| state.pending_delegator_rewards),
            inflation_rewards_collector_owner: inflation_health
                .and_then(|h| h.owner)
                .map(|owner| owner.to_string()),
            inflation_rewards_collector_lamports: inflation_health.map(|h| h.lamports),
            inflation_rewards_collector_healthy: inflation_health.and_then(|h| h.healthy),
            block_revenue_collector_owner: block_revenue_health
                .and_then(|h| h.owner)
                .map(|owner| owner.to_string()),
            block_revenue_collector_lamports: block_revenue_health.map(|h| h.lamports),
            block_revenue_collector_healthy: block_revenue_health.and_then(|h| h.healthy),
            bls_pubkey: vote_state.and_then(|state| state.bls_pubkey.clone()),
            vote_account_lamports: vote_state.and_then(|state| state.lamports),
            vat_admitted: vat_admission
                .get(&vote_pubkey)
                .map(|reason| reason.is_none()),
            vat_admission_reason: vat_admission.get(&vote_pubkey).copied().flatten(),
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
