use crate::docs::ClusterInfoSample;
use crate::utils::SLOTS_IN_EPOCH;
use crate::warehouse::Warehouse;
use rust_decimal::prelude::*;
use std::collections::BTreeMap;

const LAMPORTS_PER_SOL: u64 = 1_000_000_000;

/// Rewards per epoch, newest first, skipping the epochs still missing too many
/// amounts to count as fully collected.
fn rewards_by_epoch<E>(
    documents: &BTreeMap<u64, BTreeMap<String, E>>,
    epochs: u64,
    limit_null_count: usize,
    amount: fn(&E) -> Option<Decimal>,
) -> Vec<(u64, f64)> {
    documents
        .iter()
        .rev()
        .filter(|(_, entries)| !entries.is_empty())
        .filter_map(|(epoch, entries)| {
            let missing = entries.values().filter(|e| amount(e).is_none()).count();
            if missing >= limit_null_count {
                return None;
            }
            let total: Decimal = entries.values().filter_map(amount).sum();
            let sol = total / Decimal::from(LAMPORTS_PER_SOL);
            Some((*epoch, sol.to_f64().unwrap_or_default()))
        })
        .take(epochs as usize)
        .collect()
}

pub fn get_mev_rewards(warehouse: &Warehouse, epochs: u64) -> Vec<(u64, f64)> {
    // limit_null_count: expecting there are many entries, we want to have at least 10 filled, then considering data is well loaded
    rewards_by_epoch(&warehouse.mev, epochs, 10, |entry| {
        entry.total_epoch_rewards
    })
}

pub fn get_jito_priority_rewards(warehouse: &Warehouse, epochs: u64) -> Vec<(u64, f64)> {
    // limit_null_count: expecting there are few entries, we want at least one filled to consider data is well loaded
    rewards_by_epoch(&warehouse.priority_fees, epochs, 1, |entry| {
        entry.total_epoch_rewards
    })
}

pub fn get_block_rewards(warehouse: &Warehouse, epochs: u64) -> Vec<(u64, f64)> {
    rewards_by_epoch(&warehouse.block_rewards, epochs, 10, |entry| {
        Some(entry.amount)
    })
}

/// Each estimate carries the nominal it was divided by, so the two can never disagree.
pub fn get_estimated_inflation_rewards(warehouse: &Warehouse, epochs: u64) -> Vec<(u64, f64, f64)> {
    warehouse
        .epochs
        .iter()
        .rev()
        .take(epochs as usize)
        .map(|(epoch, record)| {
            let nominal_epochs_per_year = record.slots_per_year / SLOTS_IN_EPOCH as f64;
            let supply = record.supply.to_f64().unwrap_or_default();
            (
                *epoch,
                supply * record.inflation / LAMPORTS_PER_SOL as f64 / nominal_epochs_per_year,
                record.slots_per_year,
            )
        })
        .collect()
}

/// The running epoch has no `epochs` document yet, so the cluster info samples
/// are the only record of its regime.
pub fn get_running_epoch_slots_per_year(warehouse: &Warehouse) -> Option<(u64, f64)> {
    let last_closed = warehouse.epochs.keys().max().copied().unwrap_or(0);
    let sealed = warehouse.cluster_info.values().flatten();
    let sample: &ClusterInfoSample = sealed
        .chain(warehouse.live.cluster_info.samples.iter())
        .filter(|sample| sample.epoch > last_closed)
        .max_by_key(|sample| (sample.epoch, sample.epoch_slot))?;

    Some((sample.epoch, sample.slots_per_year))
}
