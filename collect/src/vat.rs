use log::info;
use solana_clock::Epoch;
use solana_rpc_client::rpc_client::RpcClient;
use std::collections::HashMap;

use crate::slot_params::{select_slot_params, SlotParams};

/// agave `VoteStateV4::size_of()`, the size agave takes the rent of.
const VOTE_STATE_V4_SIZE: usize = 3762;

/// The agave SIMD-0357 cap on the voting set.
pub const MAX_VAT_ADMITTED_VOTE_ACCOUNTS: usize = 2000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VatMinimumBalance {
    pub rent: u64,
    pub vat: u64,
}

impl VatMinimumBalance {
    pub fn total(&self) -> u64 {
        self.rent + self.vat
    }
}

/// agave `minimum_vote_account_balance_for_vat` at the end of `epoch`, with the VAT counted
/// also before Alpenglow activates.
pub fn get_vat_minimum_balance(
    client: &RpcClient,
    slot_time_activations: &[(SlotParams, Option<Epoch>)],
    epoch: Epoch,
) -> anyhow::Result<VatMinimumBalance> {
    let rent = client.get_minimum_balance_for_rent_exemption(VOTE_STATE_V4_SIZE)?;
    // agave burns the VAT with the slot params of the first slot after the boundary.
    let vat = select_slot_params(slot_time_activations, epoch + 1).vat_lamports_per_epoch;
    info!("VAT minimum balance at the end of epoch {epoch}: rent {rent} + VAT {vat}");
    Ok(VatMinimumBalance { rent, vat })
}

/// The first check of the agave VAT filter that drops the vote account.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum VatRejection {
    NoBlsKey,
    NoStake,
    LowBalance,
    BelowCutoff,
}

impl VatRejection {
    pub fn as_str(&self) -> &'static str {
        match self {
            VatRejection::NoBlsKey => "no_bls_key",
            VatRejection::NoStake => "no_stake",
            VatRejection::LowBalance => "low_balance",
            VatRejection::BelowCutoff => "below_cutoff",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct VatCandidate {
    pub has_bls_pubkey: bool,
    /// Stake projected to the next epoch boundary.
    pub stake: u64,
    pub lamports: u64,
    /// 0 while block revenue sharing is off.
    pub pending_delegator_rewards: u64,
}

/// The agave VAT filter. `None` means admitted.
pub fn vat_admissions(
    candidates: &HashMap<String, VatCandidate>,
    minimum_balance: u64,
) -> HashMap<String, Option<VatRejection>> {
    vat_admissions_with_max(candidates, minimum_balance, MAX_VAT_ADMITTED_VOTE_ACCOUNTS)
}

fn vat_admissions_with_max(
    candidates: &HashMap<String, VatCandidate>,
    minimum_balance: u64,
    max_vote_accounts: usize,
) -> HashMap<String, Option<VatRejection>> {
    let mut verdicts = HashMap::with_capacity(candidates.len());
    let mut passed: Vec<(&String, u64)> = Vec::new();
    for (vote_account, candidate) in candidates {
        let rejection = if !candidate.has_bls_pubkey {
            VatRejection::NoBlsKey
        } else if candidate.stake == 0 {
            VatRejection::NoStake
        } else if candidate
            .lamports
            .saturating_sub(candidate.pending_delegator_rewards)
            < minimum_balance
        {
            VatRejection::LowBalance
        } else {
            passed.push((vote_account, candidate.stake));
            continue;
        };
        verdicts.insert(vote_account.clone(), Some(rejection));
    }

    // agave drops every account with stake at or below the first truncated one, ties included.
    let floor_stake = (passed.len() > max_vote_accounts).then(|| {
        let mut stakes: Vec<u64> = passed.iter().map(|(_, stake)| *stake).collect();
        stakes.sort_unstable_by(|a, b| b.cmp(a));
        stakes[max_vote_accounts]
    });
    for (vote_account, stake) in passed {
        let rejection = floor_stake
            .filter(|floor_stake| stake <= *floor_stake)
            .map(|_| VatRejection::BelowCutoff);
        verdicts.insert(vote_account.clone(), rejection);
    }
    verdicts
}

/// Epochs of VAT the balance pays before it falls below rent, at the current VAT.
pub fn vat_runway_epochs(
    lamports: u64,
    pending_delegator_rewards: u64,
    minimum_balance: &VatMinimumBalance,
) -> Option<u64> {
    lamports
        .saturating_sub(pending_delegator_rewards)
        .saturating_sub(minimum_balance.rent)
        .checked_div(minimum_balance.vat)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMUM: u64 = 1_000;

    fn candidate(has_bls_pubkey: bool, stake: u64, lamports: u64) -> VatCandidate {
        VatCandidate {
            has_bls_pubkey,
            stake,
            lamports,
            pending_delegator_rewards: 0,
        }
    }

    fn admit(
        candidates: &[(&str, VatCandidate)],
        max: usize,
    ) -> HashMap<String, Option<VatRejection>> {
        let candidates = candidates
            .iter()
            .map(|(vote_account, candidate)| (vote_account.to_string(), *candidate))
            .collect();
        vat_admissions_with_max(&candidates, MINIMUM, max)
    }

    #[test]
    fn each_check_gives_its_own_reason_in_the_agave_order() {
        let verdicts = admit(
            &[
                ("admitted", candidate(true, 10, MINIMUM)),
                ("no_bls", candidate(false, 0, 0)),
                ("no_stake", candidate(true, 0, 0)),
                ("low_balance", candidate(true, 10, MINIMUM - 1)),
            ],
            10,
        );
        assert_eq!(verdicts["admitted"], None);
        assert_eq!(verdicts["no_bls"], Some(VatRejection::NoBlsKey));
        assert_eq!(verdicts["no_stake"], Some(VatRejection::NoStake));
        assert_eq!(verdicts["low_balance"], Some(VatRejection::LowBalance));
    }

    #[test]
    fn the_balance_check_leaves_out_the_pending_delegator_rewards() {
        let verdicts = admit(
            &[(
                "pending",
                VatCandidate {
                    pending_delegator_rewards: 1,
                    ..candidate(true, 10, MINIMUM)
                },
            )],
            10,
        );
        assert_eq!(verdicts["pending"], Some(VatRejection::LowBalance));
    }

    #[test]
    fn the_cutoff_drops_the_accounts_tied_with_the_first_truncated_one() {
        let verdicts = admit(
            &[
                ("top", candidate(true, 30, MINIMUM)),
                ("tied_a", candidate(true, 20, MINIMUM)),
                ("tied_b", candidate(true, 20, MINIMUM)),
                ("low", candidate(true, 10, MINIMUM)),
                ("not_ranked", candidate(true, 5, 0)),
            ],
            2,
        );
        assert_eq!(verdicts["top"], None);
        assert_eq!(verdicts["tied_a"], Some(VatRejection::BelowCutoff));
        assert_eq!(verdicts["tied_b"], Some(VatRejection::BelowCutoff));
        assert_eq!(verdicts["low"], Some(VatRejection::BelowCutoff));
        assert_eq!(verdicts["not_ranked"], Some(VatRejection::LowBalance));
    }

    #[test]
    fn one_more_equal_stake_than_the_max_admits_none() {
        let candidates: Vec<(String, VatCandidate)> = (0..=MAX_VAT_ADMITTED_VOTE_ACCOUNTS)
            .map(|i| (i.to_string(), candidate(true, 7, MINIMUM)))
            .collect();
        let candidates: Vec<(&str, VatCandidate)> = candidates
            .iter()
            .map(|(vote_account, candidate)| (vote_account.as_str(), *candidate))
            .collect();
        let verdicts = admit(&candidates, MAX_VAT_ADMITTED_VOTE_ACCOUNTS);
        assert!(verdicts
            .values()
            .all(|verdict| *verdict == Some(VatRejection::BelowCutoff)));
    }

    #[test]
    fn exactly_the_max_admits_all() {
        let verdicts = admit(
            &[
                ("a", candidate(true, 7, MINIMUM)),
                ("b", candidate(true, 7, MINIMUM)),
            ],
            2,
        );
        assert!(verdicts.values().all(Option::is_none));
    }

    #[test]
    fn the_runway_counts_whole_epochs_above_rent_less_pending_rewards() {
        let minimum = VatMinimumBalance { rent: 100, vat: 50 };
        assert_eq!(vat_runway_epochs(299, 0, &minimum), Some(3));
        assert_eq!(vat_runway_epochs(299, 100, &minimum), Some(1));
        assert_eq!(vat_runway_epochs(50, 0, &minimum), Some(0));
        assert_eq!(
            vat_runway_epochs(299, 0, &VatMinimumBalance { rent: 100, vat: 0 }),
            None
        );
    }
}
