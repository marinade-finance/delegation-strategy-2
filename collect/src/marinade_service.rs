use crate::validators::BondsResponse;
use crate::validators::ValidatorBond;
use solana_account_decoder::*;
use solana_client::{
    rpc_client::RpcClient,
    rpc_config::{RpcAccountInfoConfig, RpcProgramAccountsConfig},
    rpc_filter::{Memcmp, MemcmpEncodedBytes, RpcFilterType},
};
use solana_program::{
    clock::*,
    pubkey::Pubkey,
    stake_history::{StakeHistory, StakeHistoryEntry},
};
use solana_sdk::stake;
use std::collections::*;

pub fn get_marinade_stakes(
    rpc_client: &RpcClient,
    epoch: Epoch,
    stake_history: &StakeHistory,
) -> anyhow::Result<HashMap<String, u64>> {
    // @todo take from state
    let delegation_authority = "4bZ6o3eUUNXhKuqjdCnCoPAoLgWiuLYixKaxoa8PpiKk".try_into()?;
    let withdrawer_authority = "9eG63CdHjsfhHmobHgLtESGC8GabbmRcaSpHAZrtmhco".try_into()?;
    Ok(get_stakes_groupped_by_validator(
        rpc_client,
        &delegation_authority,
        Some(&withdrawer_authority),
        epoch,
        stake_history,
    )?)
}

pub fn get_institutional_stakes(
    rpc_client: &RpcClient,
    epoch: Epoch,
    stake_history: &StakeHistory,
) -> anyhow::Result<HashMap<String, u64>> {
    let mut institutional_authority = "STNi1NHDUi6Hvibvonawgze8fM83PFLeJhuGMEXyGps".try_into()?;

    let institutional_stakes = get_stakes_groupped_by_validator(
        rpc_client,
        &institutional_authority,
        None,
        epoch,
        stake_history,
    )?;

    Ok(institutional_stakes)
}

pub fn get_foundation_stakes(
    rpc_client: &RpcClient,
    epoch: Epoch,
    stake_history: &StakeHistory,
) -> anyhow::Result<HashMap<String, u64>> {
    let mut foundation_authority = "mpa4abUkjQoAvPzREkh5Mo75hZhPFQ2FSH6w7dWKuQ5".try_into()?;

    if rpc_client.url().contains("testnet") {
        foundation_authority = "spa8QF2uL9Z5EkYKFeVKNWjgTJgkwV5CMkdKHZwn3P6".try_into()?;
    }

    let foundation_stakes = get_stakes_groupped_by_validator(
        rpc_client,
        &foundation_authority,
        None,
        epoch,
        stake_history,
    )?;

    assert!(
        !foundation_stakes.is_empty(),
        "Failed to fetch foundation stake data"
    );
    Ok(foundation_stakes)
}

pub fn get_marinade_native_stakes(
    rpc_client: &RpcClient,
    epoch: Epoch,
    stake_history: &StakeHistory,
) -> anyhow::Result<HashMap<String, u64>> {
    // @todo take from config
    let marinade_native_stake_authority =
        "stWirqFCf2Uts1JBL1Jsd3r6VBWhgnpdPxCTe1MFjrq".try_into()?;
    Ok(get_stakes_groupped_by_validator(
        rpc_client,
        &marinade_native_stake_authority,
        None,
        epoch,
        stake_history,
    )?)
}

fn get_stakes_groupped_by_validator(
    rpc_client: &RpcClient,
    delegation_authority: &Pubkey,
    withdrawer_authority: Option<&Pubkey>,
    epoch: Epoch,
    stake_history: &StakeHistory,
) -> anyhow::Result<HashMap<String, u64>> {
    let stakes = get_stake_accounts(rpc_client, &delegation_authority, withdrawer_authority)?;

    let stakes: Vec<_> = stakes
        .iter()
        .filter_map(|(_, stake_account)| {
            stake_account.stake().and_then(|stake| {
                let StakeHistoryEntry {
                    effective,
                    activating,
                    deactivating,
                } = stake.delegation.stake_activating_and_deactivating(
                    epoch,
                    Some(stake_history),
                    None,
                );
                if effective == 0 {
                    None
                } else {
                    Some((stake.delegation.voter_pubkey.to_string(), effective))
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

    let mut filters = vec![RpcFilterType::Memcmp(Memcmp::new(
        4 + 8, // enum StakeState + rent_exempt_reserve: u64
        MemcmpEncodedBytes::Base58(delegation_authority.to_string()),
    ))];

    if let Some(withdrawer_authority) = withdrawer_authority {
        filters.push(RpcFilterType::Memcmp(Memcmp::new(
            4 + 8 + 32, // enum StakeState + rent_exempt_reserve: u64 + delegation_authority: Pubkey
            MemcmpEncodedBytes::Base58(withdrawer_authority.to_string()),
        )));
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

pub fn fetch_bonds(bonds_url: &str) -> anyhow::Result<Vec<ValidatorBond>> {
    let response = reqwest::blocking::get(bonds_url)?;

    if response.status().is_success() {
        if let Ok(bonds_response) = response.json::<BondsResponse>() {
            Ok(bonds_response.bonds)
        } else {
            Err(anyhow::anyhow!("Failed to parse bonds response JSON"))
        }
    } else {
        Err(anyhow::anyhow!(
            "Failed to fetch bonds. Status: {}",
            response.status()
        ))
    }
}
