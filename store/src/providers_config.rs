use anyhow::{bail, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::OnceLock;

const DEFAULT_YAML: &str = include_str!("../../providers-config.yaml");

static CONFIG: OnceLock<ProvidersConfig> = OnceLock::new();

#[derive(Debug, Deserialize)]
struct File {
    #[serde(default)]
    asn_groups: Option<HashMap<u32, Vec<u32>>>,
}

#[derive(Debug, Default)]
pub struct ProvidersConfig {
    /// Every ASN in the file, mapped to its group's parent. A parent maps to itself.
    asn_aliases: HashMap<u32, u32>,
}

/// Replaces the vendored config for the whole process. Has to run before the first lookup.
pub fn install_from_path(path: &str) -> Result<()> {
    let yaml = std::fs::read_to_string(path)?;
    let config = parse(&yaml)?;
    if CONFIG.set(config).is_err() {
        bail!("a providers config is already in use, so this one would not be read");
    }

    Ok(())
}

fn config() -> &'static ProvidersConfig {
    CONFIG.get_or_init(|| {
        parse(DEFAULT_YAML).unwrap_or_else(|err| {
            panic!("parse the vendored providers-config.yaml failed: {err:#}")
        })
    })
}

fn parse(yaml: &str) -> Result<ProvidersConfig> {
    let file: File = serde_yaml::from_str(yaml)?;
    let groups = file.asn_groups.unwrap_or_default();

    let mut asn_aliases: HashMap<u32, u32> =
        groups.keys().map(|parent| (*parent, *parent)).collect();
    if asn_aliases.contains_key(&0) {
        bail!("ASN 0 is reserved");
    }

    for (parent, members) in &groups {
        for member in members {
            if *member == 0 {
                bail!("ASN 0 is reserved");
            }
            if member == parent {
                bail!("ASN {member} lists itself");
            }
            if asn_aliases.get(member) == Some(member) {
                bail!("ASN {member} is a parent, so {parent} cannot claim it");
            }
            if let Some(claimed) = asn_aliases.insert(*member, *parent) {
                bail!("ASN {member} is claimed by both {claimed} and {parent}");
            }
        }
    }

    Ok(ProvidersConfig { asn_aliases })
}

// `validators.dc_asn` is `i32`, so an ASN above 2^31 is stored negative. The cast round-trips it,
// so the config always spells the real number.
fn lookup(config: &ProvidersConfig, asn: Option<i32>) -> Option<u32> {
    config.asn_aliases.get(&(asn? as u32)).copied()
}

/// The group parent of `asn`; `None` when the config does not name it.
pub fn asn_group_of(asn: Option<i32>) -> Option<u32> {
    #[cfg(test)]
    if let Some(found) =
        OVERRIDE.with(|cell| cell.borrow().as_ref().map(|config| lookup(config, asn)))
    {
        return found;
    }

    lookup(config(), asn)
}

#[cfg(test)]
thread_local! {
    /// One per test thread, so an override reaches neither another test nor the `OnceLock`.
    static OVERRIDE: std::cell::RefCell<Option<ProvidersConfig>> =
        const { std::cell::RefCell::new(None) };
}

/// Runs `body` against `yaml` instead of the installed config.
#[cfg(test)]
pub fn with_config<T>(yaml: &str, body: impl FnOnce() -> T) -> T {
    let config = parse(yaml).expect("the test config parses");
    OVERRIDE.with(|cell| *cell.borrow_mut() = Some(config));
    let result = body();
    OVERRIDE.with(|cell| *cell.borrow_mut() = None);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_vendored_config_groups_latitude_under_its_heavier_asn() {
        let config = parse(DEFAULT_YAML).expect("providers-config.yaml parses");

        assert_eq!(lookup(&config, Some(396356)), Some(396356));
        assert_eq!(lookup(&config, Some(262287)), Some(396356));
    }

    #[test]
    fn a_key_left_empty_groups_nothing() {
        assert!(parse("asn_groups:\n").unwrap().asn_aliases.is_empty());
        assert!(parse("asn_groups: {}\n").unwrap().asn_aliases.is_empty());
    }

    #[test]
    fn members_and_the_parent_resolve_to_the_parent() {
        let config = parse("asn_groups:\n  24940:\n    - 213230\n    - 212317\n").unwrap();

        assert_eq!(lookup(&config, Some(24940)), Some(24940));
        assert_eq!(lookup(&config, Some(213230)), Some(24940));
        assert_eq!(lookup(&config, Some(212317)), Some(24940));
        assert_eq!(lookup(&config, Some(16509)), None);
        assert_eq!(lookup(&config, None), None);
    }

    #[test]
    fn a_parent_with_no_members_resolves_to_itself() {
        let config = parse("asn_groups:\n  24940: []\n").unwrap();

        assert_eq!(lookup(&config, Some(24940)), Some(24940));
    }

    #[test]
    fn an_asn_above_the_i32_range_round_trips_the_stored_column() {
        let config = parse("asn_groups:\n  4200000000:\n    - 4200000001\n").unwrap();

        assert_eq!(
            lookup(&config, Some(4200000001u32 as i32)),
            Some(4200000000)
        );
    }

    #[test]
    fn a_name_where_an_asn_belongs_is_rejected() {
        assert!(parse("asn_groups:\n  Hetzner:\n    - 213230\n").is_err());
    }

    #[test]
    fn a_self_referencing_group_is_rejected() {
        let err = parse("asn_groups:\n  24940:\n    - 24940\n")
            .unwrap_err()
            .to_string();

        assert!(err.contains("24940 lists itself"), "{err}");
    }

    #[test]
    fn a_member_that_is_a_parent_of_its_own_is_rejected() {
        let err = parse("asn_groups:\n  24940:\n    - 16509\n  16509:\n    - 14618\n")
            .unwrap_err()
            .to_string();

        assert!(err.contains("16509 is a parent"), "{err}");
    }

    #[test]
    fn a_member_claimed_twice_is_rejected() {
        let err = parse("asn_groups:\n  24940:\n    - 213230\n  16509:\n    - 213230\n")
            .unwrap_err()
            .to_string();

        assert!(err.contains("213230 is claimed by both"), "{err}");
    }

    #[test]
    fn asn_zero_is_rejected() {
        assert!(parse("asn_groups:\n  0:\n    - 24940\n").is_err());
        assert!(parse("asn_groups:\n  24940:\n    - 0\n").is_err());
    }
}
