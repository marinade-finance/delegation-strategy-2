use super::{BlacklistRule, Hit, RuleInputs};
use crate::blacklist::config::{RuleId, SlowSlotsConfig};
use serde_json::json;
use std::collections::HashMap;

/// Slot-time median over `median_mult` × the stake-weighted network median, in each of
/// `consecutive_epochs` closed epochs, each against its own median.
pub struct SlowSlots {
    median_mult: f64,
    consecutive_epochs: u64,
}

impl From<&SlowSlotsConfig> for SlowSlots {
    fn from(config: &SlowSlotsConfig) -> Self {
        SlowSlots {
            median_mult: config.median_mult,
            consecutive_epochs: config.consecutive_epochs,
        }
    }
}

/// The value at which half of the stake sits at or below. `None` without any stake.
pub fn stake_weighted_median(values: impl IntoIterator<Item = (f64, u64)>) -> Option<f64> {
    let mut values: Vec<(f64, u64)> = values.into_iter().filter(|(_, s)| *s > 0).collect();
    let total: u128 = values.iter().map(|(_, stake)| *stake as u128).sum();
    if total == 0 {
        return None;
    }
    values.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut cumulative = 0u128;
    for (value, stake) in values {
        cumulative += stake as u128;
        if 2 * cumulative >= total {
            return Some(value);
        }
    }
    None
}

impl BlacklistRule for SlowSlots {
    fn id(&self) -> RuleId {
        RuleId::SlowSlots
    }

    fn epochs_window(&self) -> u64 {
        self.consecutive_epochs
    }

    fn evaluate(&self, inputs: &RuleInputs) -> Vec<Hit> {
        let epochs: Vec<u64> = (inputs.epoch + 1 - self.consecutive_epochs.min(inputs.epoch + 1)
            ..=inputs.epoch)
            .collect();
        let empty = HashMap::new();

        let mut medians = Vec::with_capacity(epochs.len());
        for epoch in &epochs {
            let Some(slot_times) = inputs.slot_times.get(epoch) else {
                return vec![];
            };
            let stakes = inputs.stakes.get(epoch).unwrap_or(&empty);
            let weighted = slot_times.iter().map(|slot_time| {
                (
                    slot_time.slot_duration_median_ms,
                    stakes.get(&slot_time.vote_account).copied().unwrap_or(0),
                )
            });
            let Some(median) = stake_weighted_median(weighted) else {
                return vec![];
            };
            medians.push(median);
        }

        let per_epoch: Vec<HashMap<&str, f64>> = epochs
            .iter()
            .map(|epoch| {
                inputs.slot_times[epoch]
                    .iter()
                    .map(|t| (t.vote_account.as_str(), t.slot_duration_median_ms))
                    .collect()
            })
            .collect();

        let mut hits: Vec<Hit> = inputs.slot_times[&inputs.epoch]
            .iter()
            .filter_map(|slot_time| {
                let account = slot_time.vote_account.as_str();
                let mut evidence = serde_json::Map::new();
                for ((epoch, values), median) in epochs.iter().zip(&per_epoch).zip(&medians) {
                    let value = *values.get(account)?;
                    if value <= self.median_mult * median {
                        return None;
                    }
                    evidence.insert(
                        epoch.to_string(),
                        json!({"slot_duration_median_ms": value, "network_median_ms": median}),
                    );
                }
                Some(Hit {
                    vote_account: account.to_string(),
                    code: format!("SLOW_SLOTS_{}", inputs.epoch),
                    epochs: epochs.clone(),
                    evidence: json!({ "epochs": evidence }),
                })
            })
            .collect();
        hits.sort_by(|a, b| a.vote_account.cmp(&b.vote_account));
        hits
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blacklist::rules::SlotTime;

    const E: u64 = 1080;

    fn rule() -> SlowSlots {
        SlowSlots {
            median_mult: 1.10,
            consecutive_epochs: 2,
        }
    }

    /// `(vote_account, slot ms, stake)`.
    type Row<'a> = (&'a str, f64, u64);

    fn inputs(epochs: &[(u64, &[Row])]) -> RuleInputs {
        let mut inputs = RuleInputs {
            epoch: E,
            ..Default::default()
        };
        for (epoch, rows) in epochs {
            inputs.slot_times.insert(
                *epoch,
                rows.iter()
                    .map(|(account, ms, _)| SlotTime {
                        vote_account: account.to_string(),
                        slot_duration_median_ms: *ms,
                    })
                    .collect(),
            );
            inputs.stakes.insert(
                *epoch,
                rows.iter()
                    .filter(|(_, _, stake)| *stake > 0)
                    .map(|(account, _, stake)| (account.to_string(), *stake))
                    .collect(),
            );
        }
        inputs
    }

    fn hit_accounts(hits: &[Hit]) -> Vec<&str> {
        hits.iter().map(|hit| hit.vote_account.as_str()).collect()
    }

    const NETWORK: [(&str, f64, u64); 3] =
        [("n1", 200.0, 100), ("n2", 200.0, 100), ("n3", 205.0, 100)];

    fn epoch_rows(extra: &[(&'static str, f64, u64)]) -> Vec<(&'static str, f64, u64)> {
        NETWORK.iter().chain(extra).copied().collect()
    }

    #[test]
    fn the_median_is_weighted_by_stake() {
        assert_eq!(
            stake_weighted_median([(200.0, 90), (300.0, 10)]),
            Some(200.0)
        );
        assert_eq!(
            stake_weighted_median([(200.0, 10), (300.0, 90)]),
            Some(300.0)
        );
        assert_eq!(stake_weighted_median([(200.0, 0)]), None);
    }

    #[test]
    fn two_consecutive_epochs_over_their_own_bars_hit() {
        let previous = epoch_rows(&[("slow", 245.0, 10)]);
        let current = epoch_rows(&[("slow", 250.0, 10)]);
        let hits = rule().evaluate(&inputs(&[(E - 1, &previous), (E, &current)]));
        assert_eq!(hit_accounts(&hits), vec!["slow"]);
    }

    #[test]
    fn one_epoch_over_the_bar_does_not_hit() {
        let previous = epoch_rows(&[("slow", 215.0, 10)]);
        let current = epoch_rows(&[("slow", 250.0, 10)]);
        assert!(rule()
            .evaluate(&inputs(&[(E - 1, &previous), (E, &current)]))
            .is_empty());
    }

    #[test]
    fn a_value_at_the_bar_does_not_hit() {
        let rows = epoch_rows(&[("edge", 220.0, 10)]);
        assert!(rule()
            .evaluate(&inputs(&[(E - 1, &rows), (E, &rows)]))
            .is_empty());
    }

    #[test]
    fn a_missing_prior_epoch_does_not_hit() {
        let current = epoch_rows(&[("slow", 250.0, 10)]);
        assert!(rule().evaluate(&inputs(&[(E, &current)])).is_empty());
    }

    #[test]
    fn a_validator_absent_in_one_epoch_does_not_hit() {
        let previous = epoch_rows(&[]);
        let current = epoch_rows(&[("slow", 250.0, 10)]);
        assert!(rule()
            .evaluate(&inputs(&[(E - 1, &previous), (E, &current)]))
            .is_empty());
    }

    #[test]
    fn a_validator_without_stake_is_judged_but_not_weighted() {
        let rows = epoch_rows(&[("unstaked", 400.0, 0), ("unstaked2", 400.0, 0)]);
        let hits = rule().evaluate(&inputs(&[(E - 1, &rows), (E, &rows)]));
        assert_eq!(hit_accounts(&hits), vec!["unstaked", "unstaked2"]);
        assert_eq!(
            hits[0].evidence["epochs"][E.to_string()]["network_median_ms"],
            200.0
        );
    }

    #[test]
    fn the_code_carries_the_evaluated_epoch() {
        let rows = epoch_rows(&[("slow", 250.0, 10)]);
        let hits = rule().evaluate(&inputs(&[(E - 1, &rows), (E, &rows)]));
        assert_eq!(hits[0].code, "SLOW_SLOTS_1080");
        assert_eq!(hits[0].epochs, vec![E - 1, E]);
    }

    #[test]
    fn evidence_holds_observed_values_only() {
        let rows = epoch_rows(&[("slow", 250.0, 10)]);
        let hit = &rule().evaluate(&inputs(&[(E - 1, &rows), (E, &rows)]))[0];
        let epochs = hit.evidence["epochs"].as_object().unwrap();
        assert_eq!(epochs.len(), 2);
        for value in epochs.values() {
            let mut keys: Vec<&String> = value.as_object().unwrap().keys().collect();
            keys.sort();
            assert_eq!(keys, vec!["network_median_ms", "slot_duration_median_ms"]);
        }
        assert_eq!(hit.evidence.as_object().unwrap().len(), 1);
    }
}
