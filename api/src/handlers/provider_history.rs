use crate::context::WrappedContext;
use crate::metrics;
use crate::utils::group_history::{group_history_reply, history_epochs, ResponseGroupHistory};
use crate::utils::response::response_error;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use warp::{http::StatusCode, Reply};

#[derive(Deserialize, Serialize, Debug, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct QueryParams {
    /// A `/providers` key. Mutually exclusive with `asn`.
    key: Option<String>,
    /// Any ASN a `/providers` row lists, followed across renames. Mutually exclusive with `key`.
    asn: Option<i32>,
    epochs: Option<usize>,
}

#[derive(Debug, PartialEq)]
enum ProviderSelector {
    Key(String),
    Asn(i32),
}

fn selector(key: Option<String>, asn: Option<i32>) -> Result<ProviderSelector, String> {
    match (key, asn) {
        (Some(key), None) => Ok(ProviderSelector::Key(key)),
        (None, Some(asn)) => Ok(ProviderSelector::Asn(asn)),
        _ => Err("Specify exactly one of key / asn".to_string()),
    }
}

#[utoipa::path(
    get,
    tag = "Validators",
    operation_id = "Get provider history",
    description = "Per-epoch stake, stake share, net APY and take rate of one `/providers` group, newest epoch first.",
    path = "/providers/history",
    params(QueryParams),
    responses(
        (status = 200, body = ResponseGroupHistory),
        (status = 400, description = "epochs is 0 or above 90, or not exactly one of key / asn"),
        (status = 404, description = "No provider with that key or ASN")
    )
)]
pub async fn handler(
    query_params: QueryParams,
    context: WrappedContext,
) -> Result<impl Reply, warp::Rejection> {
    metrics::REQUEST_COUNT_PROVIDER_HISTORY.inc();

    let epochs = match history_epochs(query_params.epochs) {
        Ok(epochs) => epochs,
        Err(message) => return Ok(response_error(StatusCode::BAD_REQUEST, message)),
    };
    let selector = match selector(query_params.key, query_params.asn) {
        Ok(selector) => selector,
        Err(message) => return Ok(response_error(StatusCode::BAD_REQUEST, message)),
    };

    let (history, net_apy_updated_at) = {
        let cache = &context.read().await.cache;
        (
            match &selector {
                ProviderSelector::Key(key) => cache.get_provider_history(key),
                ProviderSelector::Asn(asn) => cache.get_provider_history_by_asn(*asn),
            },
            cache
                .net_apy_history_updated_at()
                .map(DateTime::<Utc>::from),
        )
    };

    Ok(group_history_reply(
        history,
        epochs,
        net_apy_updated_at,
        || match selector {
            ProviderSelector::Key(key) => format!("No provider {key}"),
            ProviderSelector::Asn(asn) => format!("No provider with ASN {asn}"),
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exactly_one_of_key_and_asn_selects_the_provider() {
        assert_eq!(
            selector(Some("OVH".to_string()), None),
            Ok(ProviderSelector::Key("OVH".to_string()))
        );
        assert_eq!(
            selector(None, Some(16276)),
            Ok(ProviderSelector::Asn(16276))
        );
        assert!(selector(None, None).is_err());
        assert!(selector(Some("OVH".to_string()), Some(16276)).is_err());
    }
}
