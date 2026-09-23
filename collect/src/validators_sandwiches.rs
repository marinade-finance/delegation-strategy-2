use crate::{common::*, solana_service::solana_client_with_timeout};
use anyhow::Context;
use clap::Parser;
use csv::{required, Column};
use log::{info, warn};
use serde::{Deserialize, Serialize};
use solana_sdk::clock::Epoch;
use std::time::Duration;

/// The service that owns this dataset: it holds the sandwiched.me sheet exports for epochs
/// 791-886, sandwiched.me's own 887-1030 export, and a per-epoch snapshot of the mev-hub
/// validators API after that. All three land in one CSV shape, so this reads only the one route.
pub const DEFAULT_API_URL: &str = "https://solana-sandwich-report.marinade.finance";

const HTTP_TIMEOUT_S: u64 = 60;
const DATA_VERSION: u16 = 1;

#[derive(Debug, Parser)]
pub struct SandwichesParams {
    #[arg(
        long = "api-url",
        help = "Base URL of the solana-sandwich-report service.",
        default_value = DEFAULT_API_URL
    )]
    api_url: String,

    #[arg(
        long = "rpc-timeout",
        help = "How long to wait for RPC response (seconds).",
        default_value = "300"
    )]
    rpc_timeout: u64,

    #[arg(
        long = "epochs-back",
        help = "How many epochs back from the current epoch to (re-)query. An epoch CSV never changes once published, so a small window is enough outside a backfill.",
        default_value = "10"
    )]
    epochs_back: u64,

    #[arg(
        long = "from-epoch",
        help = "Read epochs from this one onwards. Overrides --epochs-back (use for historical backfill)."
    )]
    from_epoch: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ValidatorsSandwichesSnapshot {
    pub version: u16,
    pub from_epoch: Epoch,
    pub loaded_at_epoch: Epoch,
    pub loaded_at_slot_index: u64,
    pub created_at: String,
    pub sandwiches: Vec<ValidatorSandwich>,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct ValidatorSandwich {
    pub epoch: Epoch,
    pub vote_account: String,
    /// Blocks over the 30-day window, not over the epoch. Both rates are percentages, one decimal.
    pub blocks_produced: u64,
    pub blocks_with_sandwiches: u64,
    pub sandwich_rate_30d: f64,
    /// Absent before epoch 820: upstream published only the 30d rate then.
    pub sandwich_rate_60d: Option<f64>,
}

/// One row of an epoch CSV. The aliases are the older column names that some sandwiched.me sheets
/// use, for example epochs 807-839.
#[derive(Debug, Deserialize)]
struct SandwichCsvRow {
    vote_account: String,
    #[serde(default, rename = "30d_blocks_produced", alias = "blocks_produced")]
    blocks_produced: Option<u64>,
    #[serde(
        default,
        rename = "30d_blocks_with_sandwiches",
        alias = "blocks_with_sandwiches"
    )]
    blocks_with_sandwiches: Option<u64>,
    #[serde(default, rename = "30d_sandwich_rate", alias = "sandwich_rate")]
    sandwich_rate_30d: Option<f64>,
    #[serde(default, rename = "60d_sandwich_rate", alias = "sandwich_rate_60d")]
    sandwich_rate_60d: Option<f64>,
}

const CSV_COLUMNS: [Column; 1] = [required("vote_account")];

/// The CSVs carry the sandwiched.me sheets own 3-line preamble, and `csv::parse` reads a header off
/// the first line it is given.
fn csv_body(text: &str) -> &str {
    text.splitn(3, '\n').nth(2).unwrap_or("")
}

pub fn parse_epoch_csv(epoch: Epoch, text: &str) -> anyhow::Result<Vec<ValidatorSandwich>> {
    let label = format!("epoch {epoch}");
    let rows: Vec<SandwichCsvRow> = csv::parse(csv_body(text), &CSV_COLUMNS, &label)?;

    rows.into_iter()
        .map(|row| {
            let missing = |column: &str| format!("{label}: {} has no {column}", row.vote_account);
            let rate_30d = row
                .sandwich_rate_30d
                .with_context(|| missing("30d sandwich rate"))?;
            let blocks_produced = row
                .blocks_produced
                .with_context(|| missing("30d blocks produced"))?;
            let blocks_with_sandwiches = row
                .blocks_with_sandwiches
                .with_context(|| missing("30d blocks with sandwiches"))?;
            Ok(ValidatorSandwich {
                epoch,
                blocks_produced,
                blocks_with_sandwiches,
                sandwich_rate_30d: rate_30d,
                sandwich_rate_60d: row.sandwich_rate_60d,
                vote_account: row.vote_account,
            })
        })
        .collect()
}

#[derive(Debug, Deserialize)]
struct EpochsResponse {
    epochs: Vec<Epoch>,
}

fn http_client() -> anyhow::Result<reqwest::blocking::Client> {
    Ok(reqwest::blocking::Client::builder()
        .user_agent("marinade-delegation-strategy")
        .timeout(Duration::from_secs(HTTP_TIMEOUT_S))
        .build()?)
}

fn fetch_epochs(client: &reqwest::blocking::Client, api_url: &str) -> anyhow::Result<Vec<Epoch>> {
    let url = format!("{}/epochs", api_url.trim_end_matches('/'));
    let response = client
        .get(&url)
        .send()
        .with_context(|| format!("requesting {url}"))?
        .error_for_status()
        .with_context(|| format!("requesting {url}"))?;
    Ok(response.json::<EpochsResponse>()?.epochs)
}

fn fetch_epoch_csv(
    client: &reqwest::blocking::Client,
    api_url: &str,
    epoch: Epoch,
) -> anyhow::Result<String> {
    let url = format!("{}/data/{epoch}.csv", api_url.trim_end_matches('/'));
    Ok(client
        .get(&url)
        .send()
        .with_context(|| format!("requesting {url}"))?
        .error_for_status()
        .with_context(|| format!("requesting {url}"))?
        .text()?)
}

pub fn collect_validator_sandwiches_info(
    common_params: CommonParams,
    sandwiches_params: SandwichesParams,
) -> anyhow::Result<()> {
    info!("Collecting validator sandwich rates snapshot");
    let timeout = Duration::from_secs(sandwiches_params.rpc_timeout);
    let rpc_client =
        solana_client_with_timeout(common_params.rpc_url, timeout, common_params.commitment);

    let created_at = chrono::Utc::now();
    let current_epoch_info = rpc_client.get_epoch_info()?;
    let from_epoch = sandwiches_params.from_epoch.unwrap_or_else(|| {
        current_epoch_info
            .epoch
            .saturating_sub(sandwiches_params.epochs_back)
    });
    info!("Reading sandwich rates from epoch: {from_epoch}");

    let client = http_client()?;
    let mut epochs: Vec<Epoch> = fetch_epochs(&client, &sandwiches_params.api_url)?
        .into_iter()
        .filter(|epoch| *epoch >= from_epoch)
        .collect();
    epochs.sort_unstable();
    info!("{} epochs published at or after {from_epoch}", epochs.len());

    let mut sandwiches = Vec::new();
    for epoch in epochs {
        let csv = fetch_epoch_csv(&client, &sandwiches_params.api_url, epoch)?;
        let rows = parse_epoch_csv(epoch, &csv)?;
        // The catalogue names the epoch, so an empty one is a broken publish, not a quiet gap.
        if rows.is_empty() {
            warn!("epoch {epoch} carries no rows");
        }
        info!("epoch {epoch}: {} validators", rows.len());
        sandwiches.extend(rows);
    }

    info!("Retrieved {} validator sandwich records", sandwiches.len());

    serde_yaml::to_writer(
        std::io::stdout(),
        &ValidatorsSandwichesSnapshot {
            version: DATA_VERSION,
            from_epoch,
            loaded_at_epoch: current_epoch_info.epoch,
            loaded_at_slot_index: current_epoch_info.slot_index,
            created_at: created_at.to_string(),
            sandwiches,
        },
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PREAMBLE: &str = "Snapshot of mev-hub validators API for Epoch 1030,,,,\n,,,,\n";

    #[test]
    fn the_three_line_preamble_is_dropped_before_the_header() {
        let csv = format!(
            "{PREAMBLE}vote_account,30d_blocks_produced,30d_blocks_with_sandwiches,30d_sandwich_rate\nvote1,2000,90,4.5\n"
        );
        assert_eq!(
            parse_epoch_csv(1030, &csv).unwrap(),
            vec![ValidatorSandwich {
                epoch: 1030,
                vote_account: "vote1".into(),
                blocks_produced: 2000,
                blocks_with_sandwiches: 90,
                sandwich_rate_30d: 4.5,
                sandwich_rate_60d: None,
            }]
        );
    }

    #[test]
    fn the_modern_shape_fills_every_field() {
        let csv = format!(
            "{PREAMBLE}validator_name,vote_account,30d_blocks_produced,30d_blocks_with_sandwiches,30d_sandwich_rate,60d_sandwich_rate\n\
             -,vote1,9520,4056,42.6,29.7\n"
        );
        assert_eq!(
            parse_epoch_csv(886, &csv).unwrap(),
            vec![ValidatorSandwich {
                epoch: 886,
                vote_account: "vote1".into(),
                blocks_produced: 9520,
                blocks_with_sandwiches: 4056,
                sandwich_rate_30d: 42.6,
                sandwich_rate_60d: Some(29.7),
            }]
        );
    }

    /// Epochs before ~820 name the column `sandwich_rate` and publish no 60d rate at all.
    #[test]
    fn the_pre_820_column_name_is_read_as_the_30d_rate() {
        let csv = format!(
            "{PREAMBLE}vote_account,30d_blocks_produced,30d_blocks_with_sandwiches,sandwich_rate\nvote1,2356,1511,64.1\n"
        );
        let rows = parse_epoch_csv(791, &csv).unwrap();
        assert_eq!(rows[0].sandwich_rate_30d, 64.1);
        assert_eq!(rows[0].sandwich_rate_60d, None);
    }

    // The header of epochs 812, 814, 816, 819, 826, 827 and 839.
    #[test]
    fn the_unprefixed_column_names_are_read() {
        let csv = format!(
            "{PREAMBLE}vote_account,blocks_produced,blocks_with_sandwiches,sandwich_rate,sandwich_rate_60d\nvote1,4672,2990,64,52\n"
        );
        assert_eq!(
            parse_epoch_csv(812, &csv).unwrap(),
            vec![ValidatorSandwich {
                epoch: 812,
                vote_account: "vote1".into(),
                blocks_produced: 4672,
                blocks_with_sandwiches: 2990,
                sandwich_rate_30d: 64.0,
                sandwich_rate_60d: Some(52.0),
            }]
        );
    }

    #[test]
    fn a_row_with_no_block_count_is_an_error() {
        let csv = format!("{PREAMBLE}vote_account,30d_sandwich_rate\nvote1,4.5\n");
        let error = parse_epoch_csv(1030, &csv).unwrap_err().to_string();
        assert!(error.contains("no 30d blocks produced"), "{error}");
    }

    #[test]
    fn a_row_with_no_rate_at_all_is_an_error() {
        let csv = format!("{PREAMBLE}vote_account,30d_blocks_produced\nvote1,2356\n");
        let error = parse_epoch_csv(1030, &csv).unwrap_err().to_string();
        assert!(error.contains("no 30d sandwich rate"), "{error}");
    }

    #[test]
    fn a_file_with_no_vote_account_column_is_an_error() {
        let csv = format!("{PREAMBLE}validator_name,30d_sandwich_rate\n-,4.5\n");
        let error = parse_epoch_csv(1030, &csv).unwrap_err().to_string();
        assert!(error.contains("vote_account"), "{error}");
    }
}
