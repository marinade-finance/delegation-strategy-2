use super::{firedancer_lineage, ReleaseEntry, ReleaseFetcher, ReleaseSource};
use crate::common::retry_blocking;
use log::{debug, info, warn};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::thread;
use std::time::Duration;

pub const SFDP_API: &str = "https://api.solana.org";
const REQUIRED_VERSIONS_PATH: &str = "/api/community/v1/sfdp_required_versions";
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
    api_url: String,
    from_epoch: u64,
    to_epoch: u64,
    client: reqwest::blocking::Client,
}

impl SfdpFetcher {
    pub fn new(api_url: String, from_epoch: u64, to_epoch: u64) -> anyhow::Result<Self> {
        Ok(Self {
            api_url,
            from_epoch,
            to_epoch,
            client: reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(super::HTTP_TIMEOUT_S))
                .build()?,
        })
    }

    fn epoch_url(&self, epoch: u64) -> String {
        format!(
            "{}{REQUIRED_VERSIONS_PATH}?cluster=mainnet-beta&epoch={epoch}",
            self.api_url
        )
    }

    fn fetch_epoch(&self, epoch: u64) -> anyhow::Result<Option<SfdpRow>> {
        let url = self.epoch_url(epoch);
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

/// First epoch of each version's latest run as the floor.
///
/// The per-epoch endpoint always answers `inherited_from_prev_epoch: false`, so the effective epoch
/// has to come from where the value changes, not from that flag.
///
/// A floor already in force at `anchor_epoch`, the epoch fetched ahead of the window, started
/// outside it and is left unrecorded rather than stamped with the window's own first epoch.
fn collapse_runs(series: &BTreeMap<u64, String>, anchor_epoch: u64) -> Vec<(String, u64)> {
    let mut effective: Vec<(String, u64)> = Vec::new();
    let mut previous: Option<&str> = None;

    for (epoch, version) in series {
        if previous == Some(version.as_str()) {
            continue;
        }
        previous = Some(version);
        if *epoch == anchor_epoch {
            continue;
        }
        if let Some((_, effective_epoch)) = effective.iter_mut().find(|(v, _)| v == version) {
            warn!(
                "Floor {version} is effective again at epoch {epoch}, replacing epoch {effective_epoch}"
            );
            *effective_epoch = *epoch;
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
        // One epoch of context: a floor already in force here is one whose own start is outside the
        // window, and must not be reported as starting at the window's edge.
        let anchor_epoch = self.from_epoch.saturating_sub(1);

        info!(
            "Fetching SFDP mainnet version floors for epochs {}..={}",
            self.from_epoch, self.to_epoch
        );

        for epoch in anchor_epoch..=self.to_epoch {
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
                    versions.insert(epoch, super::normalize_version(&version));
                }
            }
        }

        let mut entries = Vec::new();
        for (series, lineage_of) in [
            (&agave, None),
            (&firedancer, Some(firedancer_lineage as fn(&str) -> &str)),
        ] {
            for (version, effective_epoch) in collapse_runs(series, anchor_epoch) {
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

    /// The anchor every fixture below is fetched with: one epoch before its window.
    fn anchor_epoch_of(rows: &[(u64, &str)]) -> u64 {
        rows.first().map_or(0, |(epoch, _)| *epoch)
    }

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
            collapse_runs(&floors, 0),
            vec![
                ("3.1.10".to_string(), 948),
                ("3.1.11".to_string(), 953),
                ("3.1.13".to_string(), 956),
            ]
        );
    }

    #[test]
    fn a_reinstated_floor_takes_the_epoch_of_its_latest_run() {
        // Answering "the floor at 1002" with 4.2.0-rc.1 would flag every node on 4.1.0-rc.1.
        let floors = series(&[
            (1000, "4.1.0-rc.1"),
            (1001, "4.2.0-rc.1"),
            (1002, "4.1.0-rc.1"),
        ]);

        assert_eq!(
            collapse_runs(&floors, 0),
            vec![
                ("4.1.0-rc.1".to_string(), 1002),
                ("4.2.0-rc.1".to_string(), 1001),
            ]
        );
    }

    #[test]
    fn a_floor_already_in_force_at_the_window_edge_is_not_recorded() {
        // What a cron window sees: 4.1.0-rc.1 has been the floor since epoch 996, long before the
        // window opened, so reporting it as starting at 1012 would overwrite the real epoch.
        let rows = [
            (1011, "4.1.0-rc.1"),
            (1012, "4.1.0-rc.1"),
            (1013, "4.1.0-rc.1"),
            (1016, "4.2.0-rc.1"),
        ];
        let floors = series(&rows);

        assert_eq!(
            collapse_runs(&floors, anchor_epoch_of(&rows)),
            vec![("4.2.0-rc.1".to_string(), 1016)]
        );
    }

    #[test]
    fn a_change_on_the_windows_first_epoch_is_recorded() {
        let rows = [(1015, "4.1.0-rc.1"), (1016, "4.2.0-rc.1")];
        let floors = series(&rows);

        assert_eq!(
            collapse_runs(&floors, anchor_epoch_of(&rows)),
            vec![("4.2.0-rc.1".to_string(), 1016)]
        );
    }

    #[test]
    fn the_first_epoch_the_endpoint_answers_for_is_a_real_start() {
        // Nothing precedes epoch 688, so the anchor fetch 404s and the series opens on a genuine
        // change rather than on a floor inherited from outside the window.
        let floors = series(&[(688, "1.18.21"), (697, "2.0.15")]);

        assert_eq!(
            collapse_runs(&floors, 687),
            vec![("1.18.21".to_string(), 688), ("2.0.15".to_string(), 697),]
        );
    }

    #[test]
    fn a_gap_in_the_series_does_not_start_a_new_run() {
        // Epochs the endpoint has no row for are simply absent.
        let floors = series(&[(990, "4.0.2"), (993, "4.0.2"), (994, "4.1.0-rc.1")]);

        assert_eq!(
            collapse_runs(&floors, 0),
            vec![("4.0.2".to_string(), 990), ("4.1.0-rc.1".to_string(), 994),]
        );
    }
}
