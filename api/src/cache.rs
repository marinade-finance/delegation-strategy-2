use crate::context::WrappedContext;
use log::{error, info};
use rust_decimal::Decimal;
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::time::{SystemTime, UNIX_EPOCH};
use store::dto::{
    ClusterStats, CommissionRecord, ScoringRunRecord, UptimeRecord, ValidatorRecord,
    ValidatorScoreRecord, ValidatorsAggregated, VersionRecord,
};
use tokio::time::{sleep, Duration, Instant};

pub(crate) use store::utils::DEFAULT_CACHE_EPOCHS;
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
    pub per_epoch: Option<PerEpochCache>,
}

/// BigQuery-sourced validator data, cached and refreshed only when a new epoch lands in BigQuery.
#[derive(Default, Clone)]
pub struct PerEpochCache {
    pub epoch: u64,
    pub unique_delegators: HashMap<String, u64>,
    pub take_rates: HashMap<String, f64>,
}

impl PerEpochCache {
    /// `Some(refreshed)` when BigQuery's latest epoch advanced past `cached`; `None` on a cache hit or failure.
    pub async fn load(cached: &Option<PerEpochCache>) -> Option<PerEpochCache> {
        let last_epoch = match store::utils::load_last_bigquery_epoch().await {
            Ok(Some(epoch)) => epoch,
            Ok(None) => return None,
            Err(err) => {
                error!("Failed to read last BigQuery epoch: {err}");
                return None;
            }
        };

        if cached.as_ref().map(|c| c.epoch) == Some(last_epoch) {
            return None;
        }

        info!(
            "BigQuery epoch changed ({:?} -> {last_epoch}), refreshing",
            cached.as_ref().map(|c| c.epoch)
        );
        let delegators = store::utils::load_latest_unique_delegators().await;
        let take_rates = store::utils::load_take_rates().await;
        match (delegators, take_rates) {
            (Ok(unique_delegators), Ok(take_rates)) => Some(PerEpochCache {
                epoch: last_epoch,
                unique_delegators,
                take_rates,
            }),
            (delegators, take_rates) => {
                if let Err(err) = delegators {
                    error!("Failed to load unique delegators from BigQuery: {err}");
                }
                if let Err(err) = take_rates {
                    error!("Failed to load take rates from BigQuery: {err}");
                }
                None
            }
        }
    }
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

    pub fn find_validator_key(&self, vote_account_or_identity: &str) -> Option<String> {
        self.validators
            .iter()
            .find(|(_, record)| {
                record.identity == vote_account_or_identity
                    || record.vote_account == vote_account_or_identity
            })
            .map(|(vote_key, _)| vote_key.clone())
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
                client_diversity_stats: cluster_stats
                    .client_diversity_stats
                    .iter()
                    .take(epochs)
                    .cloned()
                    .collect(),
                client_lineage_stats: cluster_stats
                    .client_lineage_stats
                    .iter()
                    .take(epochs)
                    .cloned()
                    .collect(),
                feature_set_stats: cluster_stats
                    .feature_set_stats
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

    let cached = context.read().await.cache.per_epoch.clone();

    let refreshed = PerEpochCache::load(&cached).await;
    let (unique_delegators, take_rates) = refreshed
        .as_ref()
        .or(cached.as_ref())
        .map(|c| (c.unique_delegators.clone(), c.take_rates.clone()))
        .unwrap_or_default();

    let validators = store::utils::load_validators(
        &context.read().await.psql_client,
        context.read().await.scoring_url.clone(),
        DEFAULT_CACHE_EPOCHS,
        DEFAULT_COMPUTING_EPOCHS,
        &unique_delegators,
        &take_rates,
    )
    .await?;

    let validators_aggregated = store::utils::aggregate_validators(&validators);
    let validators_len = validators.len();

    {
        let mut ctx = context.write().await;
        if let Some(refreshed) = refreshed {
            ctx.cache.per_epoch = Some(refreshed);
        }
        ctx.cache.validators = validators;
        ctx.cache.validators_aggregated = validators_aggregated;
    }

    info!(
        "Loaded {} validators to cache in {} ms",
        validators_len,
        warmup_timer.elapsed().as_millis()
    );

    Ok(())
}
pub async fn warm_commissions_cache(context: &WrappedContext) -> anyhow::Result<()> {
    info!("Loading commissions from DB");
    let warmup_timer = Instant::now();
    let commissions =
        store::utils::load_commissions(&context.read().await.psql_client, DEFAULT_CACHE_EPOCHS)
            .await?;

    let commissions_len = commissions.len();
    context.write().await.cache.commissions = commissions;
    info!(
        "Loaded {} commissions to cache in {} ms",
        commissions_len,
        warmup_timer.elapsed().as_millis()
    );

    Ok(())
}
pub async fn warm_versions_cache(context: &WrappedContext) -> anyhow::Result<()> {
    info!("Loading versions from DB");
    let warmup_timer = Instant::now();
    let versions =
        store::utils::load_versions(&context.read().await.psql_client, DEFAULT_CACHE_EPOCHS)
            .await?;

    let versions_len = versions.len();
    context.write().await.cache.versions = versions;
    info!(
        "Loaded {} versions to cache in {} ms",
        versions_len,
        warmup_timer.elapsed().as_millis()
    );

    Ok(())
}
pub async fn warm_uptimes_cache(context: &WrappedContext) -> anyhow::Result<()> {
    info!("Loading uptimes from DB");
    let warmup_timer = Instant::now();
    let uptimes =
        store::utils::load_uptimes(&context.read().await.psql_client, DEFAULT_CACHE_EPOCHS).await?;

    let uptimes_len = uptimes.len();
    context.write().await.cache.uptimes = uptimes;
    info!(
        "Loaded {} uptimes to cache in {} ms",
        uptimes_len,
        warmup_timer.elapsed().as_millis()
    );

    Ok(())
}
pub async fn warm_cluster_stats_cache(context: &WrappedContext) -> anyhow::Result<()> {
    info!("Loading cluster_stats from DB");
    let warmup_timer = Instant::now();
    let cluster_stats =
        store::utils::load_cluster_stats(&context.read().await.psql_client, DEFAULT_CACHE_EPOCHS)
            .await?;

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

    let multi_run_scoring_runs =
        store::scoring::load_scoring_runs(&context.read().await.psql_client).await?;

    let scores_len = scores.len();
    let multi_run_scores_len: usize = multi_run_scores.values().map(|v| v.len()).sum();

    context.write().await.cache.validators_single_run_scores = CachedSingleRunScores {
        scoring_run: last_scoring_run,
        scores,
    };
    info!(
        "Loaded {} single run scores to cache in {} ms",
        scores_len,
        warmup_timer.elapsed().as_millis()
    );

    context.write().await.cache.validators_multi_run_scores = CachedMultiRunScores {
        scoring_runs: Some(multi_run_scoring_runs),
        scores: multi_run_scores,
    };
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
            let sleep_seconds = warmup_sleep_seconds(
                now.as_secs(),
                warmup_phase_seconds(CACHE_WARMUP_TIME_S),
                CACHE_WARMUP_TIME_S,
            );
            sleep(Duration::from_secs(sleep_seconds)).await;
        }
    });
}

// replicas warming on the same wall-clock boundary hit the shared connection as a 2x spike
fn warmup_phase_seconds(period_seconds: u64) -> u64 {
    let mut hasher = DefaultHasher::new();
    std::env::var("HOSTNAME")
        .unwrap_or_default()
        .hash(&mut hasher);
    hasher.finish() % period_seconds
}

fn warmup_sleep_seconds(now_seconds: u64, phase_seconds: u64, period_seconds: u64) -> u64 {
    match (period_seconds + phase_seconds - now_seconds % period_seconds) % period_seconds {
        0 => period_seconds,
        wait => wait,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache_with(records: &[(&str, &str)]) -> Cache {
        let mut cache = Cache::new();
        for (vote_account, identity) in records {
            cache.validators.insert(
                vote_account.to_string(),
                ValidatorRecord {
                    vote_account: vote_account.to_string(),
                    identity: identity.to_string(),
                    ..Default::default()
                },
            );
        }
        cache
    }

    #[test]
    fn find_validator_key_resolves_vote_account_and_identity() {
        let cache = cache_with(&[("vote-a", "id-a"), ("vote-b", "id-b")]);

        assert_eq!(
            cache.find_validator_key("vote-a"),
            Some("vote-a".to_string())
        );
        assert_eq!(cache.find_validator_key("id-b"), Some("vote-b".to_string()));
        assert_eq!(cache.find_validator_key("nope"), None);
    }

    #[test]
    fn find_validator_key_on_an_empty_cache_is_none() {
        assert_eq!(Cache::new().find_validator_key("vote-a"), None);
    }

    #[test]
    fn warmup_without_a_phase_keeps_the_previous_boundary_alignment() {
        assert_eq!(warmup_sleep_seconds(0, 0, 600), 600);
        assert_eq!(warmup_sleep_seconds(1, 0, 600), 599);
        assert_eq!(warmup_sleep_seconds(599, 0, 600), 1);
        assert_eq!(warmup_sleep_seconds(600, 0, 600), 600);
    }

    #[test]
    fn warmup_wakes_on_its_own_phase_within_one_period() {
        for phase in [0, 1, 137, 599] {
            for now in [0, 1, 137, 599, 600, 12_345] {
                let sleep = warmup_sleep_seconds(now, phase, 600);
                assert!((1..=600).contains(&sleep), "phase {phase} now {now}");
                assert_eq!((now + sleep) % 600, phase, "phase {phase} now {now}");
            }
        }
    }

    #[test]
    fn warmup_phase_is_stable_and_within_the_period() {
        assert_eq!(warmup_phase_seconds(600), warmup_phase_seconds(600));
        assert!(warmup_phase_seconds(600) < 600);
    }
}
