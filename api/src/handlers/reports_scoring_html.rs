use crate::context::WrappedContext;
use lazy_static::lazy_static;
use log::{error, info};
use regex::Regex;
use warp::{http::header::CONTENT_TYPE, http::StatusCode, reply, Reply};

#[utoipa::path(
    get,
    tag = "Scoring",
    operation_id = "Show the scoring report",
    path = "/reports/scoring/{report_id}",
    params(
        ("report_id" = String, Path, description = "Scoring run ID in format epoch.run_number")
    ),
    responses(
        (status = 200)
    )
)]
pub async fn handler(
    scoring_ui_id: String,
    _context: WrappedContext,
) -> Result<impl Reply, warp::Rejection> {
    info!("Serving the scoring HTML report");
    lazy_static! {
        static ref VALID_SCORING_RUN_UI_ID: Regex = Regex::new("^\\d+\\.\\d+$").unwrap();
    }

    if !VALID_SCORING_RUN_UI_ID.is_match(&scoring_ui_id) {
        return Ok(
            reply::with_status("Invalid scoring ID", StatusCode::BAD_REQUEST).into_response(),
        );
    }

    let report_remote_url = format!("https://raw.githubusercontent.com/marinade-finance/delegation-strategy-pipeline/master/scoring/{scoring_ui_id}/report.html");

    let response = match reqwest::get(&report_remote_url).await {
        Ok(response) => response,
        Err(err) => {
            error!("Failed to fetch the HTML ({report_remote_url}) from the remote: {err}");
            return Ok(reply::with_status(
                "Failed to fetch the HTML report",
                StatusCode::INTERNAL_SERVER_ERROR,
            )
            .into_response());
        }
    };

    let status = response.status();
    let body = reply::stream(response.bytes_stream());

    Ok(
        reply::with_status(reply::with_header(body, CONTENT_TYPE, "text/html"), status)
            .into_response(),
    )
}
