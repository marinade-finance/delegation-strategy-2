use std::collections::HashMap;

use crate::{context::WrappedContext, utils::response_error_500};
use chrono::{DateTime, Utc};
use log::{error, info};
use serde::Serialize;
use store::dto::ScoringRunRecord;
use warp::{http::StatusCode, reply, Reply};

#[derive(Serialize, Debug)]
struct Report {
    created_at: DateTime<Utc>,
    md: String,
}

#[derive(Serialize, Debug)]
struct Response {
    reports: HashMap<i32, Vec<Report>>,
}

fn scoring_run_to_report(scoring_run: ScoringRunRecord) -> Report {
    Report {
        created_at: scoring_run.created_at,
        md: format!(
            "# Report {}\n\
            - [HTML report](https://validators-api-dev.marinade.finance/reports/scoring/{})\n\
            - [CSV Scores](https://raw.githubusercontent.com/marinade-finance/delegation-strategy-pipeline/master/scoring/{}/scores.csv)\n\
            ## Reproduce the results\n\
            Get the source data:\n\
            ```bash\n\
            mkdir -p \"scoring-{}\"\n\
            cd \"scoring-{}\"\n\
            wget --base \"https://raw.githubusercontent.com/marinade-finance/delegation-strategy-pipeline/master/scoring/{}/\" \
                --input-file - --no-clobber <<<$'validators.csv\nself-stake.csv\nparams.env\nblacklist.csv'\n\
            ```\n\
            Generate scores:\n\
            ```bash\n\
            wget https://raw.githubusercontent.com/marinade-finance/delegation-strategy-2/master/scripts/scoring.R\n\
            export SCORING_WORKING_DIRECTORY=.\n\
            export SCORING_R=./scoring.R\n\
            bash -c \"$(curl -sSfL https://raw.githubusercontent.com/marinade-finance/delegation-strategy-2/master/scripts/scoring-run.bash)\"\n\
            ```\n\
        ",
            scoring_run.ui_id,
            scoring_run.ui_id,
            scoring_run.ui_id,
            scoring_run.ui_id,
            scoring_run.ui_id,
            scoring_run.ui_id,
        ),
    }
}

pub async fn handler(context: WrappedContext) -> Result<impl Reply, warp::Rejection> {
    info!("Serving the scoring reports");

    let scoring_runs =
        match store::scoring::load_scoring_runs(&context.read().await.psql_client).await {
            Ok(scoring_runs) => scoring_runs,
            Err(err) => {
                error!("Failed to fetch scoring run records: {}", err);
                return Ok(response_error_500("Failed to fetch records!".into()));
            }
        };

    Ok(warp::reply::with_status(
        reply::json(&Response {
            reports: scoring_runs
                .into_iter()
                .fold(Default::default(), |mut acc, scoring_run| {
                    acc.entry(scoring_run.epoch)
                        .or_default()
                        .push(scoring_run_to_report(scoring_run));

                    acc
                }),
        }),
        StatusCode::OK,
    ))
}
