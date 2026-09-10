//! What the store already holds, which is what every check compares against.

use serde::Deserialize;
use std::collections::BTreeMap;
use store::directory::Directory;

/// Only the two fields every per-epoch entry carries; the rest of the document
/// is not this check's business.
#[derive(Debug, Deserialize)]
struct StoredSample {
    epoch_slot: u64,
}

/// The newest epoch with a document under `dir`.
pub async fn last_epoch(directory: &Directory, dir: &str) -> anyhow::Result<Option<u64>> {
    Ok(directory
        .list(dir)
        .await?
        .into_iter()
        .filter_map(|entry| entry.name.parse::<u64>().ok())
        .max())
}

/// How far into `epoch` the last write to `dir` was made.
pub async fn last_epoch_slot(
    directory: &Directory,
    dir: &str,
    epoch: u64,
) -> anyhow::Result<Option<u64>> {
    let path = store::docs::epoch_doc_path(dir, epoch);
    let Some(document) = directory
        .get::<BTreeMap<String, StoredSample>>(&path)
        .await?
    else {
        return Ok(None);
    };

    Ok(document.body.values().map(|sample| sample.epoch_slot).max())
}
