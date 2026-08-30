use crate::{common::*, solana_service::solana_client_with_timeout};
use anyhow::Context;
use clap::Parser;
use google_cloud_bigquery::client::{Client as BqClient, ClientConfig as BqClientConfig};
use google_cloud_bigquery::http::job::query::QueryRequest;
use google_cloud_bigquery::query::row::Row;
use log::{debug, info};
use serde::{Deserialize, Serialize};
use serde_yaml;
use solana_sdk::clock::Epoch;
use std::time::Duration;

const GOOGLE_BQ_PROJECT_ID: &str = "data-store-406413";
const GOOGLE_BQ_DATASET: &str = "mainnet_beta_stakes";

#[derive(Debug, Parser)]
pub struct TakeRatesParams {
    #[arg(
        long = "rpc-timeout",
        help = "How long to wait for RPC response (seconds).",
        default_value = "300"
    )]
    rpc_timeout: u64,

    #[arg(
        long = "epochs-back",
        help = "How many epochs back from the current epoch to (re-)query. Reward data can arrive a little late, so a small window is re-queried each run; already-stored epochs are idempotently upserted.",
        default_value = "2"
    )]
    epochs_back: u64,

    #[arg(
        long = "from-epoch",
        help = "Query take rates from this epoch onwards. Overrides --epochs-back (use for historical backfill)."
    )]
    from_epoch: Option<u64>,
}

const DATA_VERSION: u16 = 1;

#[derive(Debug, Serialize, Deserialize)]
pub struct ValidatorRewardsSnapshot {
    pub version: u16,
    pub from_epoch: Epoch,
    pub loaded_at_epoch: Epoch,
    pub loaded_at_slot_index: u64,
    pub created_at: String,
    pub rewards: Vec<ValidatorEpochRewards>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ValidatorEpochRewards {
    pub epoch: Epoch,
    pub vote_account: String,
    // Lamports. What the validator kept: its inflation and MEV commissions plus the block rewards it
    // did not pass through to stakers via Jito's PriorityFeeDistribution.
    pub validator_rewards: u64,
    // Lamports. Counts both sides of every component and equals inflation + mev + block below. The
    // Jito passthrough is already inside `block_rewards`, so it is never added twice.
    pub total_rewards: u64,
    // Lamports, both sides. Summed cluster-wide these are the reward mix `expected_take_rate`
    // weights commissions by.
    pub inflation_rewards: u64,
    pub mev_rewards: u64,
    pub block_rewards: u64,
}

async fn create_bigquery_client() -> anyhow::Result<BqClient> {
    let (config, _) = BqClientConfig::new_with_auth().await?;
    Ok(BqClient::new(config).await?)
}

/// Per-(vote_account, epoch) realized reward split, computed from the BigQuery reward tables, for
/// epochs `>= from_epoch`. The single source of the take-rate math: `collect` persists these rows and
/// `store::utils::load_take_rates` collapses them into the windowed per-validator scalar and the
/// cluster reward mix. `from_epoch` is interpolated as a literal on every reward table because they
/// are epoch-partitioned and would otherwise be scanned whole.
pub async fn query_validator_rewards(
    bq_client: &BqClient,
    from_epoch: Epoch,
) -> anyhow::Result<Vec<ValidatorEpochRewards>> {
    let ds = format!("{GOOGLE_BQ_PROJECT_ID}.{GOOGLE_BQ_DATASET}");
    info!("Querying BigQuery for take rates from epoch {from_epoch} in dataset {ds}");

    let query = format!(
        "SELECT vote_account, epoch, \
                CAST(validator_rewards AS STRING) AS validator_rewards, \
                CAST(total_rewards AS STRING) AS total_rewards, \
                CAST(inflation_rewards AS STRING) AS inflation_rewards, \
                CAST(mev_rewards AS STRING) AS mev_rewards, \
                CAST(block_rewards AS STRING) AS block_rewards FROM (
            WITH stakers AS (
                SELECT
                    stakes.vote_account AS vote_account,
                    stakes.epoch AS epoch,
                    SUM(COALESCE(inflation.amount, 0)) AS staker_inflation,
                    SUM(COALESCE(mev.amount, 0)) AS staker_mev,
                    SUM(COALESCE(prio.amount, 0)) AS staker_blocks
                FROM `{ds}.stakes` stakes
                LEFT JOIN `{ds}.rewards_inflation` inflation
                    ON stakes.stake_account = inflation.stake_account
                    AND stakes.epoch = inflation.epoch AND inflation.epoch >= {from_epoch}
                LEFT JOIN `{ds}.rewards_mev` mev
                    ON stakes.stake_account = mev.stake_account
                    AND stakes.epoch = mev.epoch AND mev.epoch >= {from_epoch}
                -- rewards_validators_blocks is gross, so what Jito's PriorityFeeDistribution passed through has to come off the validator's keep rather than add to the pot.
                LEFT JOIN `{ds}.rewards_jito_priority_fee` prio
                    ON stakes.stake_account = prio.stake_account
                    AND stakes.epoch = prio.epoch AND prio.epoch >= {from_epoch}
                WHERE stakes.vote_account IS NOT NULL AND stakes.epoch >= {from_epoch}
                GROUP BY stakes.vote_account, stakes.epoch
            )
            SELECT
                stakers.vote_account AS vote_account,
                stakers.epoch AS epoch,
                -- GREATEST guards the epochs where the two tables attribute one distribution to different sides of a boundary.
                CAST(SUM(COALESCE(vi.amount, 0) + COALESCE(vm.amount, 0)
                    + GREATEST(COALESCE(vb.amount, 0) - staker_blocks, 0)) AS INT64) AS validator_rewards,
                CAST(SUM(staker_inflation + COALESCE(vi.amount, 0)) AS INT64) AS inflation_rewards,
                CAST(SUM(staker_mev + COALESCE(vm.amount, 0)) AS INT64) AS mev_rewards,
                CAST(SUM(COALESCE(vb.amount, 0)) AS INT64) AS block_rewards,
                CAST(SUM(staker_inflation + COALESCE(vi.amount, 0)
                    + staker_mev + COALESCE(vm.amount, 0)
                    + COALESCE(vb.amount, 0)) AS INT64) AS total_rewards
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
        -- validator_rewards <= total_rewards by construction, so this drops only epochs that paid nothing.
        WHERE total_rewards > 0
        ORDER BY epoch DESC"
    );

    debug!("Executing query: {query}");

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
        // BigQuery hands back every scalar as text.
        let u64_column = |index: usize, name: &str| -> anyhow::Result<u64> {
            let raw = row
                .column::<String>(index)
                .context(format!("Failed to read {name}"))?;
            raw.parse()
                .context(format!("Failed to parse {name} '{raw}' as u64"))
        };

        results.push(ValidatorEpochRewards {
            epoch: u64_column(1, "epoch")?,
            vote_account,
            validator_rewards: u64_column(2, "validator_rewards")?,
            total_rewards: u64_column(3, "total_rewards")?,
            inflation_rewards: u64_column(4, "inflation_rewards")?,
            mev_rewards: u64_column(5, "mev_rewards")?,
            block_rewards: u64_column(6, "block_rewards")?,
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
    let rewards = runtime.block_on(async {
        let bq_client = create_bigquery_client().await?;
        query_validator_rewards(&bq_client, from_epoch).await
    })?;

    info!("Retrieved {} validator reward records", rewards.len());

    serde_yaml::to_writer(
        std::io::stdout(),
        &ValidatorRewardsSnapshot {
            version: DATA_VERSION,
            from_epoch,
            loaded_at_epoch: current_epoch_info.epoch,
            loaded_at_slot_index: current_epoch_info.slot_index,
            created_at: created_at.to_rfc3339(),
            rewards,
        },
    )?;

    Ok(())
}
