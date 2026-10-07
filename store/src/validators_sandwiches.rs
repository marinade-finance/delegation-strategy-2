use crate::directory::Directory;
use crate::docs::{
    epoch_doc_path, merge_into, merge_sandwiches, SandwichEntry, SandwichesDoc, SANDWICHES_DIR,
};
use crate::incidents::{cluster_sandwich_medians, EpochSandwiches};
use crate::warehouse::Warehouse;
use chrono::{DateTime, Utc};
use clap::Parser;
use collect::validators_sandwiches::ValidatorsSandwichesSnapshot;
use log::info;
use serde_yaml;
use std::collections::{BTreeMap, HashMap};
use std::ops::RangeInclusive;

#[derive(Debug, Parser)]
pub struct StoreSandwichesParams {
    #[arg(long = "snapshot-file")]
    snapshot_path: String,
}

pub async fn store_sandwiches(
    params: StoreSandwichesParams,
    directory: &Directory,
) -> anyhow::Result<()> {
    info!("Storing validator sandwich rates snapshot...");

    let path = params.snapshot_path;
    let snapshot_file = std::fs::File::open(&path)
        .map_err(|e| anyhow::anyhow!("Failed to open snapshot sandwiches file '{path}': {e}"))?;
    let snapshot: ValidatorsSandwichesSnapshot = serde_yaml::from_reader(snapshot_file)
        .map_err(|e| anyhow::anyhow!("Failed to parse snapshot sandwiches file '{path}': {e}"))?;

    let created_at: DateTime<Utc> = snapshot.created_at.parse()?;

    info!(
        "Loaded the sandwiches snapshot from epoch {}. Snapshot created at {} loaded at epoch {}, slot index {}. {} records.",
        snapshot.from_epoch,
        created_at,
        snapshot.loaded_at_epoch,
        snapshot.loaded_at_slot_index,
        snapshot.sandwiches.len()
    );

    // One snapshot reaches back over several epochs; each has its own
    // document. The last row wins where the snapshot names a pair twice.
    let mut sandwiches_by_epoch: BTreeMap<u64, SandwichesDoc> = Default::default();
    for sandwich in snapshot.sandwiches.iter() {
        sandwiches_by_epoch
            .entry(sandwich.epoch)
            .or_default()
            .insert(
                sandwich.vote_account.clone(),
                SandwichEntry {
                    blocks_produced: sandwich.blocks_produced,
                    blocks_with_sandwiches: sandwich.blocks_with_sandwiches,
                    sandwich_rate_30d: sandwich.sandwich_rate_30d,
                    sandwich_rate_60d: sandwich.sandwich_rate_60d,
                    created_at,
                    updated_at: created_at,
                },
            );
    }

    let mut total = 0;
    for (epoch, sandwiches) in sandwiches_by_epoch {
        let path = epoch_doc_path(SANDWICHES_DIR, epoch);
        total += sandwiches.len();
        merge_into(directory, &path, sandwiches, merge_sandwiches).await?;
        info!("Stored sandwich records at {path}");
    }

    info!("Stored sandwiches snapshot: {total} total records");

    Ok(())
}

/// Keyed by vote account, over the sealed epochs of the range: the epoch still
/// running has no start or end to report, so it stays out.
pub fn load_validator_sandwiches(
    warehouse: &Warehouse,
    epochs: RangeInclusive<u64>,
) -> HashMap<String, Vec<EpochSandwiches>> {
    let mut loaded: Vec<(String, EpochSandwiches)> = Vec::new();
    for (epoch, sandwiches) in warehouse.sandwiches.range(epochs) {
        let Some(record) = warehouse.epochs.get(epoch) else {
            continue;
        };
        for (vote_account, entry) in sandwiches.iter() {
            loaded.push((
                vote_account.clone(),
                EpochSandwiches {
                    epoch: *epoch,
                    epoch_start_at: record.start_at,
                    epoch_end_at: record.end_at,
                    blocks_produced: entry.blocks_produced,
                    blocks_with_sandwiches: entry.blocks_with_sandwiches,
                    sandwich_rate_30d: entry.sandwich_rate_30d,
                    sandwich_rate_60d: entry.sandwich_rate_60d,
                    cluster_median_rate: 0.0,
                },
            ));
        }
    }

    let medians = cluster_sandwich_medians(loaded.iter().map(|(_, epoch)| epoch));
    let mut sandwiches: HashMap<String, Vec<EpochSandwiches>> = Default::default();
    for (vote_account, mut epoch) in loaded {
        epoch.cluster_median_rate = medians.get(&epoch.epoch).copied().unwrap_or(0.0);
        sandwiches.entry(vote_account).or_default().push(epoch);
    }

    sandwiches
}
