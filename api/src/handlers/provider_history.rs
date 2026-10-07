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
    name: String,
    epochs: Option<usize>,
}

#[utoipa::path(
    get,
    tag = "Validators",
    operation_id = "Get provider history",
    description = "Per-epoch stake, stake share, net APY and take rate of one `/providers` group, newest epoch first, one row per cached epoch. `name` is a `/providers` key, matched case-insensitively. Each epoch counts the validators whose own row for that epoch names the provider; `stake_share` is of every validator with a row that epoch. `net_apy` (apy-api 14-day rolling staker APY at the epoch end) and `take_rate` (realized) are weighted by each member's stake that epoch, and null for the open epoch, whose `epoch_end_at` is null. `epochs` defaults to and is capped at 90.",
    path = "/providers/history",
    params(QueryParams),
    responses(
        (status = 200, body = ResponseGroupHistory),
        (status = 400, description = "epochs is 0 or above 90"),
        (status = 404, description = "No provider with that name")
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

    let (history, net_apy_updated_at) = {
        let cache = &context.read().await.cache;
        (
            cache.get_provider_history(&query_params.name),
            cache
                .net_apy_history_updated_at()
                .map(DateTime::<Utc>::from),
        )
    };

    Ok(group_history_reply(
        history,
        epochs,
        net_apy_updated_at,
        || format!("No provider named {}", query_params.name),
    ))
}
