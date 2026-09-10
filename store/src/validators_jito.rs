use crate::directory::Directory;
use crate::docs::{
    epoch_doc_path, merge_into, replace_entries, MevDoc, MevEntry, PriorityFeeDoc,
    PriorityFeeEntry, MEV_DIR, PRIORITY_FEE_DIR,
};
use crate::dto::{
    JitoMevRecord, JitoPriorityFeeRecord, JitoRecord, ValidatorJitoMEVInfo,
    ValidatorJitoPriorityFeeInfo,
};
use crate::warehouse::Warehouse;
use chrono::{DateTime, Utc};
use collect::validators_jito::{JitoAccountType, JitoSnapshot};
use log::info;
use rust_decimal::prelude::*;
use serde_yaml;
use std::collections::{HashMap, HashSet};
use structopt::StructOpt;

#[derive(Debug, StructOpt)]
pub struct StoreJitoParams {
    #[structopt(long = "snapshot-file")]
    snapshot_path: String,
}

pub async fn store_jito(
    params: StoreJitoParams,
    directory: &Directory,
    account_type: JitoAccountType,
) -> anyhow::Result<()> {
    info!("Storing JITO account {account_type} snapshot...");

    let path = params.snapshot_path;
    let snapshot_file = std::fs::File::open(&path)
        .map_err(|e| anyhow::anyhow!("Failed to open snapshot file '{path}': {e}"))?;
    let snapshot: JitoSnapshot = serde_yaml::from_reader(snapshot_file)
        .map_err(|e| anyhow::anyhow!("Failed to parse snapshot file '{path}': {e}",))?;
    let created_at: DateTime<Utc> = snapshot.created_at.parse()?;
    let epoch_slot = Decimal::from(snapshot.loaded_at_slot_index);

    info!(
        "Loaded the snapshot for epoch {}. Snapshot created at {} loaded at epoch {}, slot index {}",
        snapshot.epoch, created_at, snapshot.loaded_at_epoch, epoch_slot
    );

    match account_type {
        JitoAccountType::MevTipDistribution => {
            let mev: MevDoc = snapshot
                .get_mev_validators()
                .iter()
                .map(|(vote_account, v)| {
                    let info = ValidatorJitoMEVInfo::from_snapshot(v);
                    (
                        vote_account.clone(),
                        MevEntry::new(&info, epoch_slot, created_at),
                    )
                })
                .collect();
            let path = epoch_doc_path(MEV_DIR, snapshot.epoch);
            info!("Processing snapshot loaded MEV records {}", mev.len());
            merge_into(directory, &path, mev, replace_entries).await?;
            info!("Stored the MEV snapshot at {path}");
        }
        JitoAccountType::PriorityFeeDistribution => {
            let priority_fees: PriorityFeeDoc = snapshot
                .get_priority_fee_validators()
                .iter()
                .map(|(vote_account, v)| {
                    let info = ValidatorJitoPriorityFeeInfo::from_snapshot(v);
                    (
                        vote_account.clone(),
                        PriorityFeeEntry::new(&info, epoch_slot, created_at),
                    )
                })
                .collect();
            let path = epoch_doc_path(PRIORITY_FEE_DIR, snapshot.epoch);
            info!(
                "Processing snapshot loaded priority fee records {}",
                priority_fees.len()
            );
            merge_into(directory, &path, priority_fees, replace_entries).await?;
            info!("Stored the priority fee snapshot at {path}");
        }
    }

    Ok(())
}

/// The latest observation per vote account inside the last `epochs` epochs,
/// which the per-epoch documents hold one of each.
fn last_per_vote_account<'a, T, E>(
    documents: &'a std::collections::BTreeMap<u64, std::collections::BTreeMap<String, E>>,
    warehouse: &Warehouse,
    epochs: u64,
    map: impl Fn(&'a E) -> anyhow::Result<T>,
) -> anyhow::Result<Vec<T>> {
    let first_epoch = warehouse.window_start(epochs);
    let mut latest: HashMap<&String, (u64, &E)> = Default::default();

    for (epoch, entries) in documents.range(first_epoch..) {
        for (vote_account, entry) in entries.iter() {
            match latest.get(vote_account) {
                Some((seen, _)) if seen >= epoch => {}
                _ => {
                    latest.insert(vote_account, (*epoch, entry));
                }
            }
        }
    }

    latest.into_values().map(|(_, entry)| map(entry)).collect()
}

pub fn get_last_mev_info(warehouse: &Warehouse, epochs: u64) -> anyhow::Result<Vec<JitoMevRecord>> {
    last_per_vote_account(&warehouse.mev, warehouse, epochs, |entry| {
        Ok(JitoMevRecord {
            epoch: entry.epoch,
            mev_commission_bps: entry.mev_commission,
            vote_account: entry.vote_account.clone(),
        })
    })
}

fn get_last_priority_fee_info(
    warehouse: &Warehouse,
    epochs: u64,
) -> anyhow::Result<Vec<JitoPriorityFeeRecord>> {
    last_per_vote_account(&warehouse.priority_fees, warehouse, epochs, |entry| {
        Ok(JitoPriorityFeeRecord {
            epoch: entry.epoch,
            priority_commission_bps: entry.priority_commission,
            vote_account: entry.vote_account.clone(),
            total_lamports_transferred: entry.total_lamports_transferred.try_into()?,
        })
    })
}

pub fn get_last_jito_info(warehouse: &Warehouse, epochs: u64) -> anyhow::Result<Vec<JitoRecord>> {
    let mev_records = get_last_mev_info(warehouse, epochs)?;
    let priority_fee_records = get_last_priority_fee_info(warehouse, epochs)?;

    // Keyed on (vote_account, epoch): the two distribution accounts resolve their last epoch separately.
    let mut mev_map: HashMap<(String, Decimal), JitoMevRecord> = HashMap::new();
    for record in mev_records {
        let key = (record.vote_account.clone(), record.epoch);
        mev_map.insert(key, record);
    }
    let mut priority_fee_map: HashMap<(String, Decimal), JitoPriorityFeeRecord> = HashMap::new();
    for record in priority_fee_records {
        let key = (record.vote_account.clone(), record.epoch);
        priority_fee_map.insert(key, record);
    }
    let mut all_keys: HashSet<(String, Decimal)> = HashSet::new();
    all_keys.extend(mev_map.keys().cloned());
    all_keys.extend(priority_fee_map.keys().cloned());

    let mut result = Vec::new();

    for (vote_account, epoch) in all_keys {
        let mev_commission_bps = mev_map
            .get(&(vote_account.clone(), epoch))
            .map(|r| r.mev_commission_bps);

        let (priority_commission_bps, total_lamports_transferred) = priority_fee_map
            .get(&(vote_account.clone(), epoch))
            .map(|r| {
                (
                    Some(r.priority_commission_bps),
                    Some(r.total_lamports_transferred),
                )
            })
            .unwrap_or((None, None));

        result.push(JitoRecord {
            vote_account,
            epoch,
            mev_commission_bps,
            priority_commission_bps,
            priority_total_lamports_transferred: total_lamports_transferred,
        });
    }

    Ok(result)
}
