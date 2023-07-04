use anchor_lang::prelude::*;
use solana_account_decoder::*;
use solana_client::{
    rpc_client::RpcClient,
    rpc_config::{RpcAccountInfoConfig, RpcProgramAccountsConfig},
    rpc_filter::{Memcmp, MemcmpEncodedBytes, RpcFilterType},
};
use solana_program::{clock::*, pubkey::Pubkey};
use solana_sdk::stake;
use std::{collections::*};

pub fn get_marinade_stakes(rpc_client: &RpcClient) -> anyhow::Result<HashMap<String, u64>> {
    // @todo take from state
    let delegation_authority = "4bZ6o3eUUNXhKuqjdCnCoPAoLgWiuLYixKaxoa8PpiKk".try_into()?;
    let withdrawer_authority = "9eG63CdHjsfhHmobHgLtESGC8GabbmRcaSpHAZrtmhco".try_into()?;
    Ok(get_stakes_groupped_by_validator(
        rpc_client,
        &delegation_authority,
        Some(&withdrawer_authority),
    )?)
}

pub fn get_decentralizer_stakes(rpc_client: &RpcClient) -> anyhow::Result<HashMap<String, u64>> {
    // @todo take from config
    let decentralizer_authority = "noMa7dN4cHQLV4ZonXrC29HTKFpxrpFbDLK5Gub8W8t".try_into()?;
    Ok(get_stakes_groupped_by_validator(
        rpc_client,
        &decentralizer_authority,
        None,
    )?)
}

fn get_stakes_groupped_by_validator(
    rpc_client: &RpcClient,
    delegation_authority: &Pubkey,
    withdrawer_authority: Option<&Pubkey>,
) -> anyhow::Result<HashMap<String, u64>> {
    let stakes = get_stake_accounts(rpc_client, &delegation_authority, withdrawer_authority)?;

    let stakes: Vec<_> = stakes
        .iter()
        .filter_map(|(_, stake_account)| {
            stake_account.delegation().and_then(|delegation| {
                if delegation.activation_epoch == Epoch::MAX {
                    None
                } else {
                    Some((delegation.voter_pubkey.to_string(), delegation.stake))
                }
            })
        })
        .collect();

    let mut total_stakes: HashMap<String, u64> = HashMap::new();
    for (pubkey, stake) in stakes {
        if let Some(sum) = total_stakes.get_mut(&pubkey) {
            *sum += stake;
        } else {
            total_stakes.insert(pubkey, stake);
        }
    }

    Ok(total_stakes)
}

fn get_stake_accounts(
    rpc_client: &RpcClient,
    delegation_authority: &Pubkey,
    withdrawer_authority: Option<&Pubkey>,
) -> anyhow::Result<HashMap<Pubkey, stake::state::StakeState>> {
    log::info!(
        "Fetching stake accounts by delegation authority: {:?}",
        delegation_authority
    );

    let mut filters = vec![RpcFilterType::Memcmp(Memcmp {
        offset: 4 + 8, // enum StakeState + rent_exempt_reserve: u64
        bytes: MemcmpEncodedBytes::Base58(delegation_authority.to_string()),
        encoding: None,
    })];

    if let Some(withdrawer_authority) = withdrawer_authority {
        filters.push(RpcFilterType::Memcmp(Memcmp {
            offset: 4 + 8 + 32, // enum StakeState + rent_exempt_reserve: u64 + delegation_authority: Pubkey
            bytes: MemcmpEncodedBytes::Base58(withdrawer_authority.to_string()),
            encoding: None,
        }));
    }

    let accounts = rpc_client.get_program_accounts_with_config(
        &stake::program::ID,
        RpcProgramAccountsConfig {
            filters: Some(filters),
            account_config: RpcAccountInfoConfig {
                encoding: Some(UiAccountEncoding::Base64),
                commitment: Some(rpc_client.commitment()),
                data_slice: None,
                min_context_slot: None,
            },
            with_context: None,
        },
    )?;

    Ok(accounts
        .iter()
        .map(|(pubkey, account)| (pubkey.clone(), bincode::deserialize(&account.data).unwrap()))
        .collect())
}

#[derive(Debug, Default, borsh::BorshDeserialize, borsh::BorshSchema)]
pub struct Gauge {
    pub gaugemeister: Pubkey,
    pub total_weight: u64,
    pub vote_count: u64,
    pub is_disabled: bool,
    // snapshots make reading more flexible and make time of reading predicted (no delays because of inet/cpu)
    pub snapshot_time: i64,
    pub snapshot_slot: u64,
    pub snapshot_total_weight: u64,
    pub info: Vec<u8>,
}

impl Gauge {
    pub const LEN: usize = 200;
}
