use crate::context::WrappedContext;
use log::{error, info};
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};
use store::dto::{
    ClusterStats, CommissionRecord, ScoringRunRecord, UptimeRecord, ValidatorRecord,
    ValidatorScoreRecord, ValidatorsAggregated, VersionRecord,
};
use tokio::time::{sleep, Duration, Instant};

pub(crate) const DEFAULT_EPOCHS: u64 = 80;
pub(crate) const DEFAULT_COMPUTING_EPOCHS: u64 = 20;
const CACHE_WARMUP_TIME_S: u64 = 10 * 60;

type CachedValidators = HashMap<String, ValidatorRecord>;
type CachedCommissions = HashMap<String, Vec<CommissionRecord>>;
type CachedVersions = HashMap<String, Vec<VersionRecord>>;
type CachedUptimes = HashMap<String, Vec<UptimeRecord>>;
type CachedClusterStats = Option<ClusterStats>;
type CachedValidatorsAggregated = Vec<ValidatorsAggregated>;

#[derive(Default, Clone)]
pub struct CachedSingleRunScores {
    pub scoring_run: Option<ScoringRunRecord>,
    pub scores: HashMap<String, ValidatorScoreRecord>,
}

#[derive(Default, Clone)]
pub struct CachedMultiRunScores {
    pub scoring_runs: Option<Vec<ScoringRunRecord>>,
    pub scores: HashMap<Decimal, Vec<ValidatorScoreRecord>>,
}

#[derive(Default)]
pub struct Cache {
    pub validators: CachedValidators,
    pub commissions: CachedCommissions,
    pub versions: CachedVersions,
    pub uptimes: CachedUptimes,
    pub cluster_stats: CachedClusterStats,
    pub validators_aggregated: CachedValidatorsAggregated,
    pub validators_single_run_scores: CachedSingleRunScores,
    pub validators_multi_run_scores: CachedMultiRunScores,
}

impl Cache {
    pub fn new() -> Self {
        Self {
            ..Default::default()
        }
    }

    pub fn get_validators(&self) -> CachedValidators {
        self.validators.clone()
    }

    pub fn get_commissions(&self, vote_account: &String) -> Option<Vec<CommissionRecord>> {
        self.commissions.get(vote_account).cloned()
    }

    pub fn get_all_commissions(&self) -> CachedCommissions {
        self.commissions.clone()
    }

    pub fn get_versions(&self, vote_account: &String) -> Option<Vec<VersionRecord>> {
        self.versions.get(vote_account).cloned()
    }

    pub fn get_uptimes(&self, vote_account: &String) -> Option<Vec<UptimeRecord>> {
        self.uptimes.get(vote_account).cloned()
    }

    pub fn get_validators_aggregated(&self) -> CachedValidatorsAggregated {
        self.validators_aggregated.clone()
    }

    pub fn get_validators_multi_run_scores(&self) -> CachedMultiRunScores {
        self.validators_multi_run_scores.clone()
    }

    pub fn get_validators_single_run_scores(&self) -> CachedSingleRunScores {
        self.validators_single_run_scores.clone()
    }

    pub fn get_cluster_stats(&self, epochs: usize) -> CachedClusterStats {
        self.cluster_stats
            .as_ref()
            .map(|cluster_stats| ClusterStats {
                block_production_stats: cluster_stats
                    .block_production_stats
                    .iter()
                    .take(epochs)
                    .cloned()
                    .collect(),
                dc_concentration_stats: cluster_stats
                    .dc_concentration_stats
                    .iter()
                    .take(epochs)
                    .cloned()
                    .collect(),
            })
    }
}

pub async fn warm_validators_cache(context: &WrappedContext) -> anyhow::Result<()> {
    info!("Loading validators from DB");
    let warmup_timer = Instant::now();
    let validators = store::utils::load_validators(
        &context.read().await.psql_client,
        context.read().await.scoring_url.clone(),
        DEFAULT_EPOCHS,
        DEFAULT_COMPUTING_EPOCHS,
    )
    .await?;

    context
        .write()
        .await
        .cache
        .validators
        .clone_from(&validators);

    context.write().await.cache.validators_aggregated =
        store::utils::aggregate_validators(&validators);

    info!(
        "Loaded {} validators to cache in {} ms",
        validators.len(),
        warmup_timer.elapsed().as_millis()
    );

    Ok(())
}
pub async fn warm_commissions_cache(context: &WrappedContext) -> anyhow::Result<()> {
    info!("Loading commissions from DB");
    let warmup_timer = Instant::now();
    let commissions =
        store::utils::load_commissions(&context.read().await.psql_client, DEFAULT_EPOCHS).await?;

    context
        .write()
        .await
        .cache
        .commissions
        .clone_from(&commissions);
    info!(
        "Loaded {} commissions to cache in {} ms",
        commissions.len(),
        warmup_timer.elapsed().as_millis()
    );

    Ok(())
}
pub async fn warm_versions_cache(context: &WrappedContext) -> anyhow::Result<()> {
    info!("Loading versions from DB");
    let warmup_timer = Instant::now();
    let versions =
        store::utils::load_versions(&context.read().await.psql_client, DEFAULT_EPOCHS).await?;

    context.write().await.cache.versions.clone_from(&versions);
    info!(
        "Loaded {} versions to cache in {} ms",
        versions.len(),
        warmup_timer.elapsed().as_millis()
    );

    Ok(())
}
pub async fn warm_uptimes_cache(context: &WrappedContext) -> anyhow::Result<()> {
    info!("Loading uptimes from DB");
    let warmup_timer = Instant::now();
    let uptimes =
        store::utils::load_uptimes(&context.read().await.psql_client, DEFAULT_EPOCHS).await?;

    context.write().await.cache.uptimes.clone_from(&uptimes);
    info!(
        "Loaded {} uptimes to cache in {} ms",
        uptimes.len(),
        warmup_timer.elapsed().as_millis()
    );

    Ok(())
}
pub async fn warm_cluster_stats_cache(context: &WrappedContext) -> anyhow::Result<()> {
    info!("Loading cluster_stats from DB");
    let warmup_timer = Instant::now();
    let cluster_stats =
        store::utils::load_cluster_stats(&context.read().await.psql_client, DEFAULT_EPOCHS).await?;

    context.write().await.cache.cluster_stats = Some(cluster_stats);
    info!(
        "Loaded cluster_stats to cache in {} ms",
        warmup_timer.elapsed().as_millis()
    );

    Ok(())
}
pub async fn warm_scores_cache(context: &WrappedContext) -> anyhow::Result<()> {
    info!("Loading scores from DB");
    let warmup_timer = Instant::now();

    let last_scoring_run =
        store::utils::load_last_scoring_run(&context.read().await.psql_client).await?;
    let scores = match &last_scoring_run {
        Some(scoring_run) => {
            store::utils::load_scores(
                &context.read().await.psql_client,
                scoring_run.scoring_run_id,
            )
            .await?
        }
        None => Default::default(),
    };
    let multi_run_scores =
        store::scoring::load_all_scores(&context.read().await.psql_client).await?;

    let last_scoring_run =
        store::utils::load_last_scoring_run(&context.read().await.psql_client).await?;

    let multi_run_scoring_runs =
        store::scoring::load_scoring_runs(&context.read().await.psql_client).await?;

    let scores_len = scores.len();
    let multi_run_scores_len: usize = multi_run_scores.values().map(|v| v.len()).sum();

    context
        .write()
        .await
        .cache
        .validators_single_run_scores
        .clone_from(&CachedSingleRunScores {
            scoring_run: last_scoring_run,
            scores,
        });
    info!(
        "Loaded {} single run scores to cache in {} ms",
        scores_len,
        warmup_timer.elapsed().as_millis()
    );

    context
        .write()
        .await
        .cache
        .validators_multi_run_scores
        .clone_from(&CachedMultiRunScores {
            scoring_runs: Some(multi_run_scoring_runs),
            scores: multi_run_scores,
        });
    info!(
        "Loaded {} multiple run scores to cache in {} ms",
        multi_run_scores_len,
        warmup_timer.elapsed().as_millis()
    );

    Ok(())
}

pub fn spawn_cache_warmer(context: WrappedContext) {
    tokio::spawn(async move {
        loop {
            info!("Warming up the cache");

            if let Err(err) = warm_scores_cache(&context).await {
                error!("Failed to update the scores: {err}");
            }

            if let Err(err) = warm_versions_cache(&context).await {
                error!("Failed to update the versions: {err}");
            }

            if let Err(err) = warm_commissions_cache(&context).await {
                error!("Failed to update the commissions: {err}");
            }

            if let Err(err) = warm_uptimes_cache(&context).await {
                error!("Failed to update the uptimes: {err}");
            }

            if let Err(err) = warm_cluster_stats_cache(&context).await {
                error!("Failed to update the cluster stats: {err}");
            }

            if let Err(err) = warm_validators_cache(&context).await {
                error!("Failed to update the validators: {err}");
            }

            let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
            let run_every = Duration::from_secs(CACHE_WARMUP_TIME_S);
            let sleep_seconds = now.as_secs() % run_every.as_secs();
            sleep(Duration::from_secs(run_every.as_secs() - sleep_seconds)).await;
        }
    });
}
