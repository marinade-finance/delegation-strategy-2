use anyhow::{bail, ensure, Context};
use collect::common::is_base58_pubkey;
use serde::Deserialize;
use std::collections::HashSet;

/// Hand-kept adds and exemptions. The rules and their thresholds belong to the SAM auction.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlacklistConfig {
    #[serde(default)]
    pub manual: Vec<ManualEntry>,
    #[serde(default)]
    pub exempt: Vec<ExemptEntry>,
}

/// What an exemption covers: the listings of one auction rule, the legacy rows, or all of them.
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

    /// The code prefix of the auction rows this rule lists; `None` for `all` and `legacy`.
    pub fn code_prefix(self) -> Option<&'static str> {
        match self {
            RuleId::SlowSlots => Some("SLOW_SLOTS_"),
            RuleId::Sandwich => Some("MALICIOUS_SM_"),
            RuleId::All | RuleId::Legacy => None,
        }
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
    /// Listings whose code names this epoch or an earlier one are exempt, e.g. 1084 for
    /// `SLOW_SLOTS_1084`. Absent → exempt for as long as the entry exists.
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
manual:
  # internal note
  - { vote_account: BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB, code: MISSING_BLS, reason: 'SIMD-0357, epoch 1006' }
exempt:
  - { vote_account: BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB, rule: all, until_epoch: null, reason: 'appeal false positive' }
  - { vote_account: AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA, rule: slow_slots, until_epoch: 1084, reason: 'fixed client' }
  - { vote_account: LLLLLLLLLLLLLLLLLLLLLLLLLLLLLLLL, rule: legacy, reason: 'imported row no longer valid' }
";

    fn error(yaml: &str) -> String {
        format!("{:#}", BlacklistConfig::parse(yaml).unwrap_err())
    }

    #[test]
    fn the_documented_example_parses() {
        let config = BlacklistConfig::parse(EXAMPLE).unwrap();
        assert_eq!(config.manual.len(), 1);
        assert_eq!(
            config.exempt.iter().map(|e| e.rule).collect::<Vec<_>>(),
            vec![RuleId::All, RuleId::SlowSlots, RuleId::Legacy]
        );
    }

    #[test]
    fn an_empty_config_has_empty_lists() {
        let config = BlacklistConfig::parse("manual: []\nexempt: []\n").unwrap();
        assert!(config.manual.is_empty() && config.exempt.is_empty());
        assert!(BlacklistConfig::parse("{}").unwrap().manual.is_empty());
    }

    // The thresholds moved to the SAM auction; a leftover rules section must not pass silently.
    #[test]
    fn a_rules_section_is_refused() {
        assert!(error("rules: { start_epoch: 1090 }").contains("unknown field"));
    }

    #[test]
    fn each_rule_names_the_code_prefix_of_its_listings() {
        assert_eq!(RuleId::Sandwich.code_prefix(), Some("MALICIOUS_SM_"));
        assert_eq!(RuleId::SlowSlots.code_prefix(), Some("SLOW_SLOTS_"));
        assert_eq!(RuleId::All.code_prefix(), None);
        assert_eq!(RuleId::Legacy.code_prefix(), None);
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
