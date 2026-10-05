use crate::context::{Context, WrappedContext};
use crate::handlers::{
    cluster_stats, commissions, config, docs, events, global_unstake_hints, glossary, health, jito,
    jito_mev, list_validators, readiness, reports_commission_changes, reports_scoring,
    reports_scoring_html, reports_staking, rewards, unstake_hints, uptimes,
    validator_score_breakdown, validator_score_breakdowns, validator_scores,
    validators_block_rewards, validators_flat, versions, workflow_metrics_upload,
};
use env_logger::Env;
use log::{error, info};
use std::convert::Infallible;
use std::sync::Arc;
use structopt::StructOpt;
use tokio::sync::RwLock;
use warp::{Filter, Rejection};

pub mod api_docs;
pub mod cache;
pub mod context;
pub mod handlers;
pub mod metrics;
pub mod utils;

#[derive(Debug, StructOpt)]
pub struct Params {
    #[structopt(long = "directory-url", env = "DIRECTORY_URL")]
    pub directory_url: String,

    #[structopt(long = "directory-token", env = "DIRECTORY_TOKEN")]
    pub directory_token: String,

    #[structopt(
        long = "validator-bonds-api-url",
        env = "VALIDATOR_BONDS_API_URL",
        default_value = "https://validator-bonds-api.marinade.finance"
    )]
    validator_bonds_api_url: String,

    #[structopt(
        long = "apy-api-url",
        env = "APY_API_URL",
        default_value = "https://apy.marinade.finance"
    )]
    apy_api_url: String,

    #[structopt(long = "glossary-path")]
    glossary_path: String,

    #[structopt(long = "blacklist-path")]
    blacklist_path: String,

    #[structopt(
        long = "blacklist-url",
        env = "BLACKLIST_URL",
        default_value = "https://raw.githubusercontent.com/marinade-finance/ds-sam-pipeline/main/blacklist.csv"
    )]
    blacklist_url: String,

    #[structopt(env = "ADMIN_AUTH_TOKEN", long = "admin-auth-token")]
    admin_auth_token: String,

    #[structopt(long = "port", default_value = "8000")]
    port: u16,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(Env::default().default_filter_or("info")).init();
    info!("Launching API");

    let params = Params::from_args();

    // Bounded so a hung upstream can stall neither startup nor a refresh.
    let http_client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(30))
        .timeout(std::time::Duration::from_secs(30))
        .build()?;

    // Scoring against a missing blacklist would blacklist nobody, so a failed
    // first fetch aborts startup.
    fetch_blacklist(&http_client, &params.blacklist_url, &params.blacklist_path)
        .await
        .map_err(|err| anyhow::anyhow!("Initial blacklist fetch failed: {err}"))?;
    {
        let client = http_client.clone();
        let url = params.blacklist_url.clone();
        let path = params.blacklist_path.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
                if let Err(err) = fetch_blacklist(&client, &url, &path).await {
                    error!("Blacklist refresh failed (keeping previous copy): {err}");
                }
            }
        });
    }

    let directory = store::directory::Directory::new(
        params.directory_url.clone(),
        params.directory_token.clone(),
    )?;

    let context = Arc::new(RwLock::new(Context::new(
        directory,
        params.glossary_path,
        params.blacklist_path,
        params.validator_bonds_api_url,
        params.apy_api_url,
    )?));
    let ready = cache::ReadyFlag::default();
    cache::spawn_cache_warmer(context.clone(), ready.clone());
    let cors = warp::cors()
        .allow_any_origin()
        .allow_headers(vec![
            "User-Agent",
            "Sec-Fetch-Mode",
            "Referer",
            "Content-Type",
            "Origin",
            "Access-Control-Request-Method",
            "Access-Control-Request-Headers",
        ])
        .allow_methods(vec!["POST", "GET"]);

    let top_level = warp::path::end()
        .and(warp::get())
        .map(|| "API for Delegation Strategy 2.0");

    let route_api_docs_oas = warp::path("docs.json")
        .and(warp::get())
        .map(|| warp::reply::json(&<crate::api_docs::ApiDoc as utoipa::OpenApi>::openapi()));

    let route_api_docs_html = warp::path("docs").and(warp::get()).and_then(docs::handler);

    // Dependency-free on purpose: a slow DB or upstream must never get the container killed.
    let route_liveness = warp::path!("healthz")
        .and(warp::path::end())
        .and(warp::get())
        .and_then(health::handler);

    // On the API port, not the metrics port: a reply also proves the Service's target port is bound.
    let route_readiness = warp::path!("readyz")
        .and(warp::path::end())
        .and(warp::get())
        .and(with_ready(ready))
        .and_then(readiness::handler);

    let route_validators = warp::path!("validators")
        .and(warp::path::end())
        .and(warp::get())
        .and(warp::query::<list_validators::QueryParams>())
        .and(with_context(context.clone()))
        .and_then(list_validators::handler);

    let route_validator_score_breakdown = warp::path!("validators" / "score-breakdown")
        .and(warp::path::end())
        .and(warp::get())
        .and(warp::query::<validator_score_breakdown::QueryParams>())
        .and(with_context(context.clone()))
        .and_then(validator_score_breakdown::handler);

    let route_validator_score_breakdowns = warp::path!("validators" / "score-breakdowns")
        .and(warp::path::end())
        .and(warp::get())
        .and(warp::query::<validator_score_breakdowns::QueryParams>())
        .and(with_context(context.clone()))
        .and_then(validator_score_breakdowns::handler);

    let route_validator_scores = warp::path!("validators" / "scores")
        .and(warp::path::end())
        .and(warp::get())
        .and(with_context(context.clone()))
        .and_then(validator_scores::handler);

    let route_validators_flat = warp::path!("validators" / "flat")
        .and(warp::path::end())
        .and(warp::get())
        .and(warp::query::<validators_flat::QueryParams>())
        .and(with_context(context.clone()))
        .and_then(validators_flat::handler);

    let route_validators_block_rewards = warp::path!("validators" / "block-rewards")
        .and(warp::path::end())
        .and(warp::get())
        .and(warp::query::<validators_block_rewards::QueryParamsLast>())
        .and(with_context(context.clone()))
        .and_then(validators_block_rewards::handler);

    let route_cluster_stats = warp::path!("cluster-stats")
        .and(warp::path::end())
        .and(warp::get())
        .and(warp::query::<cluster_stats::QueryParams>())
        .and(with_context(context.clone()))
        .and_then(cluster_stats::handler);

    let route_uptimes = warp::path!("validators" / String / "uptimes")
        .and(warp::path::end())
        .and(warp::get())
        .and(warp::query::<uptimes::QueryParams>())
        .and(with_context(context.clone()))
        .and_then(uptimes::handler);

    let route_events = warp::path!("validators" / String / "events")
        .and(warp::path::end())
        .and(warp::get())
        .and(warp::query::<events::QueryParams>())
        .and(with_context(context.clone()))
        .and_then(events::handler);

    let route_versions = warp::path!("validators" / String / "versions")
        .and(warp::path::end())
        .and(warp::get())
        .and(warp::query::<versions::QueryParams>())
        .and(with_context(context.clone()))
        .and_then(versions::handler);

    let route_commissions = warp::path!("validators" / String / "commissions")
        .and(warp::path::end())
        .and(warp::get())
        .and(warp::query::<commissions::QueryParams>())
        .and(with_context(context.clone()))
        .and_then(commissions::handler);

    let route_glossary = warp::path!("static" / "glossary.md")
        .and(warp::path::end())
        .and(warp::get())
        .and(with_context(context.clone()))
        .and_then(glossary::handler);

    let route_config = warp::path!("static" / "config")
        .and(warp::path::end())
        .and(warp::get())
        .and(with_context(context.clone()))
        .and_then(config::handler);

    let route_reports_commission_changes = warp::path!("reports" / "commission-changes")
        .and(warp::path::end())
        .and(warp::get())
        .and(with_context(context.clone()))
        .and_then(reports_commission_changes::handler);

    let route_reports_scoring = warp::path!("reports" / "scoring")
        .and(warp::path::end())
        .and(warp::get())
        .and(with_context(context.clone()))
        .and_then(reports_scoring::handler);

    let route_reports_scoring_html = warp::path!("reports" / "scoring" / String)
        .and(warp::path::end())
        .and(warp::get())
        .and(with_context(context.clone()))
        .and_then(reports_scoring_html::handler);

    let route_reports_staking = warp::path!("reports" / "staking")
        .and(warp::path::end())
        .and(warp::get())
        .and(with_context(context.clone()))
        .and_then(reports_staking::handler);

    let route_rewards = warp::path!("rewards")
        .and(warp::path::end())
        .and(warp::get())
        .and(warp::query::<rewards::QueryParams>())
        .and(with_context(context.clone()))
        .and_then(rewards::handler);

    let route_jito_mev = warp::path!("mev")
        .and(warp::path::end())
        .and(warp::get())
        .and(warp::query::<jito_mev::QueryParams>())
        .and(with_context(context.clone()))
        .and_then(jito_mev::handler);

    let route_jito_priority_fee = warp::path!("jito")
        .and(warp::path::end())
        .and(warp::get())
        .and(warp::query::<jito::QueryParams>())
        .and(with_context(context.clone()))
        .and_then(jito::handler);

    let route_unstake_hints = warp::path!("unstake-hints")
        .and(warp::path::end())
        .and(warp::get())
        .and(warp::query::<unstake_hints::QueryParams>())
        .and(with_context(context.clone()))
        .and_then(unstake_hints::handler);

    let route_global_unstake_hints = warp::path!("global-unstake-hints")
        .and(warp::path::end())
        .and(warp::get())
        .and(warp::query::<global_unstake_hints::QueryParams>())
        .and(with_context(context.clone()))
        .and_then(global_unstake_hints::handler);

    let route_workflow_metrics_upload = warp::path!("admin" / "metrics")
        .and(warp::path::end())
        .and(warp::post())
        .and(with_admin_auth(params.admin_auth_token.clone()))
        .and(warp::query::<workflow_metrics_upload::QueryParams>())
        .and_then(workflow_metrics_upload::handler);

    let routes = top_level
        .or(route_api_docs_oas)
        .or(route_api_docs_html)
        .or(route_liveness)
        .or(route_readiness)
        .or(route_cluster_stats)
        .or(route_validators)
        .or(route_validator_score_breakdown)
        .or(route_validator_score_breakdowns)
        .or(route_validator_scores)
        .or(route_validators_flat)
        .or(route_validators_block_rewards)
        .or(route_uptimes)
        .or(route_events)
        .or(route_versions)
        .or(route_commissions)
        .or(route_glossary)
        .or(route_jito_mev)
        .or(route_jito_priority_fee)
        .or(route_config)
        .or(route_reports_scoring)
        .or(route_reports_scoring_html)
        .or(route_reports_staking)
        .or(route_rewards)
        .or(route_unstake_hints)
        .or(route_global_unstake_hints)
        .or(route_reports_commission_changes)
        .or(route_workflow_metrics_upload)
        .with(cors);

    metrics::spawn_server();

    warp::serve(routes).run(([0, 0, 0, 0], params.port)).await;

    Ok(())
}

// A valid blacklist is far above this floor; a body below it is empty, HTML or
// truncated and must not overwrite the last good copy.
const MIN_BLACKLIST_ROWS: usize = 50;

// Validated before the temp + rename, so a 200 carrying a bad payload leaves the
// previous file untouched and a reader never sees a half file.
async fn fetch_blacklist(client: &reqwest::Client, url: &str, path: &str) -> anyhow::Result<()> {
    let body = client
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;

    let mut lines = body.lines();
    if lines.next() != Some("vote_account,code") {
        anyhow::bail!("blacklist header missing/invalid (expected 'vote_account,code')");
    }
    let rows = lines.filter(|l| !l.trim().is_empty()).count();
    if rows < MIN_BLACKLIST_ROWS {
        anyhow::bail!("blacklist has only {rows} rows (minimum {MIN_BLACKLIST_ROWS})");
    }

    let tmp = format!("{path}.tmp");
    tokio::fs::write(&tmp, body.as_bytes()).await?;
    tokio::fs::rename(&tmp, path).await?;
    info!(
        "Fetched blacklist from {url} -> {path} ({} bytes, {rows} rows)",
        body.len()
    );
    Ok(())
}

fn with_context(
    context: WrappedContext,
) -> impl Filter<Extract = (WrappedContext,), Error = Infallible> + Clone {
    warp::any().map(move || context.clone())
}

fn with_ready(
    ready: cache::ReadyFlag,
) -> impl Filter<Extract = (cache::ReadyFlag,), Error = Infallible> + Clone {
    warp::any().map(move || ready.clone())
}

fn with_admin_auth(
    expected_token: String,
) -> impl Filter<Extract = (bool,), Error = Rejection> + Clone {
    warp::header::<String>("authorization")
        .map(move |token: String| token == expected_token.clone())
}
