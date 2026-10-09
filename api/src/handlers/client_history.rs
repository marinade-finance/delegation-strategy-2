use crate::context::WrappedContext;
use crate::metrics;
use crate::utils::group_history::{group_history_reply, history_epochs, ResponseGroupHistory};
use crate::utils::response::response_error;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use store::dto::ClientLevel;
use warp::{http::StatusCode, Reply};

#[derive(Deserialize, Serialize, Debug, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct QueryParams {
    key: String,
    level: Option<ClientLevel>,
    epochs: Option<usize>,
}

#[utoipa::path(
    get,
    tag = "Validators",
    operation_id = "Get client history",
    description = "Per-epoch stake, stake share, net APY and take rate of one `/clients` group (`level=lineage` or `label`), newest epoch first.",
    path = "/clients/history",
    params(QueryParams),
    responses(
        (status = 200, body = ResponseGroupHistory),
        (status = 400, description = "epochs is 0 or above 90, or level is not lineage or label"),
        (status = 404, description = "No client group with that key at that level")
    )
)]
pub async fn handler(
    query_params: QueryParams,
    context: WrappedContext,
) -> Result<impl Reply, warp::Rejection> {
    metrics::REQUEST_COUNT_CLIENT_HISTORY.inc();

    let epochs = match history_epochs(query_params.epochs) {
        Ok(epochs) => epochs,
        Err(message) => return Ok(response_error(StatusCode::BAD_REQUEST, message)),
    };
    let level = query_params.level.unwrap_or_default();

    let (history, net_apy_updated_at) = {
        let cache = &context.read().await.cache;
        (
            cache.get_client_history(&query_params.key, level),
            cache
                .net_apy_history_updated_at()
                .map(DateTime::<Utc>::from),
        )
    };

    Ok(group_history_reply(
        history,
        epochs,
        net_apy_updated_at,
        || format!("No client {} at level {level:?}", query_params.key),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_reads_lowercase_and_defaults_to_lineage() {
        assert_eq!(
            serde_json::from_str::<ClientLevel>(r#""lineage""#).unwrap(),
            ClientLevel::Lineage
        );
        assert_eq!(
            serde_json::from_str::<ClientLevel>(r#""label""#).unwrap(),
            ClientLevel::Label
        );
        assert!(serde_json::from_str::<ClientLevel>(r#""vendor""#).is_err());
        assert_eq!(ClientLevel::default(), ClientLevel::Lineage);
    }
}
