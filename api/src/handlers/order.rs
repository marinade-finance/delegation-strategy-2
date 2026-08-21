use serde::{Deserialize, Serialize};
use std::cmp::Ordering;

pub const DEFAULT_ORDER_FIELD: OrderField = OrderField::Stake;
pub const DEFAULT_ORDER_DIRECTION: OrderDirection = OrderDirection::DESC;

#[derive(Deserialize, Serialize, Debug, Clone, Copy, utoipa::ToSchema)]
pub enum OrderField {
    Name,
    Stake,
    StakeDelta7d,
    StakeDelta30d,
    Credits,
    MarinadeScore,
    Apy,
    /// Orders by the MEV-inclusive `net_apy`, which is what the validators list renders; `Apy` orders by the inflation-only `avg_apy`.
    NetApy,
    Commission,
    Uptime,
    TakeRate,
    /// Orders by the commission-derived `expected_take_rate`; `TakeRate` orders by the measured `avg_take_rate`.
    ExpectedTakeRate,
    Validators,
    DelegationRelationships,
    Incidents,
}

#[derive(Deserialize, Serialize, Debug, Clone, Copy, utoipa::ToSchema)]
pub enum OrderDirection {
    ASC,
    DESC,
}

pub fn directed(ordering: Ordering, order_direction: &OrderDirection) -> Ordering {
    match order_direction {
        OrderDirection::ASC => ordering,
        OrderDirection::DESC => ordering.reverse(),
    }
}
