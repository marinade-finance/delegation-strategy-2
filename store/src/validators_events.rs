use crate::directory::Directory;
use crate::docs::{
    epoch_doc_path, merge_events, merge_into, upsert_event, EventEntry, EventsDoc, EVENTS_DIR,
};
use crate::dto::{EventEpochRecord, PerformanceRecord, SettlementRecord};
use crate::utils::DEFAULT_CACHE_EPOCHS;
use crate::warehouse::Warehouse;
use chrono::{DateTime, Utc};
use collect::validators_events::ValidatorsEventsSnapshot;
use log::info;
use rust_decimal::prelude::*;
use serde_yaml;
use std::collections::{BTreeMap, HashMap};

#[derive(Debug, clap::Parser)]
pub struct StoreEventsParams {
    #[arg(long = "snapshot-file")]
    snapshot_path: String,
}

pub async fn store_events(params: StoreEventsParams, directory: &Directory) -> anyhow::Result<()> {
    info!("Storing events (PSR settlements) snapshot...");

    let path = params.snapshot_path;
    let snapshot_file = std::fs::File::open(&path)
        .map_err(|e| anyhow::anyhow!("Failed to open snapshot events file '{path}': {e}"))?;
    let snapshot: ValidatorsEventsSnapshot = serde_yaml::from_reader(snapshot_file)
        .map_err(|e| anyhow::anyhow!("Failed to parse snapshot events file '{path}': {e}"))?;

    let created_at: DateTime<Utc> = snapshot.created_at.parse()?;

    info!(
        "Loaded the events snapshot from epoch {}. Snapshot created at {} loaded at epoch {}, slot index {}. {} records.",
        snapshot.from_epoch,
        created_at,
        snapshot.loaded_at_epoch,
        snapshot.loaded_at_slot_index,
        snapshot.events.len()
    );

    // One snapshot reaches back over several epochs; each has its own document.
    let mut events_by_epoch: BTreeMap<u64, EventsDoc> = Default::default();
    for settlement in snapshot.events.iter() {
        let events = events_by_epoch
            .entry(settlement.epoch)
            .or_default()
            .entry(settlement.vote_account.clone())
            .or_default();
        upsert_event(
            events,
            EventEntry {
                reason: settlement.reason.clone(),
                meta: settlement.meta.clone(),
                amount: Decimal::from(settlement.amount),
                created_at,
                updated_at: created_at,
            },
        );
    }

    for (epoch, events) in events_by_epoch {
        let path = epoch_doc_path(EVENTS_DIR, epoch);
        let count: usize = events.values().map(Vec::len).sum();
        merge_into(directory, &path, events, merge_events).await?;
        info!("Stored {count} events at {path}");
    }

    Ok(())
}

/// `from = true` -> smallest epoch ending on/after `date`; else largest ending on/before.
pub fn resolve_epoch_for_date(
    warehouse: &Warehouse,
    date: DateTime<Utc>,
    from: bool,
) -> Option<u64> {
    match from {
        true => warehouse
            .epochs
            .iter()
            .find(|(_, record)| record.end_at >= date)
            .map(|(epoch, _)| *epoch),
        false => warehouse
            .epochs
            .iter()
            .rev()
            .find(|(_, record)| record.end_at <= date)
            .map(|(epoch, _)| *epoch),
    }
}

pub fn get_events_with_context(
    warehouse: &Warehouse,
    vote_account: &str,
    from_epoch: Option<u64>,
) -> anyhow::Result<Vec<EventEpochRecord>> {
    let from_epoch = from_epoch.unwrap_or_else(|| {
        warehouse
            .last_cluster_epoch()
            .saturating_add(1)
            .saturating_sub(DEFAULT_CACHE_EPOCHS)
    });

    let mut by_epoch: HashMap<u64, EventEpochRecord> = Default::default();
    for (epoch, snapshot) in warehouse.snapshots.range(from_epoch..) {
        let Some(validator) = snapshot.get(vote_account) else {
            continue;
        };
        by_epoch.insert(
            *epoch,
            EventEpochRecord {
                epoch: *epoch,
                epoch_end_at: warehouse.epochs.get(epoch).map(|record| record.end_at),
                performance: Some(PerformanceRecord {
                    blocks_produced: validator.blocks_produced.try_into()?,
                    leader_slots: validator.leader_slots.try_into()?,
                    skip_rate: validator.skip_rate,
                    credits: validator.credits.try_into()?,
                }),
                uptime_pct: validator.uptime_pct,
                downtime: validator.downtime.map(u64::try_from).transpose()?,
                settlements: Vec::new(),
            },
        );
    }

    // Settlement-only epochs (no matching snapshot entry) are preserved with no performance.
    for (epoch, events) in warehouse.events.range(from_epoch..) {
        let Some(settlements) = events.get(vote_account) else {
            continue;
        };
        by_epoch
            .entry(*epoch)
            .or_insert_with(|| EventEpochRecord {
                epoch: *epoch,
                epoch_end_at: None,
                performance: None,
                uptime_pct: None,
                downtime: None,
                settlements: Vec::new(),
            })
            .settlements = settlements
            .iter()
            .map(|event| SettlementRecord {
                reason: event.reason.clone(),
                meta: event.meta.clone(),
                amount: event.amount,
            })
            .collect();
    }

    let mut records: Vec<EventEpochRecord> = by_epoch.into_values().collect();
    records.sort_by_key(|record| record.epoch);

    Ok(records)
}
