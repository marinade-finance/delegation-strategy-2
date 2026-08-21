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

#[derive(Debug, StructOpt)]
pub struct TakeRatesParams {
    #[structopt(
        long = "rpc-timeout",
        help = "How long to wait for RPC response (seconds).",
        default_value = "300"
    )]
    rpc_timeout: u64,

    #[structopt(
        long = "epochs-back",
        help = "How many epochs back from the current epoch to (re-)query. Reward data can arrive a little late, so a small window is re-queried each run; already-stored epochs are idempotently upserted.",
        default_value = "2"
    )]
    epochs_back: u64,

    #[structopt(
        long = "from-epoch",
        help = "Query take rates from this epoch onwards. Overrides --epochs-back (use for historical backfill)."
    )]
    from_epoch: Option<u64>,
}

const DATA_VERSION: u16 = 1;

#[derive(Debug, Serialize, Deserialize)]
pub struct TakeRatesSnapshot {
    pub version: u16,
    pub from_epoch: Epoch,
    pub loaded_at_epoch: Epoch,
    pub loaded_at_slot_index: u64,
    pub created_at: String,
    pub take_rates: Vec<ValidatorTakeRate>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ValidatorTakeRate {
    pub epoch: Epoch,
    pub vote_account: String,
    // Realized reward split, in lamports, per (vote_account, epoch):
    // validator_rewards = validator inflation + MEV + block commission
    // total_rewards     = staker rewards + validator_rewards
    pub validator_rewards: u64,
    pub total_rewards: u64,
}

async fn create_bigquery_client() -> anyhow::Result<BqClient> {
    let (config, _) = BqClientConfig::new_with_auth().await?;
    Ok(BqClient::new(config).await?)
}

/// Per-(vote_account, epoch) realized reward split, computed from BigQuery reward tables. Same joins
/// as the legacy scalar `load_take_rates`, but returns the numerator and denominator separately and
/// grouped per epoch (not collapsed), so the API can serve a timeseries and derive the windowed
/// scalar as SUM(num)/SUM(denom). `from_epoch` is a resolved literal so the epoch-partitioned reward
/// tables are pruned to just the requested window.
async fn query_take_rates(
    bq_client: &BqClient,
    from_epoch: Epoch,
) -> anyhow::Result<Vec<ValidatorTakeRate>> {
    let ds = format!("{GOOGLE_BQ_PROJECT_ID}.{GOOGLE_BQ_DATASET}");
    info!("Querying BigQuery for take rates from epoch {from_epoch} in dataset {ds}");

    let query = format!(
        "SELECT vote_account, epoch, CAST(validator_rewards AS STRING) AS validator_rewards, \
                CAST(total_rewards AS STRING) AS total_rewards FROM (
            WITH stakers AS (
                SELECT
                    stakes.vote_account AS vote_account,
                    stakes.epoch AS epoch,
                    SUM(COALESCE(inflation.amount, 0)) AS staker_inflation,
                    SUM(COALESCE(mev.amount, 0)) AS staker_mev
                FROM `{ds}.stakes` stakes
                LEFT JOIN `{ds}.rewards_inflation` inflation
                    ON stakes.stake_account = inflation.stake_account
                    AND stakes.epoch = inflation.epoch AND inflation.epoch >= {from_epoch}
                LEFT JOIN `{ds}.rewards_mev` mev
                    ON stakes.stake_account = mev.stake_account
                    AND stakes.epoch = mev.epoch AND mev.epoch >= {from_epoch}
                WHERE stakes.vote_account IS NOT NULL AND stakes.epoch >= {from_epoch}
                GROUP BY stakes.vote_account, stakes.epoch
            )
            SELECT
                stakers.vote_account AS vote_account,
                stakers.epoch AS epoch,
                SUM(COALESCE(vi.amount, 0) + COALESCE(vm.amount, 0) + COALESCE(vb.amount, 0))
                    AS validator_rewards,
                SUM(staker_inflation + staker_mev
                    + COALESCE(vi.amount, 0) + COALESCE(vm.amount, 0) + COALESCE(vb.amount, 0))
                    AS total_rewards
            FROM stakers
            -- Pre-aggregate to one row per (vote_account, epoch) so raw duplicate keys can't fan out the SUM().
            LEFT JOIN (
                SELECT vote_account, epoch, SUM(amount) AS amount
                FROM `{ds}.rewards_validators_inflation`
                WHERE epoch >= {from_epoch}
                GROUP BY vote_account, epoch
            ) vi
                ON stakers.vote_account = vi.vote_account AND stakers.epoch = vi.epoch
            LEFT JOIN (
                SELECT vote_account, epoch, SUM(amount) AS amount
                FROM `{ds}.rewards_validators_mev`
                WHERE epoch >= {from_epoch}
                GROUP BY vote_account, epoch
            ) vm
                ON stakers.vote_account = vm.vote_account AND stakers.epoch = vm.epoch
            LEFT JOIN (
                SELECT vote_account, epoch, SUM(amount) AS amount
                FROM `{ds}.rewards_validators_blocks`
                WHERE epoch >= {from_epoch}
                GROUP BY vote_account, epoch
            ) vb
                ON stakers.vote_account = vb.vote_account AND stakers.epoch = vb.epoch
            GROUP BY stakers.vote_account, stakers.epoch
        )
        WHERE total_rewards > 0
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

        let vote_account = row
            .column::<String>(0)
            .context("Failed to parse vote_account")?;
        let epoch_str = row.column::<String>(1).context("Failed to parse epoch")?;
        let validator_rewards_str = row
            .column::<String>(2)
            .context("Failed to parse validator_rewards")?;
        let total_rewards_str = row
            .column::<String>(3)
            .context("Failed to parse total_rewards")?;
        let epoch: Epoch = epoch_str
            .parse()
            .context(format!("Failed to parse epoch '{epoch_str}' as u64"))?;
        let validator_rewards: u64 = validator_rewards_str.parse().context(format!(
            "Failed to parse validator_rewards '{validator_rewards_str}' as u64"
        ))?;
        let total_rewards: u64 = total_rewards_str.parse().context(format!(
            "Failed to parse total_rewards '{total_rewards_str}' as u64"
        ))?;

        results.push(ValidatorTakeRate {
            epoch,
            vote_account,
            validator_rewards,
            total_rewards,
        });
    }

    info!("Retrieved {row_count} take rate rows from BigQuery from epoch {from_epoch}");

    Ok(results)
}

pub fn collect_take_rates_info(
    common_params: CommonParams,
    take_rates_params: TakeRatesParams,
) -> anyhow::Result<()> {
    info!("Collecting validator take rates snapshot");
    let timeout = Duration::from_secs(take_rates_params.rpc_timeout);
    let client =
        solana_client_with_timeout(common_params.rpc_url, timeout, common_params.commitment);

    let created_at = chrono::Utc::now();
    let current_epoch_info = client.get_epoch_info()?;
    info!("Current epoch: {current_epoch_info:?}");
    let from_epoch = take_rates_params.from_epoch.unwrap_or_else(|| {
        current_epoch_info
            .epoch
            .saturating_sub(take_rates_params.epochs_back)
    });
    info!("Querying take rates from epoch: {from_epoch}");

    let runtime = tokio::runtime::Runtime::new().context("Failed to create tokio runtime")?;
    let take_rates = runtime.block_on(async {
        let bq_client = create_bigquery_client().await?;
        query_take_rates(&bq_client, from_epoch).await
    })?;

    info!("Retrieved {} validator take rate records", take_rates.len());

    serde_yaml::to_writer(
        std::io::stdout(),
        &TakeRatesSnapshot {
            version: DATA_VERSION,
            from_epoch,
            loaded_at_epoch: current_epoch_info.epoch,
            loaded_at_slot_index: current_epoch_info.slot_index,
            created_at: created_at.to_rfc3339(),
            take_rates,
        },
    )?;

    Ok(())
}
