use anyhow::{bail, ensure, Context};
use collect::common::is_base58_pubkey;
use serde::Deserialize;
use std::collections::HashSet;

/// Thresholds, hand-kept adds and exemptions. A rule section left out turns that rule off.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlacklistConfig {
    pub rules: Option<RulesConfig>,
    #[serde(default)]
    pub manual: Vec<ManualEntry>,
    #[serde(default)]
    pub exempt: Vec<ExemptEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RulesConfig {
    /// First epoch the rules judge. Absent → rules do not run.
    pub start_epoch: Option<u64>,
    pub slow_slots: Option<SlowSlotsConfig>,
    pub sandwich: Option<SandwichConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SlowSlotsConfig {
    /// Bar = this × the stake-weighted network median of the slot-time medians.
    pub median_mult: f64,
    pub consecutive_epochs: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SandwichConfig {
    /// Percent, the floor under the cluster-relative bar.
    pub rate_pct: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleId {
    All,
    SlowSlots,
    Sandwich,
    Legacy,
}

impl RuleId {
    pub fn as_str(self) -> &'static str {
        match self {
            RuleId::All => "all",
            RuleId::SlowSlots => "slow_slots",
            RuleId::Sandwich => "sandwich",
            RuleId::Legacy => "legacy",
        }
    }

    fn all() -> Self {
        RuleId::All
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManualEntry {
    pub vote_account: String,
    pub code: String,
    pub reason: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExemptEntry {
    pub vote_account: String,
    #[serde(default = "RuleId::all")]
    pub rule: RuleId,
    /// Evidence up to this epoch is ignored. Absent → exempt for as long as the entry exists.
    pub until_epoch: Option<u64>,
    pub reason: String,
}

impl BlacklistConfig {
    pub fn load(path: &str) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read blacklist config '{path}'"))?;
        Self::parse(&text).with_context(|| format!("Invalid blacklist config '{path}'"))
    }

    pub fn parse(yaml: &str) -> anyhow::Result<Self> {
        let config: Self = serde_yaml::from_str(yaml)?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> anyhow::Result<()> {
        if let Some(rules) = &self.rules {
            if let Some(slow_slots) = &rules.slow_slots {
                ensure!(
                    slow_slots.median_mult > 1.0,
                    "rules.slow_slots.median_mult must be above 1"
                );
                ensure!(
                    slow_slots.consecutive_epochs >= 1,
                    "rules.slow_slots.consecutive_epochs must be at least 1"
                );
            }
            if let Some(sandwich) = &rules.sandwich {
                ensure!(
                    sandwich.rate_pct > 0.0 && sandwich.rate_pct <= 100.0,
                    "rules.sandwich.rate_pct must be in (0, 100]"
                );
            }
        }

        let mut manual = HashSet::new();
        for entry in &self.manual {
            non_empty("manual", &entry.vote_account, "vote_account")?;
            non_empty("manual", &entry.code, "code")?;
            non_empty("manual", &entry.reason, "reason")?;
            vote_account_shape("manual", &entry.vote_account)?;
            code_shape(&entry.code)?;
            if !manual.insert((&entry.vote_account, &entry.code)) {
                bail!(
                    "manual: {} {} is listed twice",
                    entry.vote_account,
                    entry.code
                );
            }
        }

        let mut exempt = HashSet::new();
        for entry in &self.exempt {
            non_empty("exempt", &entry.vote_account, "vote_account")?;
            non_empty("exempt", &entry.reason, "reason")?;
            vote_account_shape("exempt", &entry.vote_account)?;
            if !exempt.insert((&entry.vote_account, entry.rule)) {
                bail!(
                    "exempt: {} {} is listed twice",
                    entry.vote_account,
                    entry.rule.as_str()
                );
            }
        }

        Ok(())
    }
}

fn non_empty(list: &str, value: &str, field: &str) -> anyhow::Result<()> {
    ensure!(
        !value.trim().is_empty(),
        "{list}: an entry has an empty {field}"
    );
    Ok(())
}

fn vote_account_shape(list: &str, vote_account: &str) -> anyhow::Result<()> {
    ensure!(
        is_base58_pubkey(vote_account),
        "{list}: {vote_account:?} is not a base58 vote account"
    );
    Ok(())
}

fn code_shape(code: &str) -> anyhow::Result<()> {
    ensure!(
        code.chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'),
        "manual: code {code:?} must match [A-Z0-9_]+"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = "
rules:
  start_epoch: 1090
  slow_slots:
    median_mult: 1.10
    consecutive_epochs: 2
  sandwich:
    rate_pct: 4.0
manual:
  # internal note
  - { vote_account: BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB, code: MISSING_BLS, reason: 'SIMD-0357, epoch 1006' }
exempt:
  - { vote_account: BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB, rule: all, until_epoch: null, reason: 'appeal false positive' }
  - { vote_account: AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA, rule: slow_slots, until_epoch: 1083, reason: 'fixed client' }
  - { vote_account: LLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLL, rule: legacy, reason: 'imported row no longer valid' }
";

    fn error(yaml: &str) -> String {
        format!("{:#}", BlacklistConfig::parse(yaml).unwrap_err())
    }

    #[test]
    fn the_documented_example_parses() {
        let config = BlacklistConfig::parse(EXAMPLE).unwrap();
        let rules = config.rules.unwrap();
        assert_eq!(rules.start_epoch, Some(1090));
        assert_eq!(rules.slow_slots.unwrap().median_mult, 1.10);
        assert_eq!(rules.sandwich.unwrap().rate_pct, 4.0);
        assert_eq!(config.manual.len(), 1);
        assert_eq!(
            config.exempt.iter().map(|e| e.rule).collect::<Vec<_>>(),
            vec![RuleId::All, RuleId::SlowSlots, RuleId::Legacy]
        );
    }

    #[test]
    fn an_empty_config_has_no_rules_and_empty_lists() {
        let config = BlacklistConfig::parse("manual: []\nexempt: []\n").unwrap();
        assert!(config.rules.is_none());
        assert!(config.manual.is_empty() && config.exempt.is_empty());
        assert!(BlacklistConfig::parse("{}").unwrap().manual.is_empty());
    }

    #[test]
    fn a_rules_section_without_rules_parses() {
        let rules = BlacklistConfig::parse("rules: { start_epoch: 1090 }")
            .unwrap()
            .rules
            .unwrap();
        assert!(rules.slow_slots.is_none() && rules.sandwich.is_none());
    }

    #[test]
    fn an_exemption_without_a_rule_covers_all() {
        let config = BlacklistConfig::parse(
            "exempt: [{ vote_account: AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA, reason: appeal }]",
        )
        .unwrap();
        assert_eq!(config.exempt[0].rule, RuleId::All);
        assert_eq!(config.exempt[0].until_epoch, None);
    }

    #[test]
    fn unknown_fields_are_refused() {
        assert!(error("rule: {}").contains("unknown field"));
        assert!(error("rules: { start_epoch: 1, slow: {} }").contains("unknown field"));
        assert!(
            error("manual: [{ vote_account: AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA, code: C, reason: R, epoch: 1 }]")
                .contains("unknown field")
        );
    }

    #[test]
    fn an_unknown_rule_id_is_refused() {
        assert!(error(
            "exempt: [{ vote_account: AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA, rule: foo, reason: R }]"
        )
        .contains("foo"));
    }

    #[test]
    fn an_empty_reason_is_refused() {
        assert!(error(
            "manual: [{ vote_account: AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA, code: C, reason: '' }]"
        )
        .contains("reason"));
        assert!(
            error("exempt: [{ vote_account: AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA, reason: ' ' }]")
                .contains("reason")
        );
    }

    #[test]
    fn a_duplicate_is_refused() {
        assert!(error(
            "manual: [{ vote_account: AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA, code: C, reason: R }, { vote_account: AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA, code: C, reason: S }]"
        )
        .contains("twice"));
        assert!(error(
            "exempt: [{ vote_account: AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA, rule: sandwich, reason: R }, { vote_account: AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA, rule: sandwich, reason: S }]"
        )
        .contains("twice"));
    }

    #[test]
    fn one_account_may_carry_several_codes_and_exemptions() {
        BlacklistConfig::parse(
            "manual: [{ vote_account: AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA, code: C, reason: R }, { vote_account: AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA, code: D, reason: R }]
exempt: [{ vote_account: AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA, rule: sandwich, reason: R }, { vote_account: AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA, rule: slow_slots, reason: R }]",
        )
        .unwrap();
    }

    #[test]
    fn thresholds_out_of_range_are_refused() {
        assert!(
            error("rules: { slow_slots: { median_mult: 1.0, consecutive_epochs: 2 } }")
                .contains("median_mult")
        );
        assert!(
            error("rules: { slow_slots: { median_mult: 1.1, consecutive_epochs: 0 } }")
                .contains("consecutive_epochs")
        );
        assert!(error("rules: { sandwich: { rate_pct: 0 } }").contains("rate_pct"));
        assert!(error("rules: { sandwich: { rate_pct: 101 } }").contains("rate_pct"));
    }

    // Both values are served unquoted in /blacklist.csv.
    #[test]
    fn a_vote_account_or_code_that_could_break_the_csv_is_refused() {
        assert!(
            error("manual: [{ vote_account: 'Bxx,evil', code: C, reason: R }]").contains("base58")
        );
        assert!(error("exempt: [{ vote_account: short, reason: R }]").contains("base58"));
        assert!(error(
            "manual: [{ vote_account: AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA, code: 'MISSING,BLS', reason: R }]"
        )
        .contains("[A-Z0-9_]+"));
        assert!(error(
            "manual: [{ vote_account: AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA, code: missing_bls, reason: R }]"
        )
        .contains("[A-Z0-9_]+"));
    }

    #[test]
    fn a_missing_file_names_its_path() {
        let error = BlacklistConfig::load("/nonexistent/blacklist.yaml").unwrap_err();
        assert!(format!("{error:#}").contains("/nonexistent/blacklist.yaml"));
    }
}
