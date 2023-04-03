use crate::context::WrappedContext;
use crate::metrics;
use crate::utils::response_error;
use log::error;
use serde::{Deserialize, Serialize};
use store::dto::ClusterStats;
use warp::{http::StatusCode, reply::json, Reply};

const DEFAULT_EPOCHS: usize = 15;

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct ResponseClusterStats {
    cluster_stats: ClusterStats,
}

#[derive(Deserialize, Serialize, Debug)]
pub struct QueryParams {
    epochs: Option<usize>,
}

#[utoipa::path(
    get,
    tag = "General",
    operation_id = "Show cluster stats",
    path = "/cluster-stats",
    responses(
        (status = 200, body = ResponseClusterStats)
    )
)]
pub async fn handler(
    query_params: QueryParams,
    context: WrappedContext,
) -> Result<impl Reply, warp::Rejection> {
    metrics::REQUEST_CLUSTER_STATS.inc();

    log::info!("Query cluster stats {:?}", query_params);

    let cluster_stats = context
        .read()
        .await
        .cache
        .get_cluster_stats(query_params.epochs.unwrap_or(DEFAULT_EPOCHS));

    Ok(match cluster_stats {
        Some(cluster_stats) => {
            warp::reply::with_status(json(&ResponseClusterStats { cluster_stats }), StatusCode::OK)
        }
        _ => {
            error!("No cluster stats found");
            response_error(StatusCode::NOT_FOUND, "Failed to fetch records!".into())
        }
    })
}
