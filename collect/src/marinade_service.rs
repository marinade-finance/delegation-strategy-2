use solana_account_decoder::*;
use solana_client::{
    rpc_client::RpcClient,
    rpc_config::{RpcAccountInfoConfig, RpcProgramAccountsConfig},
    rpc_filter::{Memcmp, MemcmpEncodedBytes, RpcFilterType},
};
use solana_program::{clock::*, pubkey::Pubkey};
use solana_sdk::stake;
use std::collections::*;

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

pub fn get_foundation_stakes(rpc_client: &RpcClient) -> anyhow::Result<HashMap<String, u64>> {
    let mut foundation_authority = "mpa4abUkjQoAvPzREkh5Mo75hZhPFQ2FSH6w7dWKuQ5".try_into()?;

    if rpc_client.url().contains("testnet") {
        foundation_authority = "spa8QF2uL9Z5EkYKFeVKNWjgTJgkwV5CMkdKHZwn3P6".try_into()?;
    }

    Ok(get_stakes_groupped_by_validator(
        rpc_client,
        &foundation_authority,
        None,
    )?)
}

pub fn get_decentralizer_stakes(rpc_client: &RpcClient) -> anyhow::Result<HashMap<String, u64>> {
    // @todo take from config
    let decentralizer_authority = "stWirqFCf2Uts1JBL1Jsd3r6VBWhgnpdPxCTe1MFjrq".try_into()?;
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