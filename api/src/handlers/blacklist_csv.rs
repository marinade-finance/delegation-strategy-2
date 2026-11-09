use crate::{context::WrappedContext, metrics, utils::response::response_error_500};
use log::{error, info};
use store::blacklist::db::{load_blacklist, BlacklistRow};
use warp::{http::Response, Reply};

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
        (status = 200, description = "`vote_account,code` per active row; header `X-Blacklist-Evaluated-Epoch` names the last epoch the rules judged", content_type = "text/csv")
    )
)]
pub async fn handler(context: WrappedContext) -> Result<impl Reply, warp::Rejection> {
    info!("Fetching blacklist CSV");
    metrics::REQUEST_BLACKLIST.inc();

    let (evaluated_epoch, rows) = match load_blacklist(&context.read().await.psql_client).await {
        Ok(loaded) => loaded,
        Err(err) => {
            error!("Failed to load the blacklist: {err}");
            return Ok(response_error_500("Failed to load the blacklist!".into()).into_response());
        }
    };

    let mut response = Response::builder().header("Content-Type", "text/csv");
    if let Some(epoch) = evaluated_epoch {
        response = response.header("X-Blacklist-Evaluated-Epoch", epoch.to_string());
    }
    Ok(response.body(render_csv(&rows)).unwrap().into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(vote_account: &str, code: &str) -> BlacklistRow {
        BlacklistRow {
            vote_account: vote_account.into(),
            source: "legacy".into(),
            rule: None,
            code: code.into(),
            reason: None,
            added_epoch: None,
            evidence: None,
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

    #[test]
    fn an_empty_table_renders_only_the_header() {
        assert_eq!(render_csv(&[]), "vote_account,code\n");
    }
}
