use crate::context::WrappedContext;
use serde::{Deserialize, Serialize};
use warp::{http::StatusCode, reply, Reply};

#[derive(Serialize, Deserialize, utoipa::ToSchema)]
pub struct ResponseConfig {
    stakes: ConfigStakes,
}

#[derive(Serialize, Deserialize, utoipa::ToSchema)]
pub struct ConfigStakes {
    delegation_authorities: Vec<StakeDelegationAuthorityRecord>,
}

#[derive(Serialize, Deserialize, utoipa::ToSchema)]
pub struct StakeDelegationAuthorityRecord {
    delegation_authority: String,
    name: String,
}

#[utoipa::path(
    get,
    tag = "General",
    operation_id = "Show configuration of the API",
    path = "/static/config",
    responses(
        (status = 200, body = ResponseConfig)
    )
)]
pub async fn handler(_context: WrappedContext) -> Result<impl Reply, warp::Rejection> {
    log::info!("Serving the configuration data");
    Ok(warp::reply::with_status(
        reply::json(&ResponseConfig {
            stakes: ConfigStakes {
                delegation_authorities: vec![
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "4bZ6o3eUUNXhKuqjdCnCoPAoLgWiuLYixKaxoa8PpiKk".into(),
                        name: "Marinade Liquid".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "stWirqFCf2Uts1JBL1Jsd3r6VBWhgnpdPxCTe1MFjrq".into(),
                        name: "Marinade Native".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "mpa4abUkjQoAvPzREkh5Mo75hZhPFQ2FSH6w7dWKuQ5".into(),
                        name: "Solana Foundation".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "6iQKfEyhr3bZMotVkW6beNZz5CPAkiwvgV2CTje9pVSS".into(),
                        name: "Jito".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "W1ZQRwUfSkDKy2oefRBUWph82Vr2zg9txWMA8RQazN5".into(),
                        name: "Lido".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "HbJTxftxnXgpePCshA8FubsRj9MW4kfPscfuUfn44fnt".into(),
                        name: "Jpool".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "6WecYymEARvjG5ZyqkrVQ6YkhPfujNzWpSPwNKXHCbV2".into(),
                        name: "Blaze Stake".into(),
                    },
                    StakeDelegationAuthorityRecord {
                        delegation_authority: "EhYXq3ANp5nAerUpbSgd7VK2RRcxK1zNuSQ755G5Mtxx".into(),
                        name: "Alameda".into(),
                    },
                ],
            },
        }),
        StatusCode::OK,
    ))
}
