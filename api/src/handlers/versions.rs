use crate::context::WrappedContext;
use crate::utils::reponse_error_500;
use log::{error, info};
use serde::{Deserialize, Serialize};
use store::dto::VersionRecord;
use warp::{http::StatusCode, reply::json, Reply};

const DEFAULT_EPOCHS: u8 = 15;

#[derive(Serialize, Debug)]
pub struct Response {
    versions: Vec<VersionRecord>,
}

#[derive(Deserialize, Serialize, Debug)]
pub struct QueryParams {}

pub async fn handler(
    identity: String,
    _query_params: QueryParams,
    context: WrappedContext,
) -> Result<impl Reply, warp::Rejection> {
    info!("Fetching versions {:?}", &identity);

    let versions =
        store::utils::load_versions(&context.read().await.psql_client, identity, DEFAULT_EPOCHS)
            .await;

    Ok(match versions {
        Ok(versions) => warp::reply::with_status(json(&Response { versions }), StatusCode::OK),
        Err(err) => {
            error!("Failed to fetch version records: {}", err);
            reponse_error_500("Failed to fetch records!".into())
        }
    })
}
