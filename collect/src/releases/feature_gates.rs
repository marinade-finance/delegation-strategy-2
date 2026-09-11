use super::{ReleaseEntry, ReleaseFetcher, ReleaseSource};
use crate::solana_service::solana_client;
use crate::validator_version::ValidatorVersion;
use anyhow::Context;
use log::{debug, info, warn};
use serde::Deserialize;
use solana_client::rpc_client::RpcClient;
use solana_sdk::clock::Epoch;
use solana_sdk::pubkey::Pubkey;
use std::collections::BTreeMap;
use std::str::FromStr;
use std::time::Duration;

pub const SCHEDULE_JSON_URL: &str =
    "https://raw.githubusercontent.com/wiki/anza-xyz/agave/feature-gate-tracker-schedule.json";
pub const VERSION_FLOOR_JSON_URL: &str =
    "https://raw.githubusercontent.com/wiki/anza-xyz/agave/version-floor.json";
/// getMultipleAccounts caps a request at 100 keys.
const ACCOUNTS_PER_CALL: usize = 100;

#[derive(Debug, Deserialize)]
struct Gate {
    #[serde(rename = "Feature ID")]
    feature_id: String,
    #[serde(rename = "Min Agave Versions")]
    agave: Vec<String>,
    #[serde(rename = "Min FRD Versions")]
    frankendancer: Vec<String>,
    #[serde(rename = "Min FD Versions")]
    firedancer: Vec<String>,
}

impl Gate {
    fn version_for(&self, lineage: &str) -> Option<ValidatorVersion> {
        let values = match lineage {
            "agave" => &self.agave,
            "frankendancer" => &self.frankendancer,
            "firedancer" => &self.firedancer,
            _ => return None,
        };
        gate_version(values)
    }
}

/// The lineages the tracker states a version for. Jito is absent on purpose: it is a vendor build
/// of the agave lineage and reports agave's version in gossip.
const LINEAGES: [&str; 3] = ["agave", "frankendancer", "firedancer"];

/// The version a gate shipped in, as the client reports it in gossip.
///
/// `v4.0.2 / v4.1.0-beta.3` offers one version per release line and the lowest of them satisfies
/// the gate. Values the tracker carries for pre-tracker gates name a release line rather than a
/// release (`v2.1`, `0.403`) and are dropped.
fn gate_version(values: &[String]) -> Option<ValidatorVersion> {
    values
        .iter()
        .flat_map(|value| value.split('/'))
        .filter_map(|value| value.parse::<ValidatorVersion>().ok())
        .min()
}

/// Whether a gate could raise the floor: only one requiring at least the published floor can, and
/// the published floor is the maximum over every gate already activated.
fn could_raise_floor(gate: &Gate, published: &BTreeMap<&str, ValidatorVersion>) -> bool {
    LINEAGES.iter().any(
        |lineage| match (gate.version_for(lineage), published.get(*lineage)) {
            (Some(required), Some(floor)) => required >= *floor,
            // No published floor for the lineage, so nothing rules the gate out.
            (Some(_), None) => true,
            (None, _) => false,
        },
    )
}

/// Each epoch the floor rose, and the version it rose to: the running maximum over the gates
/// activated by then, which is how the tracker itself defines the floor.
fn floor_timeline(
    activations: &BTreeMap<Epoch, Vec<ValidatorVersion>>,
) -> Vec<(Epoch, ValidatorVersion)> {
    let mut timeline: Vec<(Epoch, ValidatorVersion)> = Vec::new();
    let mut floor: Option<ValidatorVersion> = None;

    for (epoch, versions) in activations {
        let Some(highest) = versions.iter().max().cloned() else {
            continue;
        };
        // Gates activate out of order, and one requiring an older version than the floor already
        // reached does not lower it.
        if floor.as_ref().is_some_and(|floor| highest <= *floor) {
            continue;
        }
        floor = Some(highest.clone());
        timeline.push((*epoch, highest));
    }

    timeline
}

pub struct FeatureGatesFetcher {
    schedule_url: String,
    version_floor_url: String,
    http: reqwest::blocking::Client,
    rpc: RpcClient,
}

impl FeatureGatesFetcher {
    pub fn new(
        schedule_url: String,
        version_floor_url: String,
        rpc_url: String,
        commitment: String,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            schedule_url,
            version_floor_url,
            http: reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(super::HTTP_TIMEOUT_S))
                .build()?,
            rpc: solana_client(rpc_url, commitment),
        })
    }

    fn get_json<T: serde::de::DeserializeOwned>(&self, url: &str) -> anyhow::Result<T> {
        Ok(self
            .http
            .get(url)
            .send()?
            .error_for_status()
            .with_context(|| format!("fetching {url}"))?
            .json()?)
    }

    /// The epoch each gate activated in, absent for one still pending.
    fn activation_epochs(&self, gates: &[&Gate]) -> anyhow::Result<BTreeMap<String, Epoch>> {
        let epoch_schedule = self.rpc.get_epoch_schedule()?;
        let mut epochs = BTreeMap::new();

        for chunk in gates.chunks(ACCOUNTS_PER_CALL) {
            let ids: Vec<Pubkey> = chunk
                .iter()
                .filter_map(|gate| Pubkey::from_str(&gate.feature_id).ok())
                .collect();
            for (id, account) in ids.iter().zip(self.rpc.get_multiple_accounts(&ids)?) {
                // A gate nobody has requested yet has no account at all.
                let Some(account) = account else { continue };
                let Some(feature) = solana_feature_gate_interface::from_account(&account) else {
                    // Funded ahead of the activation request, so still an empty system account.
                    debug!("Gate {id} holds no feature yet");
                    continue;
                };
                if let Some(activated_at) = feature.activated_at {
                    epochs.insert(id.to_string(), epoch_schedule.get_epoch(activated_at));
                }
            }
        }

        Ok(epochs)
    }

    /// Each epoch the floor rose, reading only gates at or above the published floor; everything
    /// below it is history the migration seeds.
    pub fn derive(&self) -> anyhow::Result<Vec<ReleaseEntry>> {
        let schedule: BTreeMap<String, Vec<Gate>> = self.get_json(&self.schedule_url)?;
        let gates: Vec<Gate> = schedule.into_values().flatten().collect();
        let published = self.published_floors()?;

        let candidates: Vec<&Gate> = gates
            .iter()
            .filter(|gate| could_raise_floor(gate, &published))
            .collect();

        let activated = self.activation_epochs(&candidates)?;
        info!(
            "{} feature gates in the tracker, {} require at least the published floor, {} of those activated",
            gates.len(),
            candidates.len(),
            activated.len()
        );

        let mut entries = Vec::new();
        for lineage in LINEAGES {
            let mut activations: BTreeMap<Epoch, Vec<ValidatorVersion>> = BTreeMap::new();
            for gate in &candidates {
                let Some(epoch) = activated.get(&gate.feature_id) else {
                    continue;
                };
                if let Some(version) = gate.version_for(lineage) {
                    // A gate below the published floor is history the migration owns.
                    if published.get(lineage).is_none_or(|floor| version >= *floor) {
                        activations.entry(*epoch).or_default().push(version);
                    }
                }
            }

            for (epoch, version) in floor_timeline(&activations) {
                entries.push(ReleaseEntry {
                    client_lineage: lineage.to_string(),
                    client_version: version,
                    released_at: None,
                    release_url: None,
                    sfdp_floor_epoch: None,
                    feature_gate_epoch: Some(epoch),
                    source: ReleaseSource::FeatureGates,
                });
            }
        }

        self.check_published_floor_was_derived(&published, &entries);

        Ok(entries)
    }

    /// The floor Anza publishes per lineage, which is the maximum over every gate already
    /// activated, and so the line below which a gate cannot raise anything.
    fn published_floors(&self) -> anyhow::Result<BTreeMap<&'static str, ValidatorVersion>> {
        let file: VersionFloorFile = self.get_json(&self.version_floor_url)?;
        let mut floors = BTreeMap::new();

        for (lineage, published) in [
            ("agave", &file.mainnet_beta.current.agave),
            ("frankendancer", &file.mainnet_beta.current.frd),
            ("firedancer", &file.mainnet_beta.current.fd),
        ] {
            // Empty while no floor is published for that client.
            if let Ok(published) = published.parse::<ValidatorVersion>() {
                floors.insert(lineage, published);
            }
        }

        Ok(floors)
    }

    /// The published floor is the version some activated gate requires, so it has to appear in what
    /// was derived. Missing means the versions or the activations were read wrong, and the gap is
    /// silent everywhere else.
    fn check_published_floor_was_derived(
        &self,
        published: &BTreeMap<&str, ValidatorVersion>,
        derived: &[ReleaseEntry],
    ) {
        for (lineage, floor) in published {
            if !derived
                .iter()
                .any(|entry| entry.client_lineage == *lineage && entry.client_version == *floor)
            {
                warn!("Derived no epoch for the published {lineage} floor {floor}");
            }
        }
    }
}

#[derive(Debug, Deserialize)]
struct VersionFloorFile {
    #[serde(rename = "Mainnet Beta")]
    mainnet_beta: ClusterFloor,
}

#[derive(Debug, Deserialize)]
struct ClusterFloor {
    current: PublishedFloor,
}

#[derive(Debug, Deserialize)]
struct PublishedFloor {
    #[serde(rename = "Agave")]
    agave: String,
    #[serde(rename = "FD")]
    fd: String,
    #[serde(rename = "FRD")]
    frd: String,
}

impl ReleaseFetcher for FeatureGatesFetcher {
    fn fetch(&self) -> anyhow::Result<Vec<ReleaseEntry>> {
        self.derive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn versions(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| v.to_string()).collect()
    }

    #[test]
    fn the_lowest_alternative_satisfies_the_gate() {
        // One value in the tracker names a version per release line.
        let version = gate_version(&versions(&["v4.0.2 / v4.1.0-beta.3"])).unwrap();
        assert_eq!(version.as_str(), "4.0.2");
    }

    #[test]
    fn a_release_line_is_not_a_version() {
        // What the tracker carries for gates that predate it.
        assert!(gate_version(&versions(&["v2.1"])).is_none());
        assert!(gate_version(&versions(&["0.403"])).is_none());
        assert!(gate_version(&versions(&[""])).is_none());
    }

    fn gate_requiring(agave: &str) -> Gate {
        Gate {
            feature_id: String::new(),
            agave: vec![agave.to_string()],
            frankendancer: vec![],
            firedancer: vec![],
        }
    }

    #[test]
    fn only_a_gate_at_or_above_the_published_floor_is_read() {
        // The published floor is the maximum over everything already activated, so a gate below it
        // is history the migration owns -- and reading it would restart the running maximum from
        // there, reporting a floor lower than the one in force.
        let published = BTreeMap::from([("agave", "4.2.0".parse().unwrap())]);

        assert!(could_raise_floor(&gate_requiring("v4.2.0"), &published));
        assert!(could_raise_floor(&gate_requiring("v4.2.2"), &published));
        assert!(!could_raise_floor(&gate_requiring("v3.0.0"), &published));
        assert!(!could_raise_floor(
            &gate_requiring("v4.1.0-beta.1"),
            &published
        ));
    }

    #[test]
    fn a_lineage_with_no_published_floor_reads_every_gate() {
        assert!(could_raise_floor(
            &gate_requiring("v3.0.0"),
            &BTreeMap::new()
        ));
    }

    #[test]
    fn the_floor_is_the_running_maximum() {
        let activations = BTreeMap::from([
            (979, vec!["4.0.0-beta.0".parse().unwrap()]),
            (986, vec!["4.0.0-beta.2".parse().unwrap()]),
            (992, vec!["4.0.2".parse().unwrap()]),
        ]);

        assert_eq!(
            floor_timeline(&activations)
                .iter()
                .map(|(epoch, version)| (*epoch, version.as_str()))
                .collect::<Vec<_>>(),
            vec![(979, "4.0.0-beta.0"), (986, "4.0.0-beta.2"), (992, "4.0.2")]
        );
    }

    #[test]
    fn a_gate_needing_an_older_version_does_not_lower_the_floor() {
        // Gates activate out of order: epoch 1031's gate shipped in 4.2.0-beta.0, below the
        // 4.2.0 that epoch 1027 already required.
        let activations = BTreeMap::from([
            (1019, vec!["4.2.0-beta.1".parse().unwrap()]),
            (1027, vec!["4.2.0".parse().unwrap()]),
            (1031, vec!["4.2.0-beta.0".parse().unwrap()]),
        ]);

        assert_eq!(
            floor_timeline(&activations)
                .iter()
                .map(|(epoch, version)| (*epoch, version.as_str()))
                .collect::<Vec<_>>(),
            vec![(1019, "4.2.0-beta.1"), (1027, "4.2.0")]
        );
    }

    #[test]
    fn one_epoch_activating_several_gates_takes_the_highest() {
        let activations = BTreeMap::from([(
            992,
            vec![
                "4.0.0-beta.6".parse().unwrap(),
                "4.0.2".parse().unwrap(),
                "4.0.0-beta.2".parse().unwrap(),
            ],
        )]);

        assert_eq!(floor_timeline(&activations)[0].1.as_str(), "4.0.2");
    }
}
