use rust_decimal::Decimal;
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

/// What a sort column reads off one row. `Missing` is a row with no value for it, which is not a row
/// holding zero.
#[derive(Debug, PartialEq, Eq)]
pub enum SortKey {
    Number(Decimal),
    Text(String),
    Missing,
}

impl From<Option<Decimal>> for SortKey {
    fn from(value: Option<Decimal>) -> Self {
        value.map_or(SortKey::Missing, SortKey::Number)
    }
}

/// `Missing` stays last whichever way the present values go.
pub fn compare_keys(a: &SortKey, b: &SortKey, order_direction: &OrderDirection) -> Ordering {
    match (a, b) {
        (SortKey::Missing, SortKey::Missing) => Ordering::Equal,
        (SortKey::Missing, _) => Ordering::Greater,
        (_, SortKey::Missing) => Ordering::Less,
        (SortKey::Number(x), SortKey::Number(y)) => directed(x.cmp(y), order_direction),
        (SortKey::Text(x), SortKey::Text(y)) => directed(x.cmp(y), order_direction),
        // One column reads one kind off every row, so the kinds never meet.
        (SortKey::Number(_), SortKey::Text(_)) | (SortKey::Text(_), SortKey::Number(_)) => {
            Ordering::Equal
        }
    }
}
