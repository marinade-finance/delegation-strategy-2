use super::{firedancer_lineage, normalize_version, ReleaseEntry, ReleaseFetcher, ReleaseSource};
use crate::common::{retry_blocking, QuadraticBackoffStrategy};
use crate::solana_service::is_plausible_node_version;
use chrono::{DateTime, Utc};
use log::{debug, info, warn};
use serde::Deserialize;
use std::time::Duration;

pub const GITHUB_API: &str = "https://api.github.com";
const PER_PAGE: usize = 100;
/// Enough for every release Agave has ever published; a higher count means the loop lost its exit.
const MAX_PAGES: usize = 30;
const FETCH_ATTEMPTS: usize = 3;

/// `None` means the lineage follows from the version numbering, not from the repository.
const REPOS: [(&str, Option<&str>); 2] = [
    ("anza-xyz/agave", Some("agave")),
    ("firedancer-io/firedancer", None),
];

#[derive(Debug, Deserialize)]
struct GithubRelease {
    tag_name: String,
    published_at: Option<DateTime<Utc>>,
    #[serde(default)]
    draft: bool,
    html_url: Option<String>,
}

pub struct GithubFetcher {
    api_url: String,
    token: Option<String>,
    client: reqwest::blocking::Client,
}

impl GithubFetcher {
    pub fn new(api_url: String, token: Option<String>) -> anyhow::Result<Self> {
        Ok(Self {
            api_url,
            token,
            client: reqwest::blocking::Client::builder()
                // GitHub answers 403 to a request without one.
                .user_agent("marinade-delegation-strategy")
                .timeout(Duration::from_secs(super::HTTP_TIMEOUT_S))
                .build()?,
        })
    }

    fn fetch_page(&self, repo: &str, page: usize) -> anyhow::Result<Vec<GithubRelease>> {
        let url = format!(
            "{}/repos/{repo}/releases?per_page={PER_PAGE}&page={page}",
            self.api_url
        );
        let mut request = self.client.get(&url);
        if let Some(token) = &self.token {
            request = request.bearer_auth(token);
        }
        Ok(request.send()?.error_for_status()?.json()?)
    }

    fn fetch_repo(&self, repo: &str, lineage: Option<&str>) -> anyhow::Result<Vec<ReleaseEntry>> {
        let mut entries = Vec::new();

        for page in 1..=MAX_PAGES {
            let releases = retry_blocking(
                || self.fetch_page(repo, page),
                QuadraticBackoffStrategy::iter_durations(FETCH_ATTEMPTS),
                |err, attempt, backoff| {
                    warn!("Failed to fetch {repo} releases page {page} (attempt {attempt}), retrying in {backoff:?}: {err}")
                },
            )?;
            let page_size = releases.len();

            for release in releases {
                if release.draft {
                    continue;
                }
                let version = normalize_version(&release.tag_name);
                if !is_plausible_node_version(&version) {
                    // Tags that are not a client version at all: tooling releases, and the odd
                    // `fdctl-` style tag.
                    debug!("Skipping {repo} tag {}", release.tag_name);
                    continue;
                }
                entries.push(ReleaseEntry {
                    client_lineage: lineage
                        .unwrap_or_else(|| firedancer_lineage(&version))
                        .to_string(),
                    client_version: version,
                    released_at: release.published_at,
                    sfdp_floor_epoch: None,
                    release_url: release.html_url,
                    source: ReleaseSource::Github,
                });
            }

            if page_size < PER_PAGE {
                break;
            }
        }

        info!("Fetched {} releases from {repo}", entries.len());

        Ok(entries)
    }
}

impl ReleaseFetcher for GithubFetcher {
    fn fetch(&self) -> anyhow::Result<Vec<ReleaseEntry>> {
        let mut entries = Vec::new();
        for (repo, lineage) in REPOS {
            entries.extend(self.fetch_repo(repo, lineage)?);
        }
        Ok(entries)
    }
}
