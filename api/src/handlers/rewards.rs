use crate::context::WrappedContext;
use crate::utils::response_error;
use log::{error, info};
use serde::{Deserialize, Serialize};
use store::rewards::{
    get_block_rewards, get_estimated_inflation_rewards, get_jito_priority_rewards, get_mev_rewards,
};
use warp::{http::StatusCode, reply::json, Reply};

const DEFAULT_EPOCHS: u64 = 20;

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct ResponseRewards {
    rewards_mev: Vec<(u64, f64)>,
    rewards_inflation_est: Vec<(u64, f64)>,
    rewards_jito_priority: Vec<(u64, f64)>,
    /// block production rewards based on signature count and priority fees
    rewards_block: Vec<(u64, f64)>,
}

#[derive(Deserialize, Serialize, Debug, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct QueryParams {
    epochs: Option<u64>,
}

#[utoipa::path(
    get,
    tag = "Rewards",
    operation_id = "List rewards",
    path = "/rewards",
    params(QueryParams),
    responses(
        (status = 200, body = ResponseRewards)
    )
)]
pub async fn handler(
    query_params: QueryParams,
    context: WrappedContext,
) -> Result<impl Reply, warp::Rejection> {
    let epochs = query_params.epochs.unwrap_or(DEFAULT_EPOCHS);
    info!("Fetching rewards for past {epochs:?}");

    let context_guard = context.read().await;
    let psql_client = &context_guard.psql_client;
    let (inflation_result, mev_result, jito_result, block_result) = tokio::join!(
        get_estimated_inflation_rewards(psql_client, epochs),
        get_mev_rewards(psql_client, epochs),
        get_jito_priority_rewards(psql_client, epochs),
        get_block_rewards(psql_client, epochs),
    );

    let rewards_inflation_est = match inflation_result {
        Ok(r) => r,
        Err(err) => {
            error!("Failed to fetch inflation rewards: {err}");
            return Ok(response_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to fetch inflation rewards!".into(),
            ));
        }
    };
    let rewards_mev = match mev_result {
        Ok(r) => r,
        Err(err) => {
            error!("Failed to fetch MEV rewards: {err}");
            return Ok(response_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to fetch MEV rewards!".into(),
            ));
        }
    };
    let rewards_jito_priority = match jito_result {
        Ok(r) => r,
        Err(err) => {
            error!("Failed to fetch Jito Priority rewards: {err}");
            return Ok(response_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to fetch Jito Priority rewards!".into(),
            ));
        }
    };

    let rewards_block = match block_result {
        Ok(r) => r,
        Err(err) => {
            error!("Failed to fetch Block rewards: {err}");
            return Ok(response_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to fetch Block rewards!".into(),
            ));
        }
    };

    Ok(warp::reply::with_status(
        json(&ResponseRewards {
            rewards_mev,
            rewards_inflation_est,
            rewards_jito_priority,
            rewards_block,
        }),
        StatusCode::OK,
    ))
}
