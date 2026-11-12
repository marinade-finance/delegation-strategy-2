use crate::{
    context::WrappedContext,
    handlers::blacklist::NOT_BOOTSTRAPPED,
    metrics,
    utils::response::{response_error, response_error_500},
};
use log::{error, info};
use store::blacklist::db::{load_blacklist, BlacklistRow};
use warp::{
    http::{Response, StatusCode},
    Reply,
};

/// Same shape as the `blacklist.csv` file it replaces, so every reader parses it unchanged.
pub fn render_csv(rows: &[BlacklistRow]) -> String {
    let mut csv = String::from("vote_account,code\n");
    for row in rows {
        csv.push_str(&format!("{},{}\n", row.vote_account, row.code));
    }
    csv
}

#[utoipa::path(
    get,
    tag = "Scoring",
    operation_id = "Blacklist as CSV",
    path = "/blacklist.csv",
    responses(
        (status = 200, description = "`vote_account,code` per active row", content_type = "text/csv"),
        (status = 503, description = "The legacy list is not imported yet; serving part of the list would unlist the rest")
    )
)]
pub async fn handler(context: WrappedContext) -> Result<impl Reply, warp::Rejection> {
    info!("Fetching blacklist CSV");
    metrics::REQUEST_BLACKLIST.inc();

    Ok(
        match load_blacklist(&context.read().await.psql_client).await {
            Ok(Some(blacklist)) => Response::builder()
                .header("Content-Type", "text/csv")
                .body(render_csv(&blacklist.rows))
                .unwrap()
                .into_response(),
            Ok(None) => response_error(StatusCode::SERVICE_UNAVAILABLE, NOT_BOOTSTRAPPED.into())
                .into_response(),
            Err(err) => {
                error!("Failed to load the blacklist: {err}");
                response_error_500("Failed to load the blacklist!".into()).into_response()
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(vote_account: &str, code: &str) -> BlacklistRow {
        BlacklistRow {
            vote_account: vote_account.into(),
            source: "legacy".into(),
            code: code.into(),
            auction_epoch: None,
        }
    }

    #[test]
    fn the_csv_starts_with_the_header_and_keeps_the_row_order() {
        let rows = [
            row("A", "SLOW_VOTER"),
            row("A", "SLOW_SLOTS_1080"),
            row("B", "MALICIOUS_SM"),
        ];
        assert_eq!(
            render_csv(&rows),
            "vote_account,code\nA,SLOW_VOTER\nA,SLOW_SLOTS_1080\nB,MALICIOUS_SM\n"
        );
    }
}
