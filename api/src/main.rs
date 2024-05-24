use crate::context::{Context, WrappedContext};
use crate::handlers::{
    admin_score_upload, cluster_stats, commissions, config, docs, global_unstake_hints, glossary, list_validators, mev, reports_commission_changes, reports_scoring, reports_scoring_html, reports_staking, rewards, unstake_hints, uptimes, validator_score_breakdown, validator_score_breakdowns, validator_scores, validators_flat, versions, workflow_metrics_upload
};
use env_logger::Env;
use log::{error, info};
use rslock::LockManager;
use std::convert::Infallible;
use std::sync::Arc;
use structopt::StructOpt;
use tokio::sync::RwLock;
use tokio_postgres::NoTls;
use warp::{Filter, Rejection};

pub mod api_docs;
pub mod cache;
pub mod context;
pub mod handlers;
pub mod metrics;
pub mod redis_cache;
pub mod utils;

#[derive(Debug, StructOpt)]
pub struct Params {
    #[structopt(long = "postgres-url")]
    postgres_url: String,

    #[structopt(long = "redis-url")]
    redis_url: String,

    #[structopt(long = "redis-tag")]
    redis_tag: String,

    #[structopt(long = "scoring-url")]
    scoring_url: String,

    #[structopt(long = "glossary-path")]
    glossary_path: String,

    #[structopt(long = "blacklist-path")]
    blacklist_path: String,

    #[structopt(env = "ADMIN_AUTH_TOKEN", long = "admin-auth-token")]
    admin_auth_token: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(Env::default().default_filter_or("info")).init();
    info!("Launching API");

    let params = Params::from_args();
    let (psql_client, psql_conn) = tokio_postgres::connect(&params.postgres_url, NoTls).await?;
    tokio::spawn(async move {
        if let Err(err) = psql_conn.await {
            error!("PSQL Connection error: {}", err);
            std::process::exit(1);
        }
    });

    let redis_client = redis::Client::open(params.redis_url.clone())?;
    let redis_locker = LockManager::new(vec![params.redis_url.clone()]);

    if let Err(err) = redis_client.get_async_connection().await {
        error!("Redis Connection error: {}", err);
        std::process::exit(1);
    }
    let redis_client = Arc::new(RwLock::new(redis_client));
    let context = Arc::new(RwLock::new(Context::new(
        psql_client,
        params.glossary_path,
        params.blacklist_path,
    )?));
    redis_cache::spawn_redis_warmer(
        context.clone(),
        redis_client.clone(),
        redis_locker,
        params.redis_tag.clone(),
        params.scoring_url.clone()
    );
    cache::spawn_cache_warmer(
        context.clone(),
        redis_client.clone(),
        params.redis_tag.clone(),
    );

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

    let route_mev = warp::path!("mev")
        .and(warp::path::end())
        .and(warp::get())
        .and(warp::query::<mev::QueryParams>())
        .and(with_context(context.clone()))
        .and_then(mev::handler);

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

    let route_admin_upload_score = warp::path!("admin" / "scores")
        .and(warp::path::end())
        .and(warp::post())
        .and(with_admin_auth(params.admin_auth_token.clone()))
        .and(warp::query::<admin_score_upload::QueryParams>())
        .and(warp::multipart::form().max_length(5_000_000))
        .and(with_context(context.clone()))
        .and_then(admin_score_upload::handler);

    let route_workflow_metrics_upload = warp::path!("admin" / "metrics")
        .and(warp::path::end())
        .and(warp::post())
        .and(with_admin_auth(params.admin_auth_token.clone()))
        .and(warp::query::<workflow_metrics_upload::QueryParams>())
        .and_then(workflow_metrics_upload::handler);

    let routes = top_level
        .or(route_api_docs_oas)
        .or(route_api_docs_html)
        .or(route_cluster_stats)
        .or(route_validators)
        .or(route_validator_score_breakdown)
        .or(route_validator_score_breakdowns)
        .or(route_validator_scores)
        .or(route_validators_flat)
        .or(route_uptimes)
        .or(route_versions)
        .or(route_commissions)
        .or(route_glossary)
        .or(route_mev)
        .or(route_config)
        .or(route_reports_scoring)
        .or(route_reports_scoring_html)
        .or(route_reports_staking)
        .or(route_rewards)
        .or(route_unstake_hints)
        .or(route_global_unstake_hints)
        .or(route_reports_commission_changes)
        .or(route_admin_upload_score)
        .or(route_workflow_metrics_upload)
        .with(cors);

    metrics::spawn_server();

    warp::serve(routes).run(([0, 0, 0, 0], 8000)).await;

    Ok(())
}

fn with_context(
    context: WrappedContext,
) -> impl Filter<Extract = (WrappedContext,), Error = Infallible> + Clone {
    warp::any().map(move || context.clone())
}

fn with_admin_auth(
    expected_token: String,
) -> impl Filter<Extract = (bool,), Error = Rejection> + Clone {
    warp::header::<String>("authorization")
        .map(move |token: String| token == expected_token.clone())
}
