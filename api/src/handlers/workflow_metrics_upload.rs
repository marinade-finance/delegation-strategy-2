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
    epoch: Option<i64>,
    epoch_slot_current: Option<i64>,
    prepare_scoring_pending: Option<bool>,
    prepare_scoring_start: Option<i64>,
    prepare_scoring_end: Option<i64>,
    apply_scoring_pending: Option<bool>,
    apply_scoring_start: Option<i64>,
    apply_scoring_end: Option<i64>,
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
    let epoch = query_params.epoch.unwrap_or(0);
    let epoch_slot_current = query_params.epoch_slot_current.unwrap_or(0);
    let prepare_scoring_pending = if query_params.prepare_scoring_pending.unwrap_or(false) {1} else {0};
    let prepare_scoring_start = query_params.prepare_scoring_start.unwrap_or(0);
    let prepare_scoring_end = query_params.prepare_scoring_end.unwrap_or(0);
    let apply_scoring_pending = if query_params.apply_scoring_pending.unwrap_or(false) {1} else {0};
    let apply_scoring_start = query_params.apply_scoring_start.unwrap_or(0);
    let apply_scoring_end = query_params.apply_scoring_end.unwrap_or(0);

    if job_scheduled {
        metrics::JOB_COUNT_SCHEDULED.inc();
    }
    if job_succeded {
        metrics::JOB_COUNT_SUCCESS.inc();
    }
    if job_failed {
        metrics::JOB_COUNT_ERROR.inc();
    }
    if epoch != 0 {
        metrics::CURRENT_EPOCH
            .with_label_values(&[&"current_epoch"])
            .set(epoch);
    }
    if epoch_slot_current != 0 {
        metrics::EPOCH_CURRENT_SLOT
            .with_label_values(&[&"current_epoch"])
            .set(epoch_slot_current);
    }

    metrics::JOB_PREPARE_SCORING_PENDING
        .with_label_values(&[&"prepare_scoring"])
        .set(prepare_scoring_pending);
    if prepare_scoring_start != 0 {
        metrics::JOB_PREPARE_SCORING_START
            .with_label_values(&[&"prepare_scoring"])
            .set(prepare_scoring_start);
    }
    if prepare_scoring_end != 0 {
        metrics::JOB_PREPARE_SCORING_END
            .with_label_values(&[&"prepare_scoring"])
            .set(prepare_scoring_end);
    }

    metrics::JOB_APPLY_SCORING_PENDING
        .with_label_values(&[&"apply_scoring"])
        .set(apply_scoring_pending);  
    if apply_scoring_start != 0 {
        metrics::JOB_APPLY_SCORING_START
            .with_label_values(&[&"apply_scoring"])
            .set(apply_scoring_start);
    }
    if apply_scoring_end != 0 {
        metrics::JOB_APPLY_SCORING_END
            .with_label_values(&[&"apply_scoring"])
            .set(apply_scoring_end);
    }

    Ok(warp::reply::with_status(json(&Response { message:("Metrics uploaded").to_string() }), StatusCode::OK))
}
