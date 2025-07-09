use crate::handlers::{
    admin_score_upload, cluster_stats, commissions, config, docs, global_unstake_hints, glossary,
    jito_mev, jito_priority_fee, list_validators, reports_commission_changes, reports_scoring,
    reports_scoring_html, reports_staking, rewards, unstake_hints, uptimes,
    validator_score_breakdown, validator_score_breakdowns, validator_scores, validators_flat,
    versions, workflow_metrics_upload,
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
        schemas(global_unstake_hints::ResponseGlobalUnstakeHints),
        schemas(list_validators::ResponseValidators),
        schemas(reports_commission_changes::CommissionChange),
        schemas(reports_commission_changes::ResponseCommissionChanges),
        schemas(reports_scoring::ResponseReportScoring),
        schemas(reports_staking::ResponseReportStaking),
        schemas(reports_staking::Stake),
        schemas(rewards::ResponseRewards),
        schemas(store::dto::BlockProductionStats),
        schemas(store::dto::ClusterStats),
        schemas(store::dto::CommissionRecord),
        schemas(store::dto::DCConcentrationStats),
        schemas(store::dto::GlobalUnstakeHintRecord),
        schemas(store::dto::UnstakeHintRecord),
        schemas(store::dto::UnstakeHint),
        schemas(store::dto::UptimeRecord),
        schemas(store::dto::ValidatorEpochStats),
        schemas(store::dto::ValidatorRecord),
        schemas(store::dto::ValidatorsAggregated),
        schemas(store::dto::ValidatorScoreRecord),
        schemas(store::dto::ValidatorWarning),
        schemas(store::dto::RuggerRecord),
        schemas(store::dto::RugInfo),
        schemas(store::dto::VersionRecord),
        schemas(store::dto::JitoMevRecord),
        schemas(unstake_hints::ResponseUnstakeHints),
        schemas(uptimes::ResponseUptimes),
        schemas(validator_score_breakdown::ResponseScoreBreakdown),
        schemas(validator_score_breakdown::ScoreBreakdown),
        schemas(validator_score_breakdowns::ResponseScoreBreakdowns),
        schemas(validator_scores::ResponseScores),
        schemas(versions::ResponseVersions),
        schemas(workflow_metrics_upload::ResponseAdminWorkflowMetrics),
        schemas(jito_mev::ResponseJitoMev),
        schemas(jito_priority_fee::ResponseJitoPriorityFee)
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
        rewards::handler,
        unstake_hints::handler,
        global_unstake_hints::handler,
        uptimes::handler,
        validator_score_breakdown::handler,
        validator_score_breakdowns::handler,
        validator_scores::handler,
        validators_flat::handler,
        versions::handler,
        workflow_metrics_upload::handler,
        jito_mev::handler,
        jito_priority_fee::handler
    )
)]
pub struct ApiDoc;
