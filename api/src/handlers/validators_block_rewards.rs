use crate::context::WrappedContext;
use crate::utils::response_error;
use log::{error, info};
use serde::{Deserialize, Serialize};
use store::dto::ValidatorBlockRewardsRecord;
use store::validators_block_rewards::{get_last_block_rewards, VALIDATORS_BLOCK_REWARDS_TABLE};
use warp::{http::StatusCode, reply::json, Reply};

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct ResponseValidatorsBlockRewards {
    validators: Vec<ValidatorBlockRewardsRecord>,
}

#[derive(Deserialize, Serialize, Debug)]
pub struct QueryParamsLast {}

const DEFAULT_EPOCHS: u64 = 10;

#[utoipa::path(
    get,
    tag = "Validators Block Rewards",
    operation_id = "List last validators block rewards",
    path = "/validators/block-rewards",
    responses(
        (status = 200, body = ResponseValidatorsBlockRewards)
    )
)]
pub async fn handler(
    _: QueryParamsLast,
    context: WrappedContext,
) -> Result<impl Reply, warp::Rejection> {
    info!("Fetching last validators block rewards");

    let validators = match get_last_block_rewards(
        &context.read().await.psql_client,
        DEFAULT_EPOCHS,
        VALIDATORS_BLOCK_REWARDS_TABLE,
    )
    .await
    {
        Ok(r) => r,
        Err(err) => {
            error!("Failed to fetch validators block rewards: {err}");
            return Ok(response_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "Failed to fetch validators block rewards for last {DEFAULT_EPOCHS} epochs!"
                ),
            ));
        }
    };

    Ok(warp::reply::with_status(
        json(&ResponseValidatorsBlockRewards { validators }),
        StatusCode::OK,
    ))
}
