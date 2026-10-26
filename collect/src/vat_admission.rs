use std::collections::HashMap;

use log::info;
use serde::{Deserialize, Serialize};
use solana_rpc_client::rpc_client::RpcClient;
use solana_rpc_client_api::response::RpcVoteAccountStatus;
use solana_sdk::clock::Epoch;
use solana_sdk::pubkey::Pubkey;

use crate::slot_params::get_slot_time_activations;
use crate::solana_service::VoteStateFields;
use crate::vat::get_vat_minimum_balance;

/// agave `MAX_ALPENGLOW_VOTE_ACCOUNTS`.
pub const MAX_ALPENGLOW_VOTE_ACCOUNTS: usize = 2000;

// Placeholder id for the revenue sharing feature gate, yet to be announced.
const BLOCK_REVENUE_SHARING_GATE: Pubkey =
    Pubkey::from_str_const("B1ockRevenueSharing111111111111111111111111");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct VatParams {
    minimum_balance: u64,
    subtract_pending_delegator_rewards: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
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

#[derive(Clone, Debug)]
struct VatCandidate {
    vote_account: String,
    stake: u64,
    lamports: u64,
    pending_delegator_rewards: u64,
    has_bls_pubkey: bool,
}

/// agave `VoteAccounts::clone_and_filter_for_vat`. None is admitted.
/// The verdicts at the end of `epoch`.
pub fn compute_vat_admission(
    client: &RpcClient,
    epoch: Epoch,
    vote_accounts: &RpcVoteAccountStatus,
    vote_account_states: &HashMap<String, VoteStateFields>,
) -> anyhow::Result<HashMap<String, Option<VatAdmissionReason>>> {
    let minimum_balance =
        get_vat_minimum_balance(client, &get_slot_time_activations(client)?, epoch)?;
    let block_revenue_sharing = match client
        .get_account_with_commitment(&BLOCK_REVENUE_SHARING_GATE, client.commitment())?
        .value
    {
        None => false,
        Some(account) => solana_feature_gate_interface::from_account(&account)
            .ok_or_else(|| {
                anyhow::anyhow!("Account {BLOCK_REVENUE_SHARING_GATE} is not a feature account")
            })?
            .activated_at
            .is_some(),
    };
    info!("VAT admission: pending delegator rewards subtracted {block_revenue_sharing}");
    let params = VatParams {
        minimum_balance,
        subtract_pending_delegator_rewards: block_revenue_sharing,
    };

    // Only accounts with a known vote state version and balance take part, so the rest get no verdict.
    let candidates = vote_accounts
        .current
        .iter()
        .chain(vote_accounts.delinquent.iter())
        .filter_map(|vote_account| {
            let state = vote_account_states.get(&vote_account.vote_pubkey)?;
            state.inflation_rewards_commission_bps_is_v4?;
            Some(VatCandidate {
                vote_account: vote_account.vote_pubkey.clone(),
                stake: vote_account.activated_stake,
                lamports: state.lamports?,
                pending_delegator_rewards: state.pending_delegator_rewards.unwrap_or(0),
                has_bls_pubkey: state.bls_pubkey.is_some(),
            })
        });
    let verdicts = filter_for_vat(candidates, params, MAX_ALPENGLOW_VOTE_ACCOUNTS);
    let mut counts: HashMap<&'static str, usize> = HashMap::new();
    for reason in verdicts.values() {
        *counts
            .entry(reason.map_or("admitted", |r| r.as_str()))
            .or_default() += 1;
    }
    info!("VAT admission verdicts: {counts:?}");
    Ok(verdicts)
}

fn filter_for_vat(
    candidates: impl IntoIterator<Item = VatCandidate>,
    params: VatParams,
    max_vote_accounts: usize,
) -> HashMap<String, Option<VatAdmissionReason>> {
    let mut checked: Vec<(VatCandidate, Option<VatAdmissionReason>)> = candidates
        .into_iter()
        .map(|candidate| {
            let available_balance = if params.subtract_pending_delegator_rewards {
                candidate
                    .lamports
                    .saturating_sub(candidate.pending_delegator_rewards)
            } else {
                candidate.lamports
            };
            let reason = if !candidate.has_bls_pubkey {
                Some(VatAdmissionReason::NoBlsKey)
            } else if candidate.stake == 0 {
                Some(VatAdmissionReason::NoStake)
            } else if available_balance < params.minimum_balance {
                Some(VatAdmissionReason::LowBalance)
            } else {
                None
            };
            (candidate, reason)
        })
        .collect();

    let mut passed_stakes: Vec<u64> = checked
        .iter()
        .filter(|(_, reason)| reason.is_none())
        .map(|(candidate, _)| candidate.stake)
        .collect();
    if passed_stakes.len() > max_vote_accounts {
        passed_stakes.sort_unstable_by(|a, b| b.cmp(a));
        // The first truncated account: agave drops every account with this stake or less, ties included.
        let floor_stake = passed_stakes[max_vote_accounts];
        for (candidate, reason) in &mut checked {
            if reason.is_none() && candidate.stake <= floor_stake {
                *reason = Some(VatAdmissionReason::BelowStakeCutoff);
            }
        }
    }

    checked
        .into_iter()
        .map(|(candidate, reason)| (candidate.vote_account, reason))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use solana_rpc_client_api::request::RpcRequest;
    use solana_rpc_client_api::response::RpcVoteAccountInfo;

    const PARAMS: VatParams = VatParams {
        minimum_balance: 1_000,
        subtract_pending_delegator_rewards: true,
    };

    fn candidate(
        vote_account: &str,
        stake: u64,
        lamports: u64,
        pending: u64,
        has_bls_pubkey: bool,
    ) -> VatCandidate {
        VatCandidate {
            vote_account: vote_account.to_string(),
            stake,
            lamports,
            pending_delegator_rewards: pending,
            has_bls_pubkey,
        }
    }

    fn admitted(vote_account: &str, stake: u64) -> VatCandidate {
        candidate(vote_account, stake, 1_000, 0, true)
    }

    #[test]
    fn the_first_failed_check_names_the_reason() {
        let verdicts = filter_for_vat(
            vec![
                admitted("ok", 5),
                candidate("no_key", 0, 0, 0, false),
                candidate("no_stake", 0, 0, 0, true),
                candidate("poor", 5, 999, 0, true),
            ],
            PARAMS,
            10,
        );
        assert_eq!(verdicts["ok"], None);
        assert_eq!(verdicts["no_key"], Some(VatAdmissionReason::NoBlsKey));
        assert_eq!(verdicts["no_stake"], Some(VatAdmissionReason::NoStake));
        assert_eq!(verdicts["poor"], Some(VatAdmissionReason::LowBalance));
    }

    #[test]
    fn a_balance_equal_to_the_minimum_passes() {
        let rows = vec![candidate("v", 5, 1_600, 600, true)];
        assert_eq!(filter_for_vat(rows, PARAMS, 10)["v"], None);
    }

    #[test]
    fn the_cap_drops_every_account_tied_with_the_first_truncated_one() {
        let rows = vec![
            admitted("a", 30),
            admitted("b", 20),
            admitted("c", 10),
            admitted("d", 10),
        ];
        let verdicts = filter_for_vat(rows, PARAMS, 3);
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
        let rows = vec![admitted("a", 10), admitted("b", 10), admitted("c", 10)];
        let verdicts = filter_for_vat(rows, PARAMS, 2);
        assert!(verdicts
            .values()
            .all(|v| *v == Some(VatAdmissionReason::BelowStakeCutoff)));
    }

    #[test]
    fn the_cap_ranks_only_the_accounts_that_passed_the_checks() {
        let rows = vec![
            admitted("a", 30),
            admitted("b", 20),
            candidate("big_but_poor", 100, 0, 0, true),
        ];
        let verdicts = filter_for_vat(rows, PARAMS, 2);
        assert_eq!(verdicts["a"], None);
        assert_eq!(verdicts["b"], None);
        assert_eq!(
            verdicts["big_but_poor"],
            Some(VatAdmissionReason::LowBalance)
        );
    }

    fn vote_account(vote_pubkey: &str) -> RpcVoteAccountInfo {
        RpcVoteAccountInfo {
            vote_pubkey: vote_pubkey.to_string(),
            node_pubkey: String::new(),
            activated_stake: 5,
            commission: 0,
            epoch_vote_account: true,
            epoch_credits: vec![],
            last_vote: 0,
            root_slot: 0,
        }
    }

    const RENT: u64 = 20;
    // Rent plus the VAT at 400 ms slots, with no slot time gate active.
    const MINIMUM_BALANCE: u64 = RENT + 1_600_000_000;

    fn mock_client(block_revenue_sharing: bool) -> RpcClient {
        // A feature account activated at slot 5.
        let gate = block_revenue_sharing.then(|| {
            json!({
                "lamports": 1,
                "data": ["AQUAAAAAAAAA", "base64"],
                "owner": "Feature111111111111111111111111111111111111",
                "executable": false,
                "rentEpoch": 0,
                "space": 9,
            })
        });
        RpcClient::new_mock_with_mocks(
            "succeeds",
            HashMap::from([
                (RpcRequest::GetMinimumBalanceForRentExemption, json!(RENT)),
                (
                    RpcRequest::GetEpochSchedule,
                    json!({
                        "slotsPerEpoch": 432_000,
                        "leaderScheduleSlotOffset": 432_000,
                        "warmup": false,
                        "firstNormalEpoch": 0,
                        "firstNormalSlot": 0,
                    }),
                ),
                (
                    RpcRequest::GetMultipleAccounts,
                    json!({"context": {"slot": 1}, "value": [null, null, null, null]}),
                ),
                (
                    RpcRequest::GetAccountInfo,
                    json!({"context": {"slot": 1}, "value": gate}),
                ),
            ]),
        )
    }

    fn vote_state(is_v4: Option<bool>, lamports: Option<u64>) -> VoteStateFields {
        VoteStateFields {
            authorized_withdrawer: Pubkey::default(),
            inflation_rewards_commission_bps: Some(0),
            inflation_rewards_commission_bps_is_v4: is_v4,
            inflation_rewards_collector: None,
            block_revenue_collector: None,
            block_revenue_commission_bps: None,
            pending_delegator_rewards: is_v4.filter(|v4| *v4).map(|_| 0),
            bls_pubkey: is_v4.filter(|v4| *v4).map(|_| "key".to_string()),
            lamports,
        }
    }

    #[test]
    fn only_vote_accounts_with_a_known_version_and_balance_get_a_verdict() {
        let vote_accounts = RpcVoteAccountStatus {
            current: [
                "current",
                "pre_v4",
                "unknown_version",
                "no_lamports",
                "no_state",
            ]
            .map(vote_account)
            .to_vec(),
            delinquent: vec![vote_account("delinquent")],
        };
        let vote_account_states = HashMap::from([
            (
                "current".to_string(),
                vote_state(Some(true), Some(MINIMUM_BALANCE)),
            ),
            (
                "delinquent".to_string(),
                vote_state(Some(true), Some(MINIMUM_BALANCE)),
            ),
            (
                "pre_v4".to_string(),
                vote_state(Some(false), Some(MINIMUM_BALANCE)),
            ),
            (
                "unknown_version".to_string(),
                vote_state(None, Some(MINIMUM_BALANCE)),
            ),
            ("no_lamports".to_string(), vote_state(Some(true), None)),
        ]);
        assert_eq!(
            compute_vat_admission(&mock_client(false), 1, &vote_accounts, &vote_account_states)
                .unwrap(),
            HashMap::from([
                ("current".to_string(), None),
                ("delinquent".to_string(), None),
                ("pre_v4".to_string(), Some(VatAdmissionReason::NoBlsKey)),
            ])
        );
    }

    #[test]
    fn the_block_revenue_sharing_gate_subtracts_pending_delegator_rewards() {
        let vote_accounts = RpcVoteAccountStatus {
            current: vec![vote_account("v")],
            delinquent: vec![],
        };
        let mut state = vote_state(Some(true), Some(MINIMUM_BALANCE));
        state.pending_delegator_rewards = Some(1);
        let vote_account_states = HashMap::from([("v".to_string(), state)]);
        let verdict = |gate| {
            compute_vat_admission(&mock_client(gate), 1, &vote_accounts, &vote_account_states)
                .unwrap()["v"]
        };
        assert_eq!(verdict(false), None);
        assert_eq!(verdict(true), Some(VatAdmissionReason::LowBalance));
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
