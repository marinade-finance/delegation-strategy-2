use crate::context::WrappedContext;
use crate::metrics;
use crate::utils::response_error_500;
use log::error;
use serde::{Deserialize, Serialize};
use warp::Reply;

const DEFAULT_EPOCHS: u64 = 10;

#[derive(Deserialize, Serialize, Debug)]
pub struct QueryParams {
    epochs: Option<u64>,
    last_epoch: u64,
}

pub async fn handler(
    query_params: QueryParams,
    context: WrappedContext,
) -> Result<impl Reply, warp::Rejection> {
    metrics::REQUEST_COUNT_VALIDATORS_FLAT.inc();

    log::info!("Query flat validators {:?}", query_params);

    let epochs = query_params.epochs.unwrap_or(DEFAULT_EPOCHS);
    let validators = store::utils::load_validators_aggregated_flat(
        &context.read().await.psql_client,
        query_params.last_epoch,
        epochs,
    )
    .await;

    let validators = match validators {
        Ok(validators) => validators,
        Err(err) => {
            error!("Failed to fetch validator records: {}", err);
            return Ok(response_error_500("Failed to fetch records!".into()).into_response());
        }
    };

    let mut csv_content = csv::Writer::from_writer(Vec::new());
    for validator in validators {
        csv_content.serialize(validator);
    }

    Ok(warp::reply::with_header(
        String::from_utf8(csv_content.into_inner().unwrap()).unwrap(),
        "Content-Type",
        "text/csv",
    )
    .into_response())
}
