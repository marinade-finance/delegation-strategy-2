use crate::context::WrappedContext;
use crate::metrics;
use log::info;
use serde::Serialize;
use std::cmp::Ordering;
use store::dto::CommissionRecord;
use warp::{http::StatusCode, reply::json, Reply};

#[derive(Serialize, Debug)]
pub struct Response {
    commission_changes: Vec<CommissionChange>,
}

#[derive(Serialize, Debug)]
struct CommissionChange {
    identity: String,
    from: u8,
    to: u8,
    epoch: u64,
    epoch_slot: u64,
}

pub async fn handler(context: WrappedContext) -> Result<impl Reply, warp::Rejection> {
    info!("Fetching commission changes");
    let mut commissions = context.read().await.cache.get_all_commissions();
    let mut commission_changes: Vec<_> = Default::default();

    for (identity, commission_records) in commissions.iter_mut() {
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
                        identity: identity.clone(),
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

    Ok(warp::reply::with_status(
        json(&Response { commission_changes }),
        StatusCode::OK,
    ))
}
