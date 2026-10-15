use std::collections::HashMap;

use log::info;
use serde::{Deserialize, Serialize};
use solana_rpc_client::rpc_client::RpcClient;
use solana_sdk::clock::Epoch;
use solana_sdk::pubkey::Pubkey;

use crate::slot_params::{get_slot_time_activations, select_slot_params};

/// agave `MAX_ALPENGLOW_VOTE_ACCOUNTS`.
pub const MAX_ALPENGLOW_VOTE_ACCOUNTS: usize = 2000;

/// agave `VoteStateV4::size_of()`, the size agave takes the rent of.
const VOTE_STATE_V4_SIZE: usize = 3762;

const ALPENGLOW_GATE: Pubkey =
    Pubkey::from_str_const("A1pengvuM6JEcyNuTnMqepBKhwHE3N6PmUrdATGawhJS");
// The id of agave 4.3, which mainnet runs. agave master declares a different id.
const BLOCK_REVENUE_SHARING_GATE: Pubkey =
    Pubkey::from_str_const("B1ockRevenueSharing111111111111111111111111");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VatParams {
    pub minimum_balance: u64,
    pub subtract_pending_delegator_rewards: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VatAdmissionReason {
    NoBlsKey,
    NoStake,
    LowBalance,
    BelowStakeCutoff,
}

impl VatAdmissionReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            VatAdmissionReason::NoBlsKey => "no_bls_key",
            VatAdmissionReason::NoStake => "no_stake",
            VatAdmissionReason::LowBalance => "low_balance",
            VatAdmissionReason::BelowStakeCutoff => "below_stake_cutoff",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct VatAdmissionInput {
    pub stake: u64,
    pub lamports: u64,
    pub pending_delegator_rewards: u64,
    pub has_bls_pubkey: bool,
}

/// The parameters of the filter at the end of `epoch`, agave `minimum_vote_account_balance_for_vat`.
pub fn get_vat_params(client: &RpcClient, epoch: Epoch) -> anyhow::Result<VatParams> {
    let rent = client.get_minimum_balance_for_rent_exemption(VOTE_STATE_V4_SIZE)?;
    let gates = client.get_multiple_accounts(&[ALPENGLOW_GATE, BLOCK_REVENUE_SHARING_GATE])?;
    let mut active = Vec::with_capacity(gates.len());
    for (gate_id, account) in [ALPENGLOW_GATE, BLOCK_REVENUE_SHARING_GATE]
        .iter()
        .zip(gates)
    {
        let activated = match account {
            None => false,
            Some(account) => solana_feature_gate_interface::from_account(&account)
                .ok_or_else(|| anyhow::anyhow!("Account {gate_id} is not a feature account"))?
                .activated_at
                .is_some(),
        };
        active.push(activated);
    }
    let (alpenglow, block_revenue_sharing) = (active[0], active[1]);

    // agave burns the VAT with the slot params of the first slot after the boundary.
    let vat =
        select_slot_params(&get_slot_time_activations(client)?, epoch + 1).vat_lamports_per_epoch;
    let minimum_balance = if alpenglow { rent + vat } else { rent };
    info!(
        "VAT admission: minimum balance {minimum_balance} (rent {rent}, VAT {vat}, alpenglow {alpenglow}), pending delegator rewards subtracted {block_revenue_sharing}"
    );

    Ok(VatParams {
        minimum_balance,
        subtract_pending_delegator_rewards: block_revenue_sharing,
    })
}

/// agave `VoteAccounts::clone_and_filter_for_vat`. None is admitted.
pub fn compute_vat_admission(
    inputs: &HashMap<String, VatAdmissionInput>,
    params: VatParams,
) -> HashMap<String, Option<VatAdmissionReason>> {
    let verdicts = filter_for_vat(inputs, params, MAX_ALPENGLOW_VOTE_ACCOUNTS);
    let mut counts: HashMap<&'static str, usize> = HashMap::new();
    for reason in verdicts.values() {
        *counts
            .entry(reason.map_or("admitted", |r| r.as_str()))
            .or_default() += 1;
    }
    info!("VAT admission verdicts: {counts:?}");
    verdicts
}

fn filter_for_vat(
    inputs: &HashMap<String, VatAdmissionInput>,
    params: VatParams,
    max_vote_accounts: usize,
) -> HashMap<String, Option<VatAdmissionReason>> {
    let mut verdicts = HashMap::with_capacity(inputs.len());
    let mut passed: Vec<(&String, u64)> = Vec::new();
    for (vote_account, input) in inputs {
        let available_balance = if params.subtract_pending_delegator_rewards {
            input
                .lamports
                .saturating_sub(input.pending_delegator_rewards)
        } else {
            input.lamports
        };
        let reason = if !input.has_bls_pubkey {
            Some(VatAdmissionReason::NoBlsKey)
        } else if input.stake == 0 {
            Some(VatAdmissionReason::NoStake)
        } else if available_balance < params.minimum_balance {
            Some(VatAdmissionReason::LowBalance)
        } else {
            passed.push((vote_account, input.stake));
            None
        };
        verdicts.insert(vote_account.clone(), reason);
    }

    if passed.len() > max_vote_accounts {
        let mut stakes: Vec<u64> = passed.iter().map(|(_, stake)| *stake).collect();
        stakes.sort_unstable_by(|a, b| b.cmp(a));
        // The first truncated account: agave drops every account with this stake or less, ties included.
        let floor_stake = stakes[max_vote_accounts];
        for (vote_account, stake) in passed {
            if stake <= floor_stake {
                verdicts.insert(
                    vote_account.clone(),
                    Some(VatAdmissionReason::BelowStakeCutoff),
                );
            }
        }
    }

    verdicts
}

#[cfg(test)]
mod tests {
    use super::*;

    const PARAMS: VatParams = VatParams {
        minimum_balance: 1_000,
        subtract_pending_delegator_rewards: true,
    };

    fn input(stake: u64, lamports: u64, pending: u64, has_bls_pubkey: bool) -> VatAdmissionInput {
        VatAdmissionInput {
            stake,
            lamports,
            pending_delegator_rewards: pending,
            has_bls_pubkey,
        }
    }

    fn admitted(stake: u64) -> VatAdmissionInput {
        input(stake, 1_000, 0, true)
    }

    fn inputs(rows: &[(&str, VatAdmissionInput)]) -> HashMap<String, VatAdmissionInput> {
        rows.iter()
            .map(|(vote, input)| (vote.to_string(), *input))
            .collect()
    }

    #[test]
    fn the_first_failed_check_names_the_reason() {
        let verdicts = filter_for_vat(
            &inputs(&[
                ("ok", admitted(5)),
                ("no_key", input(0, 0, 0, false)),
                ("no_stake", input(0, 0, 0, true)),
                ("poor", input(5, 999, 0, true)),
            ]),
            PARAMS,
            10,
        );
        assert_eq!(verdicts["ok"], None);
        assert_eq!(verdicts["no_key"], Some(VatAdmissionReason::NoBlsKey));
        assert_eq!(verdicts["no_stake"], Some(VatAdmissionReason::NoStake));
        assert_eq!(verdicts["poor"], Some(VatAdmissionReason::LowBalance));
    }

    #[test]
    fn pending_delegator_rewards_count_only_once_block_revenue_sharing_is_active() {
        let rows = inputs(&[("v", input(5, 1_500, 600, true))]);
        assert_eq!(
            filter_for_vat(&rows, PARAMS, 10)["v"],
            Some(VatAdmissionReason::LowBalance)
        );
        let without = VatParams {
            subtract_pending_delegator_rewards: false,
            ..PARAMS
        };
        assert_eq!(filter_for_vat(&rows, without, 10)["v"], None);
    }

    #[test]
    fn a_balance_equal_to_the_minimum_passes() {
        let rows = inputs(&[("v", input(5, 1_600, 600, true))]);
        assert_eq!(filter_for_vat(&rows, PARAMS, 10)["v"], None);
    }

    #[test]
    fn the_cap_drops_every_account_tied_with_the_first_truncated_one() {
        let rows = inputs(&[
            ("a", admitted(30)),
            ("b", admitted(20)),
            ("c", admitted(10)),
            ("d", admitted(10)),
        ]);
        let verdicts = filter_for_vat(&rows, PARAMS, 3);
        assert_eq!(verdicts["a"], None);
        assert_eq!(verdicts["b"], None);
        assert_eq!(
            verdicts["c"],
            Some(VatAdmissionReason::BelowStakeCutoff),
            "a plain top 3 would keep one of c and d"
        );
        assert_eq!(verdicts["d"], Some(VatAdmissionReason::BelowStakeCutoff));
    }

    #[test]
    fn a_tie_that_spans_the_cut_off_drops_the_admitted_side_too() {
        let rows = inputs(&[
            ("a", admitted(10)),
            ("b", admitted(10)),
            ("c", admitted(10)),
        ]);
        let verdicts = filter_for_vat(&rows, PARAMS, 2);
        assert!(verdicts
            .values()
            .all(|v| *v == Some(VatAdmissionReason::BelowStakeCutoff)));
    }

    #[test]
    fn the_cap_ranks_only_the_accounts_that_passed_the_checks() {
        let rows = inputs(&[
            ("a", admitted(30)),
            ("b", admitted(20)),
            ("big_but_poor", input(100, 0, 0, true)),
        ]);
        let verdicts = filter_for_vat(&rows, PARAMS, 2);
        assert_eq!(verdicts["a"], None);
        assert_eq!(verdicts["b"], None);
        assert_eq!(
            verdicts["big_but_poor"],
            Some(VatAdmissionReason::LowBalance)
        );
    }

    #[test]
    fn reasons_serialize_as_the_stored_strings() {
        for reason in [
            VatAdmissionReason::NoBlsKey,
            VatAdmissionReason::NoStake,
            VatAdmissionReason::LowBalance,
            VatAdmissionReason::BelowStakeCutoff,
        ] {
            assert_eq!(
                serde_json::to_value(reason).unwrap(),
                serde_json::Value::from(reason.as_str())
            );
        }
    }
}
