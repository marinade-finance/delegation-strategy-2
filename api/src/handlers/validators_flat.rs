use crate::context::WrappedContext;
use crate::metrics;
use crate::utils::response::response_error_500;
use log::error;
use serde::{Deserialize, Serialize};
use warp::Reply;

const DEFAULT_EPOCHS: u64 = 10;

#[derive(Deserialize, Serialize, Debug, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct QueryParams {
    epochs: Option<u64>,
    last_epoch: u64,
}

#[utoipa::path(
    get,
    tag = "Scoring",
    operation_id = "List aggregated validators",
    path = "/validators/flat",
    params(QueryParams),
    responses(
        (status = 200)
    )
)]
pub async fn handler(
    query_params: QueryParams,
    context: WrappedContext,
) -> Result<impl Reply, warp::Rejection> {
    metrics::REQUEST_COUNT_VALIDATORS_FLAT.inc();

    log::info!("Query flat validators {query_params:?}");

    let epochs = query_params.epochs.unwrap_or(DEFAULT_EPOCHS);
    let validators = store::utils::load_validators_aggregated_flat(
        &context.read().await.psql_client,
        query_params.last_epoch,
        epochs,
    )
    .await;

    let validators = match validators {
        Ok(validators) => validators,
        Err(err) => {
            error!("Failed to fetch validator records: {err}");
            return Ok(response_error_500("Failed to fetch records!".into()).into_response());
        }
    };

    let mut csv_content = csv::Writer::from_writer(Vec::new());
    for validator in validators {
        let _ = csv_content.serialize(validator);
    }

    Ok(warp::reply::with_header(
        String::from_utf8(csv_content.into_inner().unwrap()).unwrap(),
        "Content-Type",
        "text/plain", // to confuse browsers and allow inline opening
    )
    .into_response())
}

#[cfg(test)]
mod tests {
    use store::dto::ValidatorAggregatedFlat;

    #[test]
    fn the_csv_header_only_gains_a_trailing_column() {
        let mut csv_content = csv::Writer::from_writer(Vec::new());
        csv_content
            .serialize(ValidatorAggregatedFlat {
                vote_account: "vote".into(),
                minimum_stake: 0.0,
                avg_stake: 0.0,
                avg_dc_concentration: 0.0,
                avg_skip_rate: 0.0,
                avg_grace_skip_rate: 0.0,
                max_commission: 0,
                avg_adjusted_credits: 0.0,
                dc_aso: "aso".into(),
                marinade_stake: 0.0,
                version: "0.0.0".into(),
                client_vendor: "unknown".into(),
                client_lineage: "unknown".into(),
                max_inflation_rewards_commission_bps: None,
            })
            .unwrap();
        let content = String::from_utf8(csv_content.into_inner().unwrap()).unwrap();
        let (header, row) = content.split_once('\n').unwrap();

        assert_eq!(
            header,
            "vote_account,minimum_stake,avg_stake,avg_dc_concentration,avg_skip_rate,\
             avg_grace_skip_rate,max_commission,avg_adjusted_credits,dc_aso,marinade_stake,\
             version,client_vendor,client_lineage,max_inflation_rewards_commission_bps"
        );
        assert!(
            row.ends_with(",unknown,unknown,\n"),
            "an unknown bps is an empty cell, not a zero: {row}"
        );
    }
}
