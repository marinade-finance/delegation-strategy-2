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
    /// A gate reaches testnet before mainnet, so this is how recent it is.
    #[serde(rename = "Testnet Epoch")]
    testnet_epoch: Option<u64>,
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
    latest_gates_to_read: usize,
    http: reqwest::blocking::Client,
    rpc: RpcClient,
}

impl FeatureGatesFetcher {
    pub fn new(
        schedule_url: String,
        version_floor_url: String,
        latest_gates_to_read: usize,
        rpc_url: String,
        commitment: String,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            schedule_url,
            version_floor_url,
            latest_gates_to_read,
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

    /// The floor each epoch it rose, over the newest gates the tracker lists.
    ///
    /// Only the newest are read: the history is seeded by the migration, and one account read per
    /// gate is the cost of looking further back.
    pub fn derive(&self) -> anyhow::Result<Vec<ReleaseEntry>> {
        let schedule: BTreeMap<String, Vec<Gate>> = self.get_json(&self.schedule_url)?;
        let gates: Vec<Gate> = schedule.into_values().flatten().collect();

        let mut newest: Vec<&Gate> = gates.iter().collect();
        // A gate reaches testnet before mainnet, so this orders by recency; one that never reached
        // testnet sorts last.
        newest.sort_by_key(|gate| std::cmp::Reverse(gate.testnet_epoch));
        newest.truncate(self.latest_gates_to_read);

        let activated = self.activation_epochs(&newest)?;
        info!(
            "{} feature gates in the tracker, read the newest {}, {} of those activated",
            gates.len(),
            newest.len(),
            activated.len()
        );

        let mut entries = Vec::new();
        for lineage in LINEAGES {
            let mut activations: BTreeMap<Epoch, Vec<ValidatorVersion>> = BTreeMap::new();
            for gate in &newest {
                let Some(epoch) = activated.get(&gate.feature_id) else {
                    continue;
                };
                if let Some(version) = gate.version_for(lineage) {
                    activations.entry(*epoch).or_default().push(version);
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

        self.warn_on_drift(&entries)?;

        Ok(entries)
    }

    /// The newest derived floor has to be the one Anza publishes; a mismatch means the versions or
    /// the activations were read wrong.
    fn warn_on_drift(&self, derived: &[ReleaseEntry]) -> anyhow::Result<()> {
        let published: VersionFloorFile = self.get_json(&self.version_floor_url)?;

        for (lineage, published) in [
            ("agave", &published.mainnet_beta.current.agave),
            ("frankendancer", &published.mainnet_beta.current.frd),
            ("firedancer", &published.mainnet_beta.current.fd),
        ] {
            let Ok(published) = published.parse::<ValidatorVersion>() else {
                // Empty while no floor is published for that client.
                continue;
            };
            match derived
                .iter()
                .filter(|entry| entry.client_lineage == lineage)
                .max_by_key(|entry| entry.feature_gate_epoch)
            {
                Some(newest) if newest.client_version == published => {}
                Some(newest) => warn!(
                    "Derived {lineage} floor {} at epoch {:?} is not the published {published}",
                    newest.client_version, newest.feature_gate_epoch
                ),
                None => warn!("Derived no {lineage} floor, the published one is {published}"),
            }
        }

        Ok(())
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

    fn gate_with_testnet_epoch(testnet_epoch: Option<u64>) -> Gate {
        Gate {
            feature_id: String::new(),
            testnet_epoch,
            agave: vec![],
            frankendancer: vec![],
            firedancer: vec![],
        }
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
    fn the_newest_gates_are_the_ones_read() {
        // Recency comes from the testnet epoch, and a gate that never reached testnet sorts last.
        let mut gates = vec![
            gate_with_testnet_epoch(Some(1000)),
            gate_with_testnet_epoch(None),
            gate_with_testnet_epoch(Some(1020)),
        ];
        gates.sort_by_key(|gate| std::cmp::Reverse(gate.testnet_epoch));

        assert_eq!(
            gates
                .iter()
                .map(|gate| gate.testnet_epoch)
                .collect::<Vec<_>>(),
            vec![Some(1020), Some(1000), None]
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
