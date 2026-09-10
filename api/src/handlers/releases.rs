use crate::context::WrappedContext;
use crate::metrics;
use crate::utils::response::response_error;
use log::{error, info};
use serde::{Deserialize, Serialize};
use store::dto::ReleaseRecord;
use store::feature_gates::{self, FeatureGateFloor};
use store::releases::load_releases;
use warp::{http::StatusCode, reply::json, Reply};

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct ResponseReleases {
    releases: Vec<ReleaseRecord>,
    /// Not filtered by `client` or `since_epoch`.
    feature_gate_floors: &'static [FeatureGateFloor],
}

#[derive(Deserialize, Serialize, Debug, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct QueryParams {
    /// Client lineage, e.g. `agave` or `frankendancer`. An unknown one serves an empty list.
    client: Option<String>,
    /// Lower-bound epoch, inclusive. Matches on either `available_epoch` or `sfdp_floor_epoch`.
    since_epoch: Option<u64>,
}

#[utoipa::path(
    get,
    tag = "Validators",
    operation_id = "List client releases",
    description = "Mainnet client releases, one row per (client_lineage, client_version): `available_epoch` is the epoch the release was published in, `sfdp_floor_epoch` the epoch it became the minimum the Solana Foundation Delegation Program required.",
    path = "/releases",
    params(QueryParams),
    responses(
        (status = 200, body = ResponseReleases),
        (status = 500, description = "Failed to fetch records")
    )
)]
pub async fn handler(
    query_params: QueryParams,
    context: WrappedContext,
) -> Result<impl Reply, warp::Rejection> {
    metrics::REQUEST_COUNT_RELEASES.inc();
    info!("Fetching releases {query_params:?}");

    let ctx = context.read().await;

    let releases = match load_releases(
        &ctx.psql_client,
        query_params.client.as_deref(),
        query_params.since_epoch,
    )
    .await
    {
        Ok(releases) => releases,
        Err(err) => {
            error!("Failed to fetch releases: {err}");
            return Ok(response_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to fetch records!".into(),
            ));
        }
    };

    Ok(warp::reply::with_status(
        json(&ResponseReleases {
            releases,
            feature_gate_floors: feature_gates::all(),
        }),
        StatusCode::OK,
    ))
}
