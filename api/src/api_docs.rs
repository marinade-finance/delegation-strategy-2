use crate::handlers::{
    admin_score_upload, cluster_stats, commissions, config, docs, glossary, list_validators,
    reports_commission_changes, reports_scoring, reports_scoring_html, reports_staking,
    unstake_hints, uptimes, validator_score_breakdown, validator_scores, validators_flat, versions,
    workflow_metrics_upload,
};
use utoipa::OpenApi;

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Marinade's Delegation Strategy API",
        description = "This API serves data about validators and their performance.",
        license(
            name = "Apache License, Version 2.0",
            url = "https://www.apache.org/licenses/LICENSE-2.0"
        )
    ),
    components(
        schemas(admin_score_upload::ResponseAdminScoreUpload),
        schemas(cluster_stats::ResponseClusterStats),
        schemas(commissions::ResponseCommissions),
        schemas(config::ConfigStakes),
        schemas(config::ResponseConfig),
        schemas(config::StakeDelegationAuthorityRecord),
        schemas(list_validators::ResponseValidators),
        schemas(reports_commission_changes::CommissionChange),
        schemas(reports_commission_changes::ResponseCommissionChanges),
        schemas(reports_scoring::ResponseReportScoring),
        schemas(reports_staking::ResponseReportStaking),
        schemas(reports_staking::Stake),
        schemas(store::dto::BlockProductionStats),
        schemas(store::dto::ClusterStats),
        schemas(store::dto::CommissionRecord),
        schemas(store::dto::DCConcentrationStats),
        schemas(store::dto::UnstakeHintRecord),
        schemas(store::dto::UptimeRecord),
        schemas(store::dto::ValidatorEpochStats),
        schemas(store::dto::ValidatorRecord),
        schemas(store::dto::ValidatorsAggregated),
        schemas(store::dto::ValidatorScoreRecord),
        schemas(store::dto::ValidatorWarning),
        schemas(store::dto::VersionRecord),
        schemas(unstake_hints::ResponseUnstakeHints),
        schemas(uptimes::ResponseUptimes),
        schemas(validator_score_breakdown::ResponseScoreBreakdown),
        schemas(validator_score_breakdown::ScoreBreakdown),
        schemas(validator_scores::ResponseScores),
        schemas(versions::ResponseVersions),
        schemas(workflow_metrics_upload::ResponseAdminWorkflowMetrics),
    ),
    paths(
        admin_score_upload::handler,
        cluster_stats::handler,
        commissions::handler,
        config::handler,
        docs::handler,
        glossary::handler,
        list_validators::handler,
        reports_commission_changes::handler,
        reports_scoring_html::handler,
        reports_scoring::handler,
        reports_staking::handler,
        unstake_hints::handler,
        uptimes::handler,
        validator_score_breakdown::handler,
        validator_scores::handler,
        validators_flat::handler,
        versions::handler,
        workflow_metrics_upload::handler,
    )
)]
pub struct ApiDoc;
