//! Feature-gate floors: the client version the cluster required, and the epoch it started
//! requiring it. Below one of these a validator forks off.
//!
//! Static rather than collected: no API serves these, and they move a handful of times a year.

use csv::{optional, required, Column};
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

const DEFAULT_CSV: &str = include_str!("../release_feature_gates.csv");

const COLUMNS: [Column; 4] = [
    required("client_lineage"),
    required("client_version"),
    optional("effective_epoch"),
    required("announced_epoch"),
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
    FLOORS.get_or_init(|| {
        csv::load_vendored(
            DEFAULT_CSV,
            "FEATURE_GATES_CSV",
            &COLUMNS,
            "release_feature_gates.csv",
        )
        .unwrap_or_else(|err| panic!("{err:#}"))
    })
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
}
