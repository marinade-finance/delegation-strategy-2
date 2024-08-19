use crate::marinade_service::fetch_bonds;
use crate::validators::*;
use bincode::deserialize;
use log::{error, info, warn};
use rust_decimal::{prelude::ToPrimitive, Decimal};
use serde_json::{Map, Value};
use solana_account_decoder::UiAccountEncoding;
use solana_client::{
    client_error::ClientError,
    rpc_client::RpcClient,
    rpc_config::{RpcAccountInfoConfig, RpcProgramAccountsConfig},
    rpc_filter::{Memcmp, RpcFilterType},
    rpc_response::RpcVoteAccountStatus,
};
use crate::common::QuadraticBackoffStrategy;
use crate::common::retry_blocking;
use solana_config_program::{get_config_data, ConfigKeys};
use solana_program::{
    stake::{self, state::StakeState},
    stake_history::{StakeHistory, StakeHistoryEntry},
    sysvar::stake_history,
};
use solana_sdk::{
    account::from_account,
    clock::{Epoch, Slot},
    commitment_config::CommitmentConfig,
    slot_history::{self, SlotHistory},
    sysvar,
};
use solana_sdk::{account::Account, pubkey::Pubkey};
use solana_vote_program::vote_state::VoteState;
use std::{
    collections::{HashMap, HashSet},
    str::FromStr,
    thread::sleep,
    time::Duration,
};

const RPC_STAKE_ACCOUNTS_FETCH_BACKOFF_MS: u64 = 200;
const WITHDRAW_AUTHORITY_OFFSET: usize = 4 + 8 + 32;
const BLOCK_TIME_OFFSET: i64 = 60 * 30; // 30 minutes in seconds

pub fn solana_client(url: String, commitment: String) -> RpcClient {
    RpcClient::new_with_commitment(url, CommitmentConfig::from_str(&commitment).unwrap())
}

pub fn solana_client_with_timeout(url: String, timeout: Duration, commitment: String) -> RpcClient {
    RpcClient::new_with_timeout_and_commitment(
        url,
        timeout,
        CommitmentConfig::from_str(&commitment).unwrap(),
    )
}

pub fn get_stake_history(rpc_client: &RpcClient) -> anyhow::Result<StakeHistory> {
    Ok(bincode::deserialize(
        &rpc_client.get_account_data(&stake_history::ID)?,
    )?)
}

pub fn get_credits(rpc_client: &RpcClient, epoch: Epoch) -> anyhow::Result<HashMap<String, u64>> {
    info!("Getting credits");
    let vote_accounts = rpc_client.get_vote_accounts()?;

    let mut credits = HashMap::new();

    for vote_account in vote_accounts
        .current
        .iter()
        .chain(vote_accounts.delinquent.iter())
    {
        for (record_epoch, end_credits, start_credits) in vote_account.epoch_credits.iter() {
            if *record_epoch == epoch {
                credits.insert(
                    vote_account.vote_pubkey.clone(),
                    end_credits - start_credits,
                );
            }
        }
    }

    Ok(credits)
}

pub fn get_cluster_nodes_versions(
    rpc_client: &RpcClient,
) -> anyhow::Result<HashMap<String, String>> {
    info!("Getting cluster nodes versions");
    let cluster_nodes = rpc_client.get_cluster_nodes()?;

    Ok(cluster_nodes
        .iter()
        .filter_map(|node| match &node.version {
            Some(version) => Some((node.pubkey.clone(), version.clone())),
            _ => None,
        })
        .collect())
}

pub fn get_cluster_nodes_ips(rpc_client: &RpcClient) -> anyhow::Result<HashMap<String, String>> {
    info!("Getting cluster nodes IPs");
    let cluster_nodes = rpc_client.get_cluster_nodes()?;

    Ok(cluster_nodes
        .iter()
        .filter_map(|node| match &node.gossip {
            Some(gossip) => Some((node.pubkey.clone(), gossip.ip().to_string())),
            _ => None,
        })
        .collect())
}

pub fn get_total_activated_stake(vote_accounts: &RpcVoteAccountStatus) -> (u64, u64) {
    (
        vote_accounts
            .current
            .iter()
            .map(|v| v.activated_stake)
            .sum(),
        vote_accounts
            .delinquent
            .iter()
            .map(|v| v.activated_stake)
            .sum(),
    )
}

pub fn get_minimum_superminority_stake(vote_accounts: &RpcVoteAccountStatus) -> u64 {
    let mut activated_stakes: Vec<_> = vote_accounts
        .current
        .iter()
        .chain(vote_accounts.delinquent.iter())
        .map(|v| v.activated_stake)
        .collect();
    let total_activated_stake: u64 = activated_stakes.iter().sum();
    let superminority_threshold = total_activated_stake / 3;
    activated_stakes.sort_by(|a, b| b.cmp(a));

    let mut accumulated = 0;
    let mut last_stake = 0;
    for stake in activated_stakes.iter() {
        accumulated += stake;
        last_stake = *stake;
        if accumulated > superminority_threshold {
            break;
        }
    }

    last_stake
}

pub fn get_block_production_by_validator(
    rpc_client: &RpcClient,
    epoch: Epoch,
) -> anyhow::Result<HashMap<String, (usize, usize)>> {
    info!("Getting block production by validator");
    let epoch_schedule = rpc_client.get_epoch_schedule()?;
    let first_slot_in_epoch = epoch_schedule.get_first_slot_in_epoch(epoch);
    let last_slot_in_epoch = epoch_schedule.get_last_slot_in_epoch(epoch);

    let current_epoch_production = rpc_client.get_block_production()?;
    if first_slot_in_epoch == current_epoch_production.value.range.first_slot {
        return Ok(current_epoch_production.value.by_identity);
    }

    let confirmed_blocks =
        get_confirmed_blocks(rpc_client, first_slot_in_epoch, last_slot_in_epoch)?;

    let leader_schedule = rpc_client
        .get_leader_schedule_with_commitment(
            Some(first_slot_in_epoch),
            CommitmentConfig::finalized(), // todo take from config
        )?
        .unwrap();

    let mut blocks_and_slots = HashMap::new();
    for (validator_identity, relative_slots) in leader_schedule {
        let mut validator_blocks = 0;
        let mut validator_slots = 0;
        for relative_slot in relative_slots {
            let slot = first_slot_in_epoch + relative_slot as Slot;
            validator_slots += 1;
            if confirmed_blocks.contains(&slot) {
                validator_blocks += 1;
            }
        }
        if validator_slots > 0 {
            let e = blocks_and_slots.entry(validator_identity).or_insert((0, 0));
            e.0 += validator_slots;
            e.1 += validator_blocks;
        }
    }

    Ok(blocks_and_slots)
}

fn get_confirmed_blocks(
    rpc_client: &RpcClient,
    start_slot: Slot,
    end_slot: Slot,
) -> anyhow::Result<HashSet<Slot>> {
    info!(
        "loading slot history. slot range is [{},{}]",
        start_slot, end_slot
    );
    let slot_history_account = rpc_client
        .get_account_with_commitment(&sysvar::slot_history::id(), CommitmentConfig::finalized())?
        .value
        .unwrap();

    let slot_history: SlotHistory = from_account(&slot_history_account).unwrap();

    if start_slot >= slot_history.oldest() && end_slot <= slot_history.newest() {
        info!("slot range within the SlotHistory sysvar");
        Ok((start_slot..=end_slot)
            .filter(|slot| slot_history.check(*slot) == slot_history::Check::Found)
            .collect())
    } else {
        anyhow::bail!("slot range is not within the SlotHistory sysvar")
    }
}

fn parse_validator_info(
    pubkey: &Pubkey,
    account: &Account,
) -> anyhow::Result<(Pubkey, ValidatorInfo)> {
    if account.owner != solana_config_program::id() {
        anyhow::bail!("{} is not a validator info account", pubkey);
    }
    let key_list: ConfigKeys = deserialize(&account.data)?;
    if !key_list.keys.is_empty() {
        let (validator_pubkey, _) = key_list.keys[1];
        let validator_info_string: String = deserialize(get_config_data(&account.data)?)?;
        let validator_info: Map<_, _> = serde_json::from_str(&validator_info_string)?;
        Ok((
            validator_pubkey,
            ValidatorInfo {
                name: extract_json_value(&validator_info, "name".to_string()),
                url: extract_json_value(&validator_info, "website".to_string()),
                details: extract_json_value(&validator_info, "details".to_string()),
                keybase: extract_json_value(&validator_info, "keybaseUsername".to_string()),
            },
        ))
    } else {
        anyhow::bail!("{} could not be parsed as a validator info account", pubkey);
    }
}
pub fn get_validators_info(
    rpc_client: &RpcClient,
) -> anyhow::Result<HashMap<String, ValidatorInfo>> {
    info!("Getting validator info");
    let validator_info = rpc_client.get_program_accounts(&solana_config_program::id())?;

    let mut validator_info_map = HashMap::new();
    if validator_info.is_empty() {
        println!("No validator info accounts found");
    }
    for (validator_info_pubkey, validator_info_account) in validator_info.iter() {
        match parse_validator_info(validator_info_pubkey, validator_info_account) {
            Ok((validator_pubkey, validator_info)) => {
                validator_info_map.insert(validator_pubkey.to_string(), validator_info);
            }
            Err(err) => warn!("Couldn't parse validator info {}", err),
        }
    }

    Ok(validator_info_map)
}

fn extract_json_value(json: &Map<String, Value>, key: String) -> Option<String> {
    json.get(&key)
        .map(|value| serde_json::from_value(value.clone()).ok())
        .flatten()
}

pub fn get_apy(
    rpc_client: &RpcClient,
    vote_accounts: &RpcVoteAccountStatus,
    credits: &HashMap<String, u64>,
) -> anyhow::Result<HashMap<String, f64>> {
    info!("Calculating APY");
    let supply = rpc_client.supply()?.value.total;
    let inflation = rpc_client.get_inflation_rate()?.total;
    let inflation_taper = rpc_client.get_inflation_governor()?.taper;

    let epochs_in_year = 160; // @todo fix

    let activated_stake: HashMap<_, _> = vote_accounts
        .current
        .iter()
        .chain(vote_accounts.delinquent.iter())
        .map(|v| (v.vote_pubkey.clone(), v.activated_stake.clone()))
        .collect();

    let commission: HashMap<_, _> = vote_accounts
        .current
        .iter()
        .chain(vote_accounts.delinquent.iter())
        .map(|v| (v.vote_pubkey.clone(), v.commission.clone()))
        .collect();

    let total_activated_stake = activated_stake.iter().map(|(_, s)| s).sum::<u64>();

    let points: HashMap<_, _> = activated_stake
        .iter()
        .filter_map(|(node, stake)| match credits.get(node) {
            Some(credits) => Some((node.clone(), *credits as u128 * *stake as u128)),
            _ => None,
        })
        .collect();

    let total_points = points.iter().map(|(_, p)| p).sum::<u128>();

    let mut total_rewards = 0.0;
    for epoch in 1..epochs_in_year + 1 {
        let tapered_inflation =
            inflation * (1.0 - inflation_taper).powf(epoch as f64 / epochs_in_year as f64);
        total_rewards += tapered_inflation / epochs_in_year as f64 * total_activated_stake as f64;
    }

    let mut apy = HashMap::new();
    for (node, points) in points.iter() {
        if let (Some(stake), Some(commission)) = (activated_stake.get(node), commission.get(node)) {
            let node_staker_rewards = (1.0 - *commission as f64 / 100.0) * *points as f64
                / total_points as f64
                * total_rewards;
            apy.insert(
                node.clone(),
                (*stake as f64 + node_staker_rewards) / *stake as f64 - 1.0,
            );
        }
    }

    Ok(apy)
}

pub fn get_withdraw_authorities(
    rpc_client: &RpcClient,
) -> anyhow::Result<HashSet<(String, String)>> {
    let mut withdraw_authorities: HashSet<(String, String)> = HashSet::default();
    let vote_program_id = solana_vote_program::id();
    let vote_accounts = rpc_client.get_program_accounts(&vote_program_id)?;

    for (account_pubkey, account) in vote_accounts {
        if let Ok(vote_state) = VoteState::deserialize(&account.data) {
            withdraw_authorities.insert((
                vote_state.authorized_withdrawer.to_string(),
                account_pubkey.to_string(),
            ));
        }
    }
    Ok(withdraw_authorities)
}

pub fn get_commission_from_inflation_rewards(
    rpc_client: &RpcClient,
    vote_accounts: &RpcVoteAccountStatus,
    epoch: Option<Epoch>,
) -> anyhow::Result<HashMap<String, u8>> {
    let vote_addresses: Vec<_> = vote_accounts
        .current
        .iter()
        .chain(vote_accounts.delinquent.iter())
        .map(|v| Pubkey::from_str(&v.vote_pubkey).unwrap())
        .collect();
    let mut result: HashMap<String, u8> = Default::default();
    for vote_addresses_chunk in vote_addresses.chunks(100) {
        let rewards = rpc_client.get_inflation_reward(vote_addresses_chunk, epoch)?;
        result.extend(vote_addresses_chunk.iter().zip(rewards).filter_map(
            |(vote_address, reward)| {
                if let Some(reward) = reward {
                    if let Some(commission) = reward.commission {
                        return Some((vote_address.to_string(), commission));
                    }
                }

                None
            },
        ));
    }

    Ok(result)
}

pub fn get_self_stake(
    rpc_client: &RpcClient,
    epoch: Epoch,
    stake_history: &StakeHistory,
    bonds_url: &String,
) -> anyhow::Result<HashMap<String, u64>> {
    let withdraw_authorities = get_withdraw_authorities(rpc_client)?;
    let mut self_stake = fetch_self_stake(rpc_client, withdraw_authorities, epoch, stake_history)?;

    assert!(!self_stake.is_empty(), "Failed to fetch self stake data");

    let bonds = fetch_bonds(bonds_url)?;
    assert!(!bonds.is_empty(), "Failed to fetch bonds data");
    assert!(
        !bonds.iter().all(|b| b.funded_amount == Decimal::ZERO),
        "All bonds have zero funded amounts, expected at least one non-zero amount"
    );

    for bond in bonds {
        let funded_amount_u64 = bond
            .funded_amount
            .to_u64()
            .ok_or_else(|| anyhow::anyhow!("Failed to convert Bond Decimal value to u64"))?;
        *self_stake.entry(bond.vote_account).or_insert(0) += funded_amount_u64;
    }
    Ok(self_stake)
}

fn fetch_stake_accounts_on_page(
    rpc_client: &RpcClient,
    page: u8,
) -> Result<Vec<(Pubkey, Account)>, ClientError> {
    let mut filters: Vec<RpcFilterType> = vec![RpcFilterType::DataSize(200)];
    filters.push(RpcFilterType::Memcmp(Memcmp::new_raw_bytes(
        WITHDRAW_AUTHORITY_OFFSET,
        vec![page],
    )));

    let self_stakes = retry_blocking(
        || {
            rpc_client.get_program_accounts_with_config(
                &stake::program::ID,
                RpcProgramAccountsConfig {
                    filters: Some(filters.clone()),
                    account_config: RpcAccountInfoConfig {
                        encoding: Some(UiAccountEncoding::Base64),
                        commitment: Some(rpc_client.commitment()),
                        data_slice: None,
                        min_context_slot: None,
                    },
                    with_context: None,
                },
            )
        },
        QuadraticBackoffStrategy::new(5),
        |err, attempt, backoff| {
            warn!(
                "Attempt {} has failed: {}, retrying in {:?} seconds",
                attempt,
                err.to_string(),
                backoff.as_secs()
            )
        },
    )?;
    Ok(self_stakes)
}

fn process_accounts_for_self_stake(
    accounts: Vec<(Pubkey, Account)>,
    self_stake: &mut HashMap<String, u64>,
    withdraw_authorities: &HashSet<(String, String)>,
    epoch: Epoch,
    stake_history: &StakeHistory,
) -> u64 {
    let mut self_stake_assigned = 0;
    for (_pubkey, account) in accounts.iter() {
        if let Ok(stake_account) = bincode::deserialize(&account.data) {
            if let Some((withdrawer_key, vote_key)) = get_withdrawer_and_vote_keys(&stake_account) {
                let StakeHistoryEntry {
                    effective,
                    activating: _,
                    deactivating: _,
                } = stake_account
                    .stake()
                    .unwrap()
                    .delegation
                    .stake_activating_and_deactivating(epoch, Some(stake_history), None);
                if withdraw_authorities.contains(&(withdrawer_key, vote_key.clone()))
                    && effective != 0
                {
                    self_stake_assigned += 1;
                    update_self_stake(self_stake, &vote_key, effective);
                }
            }
        }
    }

    self_stake_assigned
}

fn get_withdrawer_and_vote_keys(stake_account: &StakeState) -> Option<(String, String)> {
    stake_account.delegation().and_then(|vote_account| {
        stake_account.authorized().map(|withdrawer| {
            (
                withdrawer.withdrawer.to_string(),
                vote_account.voter_pubkey.to_string(),
            )
        })
    })
}

fn update_self_stake(self_stake: &mut HashMap<String, u64>, vote_key: &str, lamports: u64) {
    let stake_entry = self_stake.entry(vote_key.to_string()).or_insert(0);
    *stake_entry += lamports;
}

pub fn fetch_self_stake(
    rpc_client: &RpcClient,
    withdraw_authorities: HashSet<(String, String)>,
    epoch: Epoch,
    stake_history: &StakeHistory,
) -> anyhow::Result<HashMap<String, u64>> {
    let mut self_stake: HashMap<String, u64> = HashMap::default();
    for page in 0..=u8::MAX {
        match fetch_stake_accounts_on_page(rpc_client, page) {
            Ok(accounts) => {
                let processed = process_accounts_for_self_stake(
                    accounts,
                    &mut self_stake,
                    &withdraw_authorities,
                    epoch,
                    stake_history,
                );
                info!("Processed {} self stakes on page {}", processed, page);
            }
            Err(err) => {
                panic!("Failed to fetch stake accounts on page {}: {}", page, err);
            }
        }

        sleep(Duration::from_millis(RPC_STAKE_ACCOUNTS_FETCH_BACKOFF_MS));
    }

    Ok(self_stake)
}
