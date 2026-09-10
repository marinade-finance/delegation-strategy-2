use crate::dto::{ReleaseRecord, SfdpFloor};
use chrono::{DateTime, Utc};
use clap::Parser;
use collect::releases::{ReleaseEntry, ReleaseSource, ReleasesSnapshot};
use log::{info, warn};
use rust_decimal::prelude::*;
use std::collections::BTreeMap;
use tokio_postgres::Client;

pub const RELEASES_TABLE: &str = "releases";

const SUPPORTED_DATA_VERSION: u16 = 1;
const DEFAULT_CHUNK_SIZE: usize = 500;

#[derive(Debug, Parser)]
pub struct StoreReleasesParams {
    #[arg(long = "snapshot-file")]
    snapshot_path: String,
}

/// Epoch boundaries as the DB knows them, so a release timestamp can be placed in an epoch.
pub struct EpochCalendar {
    closed: Vec<(u64, DateTime<Utc>, DateTime<Utc>)>,
    running: Option<u64>,
}

impl EpochCalendar {
    pub fn new(closed: Vec<(u64, DateTime<Utc>, DateTime<Utc>)>, running: Option<u64>) -> Self {
        Self { closed, running }
    }

    /// `None` where the timestamp predates the epochs we hold, or falls in a gap between them:
    /// guessing an epoch there would invent the very lateness this data is meant to measure.
    pub fn epoch_at(&self, at: DateTime<Utc>) -> Option<u64> {
        if let Some((epoch, _, _)) = self
            .closed
            .iter()
            .find(|(_, start_at, end_at)| at >= *start_at && at < *end_at)
        {
            return Some(*epoch);
        }

        // Past the last closed epoch: the running one is the only epoch it can belong to, and it
        // has no `epochs` row until `store close-epoch` writes one.
        let (_, _, last_end_at) = self.closed.last()?;
        if at >= *last_end_at {
            return self.running;
        }

        None
    }
}

pub async fn load_epoch_calendar(psql_client: &Client) -> anyhow::Result<EpochCalendar> {
    let mut closed = Vec::new();
    for row in psql_client
        .query(
            "SELECT epoch, start_at, end_at FROM epochs ORDER BY epoch",
            &[],
        )
        .await?
    {
        closed.push((
            row.get::<_, Decimal>("epoch").try_into()?,
            row.get("start_at"),
            row.get("end_at"),
        ));
    }

    let running = psql_client
        .query_opt(
            "
        SELECT epoch FROM cluster_info
        WHERE epoch > COALESCE((SELECT MAX(epoch) FROM epochs), 0)
        ORDER BY epoch DESC, epoch_slot DESC LIMIT 1
    ",
            &[],
        )
        .await?
        .map(|row| row.get::<_, Decimal>("epoch").try_into())
        .transpose()?;

    Ok(EpochCalendar::new(closed, running))
}

struct ReleaseRow {
    client_lineage: String,
    client_version: String,
    available_epoch: Option<Decimal>,
    released_at: Option<DateTime<Utc>>,
    release_url: Option<String>,
    sfdp_floor_epoch: Option<Decimal>,
}

fn to_row(entry: &ReleaseEntry, calendar: &EpochCalendar) -> ReleaseRow {
    let available_epoch = entry.released_at.and_then(|at| calendar.epoch_at(at));

    ReleaseRow {
        client_lineage: entry.client_lineage.clone(),
        client_version: entry.client_version.clone(),
        available_epoch: available_epoch.map(Decimal::from),
        released_at: entry.released_at,
        release_url: entry.release_url.clone(),
        sfdp_floor_epoch: entry.sfdp_floor_epoch.map(Decimal::from),
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

    let calendar = load_epoch_calendar(psql_client).await?;

    // Split by what the entry fills, because each set of columns is upserted on its own and one
    // statement must never touch the same conflict target twice.
    let mut availability: BTreeMap<(String, String), ReleaseRow> = BTreeMap::new();
    let mut floors: BTreeMap<(String, String), ReleaseRow> = BTreeMap::new();
    for entry in &snapshot.releases {
        let row = to_row(entry, &calendar);
        let key = (row.client_lineage.clone(), row.client_version.clone());
        let rows = match entry.source {
            ReleaseSource::Github => &mut availability,
            ReleaseSource::Sfdp => &mut floors,
        };
        if rows.insert(key.clone(), row).is_some() {
            warn!("Snapshot carries {key:?} more than once, keeping the last");
        }
    }

    let unplaced = availability
        .values()
        .filter(|row| row.released_at.is_some() && row.available_epoch.is_none())
        .count();
    if unplaced > 0 {
        info!("{unplaced} releases predate the epochs we hold, so they carry no available_epoch");
    }

    let upserted_availability =
        upsert_availability(psql_client, &availability, snapshot_created_at).await?;
    let upserted_floors = upsert_floors(psql_client, &floors, snapshot_created_at).await?;

    info!("Stored releases snapshot: {upserted_availability} availability rows, {upserted_floors} floors");

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
        let available_epochs: Vec<Option<&Decimal>> =
            chunk.iter().map(|r| r.available_epoch.as_ref()).collect();
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
                client_lineage, client_version, available_epoch, released_at, release_url,
                created_at, updated_at
            )
            SELECT * FROM UNNEST(
                $1::TEXT[],
                $2::TEXT[],
                $3::NUMERIC[],
                $4::TIMESTAMP WITH TIME ZONE[],
                $5::TEXT[],
                $6::TIMESTAMP WITH TIME ZONE[],
                $7::TIMESTAMP WITH TIME ZONE[]
            )
            ON CONFLICT (client_lineage, client_version)
            DO UPDATE SET
                available_epoch = EXCLUDED.available_epoch,
                released_at = EXCLUDED.released_at,
                release_url = EXCLUDED.release_url,
                updated_at = EXCLUDED.updated_at"
                ),
                &[
                    &client_lineages,
                    &client_versions,
                    &available_epochs,
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
            client_lineage, client_version, available_epoch, released_at, release_url, updated_at
        FROM {RELEASES_TABLE}
        -- A row with no timestamp exists only because SFDP named the version as a floor; it belongs
        -- in the floor list, not here.
        WHERE released_at IS NOT NULL
          AND ($1::TEXT IS NULL OR client_lineage = $1::TEXT)
          AND ($2::NUMERIC IS NULL OR available_epoch >= $2::NUMERIC)
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
    let since_epoch = since_epoch.map(Decimal::from);
    let rows = psql_client
        .query(
            &format!(
                "
        SELECT client_lineage, client_version, sfdp_floor_epoch
        FROM {RELEASES_TABLE}
        WHERE sfdp_floor_epoch IS NOT NULL
          AND ($1::TEXT IS NULL OR client_lineage = $1::TEXT)
          AND ($2::NUMERIC IS NULL OR sfdp_floor_epoch >= $2::NUMERIC)
        ORDER BY client_lineage, sfdp_floor_epoch DESC
    "
            ),
            &[&client_lineage, &since_epoch],
        )
        .await?;

    rows.into_iter()
        .map(|row| {
            Ok(SfdpFloor {
                client_lineage: row.get("client_lineage"),
                client_version: row.get("client_version"),
                effective_epoch: row.get::<_, Decimal>("sfdp_floor_epoch").try_into()?,
            })
        })
        .collect()
}

/// The SFDP floor in force at `epoch`, one row per lineage: the latest one to take effect by then.
///
/// This is the delegation-program requirement only. The cluster's feature-gate floor is static data
/// and answered by `crate::feature_gates::floor_at_epoch`.
pub async fn get_sfdp_floor_at_epoch(
    psql_client: &Client,
    client_lineage: Option<&str>,
    epoch: u64,
) -> anyhow::Result<Vec<SfdpFloor>> {
    let epoch = Decimal::from(epoch);
    let rows = psql_client
        .query(
            &format!(
                "
        SELECT DISTINCT ON (client_lineage)
            client_lineage, client_version, sfdp_floor_epoch
        FROM {RELEASES_TABLE}
        WHERE sfdp_floor_epoch IS NOT NULL
          AND sfdp_floor_epoch <= $1::NUMERIC
          AND ($2::TEXT IS NULL OR client_lineage = $2::TEXT)
        ORDER BY client_lineage, sfdp_floor_epoch DESC
    "
            ),
            &[&epoch, &client_lineage],
        )
        .await?;

    let mut records = Vec::with_capacity(rows.len());
    for row in rows {
        records.push(SfdpFloor {
            client_lineage: row.get("client_lineage"),
            client_version: row.get("client_version"),
            effective_epoch: row.get::<_, Decimal>("sfdp_floor_epoch").try_into()?,
        });
    }

    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(timestamp: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(timestamp)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn calendar() -> EpochCalendar {
        EpochCalendar::new(
            vec![
                (1029, at("2026-08-30T00:00:00Z"), at("2026-09-01T00:00:00Z")),
                (1030, at("2026-09-01T00:00:00Z"), at("2026-09-03T00:00:00Z")),
            ],
            Some(1031),
        )
    }

    #[test]
    fn a_release_lands_in_the_epoch_that_contains_its_timestamp() {
        assert_eq!(calendar().epoch_at(at("2026-08-30T00:00:00Z")), Some(1029));
        assert_eq!(calendar().epoch_at(at("2026-09-02T23:59:59Z")), Some(1030));
    }

    #[test]
    fn a_release_after_the_last_closed_epoch_lands_in_the_running_one() {
        assert_eq!(calendar().epoch_at(at("2026-09-03T00:00:01Z")), Some(1031));
    }

    #[test]
    fn a_release_older_than_the_epochs_we_hold_lands_nowhere() {
        assert_eq!(calendar().epoch_at(at("2020-03-19T00:00:00Z")), None);
    }

    #[test]
    fn with_no_running_epoch_a_recent_release_lands_nowhere() {
        let calendar = EpochCalendar::new(
            vec![(1030, at("2026-09-01T00:00:00Z"), at("2026-09-03T00:00:00Z"))],
            None,
        );
        assert_eq!(calendar.epoch_at(at("2026-09-04T00:00:00Z")), None);
        assert_eq!(calendar.epoch_at(at("2026-09-02T00:00:00Z")), Some(1030));
    }

    #[test]
    fn an_empty_calendar_places_nothing() {
        let calendar = EpochCalendar::new(vec![], Some(1031));
        assert_eq!(calendar.epoch_at(at("2026-09-04T00:00:00Z")), None);
    }
}
