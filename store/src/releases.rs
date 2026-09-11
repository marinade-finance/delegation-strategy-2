use crate::dto::{ClientRelease, FeatureGateFloor, ReleaseRecord, SfdpFloor};
use chrono::{DateTime, Utc};
use clap::Parser;
use collect::releases::{ReleaseEntry, ReleaseSource, ReleasesSnapshot};
use collect::validator_version::ValidatorVersion;
use log::{info, warn};
use rust_decimal::prelude::*;
use std::collections::{BTreeMap, HashMap};
use tokio_postgres::Client;

pub const RELEASES_TABLE: &str = "releases";

const SUPPORTED_DATA_VERSION: u16 = 1;
const DEFAULT_CHUNK_SIZE: usize = 500;

#[derive(Debug, Parser)]
pub struct StoreReleasesParams {
    #[arg(long = "snapshot-file")]
    snapshot_path: String,
}

struct ReleaseRow {
    client_lineage: String,
    client_version: String,
    released_at: Option<DateTime<Utc>>,
    release_url: Option<String>,
    sfdp_floor_epoch: Option<Decimal>,
    feature_gate_epoch: Option<Decimal>,
}

fn to_row(entry: &ReleaseEntry) -> ReleaseRow {
    ReleaseRow {
        client_lineage: entry.client_lineage.clone(),
        client_version: entry.client_version.to_string(),
        released_at: entry.released_at,
        release_url: entry.release_url.clone(),
        sfdp_floor_epoch: entry.sfdp_floor_epoch.map(Decimal::from),
        feature_gate_epoch: entry.feature_gate_epoch.map(Decimal::from),
    }
}

pub async fn store_releases(
    params: StoreReleasesParams,
    psql_client: &mut Client,
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

    let snapshot_created_at: DateTime<Utc> = snapshot.created_at.parse()?;
    info!(
        "Loaded the snapshot of {} releases, created at {snapshot_created_at}",
        snapshot.releases.len()
    );

    // Split by what the entry fills, because each set of columns is upserted on its own and one
    // statement must never touch the same conflict target twice.
    let mut availability: BTreeMap<(String, String), ReleaseRow> = BTreeMap::new();
    let mut floors: BTreeMap<(String, String), ReleaseRow> = BTreeMap::new();
    let mut gates: BTreeMap<(String, String), ReleaseRow> = BTreeMap::new();
    for entry in &snapshot.releases {
        let row = to_row(entry);
        let key = (row.client_lineage.clone(), row.client_version.clone());
        let rows = match entry.source {
            ReleaseSource::Github => &mut availability,
            ReleaseSource::Sfdp => &mut floors,
            ReleaseSource::FeatureGates => &mut gates,
        };
        if rows.insert(key.clone(), row).is_some() {
            warn!("Snapshot carries {key:?} more than once, keeping the last");
        }
    }

    let upserted_availability =
        upsert_availability(psql_client, &availability, snapshot_created_at).await?;
    let upserted_floors = upsert_floors(psql_client, &floors, snapshot_created_at).await?;
    let upserted_gates = upsert_feature_gates(psql_client, &gates, snapshot_created_at).await?;

    info!("Stored releases snapshot: {upserted_availability} availability rows, {upserted_floors} SFDP floors, {upserted_gates} feature-gate floors");

    Ok(())
}

/// Writes only the columns GitHub answers for, so a run cannot blank a floor.
async fn upsert_availability(
    psql_client: &Client,
    rows: &BTreeMap<(String, String), ReleaseRow>,
    written_at: DateTime<Utc>,
) -> anyhow::Result<u64> {
    let records: Vec<_> = rows.values().collect();
    let mut total = 0;

    for chunk in records.chunks(DEFAULT_CHUNK_SIZE) {
        let client_lineages: Vec<&str> = chunk.iter().map(|r| r.client_lineage.as_str()).collect();
        let client_versions: Vec<&str> = chunk.iter().map(|r| r.client_version.as_str()).collect();
        let released_ats: Vec<Option<&DateTime<Utc>>> =
            chunk.iter().map(|r| r.released_at.as_ref()).collect();
        let release_urls: Vec<Option<&str>> =
            chunk.iter().map(|r| r.release_url.as_deref()).collect();
        let updated_ats: Vec<&DateTime<Utc>> = vec![&written_at; chunk.len()];
        let created_ats = updated_ats.clone();

        total += psql_client
            .execute(
                &format!(
                    "INSERT INTO {RELEASES_TABLE} (
                client_lineage, client_version, released_at, release_url,
                created_at, updated_at
            )
            SELECT * FROM UNNEST(
                $1::TEXT[],
                $2::TEXT[],
                $3::TIMESTAMP WITH TIME ZONE[],
                $4::TEXT[],
                $5::TIMESTAMP WITH TIME ZONE[],
                $6::TIMESTAMP WITH TIME ZONE[]
            )
            ON CONFLICT (client_lineage, client_version)
            DO UPDATE SET
                released_at = EXCLUDED.released_at,
                release_url = EXCLUDED.release_url,
                updated_at = EXCLUDED.updated_at"
                ),
                &[
                    &client_lineages,
                    &client_versions,
                    &released_ats,
                    &release_urls,
                    &created_ats,
                    &updated_ats,
                ],
            )
            .await?;
    }

    Ok(total)
}

/// Writes only the floor, so a run cannot blank a release's publish timestamp.
async fn upsert_floors(
    psql_client: &Client,
    rows: &BTreeMap<(String, String), ReleaseRow>,
    written_at: DateTime<Utc>,
) -> anyhow::Result<u64> {
    let records: Vec<_> = rows.values().collect();
    let mut total = 0;

    for chunk in records.chunks(DEFAULT_CHUNK_SIZE) {
        let client_lineages: Vec<&str> = chunk.iter().map(|r| r.client_lineage.as_str()).collect();
        let client_versions: Vec<&str> = chunk.iter().map(|r| r.client_version.as_str()).collect();
        let sfdp_floor_epochs: Vec<Option<&Decimal>> =
            chunk.iter().map(|r| r.sfdp_floor_epoch.as_ref()).collect();
        let updated_ats: Vec<&DateTime<Utc>> = vec![&written_at; chunk.len()];
        let created_ats = updated_ats.clone();

        total += psql_client
            .execute(
                &format!(
                    "INSERT INTO {RELEASES_TABLE} (
                client_lineage, client_version, sfdp_floor_epoch, created_at, updated_at
            )
            SELECT * FROM UNNEST(
                $1::TEXT[],
                $2::TEXT[],
                $3::NUMERIC[],
                $4::TIMESTAMP WITH TIME ZONE[],
                $5::TIMESTAMP WITH TIME ZONE[]
            )
            ON CONFLICT (client_lineage, client_version)
            DO UPDATE SET
                sfdp_floor_epoch = EXCLUDED.sfdp_floor_epoch,
                updated_at = EXCLUDED.updated_at"
                ),
                &[
                    &client_lineages,
                    &client_versions,
                    &sfdp_floor_epochs,
                    &created_ats,
                    &updated_ats,
                ],
            )
            .await?;
    }

    Ok(total)
}

/// What was published: one row per version the client's releases carry a timestamp for.
/// Writes only the feature-gate floor.
async fn upsert_feature_gates(
    psql_client: &Client,
    rows: &BTreeMap<(String, String), ReleaseRow>,
    written_at: DateTime<Utc>,
) -> anyhow::Result<u64> {
    let records: Vec<_> = rows.values().collect();
    let mut total = 0;

    for chunk in records.chunks(DEFAULT_CHUNK_SIZE) {
        let client_lineages: Vec<&str> = chunk.iter().map(|r| r.client_lineage.as_str()).collect();
        let client_versions: Vec<&str> = chunk.iter().map(|r| r.client_version.as_str()).collect();
        let feature_gate_epochs: Vec<Option<&Decimal>> = chunk
            .iter()
            .map(|r| r.feature_gate_epoch.as_ref())
            .collect();
        let updated_ats: Vec<&DateTime<Utc>> = vec![&written_at; chunk.len()];
        let created_ats = updated_ats.clone();

        total += psql_client
            .execute(
                &format!(
                    "INSERT INTO {RELEASES_TABLE} (
                client_lineage, client_version, feature_gate_epoch, created_at, updated_at
            )
            SELECT * FROM UNNEST(
                $1::TEXT[],
                $2::TEXT[],
                $3::NUMERIC[],
                $4::TIMESTAMP WITH TIME ZONE[],
                $5::TIMESTAMP WITH TIME ZONE[]
            )
            ON CONFLICT (client_lineage, client_version)
            DO UPDATE SET
                feature_gate_epoch = EXCLUDED.feature_gate_epoch,
                updated_at = EXCLUDED.updated_at"
                ),
                &[
                    &client_lineages,
                    &client_versions,
                    &feature_gate_epochs,
                    &created_ats,
                    &updated_ats,
                ],
            )
            .await?;
    }

    Ok(total)
}

pub async fn load_releases(
    psql_client: &Client,
    client_lineage: Option<&str>,
    since_epoch: Option<u64>,
) -> anyhow::Result<Vec<ReleaseRecord>> {
    let since_epoch = since_epoch.map(Decimal::from);
    let rows = psql_client
        .query(
            &format!(
                "
        SELECT
            client_lineage, client_version, released_at, release_url, updated_at,
            -- Null until the epoch the release landed in closes and gets its row.
            epochs.epoch AS available_epoch
        FROM {RELEASES_TABLE}
        LEFT JOIN epochs
            ON released_at >= epochs.start_at
           AND released_at < epochs.end_at
        -- A row with no timestamp exists only because a floor named the version; it belongs in a
        -- floor list, not here.
        WHERE released_at IS NOT NULL
          AND ($1::TEXT IS NULL OR client_lineage = $1::TEXT)
          AND ($2::NUMERIC IS NULL OR released_at >= (
              -- The epoch asked for may have no row: older than the history we keep takes
              -- everything, newer than it -- the running epoch -- takes what followed the last
              -- close.
              SELECT CASE
                  WHEN $2::NUMERIC <= (SELECT MIN(epoch) FROM epochs) THEN '-infinity'::TIMESTAMPTZ
                  ELSE COALESCE(
                      (SELECT MIN(start_at) FROM epochs WHERE epoch >= $2::NUMERIC),
                      (SELECT MAX(end_at) FROM epochs)
                  )
              END
          ))
        ORDER BY released_at DESC, client_lineage, client_version
    "
            ),
            &[&client_lineage, &since_epoch],
        )
        .await?;

    rows.into_iter()
        .map(|row| {
            Ok(ReleaseRecord {
                client_lineage: row.get("client_lineage"),
                client_version: row.get("client_version"),
                available_epoch: row
                    .get::<_, Option<Decimal>>("available_epoch")
                    .map(u64::try_from)
                    .transpose()?,
                released_at: row.get("released_at"),
                release_url: row.get("release_url"),
                updated_at: row.get("updated_at"),
            })
        })
        .collect()
}

/// Every version SFDP has required, newest floor first.
pub async fn load_sfdp_floors(
    psql_client: &Client,
    client_lineage: Option<&str>,
    since_epoch: Option<u64>,
) -> anyhow::Result<Vec<SfdpFloor>> {
    Ok(
        load_floors(psql_client, "sfdp_floor_epoch", client_lineage, since_epoch)
            .await?
            .into_iter()
            .map(
                |(client_lineage, client_version, effective_epoch)| SfdpFloor {
                    client_lineage,
                    client_version,
                    effective_epoch,
                },
            )
            .collect(),
    )
}

/// Every version the cluster's feature gates have required, newest floor first.
pub async fn load_feature_gate_floors(
    psql_client: &Client,
    client_lineage: Option<&str>,
    since_epoch: Option<u64>,
) -> anyhow::Result<Vec<FeatureGateFloor>> {
    Ok(load_floors(
        psql_client,
        "feature_gate_epoch",
        client_lineage,
        since_epoch,
    )
    .await?
    .into_iter()
    .map(
        |(client_lineage, client_version, effective_epoch)| FeatureGateFloor {
            client_lineage,
            client_version,
            effective_epoch,
        },
    )
    .collect())
}

/// `column` is one of this module's own floor columns, never caller input.
async fn load_floors(
    psql_client: &Client,
    column: &str,
    client_lineage: Option<&str>,
    since_epoch: Option<u64>,
) -> anyhow::Result<Vec<(String, String, u64)>> {
    let since_epoch = since_epoch.map(Decimal::from);
    let rows = psql_client
        .query(
            &format!(
                "
        SELECT client_lineage, client_version, {column} AS effective_epoch
        FROM {RELEASES_TABLE}
        WHERE {column} IS NOT NULL
          AND ($1::TEXT IS NULL OR client_lineage = $1::TEXT)
          AND ($2::NUMERIC IS NULL OR {column} >= $2::NUMERIC)
        ORDER BY client_lineage, {column} DESC, client_version
    "
            ),
            &[&client_lineage, &since_epoch],
        )
        .await?;

    rows.into_iter()
        .map(|row| {
            Ok((
                row.get("client_lineage"),
                row.get("client_version"),
                row.get::<_, Decimal>("effective_epoch").try_into()?,
            ))
        })
        .collect()
}

/// The SFDP floor in force at `epoch`, one row per lineage: the latest one to take effect by then.
pub async fn get_sfdp_floor_at_epoch(
    psql_client: &Client,
    client_lineage: Option<&str>,
    epoch: u64,
) -> anyhow::Result<Vec<SfdpFloor>> {
    Ok(
        floor_at_epoch(psql_client, "sfdp_floor_epoch", client_lineage, epoch)
            .await?
            .into_iter()
            .map(
                |(client_lineage, client_version, effective_epoch)| SfdpFloor {
                    client_lineage,
                    client_version,
                    effective_epoch,
                },
            )
            .collect(),
    )
}

/// The feature-gate floor in force at `epoch`, one row per lineage. Below it a validator forks off,
/// which is the harder of the two obligations.
pub async fn get_feature_gate_floor_at_epoch(
    psql_client: &Client,
    client_lineage: Option<&str>,
    epoch: u64,
) -> anyhow::Result<Vec<FeatureGateFloor>> {
    Ok(
        floor_at_epoch(psql_client, "feature_gate_epoch", client_lineage, epoch)
            .await?
            .into_iter()
            .map(
                |(client_lineage, client_version, effective_epoch)| FeatureGateFloor {
                    client_lineage,
                    client_version,
                    effective_epoch,
                },
            )
            .collect(),
    )
}

async fn floor_at_epoch(
    psql_client: &Client,
    column: &str,
    client_lineage: Option<&str>,
    epoch: u64,
) -> anyhow::Result<Vec<(String, String, u64)>> {
    let epoch = Decimal::from(epoch);
    let rows = psql_client
        .query(
            &format!(
                "
        SELECT client_lineage, client_version, {column} AS effective_epoch
        FROM {RELEASES_TABLE}
        WHERE {column} IS NOT NULL
          AND {column} <= $1::NUMERIC
          AND ($2::TEXT IS NULL OR client_lineage = $2::TEXT)
    "
            ),
            &[&epoch, &client_lineage],
        )
        .await?;

    // Picked here rather than with DISTINCT ON: two rows can share the newest epoch, and the higher
    // version settles that, which SQL would order as text.
    let mut newest: BTreeMap<String, (u64, ValidatorVersion)> = BTreeMap::new();
    for row in rows {
        let lineage: String = row.get("client_lineage");
        let version: String = row.get("client_version");
        let effective_epoch: u64 = row.get::<_, Decimal>("effective_epoch").try_into()?;
        let Ok(version) = version.parse::<ValidatorVersion>() else {
            warn!("Floor row {lineage} {version} is not a version, skipping it");
            continue;
        };

        newest
            .entry(lineage)
            .and_modify(|floor| {
                if (effective_epoch, &version) > (floor.0, &floor.1) {
                    *floor = (effective_epoch, version.clone());
                }
            })
            .or_insert((effective_epoch, version));
    }

    Ok(newest
        .into_iter()
        .map(|(lineage, (epoch, version))| (lineage, version.to_string(), epoch))
        .collect())
}

/// The newest release each lineage has published, keyed by the lowercased lineage. Reads `releases`
/// in the order `load_releases` returns them, newest first.
pub fn latest_releases(releases: Vec<ReleaseRecord>) -> HashMap<String, ClientRelease> {
    let mut latest: HashMap<String, ClientRelease> = Default::default();
    for release in releases {
        latest
            .entry(release.client_lineage.to_lowercase())
            .or_insert(ClientRelease {
                version: release.client_version,
                released_at: release.released_at,
                url: release.release_url,
            });
    }
    latest
}
