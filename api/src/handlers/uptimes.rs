use crate::context::WrappedContext;
use crate::utils::reponse_error_500;
use log::{error, info};
use serde::{Deserialize, Serialize};
use store::dto::UptimeRecord;
use warp::{http::StatusCode, reply::json, Reply};

const DEFAULT_EPOCHS: u8 = 15;

#[derive(Serialize, Debug)]
pub struct Response {
    uptimes: Vec<UptimeRecord>,
}

#[derive(Deserialize, Serialize, Debug)]
enum OrderField {
    Stake,
    OtherField,
}

#[derive(Deserialize, Serialize, Debug)]
enum OrderDirection {
    ASC,
    DESC,
}

#[derive(Deserialize, Serialize, Debug)]
pub struct QueryParams {}

pub async fn handler(
    identity: String,
    _query_params: QueryParams,
    context: WrappedContext,
) -> Result<impl Reply, warp::Rejection> {
    info!("Fetching uptimes {:?}", &identity);

    let uptimes =
        store::utils::load_uptimes(&context.read().await.psql_client, identity, DEFAULT_EPOCHS)
            .await;

    Ok(match uptimes {
        Ok(uptimes) => warp::reply::with_status(json(&Response { uptimes }), StatusCode::OK),
        Err(err) => {
            error!("Failed to fetch uptime records: {}", err);
            reponse_error_500("Failed to fetch records!".into())
        }
    })
}
