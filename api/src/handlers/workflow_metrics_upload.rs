use crate::metrics;
use crate::utils::response_error;
use serde::{Deserialize, Serialize};
use warp::{
    http::StatusCode,
    reply::json,
    Reply,
};

#[derive(Deserialize, Serialize, Debug)]
pub struct QueryParams {
    job_scheduled: Option<bool>,
    job_success: Option<bool>,
    job_error: Option<bool>,
    prepare_scoring_duration: Option<i64>,
    apply_scoring_duration: Option<i64>,
}

#[derive(Serialize)]
struct Response {
    message: String,
}

pub async fn handler(
    logged_in: bool,
    query_params: QueryParams,
) -> Result<impl Reply, warp::Rejection> {
    log::info!("Uploading metrics {:?}", query_params);

    if !logged_in {
        log::error!("Unauthorized access!");
        return Ok(response_error(
            StatusCode::UNAUTHORIZED,
            "Not authorized!".into(),
        ));
    }
    let job_scheduled = query_params.job_scheduled.unwrap_or(false);
    let job_succeded = query_params.job_success.unwrap_or(false);
    let job_failed = query_params.job_error.unwrap_or(false);
    let prepare_scoring_duration = query_params.prepare_scoring_duration.unwrap_or(0);
    let apply_scoring_duration = query_params.apply_scoring_duration.unwrap_or(0);

    if job_scheduled {
        metrics::JOB_COUNT_SCHEDULED.inc();
    }
    if job_succeded {
        metrics::JOB_COUNT_SUCCESS.inc();
    }
    if job_failed {
        metrics::JOB_COUNT_ERROR.inc();
    }

    if prepare_scoring_duration != 0 {
        metrics::JOB_DURATION
            .with_label_values(&[&"prepare_scoring"])
            .set(prepare_scoring_duration);
    }
    if apply_scoring_duration != 0 {
        metrics::JOB_DURATION
            .with_label_values(&[&"apply_scoring"])
            .set(apply_scoring_duration);
    }


    Ok(warp::reply::with_status(json(&Response { message:("Metrics uploaded").to_string() }), StatusCode::OK))
}
