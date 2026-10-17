use crate::close_epoch::{resolve_commission_effective, SampledCommission};
use crate::dto::{COMMISSION_EFFECTIVE_SOURCE_REWARD_ROW, COMMISSION_EFFECTIVE_SOURCE_VOTE_STATE};

#[test]
fn a_reward_row_still_wins_so_closed_epochs_reprocess_unchanged() {
    assert_eq!(
        resolve_commission_effective(Some(7), Some(SampledCommission::Bps(300))),
        (Some(7), None, Some(COMMISSION_EFFECTIVE_SOURCE_REWARD_ROW))
    );
}

#[test]
fn a_missing_reward_row_falls_back_to_the_sampled_vote_state() {
    assert_eq!(
        resolve_commission_effective(None, Some(SampledCommission::Bps(700))),
        (
            Some(7),
            Some(700),
            Some(COMMISSION_EFFECTIVE_SOURCE_VOTE_STATE)
        )
    );
}

#[test]
fn the_fallback_rounds_basis_points_up_so_the_eligibility_cap_stays_strict() {
    assert_eq!(
        resolve_commission_effective(None, Some(SampledCommission::Bps(1_001))).0,
        Some(11)
    );
    assert_eq!(
        resolve_commission_effective(None, Some(SampledCommission::Bps(1_000))).0,
        Some(10)
    );
    assert_eq!(
        resolve_commission_effective(None, Some(SampledCommission::Bps(1_001))).1,
        Some(1_001)
    );
    assert_eq!(
        resolve_commission_effective(None, Some(SampledCommission::Bps(25_600))).0,
        Some(100)
    );
}

#[test]
fn an_unparsed_vote_state_resolves_from_its_advertised_percent_without_bps() {
    assert_eq!(
        resolve_commission_effective(None, Some(SampledCommission::Percent(7))),
        (Some(7), None, Some(COMMISSION_EFFECTIVE_SOURCE_VOTE_STATE))
    );
}

#[test]
fn neither_source_leaves_the_rate_unknown_rather_than_zero() {
    assert_eq!(resolve_commission_effective(None, None), (None, None, None));
}

#[test]
fn a_genuine_zero_is_resolved_not_missing() {
    assert_eq!(
        resolve_commission_effective(None, Some(SampledCommission::Bps(0))),
        (
            Some(0),
            Some(0),
            Some(COMMISSION_EFFECTIVE_SOURCE_VOTE_STATE)
        )
    );
    assert_eq!(
        resolve_commission_effective(Some(0), None),
        (Some(0), None, Some(COMMISSION_EFFECTIVE_SOURCE_REWARD_ROW))
    );
}
