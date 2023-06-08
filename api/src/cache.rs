use crate::context::WrappedContext;
use crate::redis_context::WrappedRedisContext;
use log::{error, info};
use redis::{JsonCommands};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};
use store::dto::{
    ClusterStats, CommissionRecord, ScoringRunRecord, UptimeRecord, ValidatorRecord,
    ValidatorScoreRecord, ValidatorsAggregated, VersionRecord,
};
use tokio::time::{sleep, Duration};

const DEFAULT_EPOCHS: u64 = 20;

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

pub async fn warm_validators_cache(context: &WrappedContext, redis_context: &WrappedRedisContext) -> anyhow::Result<()> {
    info!("Loading validators from Redis");

    let client = &redis_context.read().await.redis_client;
    let mut conn = client.get_connection()?;
    let validators_json: String = conn.json_get("validators",".")?;
    let validators : HashMap<String, ValidatorRecord> = serde_json::from_str(&validators_json).unwrap();

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
pub async fn warm_validators_redis(context: &WrappedContext, redis_context: &WrappedRedisContext) -> anyhow::Result<()> {
    info!("Loading validators from DB");
    let validators =
    store::utils::load_validators(&context.read().await.psql_client, DEFAULT_EPOCHS).await?;

    let client = &redis_context.write().await.redis_client;
    let mut conn = client.get_connection()?;
    conn.json_set("validators",".", &validators)?;
    info!("Loaded validators to Redis: {}", validators.len());

    Ok(())
}

pub async fn warm_commissions_cache(context: &WrappedContext, redis_context: &WrappedRedisContext) -> anyhow::Result<()> {
    info!("Loading commissions from Redis");

    let client = &redis_context.read().await.redis_client;
    let mut conn = client.get_connection()?;
    let commissions_json: String = conn.json_get("commissions",".")?;
    let commissions : HashMap<String, Vec<CommissionRecord>> = serde_json::from_str(&commissions_json).unwrap();

    context
        .write()
        .await
        .cache
        .commissions
        .clone_from(&commissions);
    info!("Loaded commissions to cache: {}", commissions.len());

    Ok(())
}
pub async fn warm_commissions_redis(context: &WrappedContext, redis_context: &WrappedRedisContext) -> anyhow::Result<()> {
    info!("Loading commissions from DB");
    let commissions =
        store::utils::load_commissions(&context.read().await.psql_client, DEFAULT_EPOCHS).await?;

    let client = &redis_context.write().await.redis_client;
    let mut conn = client.get_connection()?;
    conn.json_set("commissions",".", &commissions)?;
    info!("Loaded commissions to Redis: {}", commissions.len());

    Ok(())
}

pub async fn warm_versions_cache(context: &WrappedContext, redis_context: &WrappedRedisContext) -> anyhow::Result<()> {
    info!("Loading versions from Redis");

    let client = &redis_context.read().await.redis_client;
    let mut conn = client.get_connection()?;
    let versions_json: String = conn.json_get("versions",".")?;
    let versions : HashMap<String, Vec<VersionRecord>> = serde_json::from_str(&versions_json).unwrap();

    context.write().await.cache.versions.clone_from(&versions);
    info!("Loaded versions to cache: {}", versions.len());

    Ok(())
}
pub async fn warm_versions_redis(context: &WrappedContext, redis_context: &WrappedRedisContext) -> anyhow::Result<()> {
    info!("Loading versions from DB");

    let versions =
        store::utils::load_versions(&context.read().await.psql_client, DEFAULT_EPOCHS).await?;
    let client = &redis_context.write().await.redis_client;
    let mut conn = client.get_connection()?;
    conn.json_set("versions",".", &versions)?;
    info!("Loaded versions to Redis: {}", versions.len());

    Ok(())
}

pub async fn warm_uptimes_cache(context: &WrappedContext, redis_context: &WrappedRedisContext) -> anyhow::Result<()> {
    info!("Loading uptimes from Redis");

    let client = &redis_context.read().await.redis_client;
    let mut conn = client.get_connection()?;
    let uptimes_json: String = conn.json_get("uptimes",".")?;
    let uptimes : HashMap<String, Vec<UptimeRecord>> = serde_json::from_str(&uptimes_json).unwrap();

    context.write().await.cache.uptimes.clone_from(&uptimes);
    info!("Loaded uptimes to cache: {}", uptimes.len());

    Ok(())
}
pub async fn warm_uptimes_redis(context: &WrappedContext, redis_context: &WrappedRedisContext) -> anyhow::Result<()> {
    info!("Loading uptimes from DB");

    let uptimes =
        store::utils::load_uptimes(&context.read().await.psql_client, DEFAULT_EPOCHS).await?;
    let client = &redis_context.write().await.redis_client;
    let mut conn = client.get_connection()?;
    conn.json_set("uptimes",".", &uptimes)?;
    info!("Loaded uptimes to Redis: {}", uptimes.len());

    Ok(())
}

pub async fn warm_cluster_stats_cache(context: &WrappedContext, redis_context: &WrappedRedisContext) -> anyhow::Result<()> {
    info!("Loading cluster_stats from Redis");

    let client = &redis_context.read().await.redis_client;
    let mut conn = client.get_connection()?;
    let cluster_stats_json: String = conn.json_get("cluster_stats",".")?;
    let cluster_stats : ClusterStats = serde_json::from_str(&cluster_stats_json).unwrap();

    context.write().await.cache.cluster_stats = Some(cluster_stats);
    info!("Loaded cluster_stats to cache");

    Ok(())
}
pub async fn warm_cluster_stats_redis(context: &WrappedContext, redis_context: &WrappedRedisContext) -> anyhow::Result<()> {
    info!("Loading cluster_stats from DB");

    let cluster_stats =
        store::utils::load_cluster_stats(&context.read().await.psql_client, DEFAULT_EPOCHS).await?;
    let client = &redis_context.write().await.redis_client;
    let mut conn = client.get_connection()?;
    conn.json_set("cluster_stats",".", &cluster_stats)?;
    info!("Loaded cluster_stats to Redis");

    Ok(())
}

pub async fn warm_scores_cache(context: &WrappedContext, redis_context: &WrappedRedisContext) -> anyhow::Result<()> {
    info!("Loading scores from Redis");

    let client = &redis_context.read().await.redis_client;
    let mut conn = client.get_connection()?;
    let scores_json: String = conn.json_get("scores",".")?;
    let scores : HashMap<String, ValidatorScoreRecord> = serde_json::from_str(&scores_json).unwrap();

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
pub async fn warm_scores_redis(context: &WrappedContext, redis_context: &WrappedRedisContext) -> anyhow::Result<()> {
    info!("Loading scores from DB");

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

    let scores_len = scores.len();

    let client = &redis_context.write().await.redis_client;
    let mut conn = client.get_connection()?;
    conn.json_set("scores",".", &scores)?;

    info!("Loaded scores to Redis: {}", scores_len);

    Ok(())
}

pub fn spawn_cache_warmer(context: WrappedContext, redis_context: WrappedRedisContext) {
    tokio::spawn(async move {
        loop {
            info!("Warming up the cache");

            if let Err(err) = warm_scores_cache(&context, &redis_context).await {
                error!("Failed to update the scores: {}", err);
            }

            if let Err(err) = warm_versions_cache(&context, &redis_context).await {
                error!("Failed to update the versions: {}", err);
            }

            if let Err(err) = warm_commissions_cache(&context, &redis_context).await {
                error!("Failed to update the commissions: {}", err);
            }

            if let Err(err) = warm_uptimes_cache(&context, &redis_context).await {
                error!("Failed to update the uptimes: {}", err);
            }

            if let Err(err) = warm_cluster_stats_cache(&context, &redis_context).await {
                error!("Failed to update the cluster stats: {}", err);
            }

            if let Err(err) = warm_validators_cache(&context, &redis_context).await {
                error!("Failed to update the validators: {}", err);
            }

            let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
            let run_every = Duration::from_secs(15 * 60);
            let run_offset_seconds = 5 * 60 + 30;
            let sleep_seconds = now.as_secs() % run_every.as_secs();
            sleep(Duration::from_secs(
                run_every.as_secs() - sleep_seconds + run_offset_seconds,
            ))
            .await;
        }
    });
}
pub fn spawn_redis_warmer(context: WrappedContext, redis_context: WrappedRedisContext) {
       tokio::spawn(async move {
        loop {
            info!("Warming up Redis");

            if let Err(err) = warm_scores_redis(&context, &redis_context).await {
                error!("Failed to update the scores in Redis: {}", err);
            }

            if let Err(err) = warm_versions_redis(&context, &redis_context).await {
                error!("Failed to update the versions in Redis: {}", err);
            }

            if let Err(err) = warm_uptimes_redis(&context, &redis_context).await {
                error!("Failed to update the uptimes in Redis: {}", err);
            }

            if let Err(err) = warm_cluster_stats_redis(&context, &redis_context).await {
                error!("Failed to update the cluster stats in Redis: {}", err);
            }

            if let Err(err) = warm_commissions_redis(&context, &redis_context).await {
                error!("Failed to update the commissions in Redis: {}", err);
            }

            if let Err(err) = warm_validators_redis(&context, &redis_context).await {
                error!("Failed to update the validators in Redis: {}", err);
            }

            let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
            let run_every = Duration::from_secs(10 * 60);
            let run_offset_seconds = 5 * 60 + 30;
            let sleep_seconds = now.as_secs() % run_every.as_secs();
            sleep(Duration::from_secs(
                run_every.as_secs() - sleep_seconds + run_offset_seconds,
            ))
            .await;
        }
    }); 
}