use crate::context::WrappedContext;
use crate::redis_cache;
use log::{error, info, warn};
use redis::JsonCommands;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use store::dto::{
    ClusterStats, CommissionRecord, ScoringRunRecord, UptimeRecord, ValidatorRecord,
    ValidatorScoreRecord, ValidatorsAggregated, VersionRecord,
};
use tokio::sync::RwLock;
use tokio::time::{sleep, Duration, Instant};

pub(crate) const DEFAULT_EPOCHS: u64 = 80;
const CACHE_WARMUP_TIME_S: u64 = 15 * 60;
const CACHE_WARMUP_OFFSET_S: u64 = 5 * 60 + 30;
const CACHE_WARMUP_RETRY_TIME_S: u64 = 120;

type CachedValidators = HashMap<String, ValidatorRecord>;
type CachedCommissions = HashMap<String, Vec<CommissionRecord>>;
type CachedVersions = HashMap<String, Vec<VersionRecord>>;
type CachedUptimes = HashMap<String, Vec<UptimeRecord>>;
type CachedClusterStats = Option<ClusterStats>;
type CachedValidatorsAggregated = Vec<ValidatorsAggregated>;

#[derive(Default, Clone)]
pub struct CachedScores {
    pub scoring_run: Option<ScoringRunRecord>,
    pub scores: HashMap<String, ValidatorScoreRecord>,
}

#[derive(Default)]
pub struct Cache {
    pub validators: CachedValidators,
    pub commissions: CachedCommissions,
    pub versions: CachedVersions,
    pub uptimes: CachedUptimes,
    pub cluster_stats: CachedClusterStats,
    pub validators_aggregated: CachedValidatorsAggregated,
    pub validators_scores: CachedScores,
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

    pub fn get_validators_scores(&self) -> CachedScores {
        self.validators_scores.clone()
    }

    pub fn get_cluster_stats(&self, epochs: usize) -> CachedClusterStats {
        match &self.cluster_stats {
            Some(cluster_stats) => Some(ClusterStats {
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
            }),
            _ => None,
        }
    }
}

pub async fn warm_validators_cache(
    context: &WrappedContext,
    redis_client: &Arc<RwLock<redis::Client>>,
) -> anyhow::Result<()> {
    info!("Loading validators from Redis");
    let mut conn = redis_cache::get_redis_connection(redis_client).await?;
    let validators_json: String = conn.json_get("validators", ".")?;
    let validators: HashMap<String, ValidatorRecord> =
        serde_json::from_str(&validators_json).unwrap();

    context
        .write()
        .await
        .cache
        .validators
        .clone_from(&validators);

    context.write().await.cache.validators_aggregated =
        store::utils::aggregate_validators(&validators);

    info!("Loaded validators to cache: {}", validators.len());

    Ok(())
}
pub async fn warm_commissions_cache(
    context: &WrappedContext,
    redis_client: &Arc<RwLock<redis::Client>>,
) -> anyhow::Result<()> {
    info!("Loading commissions from Redis");

    let mut conn = redis_cache::get_redis_connection(redis_client).await?;
    let commissions_json: String = conn.json_get("commissions", ".")?;
    let commissions: HashMap<String, Vec<CommissionRecord>> =
        serde_json::from_str(&commissions_json).unwrap();

    context
        .write()
        .await
        .cache
        .commissions
        .clone_from(&commissions);
    info!("Loaded commissions to cache: {}", commissions.len());

    Ok(())
}
pub async fn warm_versions_cache(
    context: &WrappedContext,
    redis_client: &Arc<RwLock<redis::Client>>,
) -> anyhow::Result<()> {
    info!("Loading versions from Redis");

    let mut conn = redis_cache::get_redis_connection(redis_client).await?;
    let versions_json: String = conn.json_get("versions", ".")?;
    let versions: HashMap<String, Vec<VersionRecord>> =
        serde_json::from_str(&versions_json).unwrap();

    context.write().await.cache.versions.clone_from(&versions);
    info!("Loaded versions to cache: {}", versions.len());

    Ok(())
}
pub async fn warm_uptimes_cache(
    context: &WrappedContext,
    redis_client: &Arc<RwLock<redis::Client>>,
) -> anyhow::Result<()> {
    info!("Loading uptimes from Redis");

    let mut conn = redis_cache::get_redis_connection(redis_client).await?;
    let uptimes_json: String = conn.json_get("uptimes", ".")?;
    let uptimes: HashMap<String, Vec<UptimeRecord>> = serde_json::from_str(&uptimes_json).unwrap();

    context.write().await.cache.uptimes.clone_from(&uptimes);
    info!("Loaded uptimes to cache: {}", uptimes.len());

    Ok(())
}
pub async fn warm_cluster_stats_cache(
    context: &WrappedContext,
    redis_client: &Arc<RwLock<redis::Client>>,
) -> anyhow::Result<()> {
    info!("Loading cluster_stats from Redis");

    let mut conn = redis_cache::get_redis_connection(redis_client).await?;
    let cluster_stats_json: String = conn.json_get("cluster_stats", ".")?;
    let cluster_stats: ClusterStats = serde_json::from_str(&cluster_stats_json).unwrap();

    context.write().await.cache.cluster_stats = Some(cluster_stats);
    info!("Loaded cluster_stats to cache");

    Ok(())
}
pub async fn warm_scores_cache(
    context: &WrappedContext,
    redis_client: &Arc<RwLock<redis::Client>>,
) -> anyhow::Result<()> {
    info!("Loading scores from Redis");

    let mut conn = redis_cache::get_redis_connection(redis_client).await?;
    let scores_json: String = conn.json_get("scores", ".")?;
    let scores: HashMap<String, ValidatorScoreRecord> = serde_json::from_str(&scores_json).unwrap();

    let last_scoring_run =
        store::utils::load_last_scoring_run(&context.read().await.psql_client).await?;

    let scores_len = scores.len();

    context
        .write()
        .await
        .cache
        .validators_scores
        .clone_from(&CachedScores {
            scoring_run: last_scoring_run,
            scores,
        });

    info!("Loaded scores to cache: {}", scores_len);

    Ok(())
}

pub fn spawn_cache_warmer(context: WrappedContext, redis_client: Arc<RwLock<redis::Client>>) {
    tokio::spawn(async move {
        let mut last_timestamp = String::new();
        loop {
            let redis_check =
                redis_cache::check_redis_timestamp(&redis_client, &last_timestamp).await;
            if let Err(_) = redis_check {
                warn!(
                    "Redis is not warmed up. Trying again in {} seconds.",
                    CACHE_WARMUP_RETRY_TIME_S
                );
                sleep(Duration::from_secs(CACHE_WARMUP_RETRY_TIME_S)).await;
                continue;
            }

            if !redis_check.ok().unwrap() {
                warn!("Redis timestamp mismatch. Cache must be updated.");
                info!("Warming up the cache");
                let warmup_timer = Instant::now();

                if let Err(err) = warm_scores_cache(&context, &redis_client).await {
                    error!("Failed to update the scores: {}", err);
                }

                if let Err(err) = warm_versions_cache(&context, &redis_client).await {
                    error!("Failed to update the versions: {}", err);
                }

                if let Err(err) = warm_commissions_cache(&context, &redis_client).await {
                    error!("Failed to update the commissions: {}", err);
                }

                if let Err(err) = warm_uptimes_cache(&context, &redis_client).await {
                    error!("Failed to update the uptimes: {}", err);
                }

                if let Err(err) = warm_cluster_stats_cache(&context, &redis_client).await {
                    error!("Failed to update the cluster stats: {}", err);
                }

                if let Err(err) = warm_validators_cache(&context, &redis_client).await {
                    error!("Failed to update the validators: {}", err);
                }
                if let Ok(timestamp) = redis_cache::get_redis_timestamp(&redis_client).await {
                    last_timestamp = timestamp;
                }
                info!(
                    "Warming up done in {} ms",
                    warmup_timer.elapsed().as_millis()
                );
            } else {
                info!("Redis timestamp matched. No actions required");
            }

            let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
            let run_every = Duration::from_secs(CACHE_WARMUP_TIME_S);
            let sleep_seconds = now.as_secs() % run_every.as_secs();
            sleep(Duration::from_secs(
                run_every.as_secs() - sleep_seconds + CACHE_WARMUP_OFFSET_S,
            ))
            .await;
        }
    });
}
