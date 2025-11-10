use crate::context::WrappedContext;
use lazy_static::lazy_static;
use log::{error, info};
use regex::Regex;
use warp::{http, http::StatusCode, hyper, Reply};

#[utoipa::path(
    get,
    tag = "Scoring",
    operation_id = "Show the scoring report",
    path = "/reports/scoring/<report_id>",
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
        return Ok(http::response::Builder::new()
            .status(StatusCode::BAD_REQUEST)
            .header(hyper::header::CONTENT_TYPE, "text/plain")
            .body(hyper::Body::from("Invalid scoring ID"))
            .unwrap());
    }

    let report_remote_url = format!("https://raw.githubusercontent.com/marinade-finance/delegation-strategy-pipeline/master/scoring/{scoring_ui_id}/report.html");

    let response = match reqwest::get(&report_remote_url).await {
        Ok(response) => response,
        Err(err) => {
            error!("Failed to fetch the HTML ({report_remote_url}) from the remote: {err}");
            return Ok(http::response::Builder::new()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .header(hyper::header::CONTENT_TYPE, "text/plain")
                .body(hyper::Body::from("Failed to fetch the HTML report"))
                .unwrap());
        }
    };

    let status = response.status();
    let body = hyper::Body::wrap_stream(response.bytes_stream());

    Ok(http::response::Builder::new()
        .status(status)
        .header(hyper::header::CONTENT_TYPE, "text/html")
        .body(body)
        .unwrap())
}
