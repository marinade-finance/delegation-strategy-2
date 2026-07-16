use crate::{common::*, solana_service::solana_client_with_timeout};
use anyhow::Context;
use google_cloud_bigquery::client::{Client as BqClient, ClientConfig as BqClientConfig};
use google_cloud_bigquery::http::job::query::QueryRequest;
use google_cloud_bigquery::query::row::Row;
use log::{info, warn};
use serde::{Deserialize, Serialize};
use serde_yaml;
use solana_sdk::clock::Epoch;
use std::time::Duration;
use structopt::StructOpt;

const GOOGLE_BQ_PROJECT_ID: &str = "data-store-406413";
const GOOGLE_BQ_DATASET: &str = "mainnet_beta_stakes";
pub const STAKES_TABLE: &str = "stakes";

#[derive(Debug, StructOpt)]
pub struct StakersParams {
    #[structopt(
        env = "gcp-table",
        help = "BigQuery table name to search in.",
        default_value = STAKES_TABLE
    )]
    gcp_table_name: String,

    #[structopt(
        long = "rpc-timeout",
        help = "How long to wait for RPC response (seconds).",
        default_value = "300"
    )]
    rpc_timeout: u64,

    #[structopt(long = "epoch", help = "Act as if current epoch was set to this value.")]
    epoch: Option<u64>,

    #[structopt(
        long = "loading-limit",
        env = "LOADING_LIMIT",
        help = "When loading validators' stakers data, we expect a higher number from the ETL to consider the data valid.",
        default_value = "10"
    )]
    loading_limit: u32,

    #[structopt(
        long = "max-data-delay-hours",
        env = "MAX_DATA_DELAY_HOURS",
        help = "Fail instead of skipping when stakers data is still missing this many hours after the target epoch ended (approximate; assumes ~400ms slots). 0 disables the check. Ignored when --epoch is set (backfill).",
        default_value = "12"
    )]
    max_data_delay_hours: u64,
}

const DATA_VERSION: u16 = 1;
const MILLISECONDS_PER_HOUR: u64 = 3_600_000;

#[derive(Debug, Serialize, Deserialize)]
pub struct ValidatorsStakersSnapshot {
    pub version: u16,
    pub epoch: Epoch,
    pub loaded_at_epoch: Epoch,
    pub loaded_at_slot_index: u64,
    pub created_at: String,
    pub stakers: Vec<ValidatorStakers>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ValidatorStakers {
    pub vote_account: String,
    pub unique_stakers: u64,
    pub active_stake: u64,
}

async fn create_bigquery_client() -> anyhow::Result<BqClient> {
    let (config, _) = BqClientConfig::new_with_auth().await?;
    Ok(BqClient::new(config).await?)
}

async fn query_validator_stakers(
    bq_client: &BqClient,
    table_name: &str,
    epoch: Epoch,
) -> anyhow::Result<Vec<ValidatorStakers>> {
    let project_table_name = format!("{GOOGLE_BQ_PROJECT_ID}.{GOOGLE_BQ_DATASET}.{table_name}");
    info!("Querying BigQuery for epoch {epoch} from project table {project_table_name}");

    let query = format!(
        "SELECT vote_account, \
                COUNT(DISTINCT withdraw_authority) AS unique_stakers, \
                CAST(SUM(active) AS INT64) AS active_stake \
         FROM `{project_table_name}` \
         WHERE active > 0 AND vote_account IS NOT NULL AND epoch = {epoch} \
         GROUP BY vote_account"
    );

    info!("Executing query: {query}");

    let request = QueryRequest {
        query,
        use_legacy_sql: false,
        ..Default::default()
    };

    let mut iter = bq_client
        .query::<Row>(GOOGLE_BQ_PROJECT_ID, request)
        .await
        .context("Failed to execute BigQuery query")?;

    let mut results = Vec::new();
    let mut row_count = 0;

    while let Some(row) = iter.next().await? {
        row_count += 1;

        let vote_account = row
            .column::<String>(0)
            .context("Failed to parse vote_account")?;
        let unique_stakers_str = row
            .column::<String>(1)
            .context("Failed to parse unique_stakers")?;
        let active_stake_str = row
            .column::<String>(2)
            .context("Failed to parse active_stake")?;
        let unique_stakers: u64 = unique_stakers_str
            .parse()
            .context(format!("Failed to parse unique_stakers '{unique_stakers_str}' as u64"))?;
        let active_stake: u64 = active_stake_str
            .parse()
            .context(format!("Failed to parse active_stake '{active_stake_str}' as u64"))?;

        results.push(ValidatorStakers {
            vote_account,
            unique_stakers,
            active_stake,
        });
    }

    info!("Retrieved {row_count} rows from BigQuery for epoch {epoch}");

    Ok(results)
}

pub fn collect_validator_stakers_info(
    common_params: CommonParams,
    stakers_params: StakersParams,
) -> anyhow::Result<()> {
    info!("Collecting validator stakers snapshot");
    let timeout = Duration::from_secs(stakers_params.rpc_timeout);
    let client =
        solana_client_with_timeout(common_params.rpc_url, timeout, common_params.commitment);

    let created_at = chrono::Utc::now();
    let current_epoch_info = client.get_epoch_info()?;
    info!("Current epoch: {current_epoch_info:?}");
    let looking_at_epoch = stakers_params.epoch.unwrap_or(current_epoch_info.epoch - 1);
    info!("Looking at epoch: {looking_at_epoch}");

    let runtime = tokio::runtime::Runtime::new().context("Failed to create tokio runtime")?;
    let stakers = runtime.block_on(async {
        let bq_client = create_bigquery_client().await?;
        query_validator_stakers(&bq_client, &stakers_params.gcp_table_name, looking_at_epoch).await
    })?;

    if stakers.len() <= stakers_params.loading_limit as usize {
        if stakers_params.epoch.is_some() {
            warn!(
                "Insufficient stakers data for backfill epoch {looking_at_epoch} (rows <= loading limit). May not be available yet. BigQuery returned {} rows (loading limit {}).",
                stakers.len(),
                stakers_params.loading_limit,
            );
        } else {
            let hours_since_epoch_end = current_epoch_info
                .slot_index
                .saturating_mul(MILLISECONDS_PER_SLOT)
                / MILLISECONDS_PER_HOUR;
            if stakers_params.max_data_delay_hours > 0
                && hours_since_epoch_end >= stakers_params.max_data_delay_hours
            {
                anyhow::bail!(
                    "Insufficient stakers data for epoch {looking_at_epoch} (rows <= loading limit): BigQuery returned {} rows (loading limit {}) ~{hours_since_epoch_end}h after the epoch ended (threshold {}h). Upstream stakes-etl load is overdue.",
                    stakers.len(),
                    stakers_params.loading_limit,
                    stakers_params.max_data_delay_hours,
                );
            }
            warn!(
                "Insufficient stakers data for epoch {looking_at_epoch} (rows <= loading limit, ~{hours_since_epoch_end}h after epoch end). May not be available yet; will retry next run. BigQuery returned {} rows (loading limit {}).",
                stakers.len(),
                stakers_params.loading_limit,
            );
        }
    } else {
        info!("Successfully retrieved {} validator stakers", stakers.len());

        serde_yaml::to_writer(
            std::io::stdout(),
            &ValidatorsStakersSnapshot {
                version: DATA_VERSION,
                epoch: looking_at_epoch,
                loaded_at_epoch: current_epoch_info.epoch,
                loaded_at_slot_index: current_epoch_info.slot_index,
                created_at: created_at.to_string(),
                stakers,
            },
        )?;
    }

    Ok(())
}
