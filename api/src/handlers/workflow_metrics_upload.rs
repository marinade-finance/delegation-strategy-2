use crate::metrics;
use crate::utils::response_error;
use serde::{Deserialize, Serialize};
use warp::{http::StatusCode, reply::json, Reply};

#[derive(Deserialize, Serialize, Debug, utoipa::IntoParams)]
pub struct QueryParams {
    job_scheduled: Option<bool>,
    job_success: Option<bool>,
    job_error: Option<bool>,
    prepare_scoring_duration: Option<i64>,
    apply_scoring_duration: Option<i64>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct ResponseAdminWorkflowMetrics {
    message: String,
}

#[utoipa::path(
    post,
    tag = "Admin",
    operation_id = "Push workflow metrics",
    path = "/admin/metrics",
    params(QueryParams),
    responses(
        (status = 200, body = ResponseAdminWorkflowMetrics)
    )
)]
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

    if job_scheduled {
        metrics::JOB_COUNT_SCHEDULED.inc();
    }
    if job_succeded {
        metrics::JOB_COUNT_SUCCESS.inc();
    }
    if job_failed {
        metrics::JOB_COUNT_ERROR.inc();
    }

    if let Some(prepare_scoring_duration) = query_params.prepare_scoring_duration {
        metrics::JOB_DURATION
            .with_label_values(&[&"prepare_scoring"])
            .set(prepare_scoring_duration);
    }
    if let Some(apply_scoring_duration) = query_params.apply_scoring_duration {
        metrics::JOB_DURATION
            .with_label_values(&[&"apply_scoring"])
            .set(apply_scoring_duration);
    }

    Ok(warp::reply::with_status(
        json(&ResponseAdminWorkflowMetrics {
            message: "Metrics uploaded".into(),
        }),
        StatusCode::OK,
    ))
}
