pub mod sandwich;
pub mod slow_slots;

use super::config::{RuleId, RulesConfig};
use std::collections::{BTreeMap, HashMap};

/// One account a rule lists at the evaluated epoch. `evidence` holds observed values only:
/// `GET /blacklist` is public and the thresholds are not.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub vote_account: String,
    pub code: String,
    pub epochs: Vec<u64>,
    pub evidence: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct SlotTime {
    pub vote_account: String,
    pub slot_duration_median_ms: f64,
}

#[derive(Debug, Clone)]
pub struct SandwichRow {
    pub vote_account: String,
    pub blocks_produced: u64,
    pub blocks_with_sandwiches: u64,
    pub sandwich_rate_30d: f64,
    pub wide_sandwich_rate_30d: Option<f64>,
}

/// Everything the rules see for epoch `epoch`. Per-epoch maps cover the longest rule window.
#[derive(Debug, Default)]
pub struct RuleInputs {
    pub epoch: u64,
    pub slot_times: BTreeMap<u64, Vec<SlotTime>>,
    pub stakes: BTreeMap<u64, HashMap<String, u64>>,
    pub sandwiches: Vec<SandwichRow>,
}

/// A stored dataset a rule reads. An epoch is judged only once every input of every rule is there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Input {
    SlotTimes,
    Stakes,
    Sandwiches,
}

/// A rule is pure over `RuleInputs`; adding one is a module, an id and a line in `registry`.
pub trait BlacklistRule {
    fn id(&self) -> RuleId;

    fn inputs(&self) -> &'static [Input];

    /// Closed epochs ending at the evaluated one that the rule reads.
    fn epochs_window(&self) -> u64 {
        1
    }

    fn evaluate(&self, inputs: &RuleInputs) -> Vec<Hit>;
}

/// First epoch of a `window` of epochs ending at `epoch`.
pub fn window_start(epoch: u64, window: u64) -> u64 {
    epoch.saturating_sub(window.saturating_sub(1))
}

/// For each input any rule reads, the longest window it is read over.
pub fn required_inputs(rules: &[Box<dyn BlacklistRule>]) -> BTreeMap<Input, u64> {
    let mut required = BTreeMap::new();
    for rule in rules {
        for input in rule.inputs() {
            let window = required.entry(*input).or_insert(0);
            *window = rule.epochs_window().max(*window);
        }
    }
    required
}

pub fn registry(rules: &RulesConfig) -> Vec<Box<dyn BlacklistRule>> {
    let mut registered: Vec<Box<dyn BlacklistRule>> = Vec::new();
    if let Some(config) = &rules.slow_slots {
        registered.push(Box::new(slow_slots::SlowSlots::from(config)));
    }
    if let Some(config) = &rules.sandwich {
        registered.push(Box::new(sandwich::Sandwich::from(config)));
    }
    registered
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blacklist::config::BlacklistConfig;

    fn rules(yaml: &str) -> RulesConfig {
        BlacklistConfig::parse(yaml).unwrap().rules.unwrap()
    }

    #[test]
    fn a_rule_without_a_config_section_is_off() {
        let ids: Vec<RuleId> = registry(&rules("rules: { sandwich: { rate_pct: 4 } }"))
            .iter()
            .map(|rule| rule.id())
            .collect();
        assert_eq!(ids, vec![RuleId::Sandwich]);
        assert!(registry(&rules("rules: { start_epoch: 1 }")).is_empty());
    }

    #[test]
    fn each_input_is_required_over_the_window_of_the_rules_that_read_it() {
        let registered = registry(&rules(
            "rules: { sandwich: { rate_pct: 4 }, slow_slots: { median_mult: 1.1, consecutive_epochs: 3 } }",
        ));
        assert_eq!(
            required_inputs(&registered).into_iter().collect::<Vec<_>>(),
            vec![
                (Input::SlotTimes, 3),
                (Input::Stakes, 3),
                (Input::Sandwiches, 1)
            ]
        );
    }

    #[test]
    fn a_rule_that_is_off_requires_nothing() {
        let registered = registry(&rules("rules: { sandwich: { rate_pct: 4 } }"));
        assert_eq!(
            required_inputs(&registered).into_iter().collect::<Vec<_>>(),
            vec![(Input::Sandwiches, 1)]
        );
    }

    #[test]
    fn the_window_starts_its_length_minus_one_before_the_epoch() {
        assert_eq!(window_start(1080, 2), 1079);
        assert_eq!(window_start(1080, 1), 1080);
        assert_eq!(window_start(0, 3), 0);
    }
}
