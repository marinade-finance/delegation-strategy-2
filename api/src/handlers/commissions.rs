use crate::context::WrappedContext;
use crate::utils::reponse_error_500;
use log::{error, info};
use serde::{Deserialize, Serialize};
use store::dto::CommissionRecord;
use warp::{http::StatusCode, reply::json, Reply};

const DEFAULT_EPOCHS: u8 = 15;

#[derive(Serialize, Debug)]
pub struct Response {
    commissions: Vec<CommissionRecord>,
}

#[derive(Deserialize, Serialize, Debug)]
pub struct QueryParams {}

pub async fn handler(
    identity: String,
    _query_params: QueryParams,
    context: WrappedContext,
) -> Result<impl Reply, warp::Rejection> {
    info!("Fetching commissions {:?}", &identity);

    let commissions =
        store::utils::load_commissions(&context.read().await.psql_client, identity, DEFAULT_EPOCHS)
            .await;

    Ok(match commissions {
        Ok(commissions) => {
            warp::reply::with_status(json(&Response { commissions }), StatusCode::OK)
        }
        Err(err) => {
            error!("Failed to fetch commission records: {}", err);
            reponse_error_500("Failed to fetch records!".into())
        }
    })
}
