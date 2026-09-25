use crate::validators::BondsResponse;
use crate::validators::ValidatorBond;
use solana_account_decoder::*;
use solana_program::{
    clock::*,
    pubkey::Pubkey,
    stake_history::{StakeHistory, StakeHistoryEntry},
};
use solana_rpc_client::rpc_client::RpcClient;
use solana_rpc_client_api::config::{RpcAccountInfoConfig, RpcProgramAccountsConfig};
use solana_rpc_client_api::filter::{Memcmp, MemcmpEncodedBytes, RpcFilterType};
use solana_sdk::pubkey;
use solana_stake_interface as stake;
use std::collections::*;

pub fn get_marinade_stakes(
    rpc_client: &RpcClient,
    epoch: Epoch,
    stake_history: &StakeHistory,
) -> anyhow::Result<HashMap<String, u64>> {
    // @todo take from state
    let delegation_authority = pubkey!("4bZ6o3eUUNXhKuqjdCnCoPAoLgWiuLYixKaxoa8PpiKk");
    let withdrawer_authority = pubkey!("9eG63CdHjsfhHmobHgLtESGC8GabbmRcaSpHAZrtmhco");
    get_stakes_grouped_by_validator(
        rpc_client,
        &delegation_authority,
        Some(&withdrawer_authority),
        epoch,
        stake_history,
    )
}

pub fn get_institutional_stakes(
    rpc_client: &RpcClient,
    epoch: Epoch,
    stake_history: &StakeHistory,
) -> anyhow::Result<HashMap<String, u64>> {
    let institutional_authority = pubkey!("STNi1NHDUi6Hvibvonawgze8fM83PFLeJhuGMEXyGps");

    let institutional_stakes = get_stakes_grouped_by_validator(
        rpc_client,
        &institutional_authority,
        None,
        epoch,
        stake_history,
    )?;

    Ok(institutional_stakes)
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct StakeAmounts {
    pub effective: u64,
    pub activating: u64,
    pub deactivating: u64,
}

pub fn get_direct_stakes(
    rpc_client: &RpcClient,
    epoch: Epoch,
    stake_history: &StakeHistory,
) -> anyhow::Result<HashMap<String, StakeAmounts>> {
    let direct_stake_authority = pubkey!("psrStL2hNx4c7hLUUks8SmDngeYriB8pF7uyHFhM8ir");
    let stakes = get_stake_accounts(rpc_client, &direct_stake_authority, None)?;
    Ok(group_by_validator(stakes.values(), epoch, stake_history))
}

pub fn get_foundation_stakes(
    rpc_client: &RpcClient,
    epoch: Epoch,
    stake_history: &StakeHistory,
) -> anyhow::Result<HashMap<String, u64>> {
    let mut foundation_authority = pubkey!("mpa4abUkjQoAvPzREkh5Mo75hZhPFQ2FSH6w7dWKuQ5");

    if rpc_client.url().contains("testnet") {
        foundation_authority = pubkey!("4UeMu1PU7goa4rYzViSiuZjDvo9px3JtsXUoinmkSrCX");
    }

    let foundation_stakes = get_stakes_grouped_by_validator(
        rpc_client,
        &foundation_authority,
        None,
        epoch,
        stake_history,
    )?;

    assert!(
        !foundation_stakes.is_empty(),
        "No stake accounts found for foundation delegation authority {foundation_authority}. \
         Authority may have been rotated; verify on-chain history of this pubkey and update the code if necessary."
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
    get_stakes_grouped_by_validator(
        rpc_client,
        &marinade_native_stake_authority,
        None,
        epoch,
        stake_history,
    )
}

fn get_stakes_grouped_by_validator(
    rpc_client: &RpcClient,
    delegation_authority: &Pubkey,
    withdrawer_authority: Option<&Pubkey>,
    epoch: Epoch,
    stake_history: &StakeHistory,
) -> anyhow::Result<HashMap<String, u64>> {
    let stakes = get_stake_accounts(rpc_client, delegation_authority, withdrawer_authority)?;
    Ok(effective_only(group_by_validator(
        stakes.values(),
        epoch,
        stake_history,
    )))
}

fn effective_only(amounts: HashMap<String, StakeAmounts>) -> HashMap<String, u64> {
    amounts
        .into_iter()
        .filter(|(_, amounts)| amounts.effective > 0)
        .map(|(vote_account, amounts)| (vote_account, amounts.effective))
        .collect()
}

fn group_by_validator<'a>(
    stake_accounts: impl Iterator<Item = &'a stake::state::StakeStateV2>,
    epoch: Epoch,
    stake_history: &StakeHistory,
) -> HashMap<String, StakeAmounts> {
    let mut totals: HashMap<String, StakeAmounts> = HashMap::new();
    for stake in stake_accounts.filter_map(|stake_account| stake_account.stake()) {
        let StakeHistoryEntry {
            effective,
            activating,
            deactivating,
        } = stake
            .delegation
            .stake_activating_and_deactivating(epoch, stake_history, None);
        if effective == 0 && activating == 0 && deactivating == 0 {
            continue;
        }
        let total = totals
            .entry(stake.delegation.voter_pubkey.to_string())
            .or_default();
        total.effective += effective;
        total.activating += activating;
        total.deactivating += deactivating;
    }
    totals
}

fn get_stake_accounts(
    rpc_client: &RpcClient,
    delegation_authority: &Pubkey,
    withdrawer_authority: Option<&Pubkey>,
) -> anyhow::Result<HashMap<Pubkey, stake::state::StakeStateV2>> {
    log::info!("Fetching stake accounts by delegation authority: {delegation_authority:?}");

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
            sort_results: None,
        },
    )?;

    Ok(accounts
        .iter()
        .map(|(pubkey, account)| (*pubkey, bincode::deserialize(&account.data).unwrap()))
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

#[cfg(test)]
mod tests {
    use super::*;
    use stake::state::StakeStateV2;

    const EPOCH: Epoch = 900;
    const VOTE: [u8; 32] = [7; 32];

    #[allow(deprecated)]
    fn stake_account(
        stake: u64,
        activation_epoch: Epoch,
        deactivation_epoch: Epoch,
    ) -> StakeStateV2 {
        StakeStateV2::Stake(
            Default::default(),
            stake::state::Stake {
                delegation: stake::state::Delegation {
                    voter_pubkey: Pubkey::new_from_array(VOTE),
                    stake,
                    activation_epoch,
                    deactivation_epoch,
                    warmup_cooldown_rate: 0.25,
                },
                credits_observed: 0,
            },
            stake::stake_flags::StakeFlags::empty(),
        )
    }

    // An empty history makes a settled account fully effective and a pending one fully pending.
    fn grouped(accounts: &[StakeStateV2]) -> HashMap<String, StakeAmounts> {
        group_by_validator(accounts.iter(), EPOCH, &StakeHistory::default())
    }

    fn vote() -> String {
        Pubkey::new_from_array(VOTE).to_string()
    }

    #[test]
    fn an_account_still_activating_is_kept_with_no_effective_stake() {
        let amounts = grouped(&[stake_account(500, EPOCH, u64::MAX)]);

        assert_eq!(
            amounts,
            HashMap::from([(
                vote(),
                StakeAmounts {
                    effective: 0,
                    activating: 500,
                    deactivating: 0
                }
            )])
        );
    }

    #[test]
    fn a_deactivating_account_counts_as_effective_and_deactivating() {
        let amounts = grouped(&[stake_account(500, 0, EPOCH)]);

        assert_eq!(
            amounts,
            HashMap::from([(
                vote(),
                StakeAmounts {
                    effective: 500,
                    activating: 0,
                    deactivating: 500
                }
            )])
        );
    }

    #[test]
    fn accounts_on_one_validator_sum_per_amount() {
        let amounts = grouped(&[
            stake_account(500, 0, u64::MAX),
            stake_account(200, EPOCH, u64::MAX),
            stake_account(300, 0, EPOCH),
        ]);

        assert_eq!(
            amounts,
            HashMap::from([(
                vote(),
                StakeAmounts {
                    effective: 800,
                    activating: 200,
                    deactivating: 300
                }
            )])
        );
    }

    #[test]
    fn a_fully_deactivated_account_adds_no_entry() {
        assert!(grouped(&[stake_account(500, 0, EPOCH - 1)]).is_empty());
    }

    #[test]
    fn the_effective_view_drops_a_validator_with_only_activating_stake() {
        let effective = effective_only(grouped(&[stake_account(500, EPOCH, u64::MAX)]));

        assert!(effective.is_empty());
    }

    #[test]
    fn the_effective_view_keeps_the_effective_sum_only() {
        let effective = effective_only(grouped(&[
            stake_account(500, 0, u64::MAX),
            stake_account(200, EPOCH, u64::MAX),
        ]));

        assert_eq!(effective, HashMap::from([(vote(), 500)]));
    }
}
