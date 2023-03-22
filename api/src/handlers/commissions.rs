use crate::context::WrappedContext;
use crate::metrics;
use crate::utils::response_error;
use log::{error, info};
use serde::{Deserialize, Serialize};
use store::dto::CommissionRecord;
use warp::{http::StatusCode, reply::json, Reply};

#[derive(Serialize, Debug)]
pub struct Response {
    commissions: Vec<CommissionRecord>,
}

#[derive(Deserialize, Serialize, Debug)]
pub struct QueryParams {}

pub async fn handler(
    vote_account: String,
    _query_params: QueryParams,
    context: WrappedContext,
) -> Result<impl Reply, warp::Rejection> {
    info!("Fetching commissions {:?}", &vote_account);
    metrics::REQUEST_COUNT_COMMISSIONS.inc();

    let commissions = context.read().await.cache.get_commissions(&vote_account);

    Ok(match commissions {
        Some(commissions) => {
            warp::reply::with_status(json(&Response { commissions }), StatusCode::OK)
        }
        _ => {
            error!("No commissions found for {}", &vote_account);
            response_error(StatusCode::NOT_FOUND, "Failed to fetch records!".into())
        }
    })
}
