use crate::cache::DEFAULT_EPOCHS;
use crate::context::WrappedContext;
use chrono::Utc;
use log::{error, info, warn};
use redis::{AsyncCommands, RedisError};
use rslock::LockManager;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::RwLock;
use tokio::time::{sleep, Duration, Instant};

const REDIS_LOCK_NAME: &str = "REDIS_WRITE_LOCK";
const REDIS_LOCK_PERIOD_S: usize = 10 * 60;
const REDIS_WARMUP_TIME_S: u64 = 15 * 60;

pub async fn warm_validators(
    context: &WrappedContext,
    redis_client: &Arc<RwLock<redis::Client>>,
) -> anyhow::Result<()> {
    info!("Loading validators from DB");
    let warmup_timer = Instant::now();
    let validators =
        store::utils::load_validators(&context.read().await.psql_client, DEFAULT_EPOCHS).await?;
    let validators_json = serde_json::to_string(&validators).unwrap();
    let mut conn = get_redis_connection(redis_client).await?;
    conn.set("validators", &validators_json).await?;
    info!(
        "Loaded {} validators to Redis in {} ms",
        validators.len(),
        warmup_timer.elapsed().as_millis()
    );
    Ok(())
}

pub async fn warm_commissions(
    context: &WrappedContext,
    redis_client: &Arc<RwLock<redis::Client>>,
) -> anyhow::Result<()> {
    info!("Loading commissions from DB");
    let warmup_timer = Instant::now();
    let commissions =
        store::utils::load_commissions(&context.read().await.psql_client, DEFAULT_EPOCHS).await?;
    let commissions_json = serde_json::to_string(&commissions).unwrap();
    let mut conn = get_redis_connection(redis_client).await.unwrap();
    conn.set("commissions", &commissions_json).await?;
    info!(
        "Loaded {} commissions to Redis in {} ms",
        commissions.len(),
        warmup_timer.elapsed().as_millis()
    );
    Ok(())
}

pub async fn warm_versions(
    context: &WrappedContext,
    redis_client: &Arc<RwLock<redis::Client>>,
) -> anyhow::Result<()> {
    info!("Loading versions from DB");
    let warmup_timer = Instant::now();
    let versions =
        store::utils::load_versions(&context.read().await.psql_client, DEFAULT_EPOCHS).await?;
    let versions_json = serde_json::to_string(&versions).unwrap();
    let mut conn = get_redis_connection(redis_client).await?;
    conn.set("versions", &versions_json).await?;
    info!(
        "Loaded {} versions to Redis in {} ms",
        versions.len(),
        warmup_timer.elapsed().as_millis()
    );
    Ok(())
}

pub async fn warm_uptimes(
    context: &WrappedContext,
    redis_client: &Arc<RwLock<redis::Client>>,
) -> anyhow::Result<()> {
    info!("Loading uptimes from DB");
    let warmup_timer = Instant::now();
    let uptimes =
        store::utils::load_uptimes(&context.read().await.psql_client, DEFAULT_EPOCHS).await?;
    let uptimes_json = serde_json::to_string(&uptimes).unwrap();
    let mut conn = get_redis_connection(redis_client).await?;
    conn.set("uptimes", &uptimes_json).await?;
    info!(
        "Loaded {} uptimes to Redis in {} ms",
        uptimes.len(),
        warmup_timer.elapsed().as_millis()
    );
    Ok(())
}

pub async fn warm_cluster_stats(
    context: &WrappedContext,
    redis_client: &Arc<RwLock<redis::Client>>,
) -> anyhow::Result<()> {
    info!("Loading cluster_stats from DB");
    let warmup_timer = Instant::now();
    let cluster_stats =
        store::utils::load_cluster_stats(&context.read().await.psql_client, DEFAULT_EPOCHS).await?;
    let cluster_stats_json = serde_json::to_string(&cluster_stats).unwrap();
    let mut conn = get_redis_connection(redis_client).await?;
    conn.set("cluster_stats", &cluster_stats_json).await?;
    info!(
        "Loaded cluster stats to Redis in {} ms",
        warmup_timer.elapsed().as_millis()
    );
    Ok(())
}

pub async fn warm_scores(
    context: &WrappedContext,
    redis_client: &Arc<RwLock<redis::Client>>,
) -> anyhow::Result<()> {
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
    let scores_json = serde_json::to_string(&scores).unwrap();
    let scores_len = scores.len();
    let all_scores = store::scoring::load_all_scores(&context.read().await.psql_client).await?;
    let all_scores_json = serde_json::to_string(&all_scores).unwrap();
    let all_scores_len = all_scores.len();
    let mut conn = get_redis_connection(redis_client).await?;
    conn.set("scores", &scores_json).await?;
    info!(
        "Loaded {} single run scores to Redis in {} ms",
        scores_len,
        warmup_timer.elapsed().as_millis()
    );
    conn.set("scores_all", &all_scores_json).await?;
    info!(
        "Loaded {} multiple run scores to Redis in {} ms",
        all_scores_len,
        warmup_timer.elapsed().as_millis()
    );
    Ok(())
}

pub async fn get_redis_timestamp(
    redis_client: &Arc<RwLock<redis::Client>>,
) -> Result<String, RedisError> {
    let mut conn = get_redis_connection(redis_client).await?;
    conn.get("last_update_timestamp").await
}

pub async fn check_redis_timestamp(
    redis_client: &Arc<RwLock<redis::Client>>,
    timestamp: &String,
) -> Result<bool, RedisError> {
    match get_redis_timestamp(&redis_client).await {
        Ok(last_timestamp) => return Ok(last_timestamp.eq(timestamp)),
        Err(err) => Err(err),
    }
}

async fn update_redis_timestamp(redis_client: &Arc<RwLock<redis::Client>>) {
    let now = Utc::now().to_rfc3339();
    let mut conn = get_redis_connection(redis_client).await.unwrap();

    match conn
        .set::<_, _, String>("last_update_timestamp", now.clone())
        .await
    {
        Ok(_) => info!("Changed Redis last update timestamp: {}", now.clone()),
        Err(_) => info!("Failed to update Redis last timestamp"),
    }
}

pub async fn get_redis_connection(
    redis_client: &Arc<RwLock<redis::Client>>,
) -> Result<redis::aio::Connection, RedisError> {
    let client = &redis_client.read().await;
    client.get_async_connection().await
}

pub fn spawn_redis_warmer(
    context: WrappedContext,
    redis_client: Arc<RwLock<redis::Client>>,
    redis_locker: LockManager,
) {
    tokio::spawn(async move {
        loop {
            let lock = redis_locker
                .lock(REDIS_LOCK_NAME.as_bytes(), REDIS_LOCK_PERIOD_S)
                .await;

            if let Ok(acquired_lock) = lock {
                info!("Warming up Redis");
                if let Err(err) = warm_scores(&context, &redis_client).await {
                    error!("Failed to update the scores in Redis: {}", err);
                }

                if let Err(err) = warm_versions(&context, &redis_client).await {
                    error!("Failed to update the versions in Redis: {}", err);
                }

                if let Err(err) = warm_uptimes(&context, &redis_client).await {
                    error!("Failed to update the uptimes in Redis: {}", err);
                }

                if let Err(err) = warm_cluster_stats(&context, &redis_client).await {
                    error!("Failed to update the cluster stats in Redis: {}", err);
                }

                if let Err(err) = warm_commissions(&context, &redis_client).await {
                    error!("Failed to update the commissions in Redis: {}", err);
                }

                if let Err(err) = warm_validators(&context, &redis_client).await {
                    error!("Failed to update the validators in Redis: {}", err);
                }
                update_redis_timestamp(&redis_client).await;
                redis_locker.unlock(&acquired_lock).await;
            } else {
                warn!("Couldn't acquire lock. Skipping updating Redis up.");
            }

            let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
            let run_every = Duration::from_secs(REDIS_WARMUP_TIME_S);
            let sleep_seconds = now.as_secs() % run_every.as_secs();
            sleep(Duration::from_secs(run_every.as_secs() - sleep_seconds)).await;
        }
    });
}
