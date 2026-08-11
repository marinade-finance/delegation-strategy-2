use crate::context::WrappedContext;
use log::{error, info};
use rust_decimal::Decimal;
use std::collections::{HashMap, HashSet};
use std::time::{SystemTime, UNIX_EPOCH};
use store::dto::{
    ClusterStats, CommissionRecord, ScoringRunRecord, UptimeRecord, ValidatorRecord,
    ValidatorScoreRecord, VersionRecord,
};
use tokio::time::{sleep, Duration, Instant};

use store::utils::ValidatorOverlays;

pub(crate) use store::utils::DEFAULT_CACHE_EPOCHS;
pub(crate) const DEFAULT_COMPUTING_EPOCHS: u64 = 20;
const CACHE_WARMUP_TIME_S: u64 = 10 * 60;

type CachedValidators = HashMap<String, ValidatorRecord>;
type CachedCommissions = HashMap<String, Vec<CommissionRecord>>;
type CachedVersions = HashMap<String, Vec<VersionRecord>>;
type CachedUptimes = HashMap<String, Vec<UptimeRecord>>;
type CachedClusterStats = Option<ClusterStats>;

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

/// One validator-bonds flag list, how many refreshes in a row fell back to it, and when upstream last answered.
#[derive(Default, Clone)]
pub struct CachedFlag {
    pub vote_accounts: HashSet<String>,
    pub consecutive_fallbacks: u32,
    pub last_success: Option<SystemTime>,
}

#[derive(Default, Clone)]
pub struct CachedBondFlags {
    pub verified: CachedFlag,
    pub protected: CachedFlag,
}

/// Latest net APY per vote account as apy-api served it, and when it last answered.
#[derive(Default, Clone)]
pub struct CachedNetApy {
    pub values: HashMap<String, f64>,
    pub last_success: Option<SystemTime>,
}

#[derive(Default)]
pub struct Cache {
    pub bond_flags: CachedBondFlags,
    pub net_apy: CachedNetApy,
    pub validators: CachedValidators,
    pub commissions: CachedCommissions,
    pub versions: CachedVersions,
    pub uptimes: CachedUptimes,
    pub cluster_stats: CachedClusterStats,
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

    // The older of the two flags, since a consumer has to assume the worse freshness of the pair.
    pub fn bond_flags_updated_at(&self) -> Option<SystemTime> {
        let verified = self.bond_flags.verified.last_success?;
        let protected = self.bond_flags.protected.last_success?;
        Some(verified.min(protected))
    }

    pub fn net_apy_updated_at(&self) -> Option<SystemTime> {
        self.net_apy.last_success
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

/// Bounded so a permanently broken bonds API surfaces as an outage instead of flags frozen forever.
const MAX_FLAG_FALLBACK_CYCLES: u32 = 6;

fn resolve_flag(
    flag: &str,
    fetched: anyhow::Result<HashSet<String>>,
    last: CachedFlag,
) -> CachedFlag {
    // An empty list is more often an upstream fault than the truth, so it is believed only once it
    // survives the same bound a failure gets.
    let err = match fetched {
        Ok(vote_accounts) if !vote_accounts.is_empty() => {
            return CachedFlag {
                vote_accounts,
                consecutive_fallbacks: 0,
                last_success: Some(SystemTime::now()),
            }
        }
        Ok(_) => None,
        Err(err) => Some(err),
    };

    let consecutive_fallbacks = last.consecutive_fallbacks + 1;
    if last.vote_accounts.is_empty() || consecutive_fallbacks >= MAX_FLAG_FALLBACK_CYCLES {
        return match err {
            None => CachedFlag {
                vote_accounts: HashSet::new(),
                consecutive_fallbacks: 0,
                last_success: Some(SystemTime::now()),
            },
            Some(err) => {
                error!("Failed to load {flag} validators, reporting the flags as unknown: {err}");
                // Erroring instead would strand the validators load, which readiness gates on.
                CachedFlag::default()
            }
        };
    }

    match &err {
        None => error!(
            "The {flag} endpoint lists nobody, holding the last known list until it repeats ({consecutive_fallbacks}/{MAX_FLAG_FALLBACK_CYCLES})"
        ),
        Some(err) => error!(
            "Failed to load {flag} validators, reusing the last known list ({consecutive_fallbacks}/{MAX_FLAG_FALLBACK_CYCLES}): {err}"
        ),
    }
    CachedFlag {
        consecutive_fallbacks,
        ..last
    }
}

/// Bounded by age, not by a fallback count like the flags: this moves once per epoch, so reuse stays accurate until three days clear a real epoch.
const MAX_NET_APY_REUSE: Duration = Duration::from_secs(3 * 24 * 3600);

/// A broken apy-api must not stop the validators cache refreshing, so it degrades to reusing and then to serving nothing.
fn resolve_net_apy(
    fetched: anyhow::Result<HashMap<String, f64>>,
    last: CachedNetApy,
) -> CachedNetApy {
    let reason = match fetched {
        Ok(values) if !values.is_empty() => {
            return CachedNetApy {
                values,
                last_success: Some(SystemTime::now()),
            }
        }
        Ok(_) => "apy-api lists no validator net APY".to_string(),
        Err(err) => format!("Failed to load validator net APY: {err}"),
    };

    if last
        .last_success
        .is_some_and(|at| at.elapsed().is_ok_and(|age| age > MAX_NET_APY_REUSE))
    {
        error!("{reason}; the last known values outlived the epoch they describe, serving none");
        return CachedNetApy::default();
    }

    error!("{reason}; reusing the last known values");
    last
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

    // Scoped so the guard is released before the awaits below, not held to the end of the call.
    let (scoring_url, bonds_url, apy_url, last_flags, last_net_apy) = {
        let ctx = context.read().await;
        (
            ctx.scoring_url.clone(),
            ctx.validator_bonds_api_url.clone(),
            ctx.apy_api_url.clone(),
            ctx.cache.bond_flags.clone(),
            ctx.cache.net_apy.clone(),
        )
    };

    let (verified, protected, net_apy) = tokio::join!(
        store::utils::load_verified_validators(&bonds_url),
        store::utils::load_protected_validators(&bonds_url),
        store::utils::load_validator_net_apy(&apy_url),
    );

    let bond_flags = CachedBondFlags {
        verified: resolve_flag("verified", verified, last_flags.verified),
        protected: resolve_flag("protected", protected, last_flags.protected),
    };
    let net_apy = resolve_net_apy(net_apy, last_net_apy);

    let overlays = ValidatorOverlays {
        unique_delegators,
        take_rates,
        net_apy: net_apy.values.clone(),
        verified: bond_flags.verified.vote_accounts.clone(),
        protected: bond_flags.protected.vote_accounts.clone(),
    };

    let validators = store::utils::load_validators(
        &context.read().await.psql_client,
        scoring_url,
        DEFAULT_CACHE_EPOCHS,
        DEFAULT_COMPUTING_EPOCHS,
        &overlays,
    )
    .await?;

    let validators_len = validators.len();
    {
        // Flags and net APY publish with the records they stamped, so their timestamps cannot outrun them.
        let mut ctx = context.write().await;
        if let Some(refreshed) = refreshed {
            ctx.cache.per_epoch = Some(refreshed);
        }
        ctx.cache.bond_flags = bond_flags;
        ctx.cache.net_apy = net_apy;
        ctx.cache.validators = validators;
    }

    info!(
        "Loaded {validators_len} validators to cache in {} ms",
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
        "Loaded {commissions_len} commissions to cache in {} ms",
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
        "Loaded {versions_len} versions to cache in {} ms",
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
        "Loaded {uptimes_len} uptimes to cache in {} ms",
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
    let scoring_run_id = last_scoring_run.as_ref().map(|run| run.scoring_run_id);
    let cached_scoring_run_id = context
        .read()
        .await
        .cache
        .validators_single_run_scores
        .scoring_run
        .as_ref()
        .map(|run| run.scoring_run_id);

    // The id is a max over the whole table, so a run backfilling an older epoch bumps it too.
    if scoring_run_id.is_some() && scoring_run_id == cached_scoring_run_id {
        info!("Scoring run unchanged, keeping the cached scores");
        return Ok(());
    }

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
            let run_every = Duration::from_secs(CACHE_WARMUP_TIME_S);
            let sleep_seconds = now.as_secs() % run_every.as_secs();
            sleep(Duration::from_secs(run_every.as_secs() - sleep_seconds)).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flag(vote_accounts: &[&str], consecutive_fallbacks: u32) -> CachedFlag {
        CachedFlag {
            vote_accounts: vote_accounts.iter().map(|v| v.to_string()).collect(),
            consecutive_fallbacks,
            last_success: None,
        }
    }

    #[test]
    fn a_successful_answer_stamps_the_fetch_time() {
        let resolved = resolve_flag(
            "protected",
            Ok(HashSet::from(["voteTwo".to_string()])),
            flag(&["voteOne"], 2),
        );

        assert_eq!(
            resolved.vote_accounts,
            HashSet::from(["voteTwo".to_string()])
        );
        assert_eq!(resolved.consecutive_fallbacks, 0);
        assert!(
            resolved.last_success.is_some(),
            "consumers read this to tell a reused list from a fresh one"
        );
    }

    #[test]
    fn an_empty_answer_holds_the_last_known_list_until_it_repeats() {
        let resolved = resolve_flag("protected", Ok(HashSet::new()), flag(&["voteOne"], 1));

        assert_eq!(
            resolved.vote_accounts,
            flag(&["voteOne"], 0).vote_accounts,
            "one empty answer is more likely an upstream fault than the truth"
        );
        assert_eq!(resolved.consecutive_fallbacks, 2);
    }

    #[test]
    fn an_empty_answer_that_survives_the_bound_becomes_the_answer() {
        let resolved = resolve_flag(
            "protected",
            Ok(HashSet::new()),
            flag(&["voteOne"], MAX_FLAG_FALLBACK_CYCLES - 1),
        );

        assert!(
            resolved.vote_accounts.is_empty(),
            "upstream saying nobody is protected has to reach the records eventually"
        );
        assert_eq!(resolved.consecutive_fallbacks, 0);
        assert!(resolved.last_success.is_some());
    }

    #[test]
    fn an_empty_answer_on_a_cold_start_is_taken_at_face_value() {
        let resolved = resolve_flag("protected", Ok(HashSet::new()), CachedFlag::default());

        assert!(
            resolved.vote_accounts.is_empty(),
            "there is no list to protect, and refusing would strand every cached endpoint"
        );
        assert!(resolved.last_success.is_some());
    }

    #[test]
    fn a_fetch_failure_reuses_the_last_known_list_and_keeps_its_fetch_time() {
        let fetched_at = SystemTime::now();
        let resolved = resolve_flag(
            "protected",
            Err(anyhow::anyhow!("bonds api down")),
            CachedFlag {
                last_success: Some(fetched_at),
                ..flag(&["voteOne"], 1)
            },
        );

        assert_eq!(resolved.vote_accounts, flag(&["voteOne"], 0).vote_accounts);
        assert_eq!(resolved.consecutive_fallbacks, 2);
        assert_eq!(
            resolved.last_success,
            Some(fetched_at),
            "a reused list has to keep reporting when upstream last answered"
        );
    }

    #[test]
    fn a_fetch_failure_with_nothing_to_reuse_reports_unknown() {
        let resolved = resolve_flag(
            "protected",
            Err(anyhow::anyhow!("bonds api down")),
            CachedFlag::default(),
        );

        assert!(resolved.vote_accounts.is_empty());
        assert!(
            resolved.last_success.is_none(),
            "a cold process has no list to fall back to, and must not claim upstream answered"
        );
    }

    #[test]
    fn the_fallback_is_bounded() {
        let resolved = resolve_flag(
            "protected",
            Err(anyhow::anyhow!("bonds api down")),
            flag(&["voteOne"], MAX_FLAG_FALLBACK_CYCLES - 1),
        );

        assert!(
            resolved.vote_accounts.is_empty(),
            "a permanently broken upstream has to surface instead of freezing the list"
        );
        assert!(
            resolved.last_success.is_none(),
            "dropping the list without dropping its fetch time would read as a fresh empty list"
        );
    }

    fn net_apy(values: &[(&str, f64)], fetched_at: Option<SystemTime>) -> CachedNetApy {
        CachedNetApy {
            values: values
                .iter()
                .map(|(vote_account, apy)| (vote_account.to_string(), *apy))
                .collect(),
            last_success: fetched_at,
        }
    }

    #[test]
    fn a_successful_net_apy_answer_replaces_the_values_and_stamps_the_fetch_time() {
        let resolved = resolve_net_apy(
            Ok(HashMap::from([("voteTwo".to_string(), 0.09)])),
            net_apy(&[("voteOne", 0.07)], Some(SystemTime::now())),
        );

        assert_eq!(
            resolved.values,
            HashMap::from([("voteTwo".to_string(), 0.09)])
        );
        assert!(
            resolved.last_success.is_some(),
            "consumers read this to tell a reused map from a fresh one"
        );
    }

    #[test]
    fn an_empty_net_apy_answer_reuses_the_last_known_values() {
        let fetched_at = SystemTime::now();
        let resolved = resolve_net_apy(
            Ok(HashMap::new()),
            net_apy(&[("voteOne", 0.07)], Some(fetched_at)),
        );

        assert_eq!(
            resolved.values,
            HashMap::from([("voteOne".to_string(), 0.07)]),
            "apy-api knowing nobody is more likely an upstream fault than the truth"
        );
        assert_eq!(
            resolved.last_success,
            Some(fetched_at),
            "a reused map has to keep reporting when upstream last answered"
        );
    }

    #[test]
    fn a_net_apy_fetch_failure_reuses_the_last_known_values_and_keeps_its_fetch_time() {
        let fetched_at = SystemTime::now();
        let resolved = resolve_net_apy(
            Err(anyhow::anyhow!("apy api down")),
            net_apy(&[("voteOne", 0.07)], Some(fetched_at)),
        );

        assert_eq!(
            resolved.values,
            HashMap::from([("voteOne".to_string(), 0.07)])
        );
        assert_eq!(resolved.last_success, Some(fetched_at));
    }

    #[test]
    fn a_net_apy_fetch_failure_on_a_cold_start_serves_no_values_instead_of_failing() {
        let resolved = resolve_net_apy(
            Err(anyhow::anyhow!("apy api down")),
            CachedNetApy::default(),
        );

        assert!(resolved.values.is_empty());
        assert_eq!(
            resolved.last_success, None,
            "a cold process must report null rather than a fetch time it never had"
        );
    }

    #[test]
    fn net_apy_values_that_outlived_the_reuse_bound_are_dropped() {
        let resolved = resolve_net_apy(
            Err(anyhow::anyhow!("apy api down")),
            net_apy(
                &[("voteOne", 0.07)],
                Some(SystemTime::now() - MAX_NET_APY_REUSE - Duration::from_secs(60)),
            ),
        );

        assert!(
            resolved.values.is_empty(),
            "a permanently broken apy-api has to stop looking like current data"
        );
        assert_eq!(
            resolved.last_success, None,
            "with nothing served there is no fetch time to report"
        );
    }

    #[test]
    fn net_apy_values_inside_the_reuse_bound_survive_the_outage() {
        let fetched_at = SystemTime::now() - MAX_NET_APY_REUSE + Duration::from_secs(60);
        let resolved = resolve_net_apy(
            Err(anyhow::anyhow!("apy api down")),
            net_apy(&[("voteOne", 0.07)], Some(fetched_at)),
        );

        assert_eq!(
            resolved.values,
            HashMap::from([("voteOne".to_string(), 0.07)]),
            "the rolling APY still describes the epoch it was computed for"
        );
        assert_eq!(resolved.last_success, Some(fetched_at));
    }
}
