use crate::common::is_base58_pubkey;
use crate::releases::github::{github_client, GITHUB_API};
use anyhow::{bail, ensure, Context};
use clap::Parser;
use csv::{required, Column};
use log::{error, info};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::time::Duration;

pub const DEFAULT_REPO: &str = "marinade-finance/ds-sam-pipeline";
pub const RAW_URL: &str = "https://raw.githubusercontent.com";
const ADDED_FILE: &str = "outputs/blacklist-added.csv";
const HTTP_TIMEOUT_S: u64 = 60;
const DATA_VERSION: u16 = 1;
const COLUMNS: [Column; 2] = [required("vote_account"), required("code")];

#[derive(Debug, Parser)]
pub struct BlacklistAuctionsParams {
    #[arg(long = "repo", default_value = DEFAULT_REPO)]
    repo: String,

    #[arg(long = "branch", default_value = "main")]
    branch: String,

    #[arg(
        long = "from-epoch",
        help = "The first SAM auction that decides the blacklist; earlier runs carry no listings."
    )]
    from_epoch: u64,

    #[arg(
        long = "epochs-back",
        help = "How many auction epochs before the latest one to (re-)read, so a missed or re-run auction is caught up.",
        default_value = "10"
    )]
    epochs_back: u64,

    #[arg(long = "github-api-url", default_value = GITHUB_API)]
    github_api_url: String,

    #[arg(long = "raw-url", default_value = RAW_URL)]
    raw_url: String,

    #[arg(
        long = "github-token",
        env = "GITHUB_TOKEN",
        help = "Optional; lifts the unauthenticated rate limit of the GitHub API."
    )]
    github_token: Option<String>,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct Listing {
    pub vote_account: String,
    pub code: String,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct AuctionListings {
    pub auction_epoch: u64,
    /// The run directory, `<epoch>.<slot>`.
    pub run_id: String,
    pub rows: Vec<Listing>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BlacklistAuctionsSnapshot {
    pub version: u16,
    pub created_at: String,
    pub runs: Vec<AuctionListings>,
    /// Runs whose file could not be read or parsed; the store fails after importing the rest.
    #[serde(default)]
    pub refused: Vec<String>,
}

/// The epoch and slot of a run directory name, `<epoch>.<slot>`.
pub fn run_epoch_slot(name: &str) -> Option<(u64, u64)> {
    let (epoch, slot) = name.split_once('.')?;
    Some((epoch.parse().ok()?, slot.parse().ok()?))
}

/// The newest run (highest slot) of every auction epoch among `auctions/` directory names. A
/// re-run of an epoch gets a later slot, so the newest one is the one applied last.
pub fn newest_runs<'a>(names: impl IntoIterator<Item = &'a str>) -> BTreeMap<u64, String> {
    let mut newest: BTreeMap<u64, (u64, String)> = BTreeMap::new();
    for name in names {
        let Some((epoch, slot)) = run_epoch_slot(name) else {
            continue;
        };
        if newest.get(&epoch).is_none_or(|(known, _)| slot > *known) {
            newest.insert(epoch, (slot, name.to_string()));
        }
    }
    newest
        .into_iter()
        .map(|(epoch, (_, name))| (epoch, name))
        .collect()
}

/// The runs of the epochs to read: from `from_epoch`, and at most `epochs_back` before the latest.
pub fn runs_to_read(
    newest: &BTreeMap<u64, String>,
    from_epoch: u64,
    epochs_back: u64,
) -> Vec<(u64, String)> {
    let Some(latest) = newest.keys().next_back() else {
        return vec![];
    };
    let first = from_epoch.max(latest.saturating_sub(epochs_back));
    newest
        .range(first..)
        .map(|(epoch, run)| (*epoch, run.clone()))
        .collect()
}

/// The rows of one run's `blacklist-added.csv`. A body without its final newline is a cut-off
/// download, so it is refused rather than imported short.
pub fn parse_added_csv(text: &str, label: &str) -> anyhow::Result<Vec<Listing>> {
    ensure!(text.ends_with('\n'), "{label}: truncated, no final newline");
    let rows: Vec<Listing> = csv::parse(text, &COLUMNS, label)?;
    for (i, row) in rows.iter().enumerate() {
        ensure!(
            is_base58_pubkey(&row.vote_account),
            "{label}: row {i} vote account is not a base58 pubkey"
        );
        ensure!(
            !row.code.is_empty()
                && row
                    .code
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'),
            "{label}: row {i} code must match [A-Z0-9_]+"
        );
    }
    Ok(rows)
}

#[derive(Debug, Deserialize)]
struct TreeEntry {
    path: String,
    #[serde(rename = "type")]
    kind: String,
    sha: String,
}

#[derive(Debug, Deserialize)]
struct Tree {
    tree: Vec<TreeEntry>,
    truncated: bool,
}

struct Github {
    client: reqwest::blocking::Client,
    params: BlacklistAuctionsParams,
}

impl Github {
    fn tree(&self, tree: &str) -> anyhow::Result<Tree> {
        let url = format!(
            "{}/repos/{}/git/trees/{tree}",
            self.params.github_api_url, self.params.repo
        );
        let mut request = self.client.get(&url);
        if let Some(token) = &self.params.github_token {
            request = request.bearer_auth(token);
        }
        let tree: Tree = request
            .send()
            .with_context(|| format!("requesting {url}"))?
            .error_for_status()
            .with_context(|| format!("requesting {url}"))?
            .json()
            .with_context(|| format!("decoding {url}"))?;
        ensure!(!tree.truncated, "{url}: GitHub truncated the tree listing");
        Ok(tree)
    }

    fn run_names(&self) -> anyhow::Result<Vec<String>> {
        let root = self.tree(&self.params.branch)?;
        let Some(auctions) = root
            .tree
            .into_iter()
            .find(|entry| entry.path == "auctions" && entry.kind == "tree")
        else {
            bail!("{} has no auctions directory", self.params.repo);
        };
        Ok(self
            .tree(&auctions.sha)?
            .tree
            .into_iter()
            .filter(|entry| entry.kind == "tree")
            .map(|entry| entry.path)
            .collect())
    }

    /// `None` when the run has no file: an auction run without the blacklist rules.
    fn added_csv(&self, run: &str) -> anyhow::Result<Option<String>> {
        let url = format!(
            "{}/{}/{}/auctions/{run}/{ADDED_FILE}",
            self.params.raw_url, self.params.repo, self.params.branch
        );
        let response = self
            .client
            .get(&url)
            .send()
            .with_context(|| format!("requesting {url}"))?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        Ok(Some(
            response
                .error_for_status()
                .with_context(|| format!("requesting {url}"))?
                .text()?,
        ))
    }
}

pub fn collect_blacklist_auctions(params: BlacklistAuctionsParams) -> anyhow::Result<()> {
    info!("Collecting the blacklist listings of the applied SAM auctions");
    let created_at = chrono::Utc::now();
    let github = Github {
        client: github_client(Duration::from_secs(HTTP_TIMEOUT_S))?,
        params,
    };

    let names = github.run_names()?;
    let newest = newest_runs(names.iter().map(String::as_str));
    let mut runs = Vec::new();
    let mut refused = Vec::new();
    for (auction_epoch, run_id) in
        runs_to_read(&newest, github.params.from_epoch, github.params.epochs_back)
    {
        let rows = match github.added_csv(&run_id) {
            Ok(None) => {
                error!(
                    "Auction run {run_id} has no {ADDED_FILE}, leaving epoch {auction_epoch} as it is"
                );
                continue;
            }
            Ok(Some(text)) => parse_added_csv(&text, &run_id),
            Err(err) => Err(err),
        };
        match rows {
            Ok(rows) => {
                info!("Auction run {run_id}: {} listings", rows.len());
                runs.push(AuctionListings {
                    auction_epoch,
                    run_id,
                    rows,
                });
            }
            Err(err) => {
                error!(
                    "Auction run {run_id} refused, leaving epoch {auction_epoch} as it is: {err:#}"
                );
                refused.push(run_id);
            }
        }
    }

    serde_yaml::to_writer(
        std::io::stdout(),
        &BlacklistAuctionsSnapshot {
            version: DATA_VERSION,
            created_at: created_at.to_string(),
            runs,
            refused,
        },
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACCOUNT: &str = "BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB";

    #[test]
    fn the_newest_slot_of_each_epoch_wins() {
        let newest = newest_runs([
            "1084.40290",
            "1084.51000",
            "1084.9999",
            "1085.100",
            "README.md",
            "1086",
        ]);
        assert_eq!(
            newest,
            BTreeMap::from([(1084, "1084.51000".into()), (1085, "1085.100".into())])
        );
    }

    #[test]
    fn runs_start_at_the_cutover_and_reach_back_a_window() {
        let newest = newest_runs(["1080.1", "1081.1", "1082.1", "1083.1", "1084.1"]);
        let epochs = |from, back| -> Vec<u64> {
            runs_to_read(&newest, from, back)
                .into_iter()
                .map(|(e, _)| e)
                .collect()
        };
        assert_eq!(epochs(1082, 10), vec![1082, 1083, 1084]);
        assert_eq!(epochs(1070, 1), vec![1083, 1084]);
        assert_eq!(epochs(1090, 10), Vec::<u64>::new());
        assert!(runs_to_read(&BTreeMap::new(), 0, 10).is_empty());
    }

    #[test]
    fn the_added_csv_parses_with_its_header() {
        assert_eq!(
            parse_added_csv(
                &format!("vote_account,code\n{ACCOUNT},MALICIOUS_SM_1083\n"),
                "run"
            )
            .unwrap(),
            vec![Listing {
                vote_account: ACCOUNT.into(),
                code: "MALICIOUS_SM_1083".into()
            }]
        );
        assert!(parse_added_csv("vote_account,code\n", "run")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn a_cut_off_or_malformed_file_is_refused() {
        let error = |text: &str| format!("{:#}", parse_added_csv(text, "run").unwrap_err());
        assert!(
            error(&format!("vote_account,code\n{ACCOUNT},MALICIOUS_SM_1083")).contains("truncated")
        );
        assert!(error("vote_account,code\nshort,MALICIOUS_SM_1083\n").contains("base58"));
        assert!(error(&format!("vote_account,code\n{ACCOUNT},malicious\n")).contains("[A-Z0-9_]+"));
        assert!(error(&format!("code,other\nX,{ACCOUNT}\n")).contains("vote_account"));
    }
}
