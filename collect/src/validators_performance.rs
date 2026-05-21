use crate::common::*;
use crate::solana_service::solana_client_with_timeout;
use crate::solana_service::*;
use log::{info, warn};
use serde::{Deserialize, Serialize};
use serde_yaml;
use solana_client::{rpc_client::RpcClient, rpc_response::RpcVoteAccountStatus};
use solana_sdk::clock::Epoch;
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use structopt::StructOpt;

#[derive(Debug, StructOpt)]
pub struct ValidatorsPerformanceParams {
    #[structopt(long = "with-rewards", help = "Whether to calculate APY and rewards.")]
    with_rewards: bool,

    #[structopt(long = "epoch", help = "Which epoch to use for epoch-based metrics.")]
    epoch: Option<Epoch>,

    #[structopt(
        long = "rpc-attempts",
        help = "How many times to retry the operation.",
        default_value = "10"
    )]
    rpc_attempts: usize,

    #[structopt(
        long = "rpc-timeout",
        help = "How long to wait for RPC response (seconds).",
        default_value = "300"
    )]
    rpc_timeout: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ValidatorRewards {
    pub commission_effective: Option<u8>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ClusterInflation {
    pub sol_total_supply: u64,
    pub inflation: f64,
    pub inflation_taper: f64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ValidatorPerformance {
    pub commission: u8,
    pub version: Option<String>,
    #[serde(default)]
    pub client_id: Option<String>,
    #[serde(default)]
    pub client_type: Option<String>,
    #[serde(default)]
    pub feature_set: Option<u32>,
    #[serde(default)]
    pub shred_version: Option<u16>,
    pub credits: u64,
    pub leader_slots: usize,
    pub blocks_produced: usize,
    pub skip_rate: f64,
    pub delinquent: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ValidatorsPerformanceSnapshot {
    pub epoch: Epoch,
    pub epoch_slot: u64,
    pub transaction_count: u64,
    pub created_at: String,
    pub cluster_inflation: Option<ClusterInflation>,
    pub validators: HashMap<String, ValidatorPerformance>,
    pub rewards: Option<HashMap<String, ValidatorRewards>>,
}

pub fn validators_performance(
    client: &RpcClient,
    epoch: Epoch,
    vote_accounts: &RpcVoteAccountStatus,
    rpc_attempts: usize,
    node_info: &HashMap<String, NodeContact>,
) -> anyhow::Result<HashMap<String, ValidatorPerformance>> {
    let mut validators: HashMap<String, ValidatorPerformance> = Default::default();

    let delinquent: HashSet<_> = vote_accounts
        .delinquent
        .iter()
        .map(|v| v.vote_pubkey.clone())
        .collect();
    // block production is the first RPC after the whois fetch; retry so a keep-alive socket dropped during that idle gap doesn't abort the whole snapshot
    let production_by_validator = retry_blocking(
        || get_block_production_by_validator(client, epoch),
        QuadraticBackoffStrategy::iter_durations(rpc_attempts),
        |err, attempt, backoff| {
            warn!("Attempt {attempt} to get block production failed: {err:?}, retrying in {backoff:?}")
        },
    )?;
    let credits = get_credits(client, epoch)?;

    for vote_account in vote_accounts
        .current
        .iter()
        .chain(vote_accounts.delinquent.iter())
    {
        let vote_pubkey = vote_account.vote_pubkey.clone();
        let identity = vote_account.node_pubkey.clone();
        let (leader_slots, blocks_produced) = production_by_validator
            .get(&identity)
            .cloned()
            .unwrap_or((0, 0));

        let node = node_info.get(&identity);

        validators.insert(
            vote_pubkey.clone(),
            ValidatorPerformance {
                commission: vote_account.commission,
                version: node.and_then(|n| n.version.clone()),
                client_id: node.and_then(|n| n.client_id.clone()),
                client_type: node.map(|n| n.client_type.as_str().to_string()),
                feature_set: node.and_then(|n| n.feature_set),
                shred_version: node.and_then(|n| n.shred_version),
                credits: credits.get(&vote_pubkey).cloned().unwrap_or(0),
                leader_slots,
                blocks_produced,
                skip_rate: if leader_slots == 0 {
                    0f64
                } else {
                    1f64 - (blocks_produced as f64 / leader_slots as f64)
                },
                delinquent: delinquent.contains(&vote_pubkey),
            },
        );
    }

    Ok(validators)
}

pub fn validator_rewards(
    client: &RpcClient,
    epoch: Epoch,
    vote_accounts: &RpcVoteAccountStatus,
) -> anyhow::Result<HashMap<String, ValidatorRewards>> {
    let commission_from_rewards =
        get_commission_from_inflation_rewards(client, vote_accounts, Some(epoch))?;

    Ok(vote_accounts
        .current
        .iter()
        .chain(vote_accounts.delinquent.iter())
        .map(|vote_account| {
            (
                vote_account.vote_pubkey.clone(),
                ValidatorRewards {
                    commission_effective: commission_from_rewards
                        .get(&vote_account.vote_pubkey)
                        .cloned(),
                },
            )
        })
        .collect())
}

pub fn collect_validators_performance_info(
    common_params: CommonParams,
    performance_params: ValidatorsPerformanceParams,
) -> anyhow::Result<()> {
    info!("Collecting snaphost of validators' performance");
    let client = solana_client_with_timeout(
        common_params.rpc_url,
        Duration::from_secs(performance_params.rpc_timeout),
        common_params.commitment,
    );

    let created_at = chrono::Utc::now();
    let current_epoch_info = client.get_epoch_info()?;
    let epoch = performance_params.epoch.unwrap_or(current_epoch_info.epoch);
    info!("Current epoch: {current_epoch_info:?}");
    info!("Looking at epoch: {epoch}");

    let vote_accounts = client.get_vote_accounts()?;
    info!(
        "Total vote accounts found: {}",
        vote_accounts.current.len() + vote_accounts.delinquent.len()
    );
    info!(
        "Delinquent vote accounts found: {}",
        vote_accounts.delinquent.len()
    );

    let node_info = get_cluster_nodes_info(&client)?;
    let validators = validators_performance(
        &client,
        epoch,
        &vote_accounts,
        performance_params.rpc_attempts,
        &node_info,
    )?;

    let rewards = if performance_params.with_rewards {
        Some(validator_rewards(&client, epoch, &vote_accounts)?)
    } else {
        None
    };

    let cluster_inflation = if performance_params.with_rewards {
        let sol_total_supply = client.supply()?.value.total;
        let inflation = client.get_inflation_rate()?.total;
        let inflation_taper = client.get_inflation_governor()?.taper;

        Some(ClusterInflation {
            sol_total_supply,
            inflation,
            inflation_taper,
        })
    } else {
        None
    };

    serde_yaml::to_writer(
        std::io::stdout(),
        &ValidatorsPerformanceSnapshot {
            epoch,
            epoch_slot: current_epoch_info.slot_index,
            transaction_count: current_epoch_info.transaction_count.unwrap(),
            created_at: created_at.to_string(),
            cluster_inflation,
            validators,
            rewards,
        },
    )?;

    Ok(())
}
