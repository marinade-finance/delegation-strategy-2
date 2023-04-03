use crate::metrics;
use crate::utils::response_error;
use crate::{context::WrappedContext, utils::response_error_500};
use bytes::BufMut;
use futures::TryStreamExt;
use serde::{Deserialize, Serialize};
use utoipa::IntoParams;
use warp::{
    http::StatusCode,
    multipart::{FormData, Part},
    reply::json,
    Reply,
};

const SCORES_CSV_PART_NAME: &str = "scores_csv";

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct ResponseAdminScoreUpload {
    rows_processed: u64,
}

#[derive(Deserialize, Serialize, Debug, IntoParams)]
pub struct QueryParams {
    epoch: i32,
    components: String,
    component_weights: String,
    ui_id: String,
}

#[utoipa::path(
    post,
    tag = "Admin",
    operation_id = "Upload score results",
    path = "/admin/scores",
    params(QueryParams),
    responses(
        (status = 200, body = ResponseAdminScoreUpload)
    )
)]
pub async fn handler(
    logged_in: bool,
    query_params: QueryParams,
    form: FormData,
    context: WrappedContext,
) -> Result<impl Reply, warp::Rejection> {
    metrics::REQUEST_ADMIN_SCORE_UPLOAD.inc();
    log::info!("Uploading scores {:?}", query_params);

    if !logged_in {
        log::error!("Unauthorized access!");
        return Ok(response_error(
            StatusCode::UNAUTHORIZED,
            "Not authorized!".into(),
        ));
    }

    let parts: Vec<Part> = form.try_collect().await.map_err(|err| {
        log::error!("Upload error: {}", err);
        warp::reject::reject()
    })?;

    let components: Vec<&str> = query_params.components.split(",").collect();
    let component_weights: Vec<f64> = query_params
        .component_weights
        .split(",")
        .map(|weight| weight.parse::<f64>().unwrap())
        .collect();

    let scores_csv_part = parts.into_iter().find_map(|part| {
        if !part.name().eq(SCORES_CSV_PART_NAME) {
            return None;
        }
        return Some(part);
    });

    let scores_csv_part = match scores_csv_part {
        Some(part) => part,
        _ => {
            log::error!("CSV with scores is not attached!");
            return Ok(response_error(
                StatusCode::BAD_REQUEST,
                "Scores CSV is missing!".into(),
            ));
        }
    };

    let scores_csv = scores_csv_part
        .stream()
        .try_fold(Vec::new(), |mut vec, data| {
            vec.put(data);
            async move { Ok(vec) }
        })
        .await
        .map_err(|err| {
            log::error!("CSV reading error: {}", err);
            warp::reject::reject()
        })?;

    let mut rows_processed = 0;
    let mut validator_scores: Vec<store::dto::ValidatorScoringCsvRow> = Default::default();
    let mut reader = csv::Reader::from_reader(std::io::Cursor::new(scores_csv));
    for result in reader.deserialize() {
        match result {
            Ok(row) => {
                validator_scores.push(row);
                rows_processed += 1;
            }
            Err(err) => {
                log::error!("Failed to parse the CSV row: {}", err);
                return Ok(response_error(
                    StatusCode::BAD_REQUEST,
                    "Cannot parse the CSV!".into(),
                ));
            }
        }
    }

    let result = store::utils::store_scoring(
        &mut context.write().await.psql_client,
        query_params.epoch,
        query_params.ui_id,
        components,
        component_weights,
        validator_scores,
    )
    .await;

    Ok(match result {
        Ok(_) => warp::reply::with_status(json(&ResponseAdminScoreUpload { rows_processed }), StatusCode::OK),
        Err(err) => {
            log::error!("Failed to store the scoring: {}", err);
            response_error_500("Failed to store the scoring!".into())
        }
    })
}
