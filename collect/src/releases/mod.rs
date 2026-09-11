use crate::common::CommonParams;
use crate::solana_service::solana_client;
use chrono::{DateTime, Utc};
use clap::Parser;
use log::info;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub mod github;
pub mod sfdp;

const DATA_VERSION: u16 = 1;

pub(crate) const HTTP_TIMEOUT_S: u64 = 30;

/// Which columns of a release row the entry fills: a new source is a new [`ReleaseFetcher`] plus a
/// variant here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseSource {
    Github,
    Sfdp,
}

impl ReleaseSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            ReleaseSource::Github => "github",
            ReleaseSource::Sfdp => "sfdp",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseEntry {
    /// agave | frankendancer | firedancer | sig, matching `ClientId::groupings()`.
    pub client_lineage: String,
    /// As the client reports it in gossip, e.g. `4.2.2` or `0.1106.40201`.
    pub client_version: String,
    /// Publish time. The epoch it falls in is resolved when the snapshot is stored, against the
    /// `epochs` table this crate cannot reach.
    pub released_at: Option<DateTime<Utc>>,
    /// First epoch SFDP required this version. The cluster's own feature-gate floors are static
    /// data, not collected: see `store::feature_gates`.
    pub sfdp_floor_epoch: Option<u64>,
    pub release_url: Option<String>,
    /// Which columns this entry fills; the table has no source column.
    pub source: ReleaseSource,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ReleasesSnapshot {
    pub version: u16,
    pub created_at: String,
    pub releases: Vec<ReleaseEntry>,
}

pub trait ReleaseFetcher {
    fn fetch(&self) -> anyhow::Result<Vec<ReleaseEntry>>;
}

/// Gossip reports `26.8.2` where the Firedancer tag zero-pads to `v26.08.2`, and no tag carries a
/// leading zero that means anything, so the padding is dropped to keep one spelling per version.
/// A prerelease suffix is carried over untouched.
pub fn normalize_version(tag: &str) -> String {
    let version = tag.trim().trim_start_matches('v');
    let (numbers, prerelease) = match version.split_once('-') {
        Some((numbers, prerelease)) => (numbers, Some(prerelease)),
        None => (version, None),
    };

    let parts: Vec<&str> = numbers.split('.').collect();
    if parts.len() != 3
        || !parts
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
    {
        return version.to_string();
    }

    let numbers = parts
        .iter()
        .map(|part| part.trim_start_matches('0'))
        .map(|part| if part.is_empty() { "0" } else { part })
        .collect::<Vec<_>>()
        .join(".");

    match prerelease {
        Some(prerelease) => format!("{numbers}-{prerelease}"),
        None => numbers,
    }
}

/// Frankendancer keeps the `0.<frankendancer>.<agave>` numbering it always had; Firedancer proper
/// releases under its own major (`1.1.4`, then calendar versions like `26.8.2`).
pub fn firedancer_lineage(version: &str) -> &'static str {
    if version.starts_with("0.") {
        "frankendancer"
    } else {
        "firedancer"
    }
}

#[derive(Debug, Parser)]
pub struct ReleasesParams {
    #[arg(
        long = "source",
        help = "Which fetchers to run.",
        value_delimiter = ',',
        default_value = "github,sfdp"
    )]
    sources: Vec<ReleaseSource>,

    #[arg(
        long = "epochs-back",
        help = "How many epochs back from the current epoch to (re-)query for version floors.",
        default_value = "20"
    )]
    epochs_back: u64,

    #[arg(
        long = "from-epoch",
        help = "Query version floors from this epoch onwards. Overrides --epochs-back (use for historical backfill; the SFDP API answers from epoch 688 on)."
    )]
    from_epoch: Option<u64>,

    #[arg(
        long = "sfdp-api-url",
        env = "SFDP_API_URL",
        help = "The Solana Foundation API root",
        default_value = sfdp::SFDP_API
    )]
    sfdp_api_url: String,

    #[arg(
        long = "github-api-url",
        env = "GITHUB_API_URL",
        help = "The GitHub API root",
        default_value = github::GITHUB_API
    )]
    github_api_url: String,

    #[arg(
        long = "github-token",
        env = "GITHUB_TOKEN",
        hide_env_values = true,
        help = "Raises the GitHub rate limit from 60 requests per hour; unauthenticated works for a single run."
    )]
    github_token: Option<String>,
}

pub fn collect_releases_info(
    common_params: CommonParams,
    params: ReleasesParams,
) -> anyhow::Result<()> {
    let created_at = Utc::now();
    let mut fetchers: Vec<Box<dyn ReleaseFetcher>> = Vec::new();

    for name in &params.sources {
        match name {
            ReleaseSource::Github => fetchers.push(Box::new(github::GithubFetcher::new(
                params.github_api_url.clone(),
                params.github_token.clone(),
            )?)),
            ReleaseSource::Sfdp => {
                // Only this fetcher needs the cluster's current epoch, so the RPC call stays inside.
                let client = solana_client(
                    common_params.rpc_url.clone(),
                    common_params.commitment.clone(),
                );
                let current_epoch = client.get_epoch_info()?.epoch;
                let from_epoch = params
                    .from_epoch
                    .unwrap_or_else(|| current_epoch.saturating_sub(params.epochs_back));
                fetchers.push(Box::new(sfdp::SfdpFetcher::new(
                    params.sfdp_api_url.clone(),
                    from_epoch,
                    // A floor is revisable until its epoch starts.
                    current_epoch,
                )?));
            }
        }
    }

    let mut releases = Vec::new();
    for fetcher in fetchers {
        releases.extend(fetcher.fetch()?);
    }

    let mut per_source: BTreeMap<&str, usize> = BTreeMap::new();
    for release in &releases {
        *per_source.entry(release.source.as_str()).or_default() += 1;
    }
    info!("Collected {} release rows: {per_source:?}", releases.len());

    serde_yaml::to_writer(
        std::io::stdout(),
        &ReleasesSnapshot {
            version: DATA_VERSION,
            created_at: created_at.to_rfc3339(),
            releases,
        },
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn firedancer_tags_normalize_to_what_gossip_reports() {
        // Live gossip at epoch 1031: Firedancer says 26.8.2, Frankendancer says 0.1106.40201.
        assert_eq!(normalize_version("v26.08.2"), "26.8.2");
        assert_eq!(normalize_version("v0.1106.40201"), "0.1106.40201");
        assert_eq!(normalize_version("v1.1.4"), "1.1.4");
        // A padded core keeps its suffix and still loses the padding.
        assert_eq!(normalize_version("v26.08.2-rc.1"), "26.8.2-rc.1");
    }

    #[test]
    fn agave_tags_keep_their_prerelease_verbatim() {
        assert_eq!(normalize_version("v4.2.2"), "4.2.2");
        assert_eq!(normalize_version("v4.0.0-rc.0"), "4.0.0-rc.0");
        // A Frankendancer prerelease encodes an Agave version that never shipped; it must survive
        // as written rather than be rewritten into something that looks like a release.
        assert_eq!(
            normalize_version("v0.905.0-beta.40007"),
            "0.905.0-beta.40007"
        );
    }

    #[test]
    fn lineage_splits_on_the_numbering_scheme() {
        assert_eq!(firedancer_lineage("0.1106.40201"), "frankendancer");
        assert_eq!(firedancer_lineage("0.905.0-beta.40007"), "frankendancer");
        assert_eq!(firedancer_lineage("26.8.2"), "firedancer");
        assert_eq!(firedancer_lineage("1.1.4"), "firedancer");
    }
}
