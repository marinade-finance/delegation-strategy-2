use super::{firedancer_lineage, ReleaseEntry, ReleaseFetcher, ReleaseSource};
use crate::common::retry_blocking;
use log::{debug, info, warn};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::thread;
use std::time::Duration;

const SFDP_URL: &str = "https://api.solana.org/api/community/v1/sfdp_required_versions";
/// The floor for epochs that have not started yet is published ahead of them.
pub const UPCOMING_EPOCHS: u64 = 4;

/// One request per epoch, and the endpoint starts answering 429 a few hundred requests in, so a
/// full backfill has to pace itself.
const REQUEST_INTERVAL: Duration = Duration::from_millis(300);

/// Long enough to outlast a per-minute quota; a quadratic few seconds is not.
const FETCH_BACKOFF: [Duration; 5] = [
    Duration::from_secs(5),
    Duration::from_secs(15),
    Duration::from_secs(30),
    Duration::from_secs(60),
    Duration::from_secs(60),
];

#[derive(Debug, Deserialize)]
struct SfdpResponse {
    data: Option<Vec<SfdpRow>>,
    /// "No version requirements found for epoch N on mainnet-beta" — an answer, not a failure.
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SfdpRow {
    agave_min_version: Option<String>,
    /// SFDP's historical field name: the numbering it carries is Frankendancer's. Firedancer proper
    /// has no SFDP floor of its own yet.
    firedancer_min_version: Option<String>,
}

pub struct SfdpFetcher {
    from_epoch: u64,
    to_epoch: u64,
    client: reqwest::blocking::Client,
}

impl SfdpFetcher {
    pub fn new(from_epoch: u64, to_epoch: u64) -> anyhow::Result<Self> {
        Ok(Self {
            from_epoch,
            to_epoch,
            client: reqwest::blocking::Client::new(),
        })
    }

    fn epoch_url(epoch: u64) -> String {
        format!("{SFDP_URL}?cluster=mainnet-beta&epoch={epoch}")
    }

    fn fetch_epoch(&self, epoch: u64) -> anyhow::Result<Option<SfdpRow>> {
        let url = Self::epoch_url(epoch);
        let response = self.client.get(&url).send()?;
        // An epoch the program stated nothing for answers 404 with the reason in the body, which is
        // an answer and not a failure: everything before epoch 688 is one.
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            debug!("No floor for epoch {epoch}");
            return Ok(None);
        }
        let response: SfdpResponse = response.error_for_status()?.json()?;
        if let Some(message) = response.message {
            debug!("No floor for epoch {epoch}: {message}");
            return Ok(None);
        }
        Ok(response.data.and_then(|rows| rows.into_iter().next()))
    }
}

/// First epoch of each version's first run as the floor.
///
/// The per-epoch endpoint always answers `inherited_from_prev_epoch: false`, so the effective epoch
/// has to come from where the value changes, not from that flag.
fn collapse_runs(series: &BTreeMap<u64, String>) -> Vec<(String, u64)> {
    let mut effective: Vec<(String, u64)> = Vec::new();
    let mut previous: Option<&str> = None;

    for (epoch, version) in series {
        if previous == Some(version.as_str()) {
            continue;
        }
        previous = Some(version);
        if let Some((_, first_epoch)) = effective.iter().find(|(v, _)| v == version) {
            // A floor that was lowered and later raised back: "the floor at epoch E" resolves
            // against the first run, which is wrong for the second one.
            warn!(
                "Floor {version} is effective again at epoch {epoch} after being effective at {first_epoch}"
            );
            continue;
        }
        effective.push((version.clone(), *epoch));
    }

    effective
}

impl ReleaseFetcher for SfdpFetcher {
    fn source(&self) -> ReleaseSource {
        ReleaseSource::Sfdp
    }

    fn fetch(&self) -> anyhow::Result<Vec<ReleaseEntry>> {
        let mut agave: BTreeMap<u64, String> = BTreeMap::new();
        let mut firedancer: BTreeMap<u64, String> = BTreeMap::new();

        info!(
            "Fetching SFDP mainnet version floors for epochs {}..={}",
            self.from_epoch, self.to_epoch
        );

        for epoch in self.from_epoch..=self.to_epoch {
            let row = retry_blocking(
                || self.fetch_epoch(epoch),
                FETCH_BACKOFF.into_iter(),
                |err, attempt, backoff| {
                    warn!("Failed to fetch the SFDP floor for epoch {epoch} (attempt {attempt}), retrying in {backoff:?}: {err}")
                },
            )?;
            thread::sleep(REQUEST_INTERVAL);
            let Some(row) = row else { continue };

            for (versions, min_version) in [
                (&mut agave, row.agave_min_version),
                (&mut firedancer, row.firedancer_min_version),
            ] {
                if let Some(version) = min_version.filter(|v| !v.trim().is_empty()) {
                    versions.insert(epoch, version.trim().to_string());
                }
            }
        }

        let mut entries = Vec::new();
        for (series, lineage_of) in [
            (&agave, None),
            (&firedancer, Some(firedancer_lineage as fn(&str) -> &str)),
        ] {
            for (version, effective_epoch) in collapse_runs(series) {
                let lineage = match lineage_of {
                    Some(resolve) => resolve(&version),
                    None => "agave",
                };
                entries.push(ReleaseEntry {
                    client_lineage: lineage.to_string(),
                    client_version: version,
                    released_at: None,
                    sfdp_floor_epoch: Some(effective_epoch),
                    release_url: None,
                    source: ReleaseSource::Sfdp,
                });
            }
        }

        info!("Derived {} SFDP floors", entries.len());

        Ok(entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn series(rows: &[(u64, &str)]) -> BTreeMap<u64, String> {
        rows.iter()
            .map(|(epoch, version)| (*epoch, version.to_string()))
            .collect()
    }

    #[test]
    fn a_run_collapses_to_the_epoch_it_starts_on() {
        // The real mainnet floors for epochs 948-957.
        let floors = series(&[
            (948, "3.1.10"),
            (949, "3.1.10"),
            (950, "3.1.10"),
            (951, "3.1.10"),
            (952, "3.1.10"),
            (953, "3.1.11"),
            (954, "3.1.11"),
            (955, "3.1.11"),
            (956, "3.1.13"),
            (957, "3.1.13"),
        ]);

        assert_eq!(
            collapse_runs(&floors),
            vec![
                ("3.1.10".to_string(), 948),
                ("3.1.11".to_string(), 953),
                ("3.1.13".to_string(), 956),
            ]
        );
    }

    #[test]
    fn a_reinstated_floor_keeps_the_epoch_of_its_first_run() {
        let floors = series(&[
            (1000, "4.1.0-rc.1"),
            (1001, "4.2.0-rc.1"),
            (1002, "4.1.0-rc.1"),
        ]);

        assert_eq!(
            collapse_runs(&floors),
            vec![
                ("4.1.0-rc.1".to_string(), 1000),
                ("4.2.0-rc.1".to_string(), 1001),
            ]
        );
    }

    #[test]
    fn a_gap_in_the_series_does_not_start_a_new_run() {
        // Epochs the endpoint has no row for are simply absent.
        let floors = series(&[(990, "4.0.2"), (993, "4.0.2"), (994, "4.1.0-rc.1")]);

        assert_eq!(
            collapse_runs(&floors),
            vec![("4.0.2".to_string(), 990), ("4.1.0-rc.1".to_string(), 994),]
        );
    }
}
