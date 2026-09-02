use crate::dto::{BlockProductionDetail, ValidatorEpochStats, ValidatorRecord};
use std::collections::HashMap;

/// Leader slots an epoch needs before its block production is judged at all. Below this there is
/// not enough signal; covers 99.2% of stake.
pub const MIN_LEADER_SLOTS: u64 = 64;

/// Missed slots an epoch needs before it counts as an incident. Solana assigns leader slots in
/// batches of 4, so a sub-turn miss is usually the cluster following a different fork.
pub const MIN_MISSED_SLOTS: u64 = 4;

/// The bar in a healthy epoch: produce 99% of assigned leader slots.
pub const MIN_SKIP_RATE_THRESHOLD: f64 = 0.01;

/// How far above the cluster's own skip rate the bar sits while the network is degraded.
pub const CLUSTER_SKIP_RATE_MULTIPLIER: f64 = 10.0;

/// Ceiling on the bar, so a degraded network can never push it somewhere non-physical. Uncapped,
/// the 500-699 era would have moved it past 100%.
pub const MAX_SKIP_RATE_THRESHOLD: f64 = 0.05;

/// The skip rate an epoch's validators had to stay under to pass.
pub fn threshold(cluster_skip_rate: f64) -> f64 {
    (CLUSTER_SKIP_RATE_MULTIPLIER * cluster_skip_rate)
        .clamp(MIN_SKIP_RATE_THRESHOLD, MAX_SKIP_RATE_THRESHOLD)
}

/// Total missed slots over total leader slots per epoch, across the validators that were evaluable
/// that epoch. Epochs where nobody reached `MIN_LEADER_SLOTS` are left out.
pub fn cluster_skip_rates<'a>(
    records: impl IntoIterator<Item = &'a ValidatorRecord>,
) -> HashMap<u64, f64> {
    let mut totals: HashMap<u64, (u64, u64)> = Default::default();

    for stats in records.into_iter().flat_map(|record| &record.epoch_stats) {
        if stats.leader_slots < MIN_LEADER_SLOTS {
            continue;
        }
        let (leader_slots, blocks_produced) = totals.entry(stats.epoch).or_default();
        *leader_slots += stats.leader_slots;
        // A node reporting more blocks than slots is upstream noise; it must not mint slots here.
        *blocks_produced += stats.blocks_produced.min(stats.leader_slots);
    }

    totals
        .into_iter()
        .filter(|(_, (leader_slots, _))| *leader_slots > 0)
        .map(|(epoch, (leader_slots, blocks_produced))| {
            (
                epoch,
                (leader_slots - blocks_produced) as f64 / leader_slots as f64,
            )
        })
        .collect()
}

/// How one epoch breached the block production rule. `None` where it passed, where the validator
/// held too few leader slots to be judged, or where the epoch has no cluster figure to judge it
/// against.
pub fn breach(
    stats: &ValidatorEpochStats,
    cluster_skip_rates: &HashMap<u64, f64>,
) -> Option<BlockProductionDetail> {
    let cluster_skip_rate = cluster_skip_rates.get(&stats.epoch).copied()?;
    if stats.leader_slots < MIN_LEADER_SLOTS {
        return None;
    }

    let missed_slots = stats.leader_slots.saturating_sub(stats.blocks_produced);
    if missed_slots < MIN_MISSED_SLOTS {
        return None;
    }

    let skip_rate = missed_slots as f64 / stats.leader_slots as f64;
    let threshold = threshold(cluster_skip_rate);
    if skip_rate < threshold {
        return None;
    }

    Some(BlockProductionDetail {
        leader_slots: stats.leader_slots,
        blocks_produced: stats.blocks_produced,
        missed_slots,
        skip_rate,
        cluster_skip_rate,
        threshold,
        cluster_skip_rate_multiple: (cluster_skip_rate > 0.0)
            .then(|| skip_rate / cluster_skip_rate),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats(epoch: u64, leader_slots: u64, blocks_produced: u64) -> ValidatorEpochStats {
        ValidatorEpochStats {
            epoch,
            leader_slots,
            blocks_produced,
            ..Default::default()
        }
    }

    fn validator(epoch_stats: Vec<ValidatorEpochStats>) -> ValidatorRecord {
        ValidatorRecord {
            epoch_stats,
            ..Default::default()
        }
    }

    fn breached(
        leader_slots: u64,
        blocks_produced: u64,
        cluster_skip_rate: f64,
    ) -> Option<BlockProductionDetail> {
        breach(
            &stats(100, leader_slots, blocks_produced),
            &HashMap::from([(100, cluster_skip_rate)]),
        )
    }

    #[test]
    fn threshold_sits_at_one_percent_in_a_healthy_epoch() {
        assert_eq!(threshold(0.0), 0.01);
        assert_eq!(threshold(0.000_5), 0.01);
    }

    #[test]
    fn threshold_scales_with_a_degraded_cluster_up_to_the_cap() {
        // Epoch 1015: a 0.413% cluster skip rate lifted the bar to 4.13%.
        assert!((threshold(0.004_13) - 0.041_3).abs() < 1e-12);
        assert_eq!(threshold(0.5), MAX_SKIP_RATE_THRESHOLD);
    }

    #[test]
    fn an_epoch_under_the_leader_slot_gate_is_not_evaluated() {
        assert!(breached(63, 0, 0.0).is_none());
    }

    #[test]
    fn an_epoch_with_no_cluster_figure_is_not_evaluated() {
        assert!(breach(&stats(100, 64, 0), &HashMap::new()).is_none());
    }

    #[test]
    fn a_sub_leader_turn_miss_is_not_an_incident() {
        // 3 of 64 is 4.7%, well over the bar, but under one full leader turn.
        assert!(breached(64, 61, 0.0).is_none());
    }

    #[test]
    fn a_full_leader_turn_missed_over_the_bar_is_an_incident() {
        let detail = breached(64, 60, 0.0).unwrap();

        assert_eq!(detail.missed_slots, 4);
        assert_eq!(detail.leader_slots, 64);
        assert_eq!(detail.threshold, 0.01);
        assert_eq!(detail.cluster_skip_rate_multiple, None);
    }

    #[test]
    fn an_epoch_under_the_bar_is_no_incident_even_with_a_full_turn_missed() {
        // 4 of 800 is 0.5%, under the 1% bar.
        assert!(breached(800, 796, 0.0).is_none());
    }

    #[test]
    fn a_degraded_cluster_lifts_the_bar_out_from_under_a_validator() {
        // 2% skipped: an incident in a healthy epoch, not in one the cluster skipped 0.413% of.
        assert!(breached(1000, 980, 0.0).is_some());
        assert!(breached(1000, 980, 0.004_13).is_none());
    }

    #[test]
    fn the_cap_keeps_the_bar_physical_when_the_cluster_is_badly_degraded() {
        // Uncapped, 10 x 3.5% would be a 35% bar and this epoch would read as all clear.
        let detail = breached(1000, 940, 0.035).unwrap();
        assert_eq!(detail.threshold, MAX_SKIP_RATE_THRESHOLD);
    }

    #[test]
    fn cluster_skip_rate_reads_only_the_evaluable_validators() {
        let validators = [
            validator(vec![stats(100, 1000, 990)]),
            validator(vec![stats(100, 1000, 990)]),
            // Under the gate: its 100% skip rate must not move the cluster figure.
            validator(vec![stats(100, 32, 0)]),
        ];
        assert_eq!(cluster_skip_rates(&validators).get(&100), Some(&0.01));
    }

    #[test]
    fn an_epoch_nobody_was_evaluable_in_has_no_cluster_skip_rate() {
        assert!(cluster_skip_rates(&[validator(vec![stats(100, 32, 0)])]).is_empty());
    }
}
