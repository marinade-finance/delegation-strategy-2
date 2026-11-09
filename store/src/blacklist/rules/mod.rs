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

/// A rule is pure over `RuleInputs`; adding one is a module, an id and a line in `registry`.
pub trait BlacklistRule {
    fn id(&self) -> RuleId;

    /// Closed epochs ending at the evaluated one that the rule reads.
    fn epochs_window(&self) -> u64 {
        1
    }

    fn evaluate(&self, inputs: &RuleInputs) -> Vec<Hit>;
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
    fn the_window_is_the_longest_rule_window() {
        let registered = registry(&rules(
            "rules: { sandwich: { rate_pct: 4 }, slow_slots: { median_mult: 1.1, consecutive_epochs: 3 } }",
        ));
        assert_eq!(
            registered.iter().map(|rule| rule.epochs_window()).max(),
            Some(3)
        );
    }
}
