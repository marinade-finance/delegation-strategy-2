use std::collections::HashMap;

use crate::{context::WrappedContext, utils::response_error_500};
use chrono::{DateTime, Utc};
use log::{error, info};
use serde::Serialize;
use store::dto::ScoringRunRecord;
use warp::{http::StatusCode, reply, Reply};

#[derive(Serialize, Debug, utoipa::ToSchema)]
struct Report {
    created_at: DateTime<Utc>,
    md: String,
}

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct ResponseReportScoring {
    #[schema(additional_properties)]
    reports: HashMap<i32, Vec<Report>>,
}

fn md_pre_msol_votes(ui_id: &String) -> String {
    format!(
        "# Report {ui_id}\n\
        - [HTML report](https://validators-api.marinade.finance/reports/scoring/{ui_id})\n\
        - [CSV Scores](https://raw.githubusercontent.com/marinade-finance/delegation-strategy-pipeline/master/scoring/{ui_id}/scores.csv)\n\
        ## Reproduce the results\n\
        Get the source data:\n\
        ```bash\n\
        mkdir -p \"scoring-{ui_id}\"\n\
        cd \"scoring-{ui_id}\"\n\
        wget --base \"https://raw.githubusercontent.com/marinade-finance/delegation-strategy-pipeline/master/scoring/{ui_id}/\" \
            --input-file - --no-clobber <<<$'validators.csv\nself-stake.csv\nparams.env\nblacklist.csv'\n\
        ```\n\
        Install dependencies for R (assumes you have R installed already):\n\
        ```bash\n\
        bash -c \"$(curl -sSfL https://raw.githubusercontent.com/marinade-finance/delegation-strategy-2/392fb39/scripts/scoring-install.bash)\"\n\
        ```\n\
        Generate scores:\n\
        ```bash\n\
        wget https://raw.githubusercontent.com/marinade-finance/delegation-strategy-2/392fb39/scripts/scoring.R\n\
        export SCORING_WORKING_DIRECTORY=.\n\
        export SCORING_R=./scoring.R\n\
        bash -c \"$(curl -sSfL https://raw.githubusercontent.com/marinade-finance/delegation-strategy-2/392fb39/scripts/scoring-run.bash)\"\n\
        ```\n\
    ")
}

fn md_pre_vemnde_votes(ui_id: &String) -> String {
    format!(
        "# Report {ui_id}\n\
        - [HTML report](https://validators-api.marinade.finance/reports/scoring/{ui_id})\n\
        - [CSV Scores](https://raw.githubusercontent.com/marinade-finance/delegation-strategy-pipeline/master/scoring/{ui_id}/scores.csv)\n\
        ## Reproduce the results\n\
        Get the source data:\n\
        ```bash\n\
        mkdir -p \"scoring-{ui_id}\"\n\
        cd \"scoring-{ui_id}\"\n\
        wget --base \"https://raw.githubusercontent.com/marinade-finance/delegation-strategy-pipeline/master/scoring/{ui_id}/\" \
            --input-file - --no-clobber <<<$'validators.csv\nmsol-votes.csv\nparams.env\nblacklist.csv'\n\
        ```\n\
        Install dependencies for R (assumes you have R installed already):\n\
        ```bash\n\
        bash -c \"$(curl -sSfL https://raw.githubusercontent.com/marinade-finance/delegation-strategy-2/master/scripts/scoring-install.bash)\"\n\
        ```\n\
        Generate scores:\n\
        ```bash\n\
        wget https://raw.githubusercontent.com/marinade-finance/delegation-strategy-2/7c51106/scripts/scoring.R\n\
        export SCORING_WORKING_DIRECTORY=.\n\
        export SCORING_R=./scoring.R\n\
        bash -c \"$(curl -sSfL https://raw.githubusercontent.com/marinade-finance/delegation-strategy-2/7c51106/scripts/scoring-run.bash)\"\n\
        ```\n\
    ")
}

fn md_pre_psr(ui_id: &String) -> String {
    format!(
        "# Report {ui_id}\n\
        - [HTML report](https://validators-api.marinade.finance/reports/scoring/{ui_id})\n\
        - [CSV Scores](https://raw.githubusercontent.com/marinade-finance/delegation-strategy-pipeline/master/scoring/{ui_id}/scores.csv)\n\
        ## Reproduce the results\n\
        Get the source data:\n\
        ```bash\n\
        mkdir -p \"scoring-{ui_id}\"\n\
        cd \"scoring-{ui_id}\"\n\
        wget --base \"https://raw.githubusercontent.com/marinade-finance/delegation-strategy-pipeline/master/scoring/{ui_id}/\" \
            --input-file - --no-clobber <<<$'validators.csv\nmsol-votes.csv\nvemnde-votes.csv\nparams.env\nblacklist.csv'\n\
        ```\n\
        Install dependencies for R (assumes you have R installed already):\n\
        ```bash\n\
        bash -c \"$(curl -sSfL https://raw.githubusercontent.com/marinade-finance/delegation-strategy-2/master/scripts/scoring-install.bash)\"\n\
        ```\n\
        Generate scores:\n\
        ```bash\n\
        wget https://raw.githubusercontent.com/marinade-finance/delegation-strategy-2/master/scripts/scoring.R\n\
        export SCORING_WORKING_DIRECTORY=.\n\
        export SCORING_R=./scoring.R\n\
        bash -c \"$(curl -sSfL https://raw.githubusercontent.com/marinade-finance/delegation-strategy-2/master/scripts/scoring-run.bash)\"\n\
        ```\n\
    ")
}

fn md_latest(ui_id: &String) -> String {
    format!(
        "# Report {ui_id}\n\
        - [HTML report](https://validators-api.marinade.finance/reports/scoring/{ui_id})\n\
        - [CSV Scores](https://raw.githubusercontent.com/marinade-finance/delegation-strategy-pipeline/master/scoring/{ui_id}/scores.csv)\n\
        ## Reproduce the results\n\
        Get the source data:\n\
        ```bash\n\
        mkdir -p \"scoring-{ui_id}\"\n\
        cd \"scoring-{ui_id}\"\n\
        wget --base \"https://raw.githubusercontent.com/marinade-finance/delegation-strategy-pipeline/master/scoring/{ui_id}/\" \
            --input-file - --no-clobber <<<$'validators.csv\nmsol-votes.csv\nvemnde-votes.csv\nparams.env\nblacklist.csv\nvalidator-bonds.csv'\n\
        ```\n\
        Install dependencies for R (assumes you have R installed already):\n\
        ```bash\n\
        bash -c \"$(curl -sSfL https://raw.githubusercontent.com/marinade-finance/delegation-strategy-2/master/scripts/scoring-install.bash)\"\n\
        ```\n\
        Generate scores:\n\
        ```bash\n\
        wget https://raw.githubusercontent.com/marinade-finance/delegation-strategy-2/master/scripts/scoring.R\n\
        export SCORING_WORKING_DIRECTORY=.\n\
        export SCORING_R=./scoring.R\n\
        bash -c \"$(curl -sSfL https://raw.githubusercontent.com/marinade-finance/delegation-strategy-2/master/scripts/scoring-run.bash)\"\n\
        ```\n\
    ")
}

fn scoring_run_to_report(scoring_run: ScoringRunRecord) -> Report {
    Report {
        created_at: scoring_run.created_at,
        md: if scoring_run.epoch < 454 {
            md_pre_msol_votes(&scoring_run.ui_id)
        } else if scoring_run.epoch < 481 {
            md_pre_vemnde_votes(&scoring_run.ui_id)
        } else if scoring_run.epoch < 575 {
            md_pre_psr(&scoring_run.ui_id)
        } else {
            md_latest(&scoring_run.ui_id)
        },
    }
}

#[utoipa::path(
    get,
    tag = "Scoring",
    operation_id = "List scoring reports",
    path = "reports/scoring",
    responses(
        (status = 200, body = ResponseReportScoring)
    )
)]
pub async fn handler(context: WrappedContext) -> Result<impl Reply, warp::Rejection> {
    info!("Serving the scoring reports");

    let scoring_runs =
        match store::scoring::load_scoring_runs(&context.read().await.psql_client).await {
            Ok(scoring_runs) => scoring_runs,
            Err(err) => {
                error!("Failed to fetch scoring run records: {err}");
                return Ok(response_error_500("Failed to fetch records!".into()));
            }
        };

    Ok(warp::reply::with_status(
        reply::json(&ResponseReportScoring {
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
