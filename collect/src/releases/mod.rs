use crate::common::CommonParams;
use crate::solana_service::solana_client;
use crate::validator_version::ValidatorVersion;
use chrono::{DateTime, Utc};
use clap::Parser;
use log::info;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub mod feature_gates;
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
    FeatureGates,
}

impl ReleaseSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            ReleaseSource::Github => "github",
            ReleaseSource::Sfdp => "sfdp",
            ReleaseSource::FeatureGates => "feature_gates",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseEntry {
    /// agave | frankendancer | firedancer | sig, matching `ClientId::groupings()`.
    pub client_lineage: String,
    pub client_version: ValidatorVersion,
    /// Publish time. The epoch it falls in is resolved when the snapshot is stored, against the
    /// `epochs` table this crate cannot reach.
    pub released_at: Option<DateTime<Utc>>,
    /// First epoch SFDP required this version.
    pub sfdp_floor_epoch: Option<u64>,
    /// First epoch the cluster's feature gates required this version.
    pub feature_gate_epoch: Option<u64>,
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

/// Frankendancer is `0.<minor >= 100>.<agave>`; anything else, `0.1.x` included, is Firedancer.
pub fn firedancer_lineage(version: &ValidatorVersion) -> &'static str {
    if version.major() == 0 && version.minor() >= 100 {
        "frankendancer"
    } else {
        "firedancer"
    }
}

/// Firedancer's main branch reports `0.1.1`; its releases are calendar versions.
pub fn is_firedancer_dev_build(lineage: &str, version: &ValidatorVersion) -> bool {
    lineage == "firedancer" && version.major() == 0
}

#[derive(Debug, Parser)]
pub struct ReleasesParams {
    #[arg(
        long = "source",
        help = "Which fetchers to run.",
        value_delimiter = ',',
        default_value = "github,sfdp,feature-gates"
    )]
    sources: Vec<ReleaseSource>,

    #[arg(
        long = "epochs-back",
        help = "How many epochs back from the current epoch to (re-)query for version floors.",
        default_value = "8"
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
        long = "feature-gate-schedule-url",
        help = "The feature gate tracker's machine-readable schedule.",
        default_value = feature_gates::SCHEDULE_JSON_URL
    )]
    feature_gate_schedule_url: String,

    #[arg(
        long = "version-floor-url",
        help = "The floor Anza publishes, read as a cross-check on the derived one.",
        default_value = feature_gates::VERSION_FLOOR_JSON_URL
    )]
    version_floor_url: String,

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
            ReleaseSource::FeatureGates => {
                fetchers.push(Box::new(feature_gates::FeatureGatesFetcher::new(
                    params.feature_gate_schedule_url.clone(),
                    params.version_floor_url.clone(),
                    common_params.rpc_url.clone(),
                    common_params.commitment.clone(),
                )?))
            }
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
    fn lineage_splits_on_the_numbering_scheme() {
        let lineage = |text: &str| firedancer_lineage(&text.parse().unwrap());
        assert_eq!(lineage("0.1106.40201"), "frankendancer");
        assert_eq!(lineage("0.905.0-beta.40007"), "frankendancer");
        assert_eq!(lineage("26.8.2"), "firedancer");
        assert_eq!(lineage("1.1.4"), "firedancer");
        assert_eq!(lineage("0.1.1"), "firedancer");
        assert_eq!(lineage("0.1.0"), "firedancer");
        assert_eq!(lineage("0.99.0"), "firedancer");
        assert_eq!(lineage("0.100.0"), "frankendancer");
        assert_eq!(lineage("0.101.0"), "frankendancer");
    }

    #[test]
    fn a_dev_build_is_a_firedancer_version_with_major_zero() {
        let dev =
            |lineage: &str, text: &str| is_firedancer_dev_build(lineage, &text.parse().unwrap());
        assert!(dev("firedancer", "0.1.1"));
        assert!(dev("firedancer", "0.1.0"));
        assert!(dev("firedancer", "0.1204.40300"));
        assert!(!dev("firedancer", "26.10.6"));
        assert!(!dev("firedancer", "1.1.4"));
        assert!(!dev("frankendancer", "0.1106.40201"));
        assert!(!dev("frankendancer", "0.100.0"));
        assert!(!dev("frankendancer", "0.1.1"));
        assert!(!dev("agave", "0.1.1"));
        assert!(!dev("sig", "0.5.0"));
    }
}
