use crate::directory::{Directory, Precondition};
use crate::docs::{ReleaseEntry, ReleasesDoc, RELEASES_PATH};
use crate::dto::{FeatureGateFloor, ReleaseRecord, SfdpFloor};
use crate::warehouse::Warehouse;
use chrono::{DateTime, Utc};
use clap::Parser;
use collect::releases::{ReleaseSource, ReleasesSnapshot};
use log::{info, warn};
use serde_yaml;
use std::collections::HashSet;

const SUPPORTED_DATA_VERSION: u16 = 1;

/// The feature-gate floor history, reconstructed from the Solana Tech Discord
/// announcements (epochs 943-1019) and from the gates' own activation slots for
/// the rest. The collector derives the same timeline from Anza's tracker, but
/// only over the newest gates it reads, so this is what gives the older epochs
/// an answer. Applied on every write, where a version the document already
/// names is left as it is.
const SEEDED_FEATURE_GATE_FLOORS: &[(&str, &str, u64)] = &[
    ("agave", "3.1.0", 946),
    ("agave", "3.1.7", 953),
    ("agave", "4.0.0-beta.0", 979),
    ("agave", "4.0.2", 992),
    ("agave", "4.1.0-beta.0", 999),
    ("agave", "4.1.0-beta.1", 1008),
    ("agave", "4.2.0-beta.1", 1019),
    ("frankendancer", "0.812.30108", 946),
    ("frankendancer", "0.902.40002", 979),
    ("frankendancer", "0.911.40002", 992),
    ("frankendancer", "0.1001.40101", 999),
    ("frankendancer", "0.1102.40201", 1019),
    ("firedancer", "1.1.1", 1019),
    ("firedancer", "26.8.0", 1026),
];

#[derive(Debug, Parser)]
pub struct StoreReleasesParams {
    #[arg(long = "snapshot-file")]
    snapshot_path: String,
}

pub async fn store_releases(
    params: StoreReleasesParams,
    directory: &Directory,
) -> anyhow::Result<()> {
    info!("Storing releases snapshot...");

    let path = params.snapshot_path;
    let snapshot_file = std::fs::File::open(&path)
        .map_err(|e| anyhow::anyhow!("Failed to open snapshot releases file '{path}': {e}"))?;
    let snapshot: ReleasesSnapshot = serde_yaml::from_reader(snapshot_file)
        .map_err(|e| anyhow::anyhow!("Failed to parse snapshot releases file '{path}': {e}"))?;

    anyhow::ensure!(
        snapshot.version == SUPPORTED_DATA_VERSION,
        "Snapshot releases file '{path}' has version {}, expected {SUPPORTED_DATA_VERSION}",
        snapshot.version
    );

    let created_at: DateTime<Utc> = snapshot.created_at.parse()?;
    info!(
        "Loaded the snapshot of {} releases, created at {created_at}",
        snapshot.releases.len()
    );

    let stored = directory.get::<ReleasesDoc>(RELEASES_PATH).await?;
    let (mut releases, precondition) = match stored {
        Some(stored) => (stored.body, Precondition::IfMatch(stored.etag)),
        None => (ReleasesDoc::new(), Precondition::Create),
    };

    seed_feature_gate_floors(&mut releases, created_at);
    let written = apply_releases(&mut releases, &snapshot, created_at);

    // No retry on a conflict: the cron is the retry.
    directory
        .put(RELEASES_PATH, &releases, precondition)
        .await?;

    info!(
        "Stored releases snapshot: {} availability rows, {} SFDP floors, {} feature-gate floors",
        written.github, written.sfdp, written.feature_gates
    );

    Ok(())
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct WrittenReleases {
    pub github: usize,
    pub sfdp: usize,
    pub feature_gates: usize,
}

/// Each source writes only the fields it answers for, so a run cannot blank
/// another source's floor or publish time. The last entry wins where a
/// snapshot carries a version twice for one source.
pub fn apply_releases(
    releases: &mut ReleasesDoc,
    snapshot: &ReleasesSnapshot,
    written_at: DateTime<Utc>,
) -> WrittenReleases {
    let mut written = WrittenReleases::default();
    let mut seen: HashSet<(&str, String, String)> = HashSet::new();

    for entry in &snapshot.releases {
        let version = entry.client_version.to_string();
        let key = (
            entry.source.as_str(),
            entry.client_lineage.clone(),
            version.clone(),
        );
        if !seen.insert(key) {
            warn!(
                "Snapshot carries {} {version} more than once for {}, keeping the last",
                entry.client_lineage,
                entry.source.as_str()
            );
        }

        let release = releases
            .entry(entry.client_lineage.clone())
            .or_default()
            .entry(version)
            .or_insert_with(|| new_entry(written_at));
        match entry.source {
            ReleaseSource::Github => {
                release.released_at = entry.released_at;
                release.release_url = entry.release_url.clone();
                written.github += 1;
            }
            ReleaseSource::Sfdp => {
                release.sfdp_floor_epoch = entry.sfdp_floor_epoch;
                written.sfdp += 1;
            }
            ReleaseSource::FeatureGates => {
                release.feature_gate_epoch = entry.feature_gate_epoch;
                written.feature_gates += 1;
            }
        }
        release.updated_at = written_at;
    }

    written
}

pub fn seed_feature_gate_floors(releases: &mut ReleasesDoc, written_at: DateTime<Utc>) {
    for (lineage, version, epoch) in SEEDED_FEATURE_GATE_FLOORS {
        releases
            .entry(lineage.to_string())
            .or_default()
            .entry(version.to_string())
            .or_insert_with(|| ReleaseEntry {
                feature_gate_epoch: Some(*epoch),
                ..new_entry(written_at)
            });
    }
}

fn new_entry(written_at: DateTime<Utc>) -> ReleaseEntry {
    ReleaseEntry {
        released_at: None,
        release_url: None,
        sfdp_floor_epoch: None,
        feature_gate_epoch: None,
        created_at: written_at,
        updated_at: written_at,
    }
}

/// What was published, newest first: one record per version a release carries
/// a timestamp for. A version only a floor named belongs in a floor list, not
/// here. `available_epoch` is resolved against the epochs the warehouse holds,
/// so a release older than that window has none.
pub fn load_releases(
    warehouse: &Warehouse,
    client_lineage: Option<&str>,
    since_epoch: Option<u64>,
) -> Vec<ReleaseRecord> {
    let since = since_epoch.and_then(|epoch| released_since(warehouse, epoch));
    let mut records: Vec<ReleaseRecord> = lineages(warehouse, client_lineage)
        .flat_map(|(lineage, versions)| {
            versions.iter().filter_map(move |(version, release)| {
                let released_at = release.released_at?;
                if since.is_some_and(|since| released_at < since) {
                    return None;
                }
                Some(ReleaseRecord {
                    client_lineage: lineage.clone(),
                    client_version: version.clone(),
                    available_epoch: epoch_of(warehouse, released_at),
                    released_at: Some(released_at),
                    release_url: release.release_url.clone(),
                    updated_at: release.updated_at,
                })
            })
        })
        .collect();

    records.sort_by(|a, b| {
        b.released_at
            .cmp(&a.released_at)
            .then_with(|| a.client_lineage.cmp(&b.client_lineage))
            .then_with(|| a.client_version.cmp(&b.client_version))
    });
    records
}

/// Every version SFDP has required, newest floor first within a lineage.
pub fn load_sfdp_floors(
    warehouse: &Warehouse,
    client_lineage: Option<&str>,
    since_epoch: Option<u64>,
) -> Vec<SfdpFloor> {
    floors(warehouse, client_lineage, since_epoch, |release| {
        release.sfdp_floor_epoch
    })
    .into_iter()
    .map(
        |(client_lineage, client_version, effective_epoch)| SfdpFloor {
            client_lineage,
            client_version,
            effective_epoch,
        },
    )
    .collect()
}

/// Every version the cluster's feature gates have required, newest floor first
/// within a lineage.
pub fn load_feature_gate_floors(
    warehouse: &Warehouse,
    client_lineage: Option<&str>,
    since_epoch: Option<u64>,
) -> Vec<FeatureGateFloor> {
    floors(warehouse, client_lineage, since_epoch, |release| {
        release.feature_gate_epoch
    })
    .into_iter()
    .map(
        |(client_lineage, client_version, effective_epoch)| FeatureGateFloor {
            client_lineage,
            client_version,
            effective_epoch,
        },
    )
    .collect()
}

fn floors(
    warehouse: &Warehouse,
    client_lineage: Option<&str>,
    since_epoch: Option<u64>,
    floor: fn(&ReleaseEntry) -> Option<u64>,
) -> Vec<(String, String, u64)> {
    let mut rows: Vec<(String, String, u64)> = lineages(warehouse, client_lineage)
        .flat_map(|(lineage, versions)| {
            versions.iter().filter_map(move |(version, release)| {
                let effective_epoch = floor(release)?;
                if since_epoch.is_some_and(|since| effective_epoch < since) {
                    return None;
                }
                Some((lineage.clone(), version.clone(), effective_epoch))
            })
        })
        .collect();

    rows.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| b.2.cmp(&a.2))
            .then_with(|| a.1.cmp(&b.1))
    });
    rows
}

fn lineages<'a>(
    warehouse: &'a Warehouse,
    client_lineage: Option<&'a str>,
) -> impl Iterator<
    Item = (
        &'a String,
        &'a std::collections::BTreeMap<String, ReleaseEntry>,
    ),
> + 'a {
    warehouse
        .releases
        .iter()
        .filter(move |(lineage, _)| client_lineage.is_none_or(|wanted| wanted == lineage.as_str()))
}

/// The epoch a moment falls in, among the sealed ones.
fn epoch_of(warehouse: &Warehouse, at: DateTime<Utc>) -> Option<u64> {
    warehouse
        .epochs
        .iter()
        .find(|(_, record)| record.start_at <= at && at < record.end_at)
        .map(|(epoch, _)| *epoch)
}

/// The moment a `since_epoch` bound starts at. The epoch asked for may have no
/// document: older than the history held takes everything, newer than it, the
/// running epoch, takes what followed the last close.
fn released_since(warehouse: &Warehouse, since_epoch: u64) -> Option<DateTime<Utc>> {
    let oldest = *warehouse.epochs.keys().next()?;
    if since_epoch <= oldest {
        return None;
    }
    warehouse
        .epochs
        .range(since_epoch..)
        .next()
        .map(|(_, record)| record.start_at)
        .or_else(|| warehouse.epochs.values().map(|record| record.end_at).max())
}
