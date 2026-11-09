use crate::{context::WrappedContext, metrics, utils::response::response_error_500};
use log::{error, info};
use serde::Serialize;
use store::blacklist::db::{last_evaluated_epoch, load_active, BlacklistRow};
use warp::{http::StatusCode, reply::json, Reply};

#[derive(Serialize, utoipa::ToSchema)]
pub struct ResponseBlacklist {
    /// The last closed epoch the rules judged; `null` before the rules first ran.
    evaluated_epoch: Option<u64>,
    rows: Vec<BlacklistEntry>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct BlacklistEntry {
    vote_account: String,
    /// `manual`, `rule` or `legacy`.
    source: String,
    /// The rule that listed the account, for `source = rule`.
    rule: Option<String>,
    code: String,
    reason: Option<String>,
    added_epoch: Option<u64>,
    /// Observed values the rule judged.
    #[schema(value_type = Option<Object>)]
    evidence: Option<serde_json::Value>,
}

impl From<BlacklistRow> for BlacklistEntry {
    fn from(row: BlacklistRow) -> Self {
        BlacklistEntry {
            vote_account: row.vote_account,
            source: row.source,
            rule: row.rule,
            code: row.code,
            reason: row.reason,
            added_epoch: row.added_epoch,
            evidence: row.evidence,
        }
    }
}

#[utoipa::path(
    get,
    tag = "Scoring",
    operation_id = "Blacklist",
    path = "/blacklist",
    responses(
        (status = 200, body = ResponseBlacklist)
    )
)]
pub async fn handler(context: WrappedContext) -> Result<impl Reply, warp::Rejection> {
    info!("Fetching blacklist");
    metrics::REQUEST_BLACKLIST.inc();

    let context = context.read().await;
    let loaded = match load_active(&context.psql_client).await {
        Ok(rows) => last_evaluated_epoch(&context.psql_client)
            .await
            .map(|epoch| (rows, epoch)),
        Err(err) => Err(err),
    };

    Ok(match loaded {
        Ok((rows, evaluated_epoch)) => warp::reply::with_status(
            json(&ResponseBlacklist {
                evaluated_epoch,
                rows: rows.into_iter().map(BlacklistEntry::from).collect(),
            }),
            StatusCode::OK,
        ),
        Err(err) => {
            error!("Failed to load the blacklist: {err}");
            response_error_500("Failed to load the blacklist!".into())
        }
    })
}
