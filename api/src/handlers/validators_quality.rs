use crate::{context::WrappedContext, metrics, utils::response::response_error_500};
use log::{error, info};
use serde::{Deserialize, Serialize};
use store::validator_quality::ValidatorQualityRecord;
use warp::{http::StatusCode, reply::json, Reply};

#[derive(Serialize, utoipa::ToSchema)]
pub struct ResponseValidatorsQuality {
    epoch: u64,
    validators: Vec<ValidatorQualityRecord>,
}

#[derive(Deserialize, Serialize, Debug, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct QueryParams {
    epoch: u64,
}

#[utoipa::path(
    get,
    tag = "Validators",
    operation_id = "List validator quality data",
    path = "/validators/quality",
    params(QueryParams),
    responses(
        (status = 200, body = ResponseValidatorsQuality)
    )
)]
pub async fn handler(
    query_params: QueryParams,
    context: WrappedContext,
) -> Result<impl Reply, warp::Rejection> {
    info!("Fetching validator quality {:?}", query_params.epoch);
    metrics::REQUEST_VALIDATORS_QUALITY.inc();

    let validators = store::validator_quality::load_validator_quality(
        &context.read().await.psql_client,
        query_params.epoch,
    )
    .await;

    Ok(match validators {
        Ok(validators) => warp::reply::with_status(
            json(&ResponseValidatorsQuality {
                epoch: query_params.epoch,
                validators,
            }),
            StatusCode::OK,
        ),
        Err(err) => {
            error!("Failed to load validator quality: {err}");
            response_error_500("Failed to load validator quality!".into())
        }
    })
}
