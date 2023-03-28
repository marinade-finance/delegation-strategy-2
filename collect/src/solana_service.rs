use crate::validators::*;
use bincode::deserialize;
use log::{error, info};
use serde_json::{Map, Value};
use solana_client::{rpc_client::RpcClient, rpc_response::RpcVoteAccountStatus};
use solana_config_program::{get_config_data, ConfigKeys};
use solana_sdk::{
    account::from_account,
    clock::{Epoch, Slot},
    commitment_config::CommitmentConfig,
    slot_history::{self, SlotHistory},
    sysvar,
};
use solana_sdk::{account::Account, pubkey::Pubkey};
use std::{
    collections::{HashMap, HashSet},
    str::FromStr,
};

pub fn solana_client(url: String, commitment: String) -> RpcClient {
    RpcClient::new_with_commitment(url, CommitmentConfig::from_str(&commitment).unwrap())
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
            Err(err) => error!("Error parsing validator info {}", err),
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
