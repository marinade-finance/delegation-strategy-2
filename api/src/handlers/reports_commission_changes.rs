use crate::context::WrappedContext;
use log::info;
use serde::Serialize;
use std::cmp::Ordering;
use std::collections::HashMap;
use store::dto::CommissionRecord;
use store::utils::APPLIED_COMMISSION_EPOCH_SLOT;
use warp::{http::StatusCode, reply::json, Reply};

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct ResponseCommissionChanges {
    commission_changes: Vec<CommissionChange>,
}

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct CommissionChange {
    vote_account: String,
    from: u8,
    to: u8,
    epoch: u64,
    epoch_slot: u64,
}

#[utoipa::path(
    get,
    tag = "Validators",
    operation_id = "List commission change reports",
    path = "/reports/commission-changes",
    responses(
        (status = 200, body = ResponseCommissionChanges)
    )
)]
pub async fn handler(context: WrappedContext) -> Result<impl Reply, warp::Rejection> {
    info!("Fetching commission changes");
    let commissions = context.read().await.cache.get_all_commissions();
    let commission_changes = commission_changes(commissions);

    Ok(warp::reply::with_status(
        json(&ResponseCommissionChanges { commission_changes }),
        StatusCode::OK,
    ))
}

// The applied rate lags the advertised one by the epoch_stakes vintage, so interleaving it fakes changes.
fn commission_changes(
    mut commissions: HashMap<String, Vec<CommissionRecord>>,
) -> Vec<CommissionChange> {
    let mut commission_changes: Vec<_> = Default::default();

    for (vote_account, commission_records) in commissions.iter_mut() {
        commission_records.retain(|record| record.epoch_slot != APPLIED_COMMISSION_EPOCH_SLOT);
        commission_records.sort_by(|a: &CommissionRecord, b: &CommissionRecord| {
            match a.epoch.cmp(&b.epoch) {
                Ordering::Equal => a.epoch_slot.cmp(&b.epoch_slot),
                Ordering::Less => Ordering::Less,
                Ordering::Greater => Ordering::Greater,
            }
        });

        let mut previous_commission: Option<u8> = None;
        for record in commission_records {
            if let Some(previous_commission) = previous_commission {
                if record.commission != previous_commission {
                    commission_changes.push(CommissionChange {
                        vote_account: vote_account.clone(),
                        from: previous_commission,
                        to: record.commission,
                        epoch: record.epoch,
                        epoch_slot: record.epoch_slot,
                    });
                }
            }
            previous_commission = Some(record.commission);
        }
    }

    commission_changes.sort_by(|a: &CommissionChange, b: &CommissionChange| {
        match a.epoch.cmp(&b.epoch) {
            Ordering::Equal => a.epoch_slot.cmp(&b.epoch_slot),
            Ordering::Less => Ordering::Less,
            Ordering::Greater => Ordering::Greater,
        }
    });
    commission_changes
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn record(epoch: u64, epoch_slot: u64, commission: u8) -> CommissionRecord {
        CommissionRecord {
            epoch,
            epoch_start_at: Utc::now(),
            epoch_end_at: Utc::now(),
            epoch_slot,
            commission,
            created_at: Utc::now(),
            commission_bps: None,
        }
    }

    #[test]
    fn a_lagging_applied_rate_does_not_turn_one_cut_into_a_sawtooth() {
        let applied = APPLIED_COMMISSION_EPOCH_SLOT;
        let commissions = HashMap::from([(
            "vote".to_string(),
            vec![
                record(1099, 100, 10),
                record(1100, 1000, 5),
                record(1100, applied, 10),
                record(1101, 50, 5),
                record(1101, applied, 10),
                record(1102, 50, 5),
                record(1102, applied, 5),
            ],
        )]);

        let changes: Vec<_> = commission_changes(commissions)
            .into_iter()
            .map(|change| (change.from, change.to, change.epoch, change.epoch_slot))
            .collect();
        assert_eq!(changes, vec![(10, 5, 1100, 1000)]);
    }
}
