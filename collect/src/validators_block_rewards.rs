use crate::{common::*, solana_service::solana_client_with_timeout};
use anyhow::Context;
use google_cloud_bigquery::client::{Client as BqClient, ClientConfig as BqClientConfig};
use google_cloud_bigquery::http::job::query::QueryRequest;
use google_cloud_bigquery::query::row::Row;
use log::{error, info};
use serde::{Deserialize, Serialize};
use serde_yaml;
use solana_sdk::clock::Epoch;
use std::time::Duration;
use structopt::StructOpt;

// BigQuery table name to store block rewards data
const GOOGLE_BQ_PROJECT_ID: &str = "data-store-406413";
const GOOGLE_BQ_DATASET: &str = "mainnet_beta_stakes";
pub const BLOCK_REWARDS_TABLE: &str = "rewards_validators_blocks";

#[derive(Debug, StructOpt)]
pub struct BlockRewardsParams {
    #[structopt(
        env = "gcp-table",
        help = "BigQuery table name to search in.",
        default_value = BLOCK_REWARDS_TABLE
    )]
    gcp_table_name: String,

    #[structopt(
        long = "rpc-timeout",
        help = "How long to wait for RPC response (seconds).",
        default_value = "300"
    )]
    rpc_timeout: u64,

    #[structopt(
        long = "epoch",
        help = "Act as if current epoch was set to this value."
    )]
    epoch: Option<u64>,
}

const DATA_VERSION: u16 = 1;

#[derive(Debug, Serialize, Deserialize)]
pub struct ValidatorsBlockRewardsSnapshot {
    pub version: u16,
    pub epoch: Epoch,
    pub loaded_at_epoch: Epoch,
    pub loaded_at_slot_index: u64,
    pub created_at: String,
    pub block_rewards: Vec<ValidatorBlockRewards>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ValidatorBlockRewards {
    pub identity_account: String,
    pub vote_account: String,
    pub node_account: String,
    pub authorized_voter: String,
    pub amount: u64,
}

async fn create_bigquery_client() -> anyhow::Result<BqClient> {
    let (config, _) = BqClientConfig::new_with_auth().await?;
    Ok(BqClient::new(config).await?)
}

async fn query_validator_block_rewards(
    bq_client: &BqClient,
    table_name: &str,
    epoch: Epoch,
) -> anyhow::Result<Vec<ValidatorBlockRewards>> {
    let project_table_name = format!("{GOOGLE_BQ_PROJECT_ID}.{GOOGLE_BQ_DATASET}.{table_name}");
    info!("Querying BigQuery for epoch {epoch} from project table {project_table_name}");

    let query = format!(
        "SELECT epoch, identity_account, vote_account, amount, node_pubkey, authorized_voter \
         FROM `{project_table_name}` \
         WHERE epoch = {epoch}"
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

        let identity_account = row
            .column::<String>(1)
            .context("Failed to parse identity_account")?;
        let vote_account = row
            .column::<String>(2)
            .context("Failed to parse vote_account")?;
        let amount_str = row.column::<String>(3).context("Failed to parse amount")?;
        let node_pubkey = row
            .column::<String>(4)
            .context("Failed to parse node_pubkey")?;
        let authorized_voter = row
            .column::<String>(5)
            .context("Failed to parse authorized_voter")?;
        let amount: u64 = amount_str
            .parse()
            .context(format!("Failed to parse amount '{amount_str}' as u64"))?;

        results.push(ValidatorBlockRewards {
            identity_account: identity_account.clone(),
            vote_account: vote_account.clone(),
            node_account: node_pubkey.clone(),
            authorized_voter: authorized_voter.clone(),
            amount,
        });
    }

    info!("Retrieved {row_count} rows from BigQuery for epoch {epoch}");

    if row_count == 0 {
        // No data found for the epoch - data are not available yet
        error!("No data found for epoch {epoch}. This epoch may not have data yet.");
        anyhow::bail!("No data found for epoch {epoch}.");
    }

    Ok(results)
}

pub fn collect_validator_block_rewards_info(
    common_params: CommonParams,
    rewards_params: BlockRewardsParams,
) -> anyhow::Result<()> {
    info!("Collecting validator block rewards snapshot");
    let timeout = Duration::from_secs(rewards_params.rpc_timeout);
    let client =
        solana_client_with_timeout(common_params.rpc_url, timeout, common_params.commitment);

    let created_at = chrono::Utc::now();
    let current_epoch_info = client.get_epoch_info()?;
    info!("Current epoch: {current_epoch_info:?}");
    let looking_at_epoch = rewards_params.epoch.unwrap_or(current_epoch_info.epoch - 1);
    info!("Looking at epoch: {looking_at_epoch}");

    let runtime = tokio::runtime::Runtime::new().context("Failed to create tokio runtime")?;
    let block_rewards = runtime.block_on(async {
        let bq_client = create_bigquery_client().await?;
        query_validator_block_rewards(&bq_client, &rewards_params.gcp_table_name, looking_at_epoch)
            .await
    })?;

    info!(
        "Successfully retrieved {} validator block rewards",
        block_rewards.len()
    );

    serde_yaml::to_writer(
        std::io::stdout(),
        &ValidatorsBlockRewardsSnapshot {
            version: DATA_VERSION,
            epoch: looking_at_epoch,
            loaded_at_epoch: current_epoch_info.epoch,
            loaded_at_slot_index: current_epoch_info.slot_index,
            created_at: created_at.to_string(),
            block_rewards,
        },
    )?;

    Ok(())
}
