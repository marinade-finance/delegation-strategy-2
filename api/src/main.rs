use crate::context::{Context, WrappedContext};
use crate::handlers::{
    admin_score_upload, cluster_stats, commissions, config, glossary, list_validators,
    reports_commission_changes, reports_scoring, reports_staking, uptimes, validators_flat,
    versions,
};
use env_logger::Env;
use log::{error, info};
use std::convert::Infallible;
use std::sync::Arc;
use structopt::StructOpt;
use tokio::sync::RwLock;
use tokio_postgres::NoTls;
use warp::Filter;

pub mod cache;
pub mod context;
pub mod handlers;
pub mod metrics;
pub mod utils;

#[derive(Debug, StructOpt)]
pub struct Params {
    #[structopt(long = "postgres-url")]
    postgres_url: String,

    #[structopt(long = "glossary-path")]
    glossary_path: String,
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

    let context = Arc::new(RwLock::new(Context::new(
        psql_client,
        params.glossary_path,
    )?));
    cache::spawn_cache_warmer(context.clone());

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

    let route_validators = warp::path!("validators")
        .and(warp::path::end())
        .and(warp::get())
        .and(warp::query::<list_validators::QueryParams>())
        .and(with_context(context.clone()))
        .and_then(list_validators::handler);

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

    let route_reports_staking = warp::path!("reports" / "staking")
        .and(warp::path::end())
        .and(warp::get())
        .and(with_context(context.clone()))
        .and_then(reports_staking::handler);

    let route_admin_upload_score = warp::path!("admin" / "scores")
        .and(warp::path::end())
        .and(warp::post())
        .and(warp::query::<admin_score_upload::QueryParams>())
        .and(warp::multipart::form().max_length(5_000_000))
        .and(with_context(context.clone()))
        .and_then(admin_score_upload::handler);

    let routes = top_level
        .or(route_cluster_stats)
        .or(route_validators)
        .or(route_validators_flat)
        .or(route_uptimes)
        .or(route_versions)
        .or(route_commissions)
        .or(route_glossary)
        .or(route_config)
        .or(route_reports_scoring)
        .or(route_reports_staking)
        .or(route_reports_commission_changes)
        .or(route_admin_upload_score)
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
