use crate::context::WrappedContext;
use crate::metrics;
use crate::utils::response_error;
use log::{error, info};
use serde::{Deserialize, Serialize};
use store::dto::VersionRecord;
use warp::{http::StatusCode, reply::json, Reply};

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct ResponseVersions {
    versions: Vec<VersionRecord>,
}

#[derive(Deserialize, Serialize, Debug)]
pub struct QueryParams {}

#[utoipa::path(
    get,
    tag = "Validators",
    operation_id = "List versions of a validator",
    path = "/validators/{vote_account}/versions",
    params(
        ("vote_account" = String, Path, description = "Vote account or identity of the validator")
    ),
    responses(
        (status = 200, body = ResponseVersions)
    )
)]
pub async fn handler(
    vote_account: String,
    _query_params: QueryParams,
    context: WrappedContext,
) -> Result<impl Reply, warp::Rejection> {
    info!("Fetching versions {:?}", &vote_account);
    metrics::REQUEST_COUNT_VERSIONS.inc();

    let validators = context.read().await.cache.get_validators();
    let validator = validators.iter().find(|(_vote_key, record)| {
        record.identity == vote_account || record.vote_account == vote_account
    });

    match validator {
        Some((vote_key, _validator)) => {
            let versions = context.read().await.cache.get_versions(vote_key);

            Ok(match versions {
                Some(versions) => {
                    warp::reply::with_status(json(&ResponseVersions { versions }), StatusCode::OK)
                }
                _ => {
                    error!("No versions found for {}", &vote_account);
                    response_error(StatusCode::NOT_FOUND, "Failed to fetch records!".into())
                }
            })
        }
        None => {
            error!("No validator found for {}", &vote_account);
            Ok(response_error(
                StatusCode::NOT_FOUND,
                "Failed to fetch records!".into(),
            ))
        }
    }
}
