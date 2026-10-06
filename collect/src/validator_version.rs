//! A client version as the node reports it in gossip, parsed enough to be ordered.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::cmp::Ordering;
use std::fmt;
use std::str::FromStr;

/// Three numeric components and an optional prerelease, e.g. `4.2.2`, `4.3.0-rc.0`,
/// `0.1106.40201`, `26.8.2`.
///
/// Ordering is by the numbers, not the text: `0.1106.40201` is above `0.812.30108`, which a string
/// comparison gets backwards. A prerelease sorts below the release it leads to. Folding a
/// prerelease into its release is a compliance question, not an ordering one, and does not belong
/// here.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ValidatorVersion {
    /// The gossip spelling: a GitHub tag's `v` prefix and zero padding are gone, so one version has
    /// one spelling wherever it is stored.
    text: String,
    numbers: [u64; 3],
    /// Empty for a release.
    prerelease: Vec<Identifier>,
}

/// One dot-separated part of a prerelease. Semver orders a numeric part by value and puts it below
/// an alphanumeric one, so `beta.9` sits below `beta.10` where a text compare has it above.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Identifier {
    Numeric(u64),
    Text(String),
}

impl Identifier {
    fn parse(part: &str) -> Self {
        match part.parse() {
            Ok(number) => Identifier::Numeric(number),
            Err(_) => Identifier::Text(part.to_string()),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct NotAVersion(String);

impl fmt::Display for NotAVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?} is not a client version", self.0)
    }
}

impl std::error::Error for NotAVersion {}

impl FromStr for ValidatorVersion {
    type Err = NotAVersion;

    fn from_str(version: &str) -> Result<Self, Self::Err> {
        let refuse = || NotAVersion(version.to_string());
        let trimmed = version.trim().trim_start_matches('v');

        let mut parts = trimmed.splitn(3, '.');
        let major = parts.next().ok_or_else(refuse)?;
        let minor = parts.next().ok_or_else(refuse)?;
        let last = parts.next().ok_or_else(refuse)?;
        let (patch, prerelease) = match last.split_once('-') {
            Some((patch, prerelease)) => (patch, prerelease),
            None => (last, ""),
        };

        let number = |part: &str| part.parse::<u64>().map_err(|_| refuse());
        let numbers = [number(major)?, number(minor)?, number(patch)?];

        // Anything a node could not have reported: an empty prerelease from a trailing dash, or one
        // carrying characters gossip versions never have.
        if last.contains('-')
            && (prerelease.is_empty()
                || !prerelease
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'.'))
        {
            return Err(refuse());
        }

        let text = match prerelease {
            "" => format!("{}.{}.{}", numbers[0], numbers[1], numbers[2]),
            prerelease => format!("{}.{}.{}-{prerelease}", numbers[0], numbers[1], numbers[2]),
        };

        Ok(Self {
            text,
            numbers,
            prerelease: prerelease
                .split('.')
                .filter(|part| !part.is_empty())
                .map(Identifier::parse)
                .collect(),
        })
    }
}

impl ValidatorVersion {
    /// What a node reported in gossip, which never carries a tag's `v` prefix: one there means the
    /// string is not a version at all and must not be stored over a good one.
    pub fn from_gossip(version: &str) -> Result<Self, NotAVersion> {
        if version.trim_start().starts_with('v') {
            return Err(NotAVersion(version.to_string()));
        }
        version.parse()
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }

    pub fn major(&self) -> u64 {
        self.numbers[0]
    }
}

impl Ord for ValidatorVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        self.numbers.cmp(&other.numbers).then_with(|| {
            // A release has no prerelease and stands above every prerelease of itself.
            self.prerelease
                .is_empty()
                .cmp(&other.prerelease.is_empty())
                .then_with(|| self.prerelease.cmp(&other.prerelease))
        })
    }
}

impl PartialOrd for ValidatorVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for ValidatorVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

impl Serialize for ValidatorVersion {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.text)
    }
}

impl<'de> Deserialize<'de> for ValidatorVersion {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(text: &str) -> ValidatorVersion {
        text.parse().unwrap()
    }

    #[test]
    fn parsing_gives_the_gossip_spelling() {
        // Live gossip: Firedancer says 26.8.2 where its tag zero-pads to v26.08.2.
        assert_eq!(version("v26.08.2").as_str(), "26.8.2");
        assert_eq!(version("v4.2.2").as_str(), "4.2.2");
        assert_eq!(version("0.1106.40201").as_str(), "0.1106.40201");
        assert_eq!(version("  v4.0.0-rc.0 ").as_str(), "4.0.0-rc.0");
        // A Frankendancer prerelease encodes an agave version that never shipped; it survives as
        // written.
        assert_eq!(
            version("v0.905.0-beta.40007").as_str(),
            "0.905.0-beta.40007"
        );
    }

    #[test]
    fn two_spellings_of_one_version_are_equal() {
        assert_eq!(version("v26.08.2"), version("26.8.2"));
    }

    #[test]
    fn ordering_follows_the_numbers_not_the_text() {
        assert!(version("0.1106.40201") > version("0.812.30108"));
        assert!(version("4.10.0") > version("4.2.2"));
        assert!(version("4.2.2") > version("4.2.1"));
    }

    #[test]
    fn a_prerelease_counter_orders_by_value() {
        // A text compare puts beta.10 below beta.9 and drops a floor step with it.
        assert!(version("4.2.0-beta.10") > version("4.2.0-beta.9"));
        assert!(version("4.2.0-rc.10") > version("4.2.0-rc.2"));
        // The Frankendancer spelling, where the prerelease carries the agave number.
        assert!(version("0.905.0-beta.40007") > version("0.905.0-beta.9"));
        // Semver puts a numeric identifier below an alphanumeric one.
        assert!(version("4.2.0-1") < version("4.2.0-alpha"));
    }

    #[test]
    fn a_prerelease_sits_below_its_release() {
        assert!(version("4.2.0-beta.1") < version("4.2.0-rc.1"));
        assert!(version("4.2.0-rc.1") < version("4.2.0"));
        assert!(version("4.2.0") < version("4.2.1"));
    }

    #[test]
    fn what_a_node_could_not_have_reported_is_refused() {
        for text in [
            "4.2",        // a release line, not a release
            "v2.1",       // the same, as the tracker writes it
            "0.403",      // and as it writes Frankendancer's
            "4.2.x",      // not a number
            "4.2.2-",     // a dash with nothing after it
            "4.2.2-rc 1", // a space gossip would never carry
            "",           //
            "unknown",    //
            "4.2.2.1",    // the fourth component lands in the prerelease slot and is not one
        ] {
            assert!(
                text.parse::<ValidatorVersion>().is_err(),
                "{text:?} should be refused"
            );
        }
    }

    #[test]
    fn a_tag_prefix_is_a_tag_not_gossip() {
        assert_eq!(version("v4.1.0").as_str(), "4.1.0");
        assert!(ValidatorVersion::from_gossip("v4.1.0").is_err());
        assert!(ValidatorVersion::from_gossip("4.1.0").is_ok());
    }

    #[test]
    fn a_custom_build_parses_and_sorts_above_every_release() {
        // Operators build with a huge minor; nothing here may reject or mis-sort them.
        assert!(version("4.32768.0") > version("4.3.0"));
    }

    #[test]
    fn it_round_trips_through_serde() {
        let json = serde_json::to_string(&version("v26.08.2")).unwrap();
        assert_eq!(json, "\"26.8.2\"");
        assert_eq!(
            serde_json::from_str::<ValidatorVersion>(&json).unwrap(),
            version("26.8.2")
        );
        assert!(serde_json::from_str::<ValidatorVersion>("\"4.2\"").is_err());
    }
}
