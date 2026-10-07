use crate::utils::{
    expected_take_rate, to_fixed_for_sort, worst_known_commission, InflationApyCalculator,
    RewardMixShares, SECONDS_IN_YEAR,
};

#[test]
fn to_fixed_for_sort_scales_usable_values() {
    assert_eq!(to_fixed_for_sort(0.0), Some(0));
    assert_eq!(to_fixed_for_sort(0.05), Some(500));
    assert_eq!(to_fixed_for_sort(1.0), Some(10_000));
}

#[test]
fn to_fixed_for_sort_rejects_values_that_would_saturate_to_zero() {
    assert_eq!(to_fixed_for_sort(-0.01), None);
    assert_eq!(to_fixed_for_sort(f64::NAN), None);
    assert_eq!(to_fixed_for_sort(f64::NEG_INFINITY), None);
}

#[test]
fn to_fixed_for_sort_rejects_values_that_would_saturate_to_max() {
    assert_eq!(to_fixed_for_sort(f64::INFINITY), None);
    assert_eq!(to_fixed_for_sort(f64::MAX), None);
    assert_eq!(to_fixed_for_sort(1e30), None);
}

const BASELINE_SLOTS_PER_YEAR: f64 = 78_892_314.984;
const SLOTS_PER_YEAR_350MS: f64 = 90_162_645.696;

const CREDITS: u64 = 400_000;

/// Mainnet-scale: 600M SOL supply, ~400k credits per validator, ~400M SOL staked cluster-wide.
fn calculator(slots_per_year: f64) -> InflationApyCalculator {
    InflationApyCalculator {
        supply: 600_000_000_000_000_000,
        duration: 182_400,
        inflation: 0.043,
        slots_per_year,
        total_weighted_credits: Some(160_000_000_000_000_000_000_000),
    }
}

// testnet 54Rwic4DqGK5NL48HyP65TCKLn6YDN5zaERSJFVQSQJB, epoch 1047, 2% commission
#[test]
fn alpenglow_yields_come_from_the_vote_reward() {
    let alpenglow = InflationApyCalculator {
        total_weighted_credits: None,
        ..calculator(BASELINE_SLOTS_PER_YEAR)
    };
    let (apr, apy) = alpenglow.yields_from_vote_reward(3_728_113_744_092, 3_343_226_367_743_619, 2);

    let rate_per_epoch = 0.98 * 3_728_113_744_092.0 / 3_343_226_367_743_619.0;
    let epochs_per_year = SECONDS_IN_YEAR / alpenglow.duration as f64;
    assert_close(apr, rate_per_epoch * epochs_per_year);
    assert_close(1.0 + apy, (1.0 + rate_per_epoch).powf(epochs_per_year));
    assert_eq!(alpenglow.estimate_yields(CREDITS, 2), (0.0, 0.0));
}

/// Relative, because these quantities span 1e-2 to 1e15 and a fixed epsilon fits neither end.
fn assert_close(left: f64, right: f64) {
    assert!(
        (left - right).abs() / right.abs() < 1e-12,
        "{left} != {right}"
    );
}

fn rate_per_epoch(calculator: &InflationApyCalculator) -> f64 {
    let (apr, _) = calculator.estimate_yields(CREDITS, 5);
    apr / (SECONDS_IN_YEAR / calculator.duration as f64)
}

#[test]
fn per_epoch_issuance_tracks_the_protocol_slot_time() {
    let baseline = calculator(BASELINE_SLOTS_PER_YEAR);
    let stage_1 = calculator(SLOTS_PER_YEAR_350MS);
    let (_, apy_baseline) = baseline.estimate_yields(CREDITS, 5);
    let (_, apy_350) = stage_1.estimate_yields(CREDITS, 5);

    // Guards the fixture: an implausible one overflows to inf, where every ratio below matches.
    assert!((0.03..0.12).contains(&apy_baseline), "{apy_baseline}");

    // Shorter slots mint proportionally less per epoch, so the rate scales by exactly 350/400.
    assert_close(
        rate_per_epoch(&stage_1) / rate_per_epoch(&baseline),
        350.0 / 400.0,
    );

    let epochs_per_year = SECONDS_IN_YEAR / stage_1.duration as f64;
    assert_close(
        1.0 + apy_350,
        (1.0 + rate_per_epoch(&stage_1)).powf(epochs_per_year),
    );
    assert!(apy_350 < apy_baseline);
}

#[test]
fn measured_epoch_length_does_not_move_per_epoch_issuance() {
    let short = InflationApyCalculator {
        duration: 151_200,
        ..calculator(BASELINE_SLOTS_PER_YEAR)
    };
    let long = InflationApyCalculator {
        duration: 182_400,
        ..calculator(BASELINE_SLOTS_PER_YEAR)
    };

    // Only the compounding exponent may depend on the measured epoch, never the minted amount.
    assert_close(rate_per_epoch(&short), rate_per_epoch(&long));
}

// Roughly mainnet's mix at epoch 1015, so the numbers below read against something real.
const MIX: RewardMixShares = RewardMixShares {
    inflation: 0.90,
    mev: 0.044,
    block: 0.056,
};

fn approx(actual: Option<f64>, expected: f64) {
    let actual = actual.expect("expected a rate");
    assert!(
        (actual - expected).abs() < 1e-12,
        "expected {expected}, got {actual}"
    );
}

#[test]
fn expected_take_rate_floors_at_the_block_share_for_a_zero_fee_validator() {
    // HelixNode's shape: 0% inflation, 0% MEV, no priority-fee account. It measures ~5.6%.
    approx(expected_take_rate(MIX, Some(0), Some(0), None), MIX.block);
}

#[test]
fn expected_take_rate_reaches_one_when_every_component_is_fully_taken() {
    approx(
        expected_take_rate(MIX, Some(100), Some(10_000), Some(10_000)),
        1.0,
    );
}

#[test]
fn expected_take_rate_weights_each_commission_by_its_component() {
    approx(
        expected_take_rate(MIX, Some(5), Some(1_000), None),
        0.05 * MIX.inflation + 0.1 * MIX.mev + MIX.block,
    );
}

#[test]
fn expected_take_rate_drops_below_the_floor_when_block_rewards_are_shared() {
    // The two validators on Jito's PriorityFeeDistribution at 0 bps keep none of their priority fees.
    approx(expected_take_rate(MIX, Some(0), Some(0), Some(0)), 0.0);
}

#[test]
fn expected_take_rate_renormalizes_when_the_validator_earns_no_mev() {
    // Without Jito there is no MEV to take a cut of, so the MEV weight must leave the denominator
    // rather than count as a 0% commission and dilute the rate.
    let no_jito = expected_take_rate(MIX, Some(10), None, None);
    approx(
        no_jito,
        (0.1 * MIX.inflation + MIX.block) / (MIX.inflation + MIX.block),
    );

    let diluted = 0.1 * MIX.inflation + MIX.block;
    assert!(
        no_jito.unwrap() > diluted,
        "renormalizing must read higher than crediting a 0% MEV commission"
    );
}

#[test]
fn expected_take_rate_is_unknown_without_an_inflation_commission() {
    // Inflation rewards are earned by every validator, so a missing commission cannot renormalize away the way a missing MEV one does.
    assert_eq!(expected_take_rate(MIX, None, Some(0), Some(0)), None);
}

#[test]
fn expected_take_rate_needs_at_least_one_component_to_weigh() {
    let empty = RewardMixShares {
        inflation: 0.0,
        mev: 0.0,
        block: 0.0,
    };
    assert_eq!(expected_take_rate(empty, Some(5), Some(1_000), None), None);
}

#[test]
fn expected_take_rate_is_unknown_for_a_mix_that_has_paid_only_block_rewards() {
    // The in-progress epoch's shape: inflation and MEV pay at the boundary, block rewards accrue.
    let accruing = RewardMixShares {
        inflation: 0.0,
        mev: 0.0,
        block: 1.0,
    };
    assert_eq!(
        expected_take_rate(accruing, Some(5), None, None),
        None,
        "weighting a 5% validator by a pure block mix would read it at 100%"
    );
}

#[test]
fn worst_known_commission_prefers_an_observed_ceiling_over_a_lower_advertised_rate() {
    // Majestysol's shape: advertises 0 early in the epoch, observed at 100 when rewards are paid.
    assert_eq!(worst_known_commission(Some(100), Some(0)), Some(100));
}

#[test]
fn worst_known_commission_takes_a_rise_without_waiting_for_the_epoch_to_close() {
    assert_eq!(worst_known_commission(Some(5), Some(10)), Some(10));
}

#[test]
fn worst_known_commission_accepts_either_side_alone() {
    assert_eq!(worst_known_commission(None, Some(5)), Some(5));
    assert_eq!(worst_known_commission(Some(7), None), Some(7));
}

#[test]
fn worst_known_commission_is_unknown_only_when_neither_side_is_known() {
    assert_eq!(worst_known_commission(None, None), None);
}

#[test]
fn expected_take_rate_does_not_read_at_the_floor_for_a_commission_gamer() {
    let gamer = worst_known_commission(Some(100), Some(0));
    let genuinely_free = worst_known_commission(Some(0), Some(0));
    approx(
        expected_take_rate(MIX, gamer, Some(0), None),
        MIX.inflation + MIX.block,
    );
    approx(
        expected_take_rate(MIX, genuinely_free, Some(0), None),
        MIX.block,
    );
    assert_ne!(
        expected_take_rate(MIX, gamer, Some(0), None),
        expected_take_rate(MIX, genuinely_free, Some(0), None),
        "trusting the advertised rate is what used to tie these two together"
    );
}
