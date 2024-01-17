use crate::context::WrappedContext;
use crate::utils::response_error;
use log::{error, info};
use serde::{Deserialize, Serialize};
use store::rewards::{get_estimated_inflation_rewards, get_mev_rewards};
use warp::{http::StatusCode, reply::json, Reply};

const DEFAULT_EPOCHS: u64 = 20;

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct ResponseRewards {
    rewards_mev: Vec<(u64, f64)>,
    rewards_inflation_est: Vec<(u64, f64)>,
}

#[derive(Deserialize, Serialize, Debug)]
pub struct QueryParams {
    epochs: Option<u64>,
}

#[utoipa::path(
    get,
    tag = "Rewards",
    operation_id = "List rewards",
    path = "/rewards",
    responses(
        (status = 200, body = ResponseRewards)
    )
)]
pub async fn handler(
    query_params: QueryParams,
    context: WrappedContext,
) -> Result<impl Reply, warp::Rejection> {
    let epochs = query_params.epochs.unwrap_or(DEFAULT_EPOCHS);
    info!("Fetching rewards for past {:?}", epochs);

    let rewards_inflation_est =
        match get_estimated_inflation_rewards(&context.read().await.psql_client, epochs).await {
            Ok(r) => r,
            Err(err) => {
                error!("Failed to fetch inflation rewards: {}", err);
                return Ok(response_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Failed to fetch inflation rewards!".into(),
                ));
            }
        };
    let rewards_mev = match get_mev_rewards(&context.read().await.psql_client, epochs).await {
        Ok(r) => r,
        Err(err) => {
            error!("Failed to fetch MEV rewards: {}", err);
            return Ok(response_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to fetch MEV rewards!".into(),
            ));
        }
    };

    Ok(warp::reply::with_status(
        json(&ResponseRewards {
            rewards_mev,
            rewards_inflation_est,
        }),
        StatusCode::OK,
    ))
}
