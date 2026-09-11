use crate::context::WrappedContext;
use crate::metrics;
use crate::utils::order::{directed, OrderDirection, DEFAULT_ORDER_DIRECTION};
use crate::utils::response::response_error;
use log::{error, info};
use serde::{Deserialize, Serialize};
use store::dto::{FeatureGateFloor, ReleaseRecord, SfdpFloor};
use store::releases::{load_feature_gate_floors, load_releases, load_sfdp_floors};
use warp::{http::StatusCode, reply::json, Reply};

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct ResponseReleases {
    releases: Vec<ReleaseRecord>,
    sfdp_floors: Vec<SfdpFloor>,
    feature_gate_floors: Vec<FeatureGateFloor>,
}

#[derive(Deserialize, Serialize, Debug, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct QueryParams {
    /// Client lineage, e.g. `agave` or `frankendancer`. An unknown one serves an empty list.
    client: Option<String>,
    /// Lower-bound epoch, inclusive. Matches on either `available_epoch` or `sfdp_floor_epoch`.
    since_epoch: Option<u64>,
    /// Orders `releases` by publish time and both floor lists by the epoch they took effect.
    order_direction: Option<OrderDirection>,
}

#[utoipa::path(
    get,
    tag = "Validators",
    operation_id = "List client releases",
    description = "Mainnet client releases and the two version floors, as three lists: what was published (`releases`), what the Solana Foundation Delegation Program required (`sfdp_floors`), and what the cluster's feature gates required (`feature_gate_floors`).",
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

    let client = query_params.client.as_deref();
    let since_epoch = query_params.since_epoch;
    let order_direction = query_params
        .order_direction
        .unwrap_or(DEFAULT_ORDER_DIRECTION);

    let (mut releases, mut sfdp_floors, mut feature_gate_floors) = match tokio::try_join!(
        load_releases(&ctx.psql_client, client, since_epoch),
        load_sfdp_floors(&ctx.psql_client, client, since_epoch),
        load_feature_gate_floors(&ctx.psql_client, client, since_epoch),
    ) {
        Ok(fetched) => fetched,
        Err(err) => {
            error!("Failed to fetch releases: {err}");
            return Ok(response_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to fetch records!".into(),
            ));
        }
    };

    releases.sort_by(|a, b| directed(a.released_at.cmp(&b.released_at), &order_direction));
    sfdp_floors
        .sort_by(|a, b| directed(a.effective_epoch.cmp(&b.effective_epoch), &order_direction));
    feature_gate_floors
        .sort_by(|a, b| directed(a.effective_epoch.cmp(&b.effective_epoch), &order_direction));

    Ok(warp::reply::with_status(
        json(&ResponseReleases {
            releases,
            sfdp_floors,
            feature_gate_floors,
        }),
        StatusCode::OK,
    ))
}
