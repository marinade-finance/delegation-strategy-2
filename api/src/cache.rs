use crate::context::WrappedContext;
use log::{error, info};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};
use store::dto::{
    ClusterStats, CommissionRecord, ScoringRunRecord, UptimeRecord, ValidatorCurrentStake,
    ValidatorRecord, ValidatorScoreRecord, ValidatorsAggregated, VersionRecord,
};
use tokio::time::{sleep, Duration};

const DEFAULT_EPOCHS: u64 = 20;

type CachedValidators = HashMap<String, ValidatorRecord>;
type CachedCommissions = HashMap<String, Vec<CommissionRecord>>;
type CachedVersions = HashMap<String, Vec<VersionRecord>>;
type CachedUptimes = HashMap<String, Vec<UptimeRecord>>;
type CachedClusterStats = Option<ClusterStats>;
type CachedValidatorsAggregated = Vec<ValidatorsAggregated>;
type CachedValidatorsCurrentStakes = HashMap<String, ValidatorCurrentStake>;
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
    pub validators_current_stakes: CachedValidatorsCurrentStakes,
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

    pub fn get_commissions(&self, identity: &String) -> Option<Vec<CommissionRecord>> {
        self.commissions.get(identity).cloned()
    }

    pub fn get_all_commissions(&self) -> CachedCommissions {
        self.commissions.clone()
    }

    pub fn get_versions(&self, identity: &String) -> Option<Vec<VersionRecord>> {
        self.versions.get(identity).cloned()
    }

    pub fn get_uptimes(&self, identity: &String) -> Option<Vec<UptimeRecord>> {
        self.uptimes.get(identity).cloned()
    }

    pub fn get_validators_aggregated(&self) -> CachedValidatorsAggregated {
        self.validators_aggregated.clone()
    }

    pub fn get_validators_scores(&self) -> CachedScores {
        self.validators_scores.clone()
    }

    pub fn get_validators_current_stakes(&self) -> CachedValidatorsCurrentStakes {
        self.validators_current_stakes.clone()
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

pub async fn warm_validators_cache(context: &WrappedContext) -> anyhow::Result<()> {
    info!("Loading validators from DB");

    let validators =
        store::utils::load_validators(&context.read().await.psql_client, DEFAULT_EPOCHS).await?;
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

pub async fn warm_commissions_cache(context: &WrappedContext) -> anyhow::Result<()> {
    info!("Loading commissions from DB");

    let commissions =
        store::utils::load_commissions(&context.read().await.psql_client, DEFAULT_EPOCHS).await?;
    context
        .write()
        .await
        .cache
        .commissions
        .clone_from(&commissions);
    info!("Loaded commissions to cache: {}", commissions.len());

    Ok(())
}

pub async fn warm_versions_cache(context: &WrappedContext) -> anyhow::Result<()> {
    info!("Loading versions from DB");

    let versions =
        store::utils::load_versions(&context.read().await.psql_client, DEFAULT_EPOCHS).await?;
    context.write().await.cache.versions.clone_from(&versions);
    info!("Loaded versions to cache: {}", versions.len());

    Ok(())
}

pub async fn warm_uptimes_cache(context: &WrappedContext) -> anyhow::Result<()> {
    info!("Loading uptimes from DB");

    let uptimes =
        store::utils::load_uptimes(&context.read().await.psql_client, DEFAULT_EPOCHS).await?;
    context.write().await.cache.uptimes.clone_from(&uptimes);
    info!("Loaded uptimes to cache: {}", uptimes.len());

    Ok(())
}

pub async fn warm_cluster_stats_cache(context: &WrappedContext) -> anyhow::Result<()> {
    info!("Loading cluster_stats from DB");

    let cluster_stats =
        store::utils::load_cluster_stats(&context.read().await.psql_client, DEFAULT_EPOCHS).await?;
    context.write().await.cache.cluster_stats = Some(cluster_stats);
    info!("Loaded cluster_stats to cache");

    Ok(())
}

pub async fn warm_scores_cache(context: &WrappedContext) -> anyhow::Result<()> {
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

pub async fn warm_validator_current_stakes_cache(context: &WrappedContext) -> anyhow::Result<()> {
    info!("Loading current stakes from DB");

    let stakes = store::utils::load_current_stake(&context.read().await.psql_client).await?;
    context
        .write()
        .await
        .cache
        .validators_current_stakes
        .clone_from(&stakes);

    info!("Loaded current stakes to cache: {}", stakes.len());

    Ok(())
}

pub fn spawn_cache_warmer(context: WrappedContext) {
    tokio::spawn(async move {
        loop {
            info!("Warming up the cache");

            if let Err(err) = warm_scores_cache(&context).await {
                error!("Failed to update the scores: {}", err);
            }

            if let Err(err) = warm_versions_cache(&context).await {
                error!("Failed to update the versions: {}", err);
            }

            if let Err(err) = warm_commissions_cache(&context).await {
                error!("Failed to update the commissions: {}", err);
            }

            if let Err(err) = warm_uptimes_cache(&context).await {
                error!("Failed to update the uptimes: {}", err);
            }

            if let Err(err) = warm_cluster_stats_cache(&context).await {
                error!("Failed to update the cluster stats: {}", err);
            }

            if let Err(err) = warm_validators_cache(&context).await {
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
