use crate::context::WrappedContext;
use crate::metrics;
use crate::utils::reponse_error;
use log::{error, info};
use serde::{Deserialize, Serialize};
use store::dto::CommissionRecord;
use warp::{http::StatusCode, reply::json, Reply};

#[derive(Serialize, Debug)]
pub struct Response {
    commission_changes: String,
}

pub async fn handler(context: WrappedContext) -> Result<impl Reply, warp::Rejection> {
    // info!("Fetching commission changes");
    // let commission_changes = context.read().await.cache.get_all_commissions();
    //
    // Ok(match commissions {
    //     Some(commissions) => warp::reply::with_status(
    //         json(&Response {
    //             commission_changes: "foo".to_string(),
    //         }),
    //         StatusCode::OK,
    //     ),
    //     _ => {
    //         error!("No commissions found for {}", &identity);
    //         reponse_error(StatusCode::NOT_FOUND, "Failed to fetch records!".into())
    //     }
    // })
    Ok(reponse_error(
        StatusCode::INTERNAL_SERVER_ERROR,
        "Endpoint is not ready yet!".into(),
    ))
}
