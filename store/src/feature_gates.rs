//! Feature-gate floors: the client version the cluster required, and the epoch it started
//! requiring it. Below one of these a validator forks off.
//!
//! Static rather than collected: no API serves these, and they move a handful of times a year.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

const DEFAULT_CSV: &str = include_str!("../release_feature_gates.csv");

const HEADER: [&str; 4] = [
    "client_lineage",
    "client_version",
    "effective_epoch",
    "announced_epoch",
];

#[derive(Deserialize, Serialize, Debug, Clone, utoipa::ToSchema)]
pub struct FeatureGateFloor {
    /// `agave`, `frankendancer`, `firedancer` or `sig`.
    pub client_lineage: String,
    /// As the client reports it in gossip, e.g. `4.2.0-beta.1` or `0.1102.40201`.
    pub client_version: String,
    /// First epoch the floor was in force.
    pub effective_epoch: Option<u64>,
    /// Epoch the raise was announced in.
    pub announced_epoch: u64,
}

fn floors() -> &'static Vec<FeatureGateFloor> {
    static FLOORS: OnceLock<Vec<FeatureGateFloor>> = OnceLock::new();
    FLOORS.get_or_init(|| match std::env::var_os("FEATURE_GATES_CSV") {
        Some(path) => {
            let csv = std::fs::read_to_string(&path)
                .unwrap_or_else(|err| panic!("read {path:?} failed: {err}"));
            parse(&csv).unwrap_or_else(|err| panic!("parse {path:?} failed: {err:#}"))
        }
        None => parse(DEFAULT_CSV)
            .unwrap_or_else(|err| panic!("parse release_feature_gates.csv failed: {err:#}")),
    })
}

/// `#` starts a comment; the file's own header block carries its provenance.
fn parse(csv: &str) -> Result<Vec<FeatureGateFloor>> {
    let mut reader = csv::ReaderBuilder::new()
        .comment(Some(b'#'))
        .from_reader(csv.as_bytes());

    let header = reader.headers().context("reading header")?.clone();
    // Without this a headerless file loses its first row to the header, dropping one floor.
    if header.iter().collect::<Vec<_>>() != HEADER {
        bail!("first line must be the {} header", HEADER.join(","));
    }

    let mut parsed = Vec::new();
    for record in reader.records() {
        let record = record.context("reading record")?;
        let field = |index: usize| record.get(index).unwrap_or_default().trim();
        let optional = |index: usize| Some(field(index)).filter(|value| !value.is_empty());

        let (client_lineage, client_version) = (field(0), field(1));
        if client_lineage.is_empty() || client_version.is_empty() {
            bail!("{record:?} does not name a client lineage and a version");
        }

        let epoch = |value: Option<&str>, what: &str| -> Result<Option<u64>> {
            value
                .map(|value| {
                    value
                        .parse()
                        .with_context(|| format!("{record:?} has an unparseable {what}"))
                })
                .transpose()
        };

        parsed.push(FeatureGateFloor {
            client_lineage: client_lineage.to_string(),
            client_version: client_version.to_string(),
            effective_epoch: epoch(optional(2), "effective_epoch")?,
            announced_epoch: epoch(optional(3), "announced_epoch")?
                .context("announced_epoch is required")?,
        });
    }

    Ok(parsed)
}

pub fn all() -> &'static [FeatureGateFloor] {
    floors()
}

/// The floor in force for one lineage at one epoch: the latest one to take effect by then.
pub fn floor_at_epoch(client_lineage: &str, epoch: u64) -> Option<&'static FeatureGateFloor> {
    floors()
        .iter()
        .filter(|floor| floor.client_lineage == client_lineage)
        .filter(|floor| {
            floor
                .effective_epoch
                .is_some_and(|effective| effective <= epoch)
        })
        .max_by_key(|floor| floor.effective_epoch)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_vendored_floors_parse() {
        assert_eq!(all().len(), 11);
        assert!(all().iter().all(|floor| floor.effective_epoch.is_some()));
    }

    #[test]
    fn the_floor_in_force_is_the_latest_one_to_take_effect() {
        // The floors the research validated by hand.
        let at_979 = floor_at_epoch("agave", 979).unwrap();
        assert_eq!(at_979.client_version, "4.0.0-beta.0");
        assert_eq!(at_979.announced_epoch, 978);

        assert_eq!(
            floor_at_epoch("agave", 978).unwrap().client_version,
            "3.1.7"
        );
        assert_eq!(
            floor_at_epoch("agave", 1030).unwrap().client_version,
            "4.2.0-beta.1"
        );
        assert_eq!(
            floor_at_epoch("frankendancer", 1030)
                .unwrap()
                .client_version,
            "0.1102.40201"
        );
    }

    #[test]
    fn nothing_answers_before_the_first_floor_or_for_an_untracked_lineage() {
        assert!(floor_at_epoch("agave", 945).is_none());
        assert!(floor_at_epoch("sig", 1030).is_none());
    }

    #[test]
    fn a_wrong_header_is_refused() {
        assert!(parse("client,version\nagave,4.0.2\n").is_err());
    }

    #[test]
    fn unusable_rows_are_refused() {
        let header = HEADER.join(",");
        for row in [
            ",4.0.2,992,991",       // no lineage
            "agave,,992,991",       // no version
            "agave,4.0.2,soon,991", // unparseable effective epoch
            "agave,4.0.2,992,",     // no announcement epoch
        ] {
            assert!(
                parse(&format!("{header}\n{row}\n")).is_err(),
                "{row} should be refused"
            );
        }
    }
}
