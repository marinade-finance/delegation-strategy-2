use crate::{common::*, solana_service::solana_client_with_timeout};
use anyhow::Context;
use google_cloud_bigquery::client::{Client as BqClient, ClientConfig as BqClientConfig};
use google_cloud_bigquery::http::job::query::QueryRequest;
use google_cloud_bigquery::query::row::Row;
use log::info;
use serde::{Deserialize, Serialize};
use serde_yaml;
use solana_sdk::clock::Epoch;
use std::time::Duration;
use structopt::StructOpt;

const GOOGLE_BQ_PROJECT_ID: &str = "data-store-406413";
const GOOGLE_BQ_DATASET: &str = "mainnet_beta_stakes";
pub const PSR_SETTLEMENTS_TABLE: &str = "psr_settlements";

#[derive(Debug, StructOpt)]
pub struct EventsParams {
    #[structopt(
        env = "gcp-table",
        help = "BigQuery table name to search in.",
        default_value = PSR_SETTLEMENTS_TABLE
    )]
    gcp_table_name: String,

    #[structopt(
        long = "rpc-timeout",
        help = "How long to wait for RPC response (seconds).",
        default_value = "300"
    )]
    rpc_timeout: u64,

    #[structopt(
        long = "epochs-back",
        help = "How many epochs back from the current epoch to (re-)query. Settlements can arrive several epochs late, so a window is re-queried each run to backfill them.",
        default_value = "10"
    )]
    epochs_back: u64,

    #[structopt(
        long = "from-epoch",
        help = "Query settlements from this epoch onwards. Overrides --epochs-back (use for historical backfill)."
    )]
    from_epoch: Option<u64>,
}

const DATA_VERSION: u16 = 1;

#[derive(Debug, Serialize, Deserialize)]
pub struct ValidatorsEventsSnapshot {
    pub version: u16,
    pub from_epoch: Epoch,
    pub loaded_at_epoch: Epoch,
    pub loaded_at_slot_index: u64,
    pub created_at: String,
    pub events: Vec<ValidatorSettlement>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ValidatorSettlement {
    pub epoch: Epoch,
    pub vote_account: String,
    pub reason: String,
    pub meta: String,
    pub amount: i64,
}

async fn create_bigquery_client() -> anyhow::Result<BqClient> {
    let (config, _) = BqClientConfig::new_with_auth().await?;
    Ok(BqClient::new(config).await?)
}

async fn query_validator_settlements(
    bq_client: &BqClient,
    table_name: &str,
    from_epoch: Epoch,
) -> anyhow::Result<Vec<ValidatorSettlement>> {
    let project_table_name = format!("{GOOGLE_BQ_PROJECT_ID}.{GOOGLE_BQ_DATASET}.{table_name}");
    info!("Querying BigQuery for settlements from epoch {from_epoch} from project table {project_table_name}");

    let query = format!(
        "SELECT epoch, vote_account, reason, meta, CAST(SUM(amount) AS INT64) AS amount \
         FROM `{project_table_name}` \
         WHERE epoch >= {from_epoch} AND vote_account IS NOT NULL \
         GROUP BY epoch, vote_account, reason, meta \
         ORDER BY epoch DESC"
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

        let epoch_str = row.column::<String>(0).context("Failed to parse epoch")?;
        let vote_account = row
            .column::<String>(1)
            .context("Failed to parse vote_account")?;
        let reason = row.column::<String>(2).context("Failed to parse reason")?;
        let meta = row.column::<String>(3).context("Failed to parse meta")?;
        let amount_str = row.column::<String>(4).context("Failed to parse amount")?;
        let epoch: Epoch = epoch_str
            .parse()
            .context(format!("Failed to parse epoch '{epoch_str}' as u64"))?;
        let amount: i64 = amount_str
            .parse()
            .context(format!("Failed to parse amount '{amount_str}' as i64"))?;

        results.push(ValidatorSettlement {
            epoch,
            vote_account,
            reason,
            meta,
            amount,
        });
    }

    info!("Retrieved {row_count} settlement rows from BigQuery from epoch {from_epoch}");

    Ok(results)
}

pub fn collect_validator_events_info(
    common_params: CommonParams,
    events_params: EventsParams,
) -> anyhow::Result<()> {
    info!("Collecting validator events (PSR settlements) snapshot");
    let timeout = Duration::from_secs(events_params.rpc_timeout);
    let client =
        solana_client_with_timeout(common_params.rpc_url, timeout, common_params.commitment);

    let created_at = chrono::Utc::now();
    let current_epoch_info = client.get_epoch_info()?;
    info!("Current epoch: {current_epoch_info:?}");
    let from_epoch = events_params.from_epoch.unwrap_or_else(|| {
        current_epoch_info
            .epoch
            .saturating_sub(events_params.epochs_back)
    });
    info!("Querying settlements from epoch: {from_epoch}");

    let runtime = tokio::runtime::Runtime::new().context("Failed to create tokio runtime")?;
    let events = runtime.block_on(async {
        let bq_client = create_bigquery_client().await?;
        query_validator_settlements(&bq_client, &events_params.gcp_table_name, from_epoch).await
    })?;

    // Settlements are legitimately sparse (most validators have none in a given epoch),
    // so we always emit the snapshot even when the set is small or empty.
    info!("Retrieved {} validator settlement records", events.len());

    serde_yaml::to_writer(
        std::io::stdout(),
        &ValidatorsEventsSnapshot {
            version: DATA_VERSION,
            from_epoch,
            loaded_at_epoch: current_epoch_info.epoch,
            loaded_at_slot_index: current_epoch_info.slot_index,
            created_at: created_at.to_string(),
            events,
        },
    )?;

    Ok(())
}
