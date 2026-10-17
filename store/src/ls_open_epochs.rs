use crate::directory::Directory;
use crate::docs::{EPOCHS_DIR, SNAPSHOT_DIR};
use clap::Parser;
use log::info;
use std::collections::HashSet;

#[derive(Debug, Parser)]
pub struct LsOpenEpochsParams {}

pub async fn list_open_epochs(directory: &Directory) -> anyhow::Result<()> {
    info!("Finding open epochs...");

    let epochs = open_epochs(directory).await?;
    for epoch in epochs.iter() {
        println!("{epoch}");
        info!("Open epoch: {epoch}");
    }

    info!("Found open epochs: {}", epochs.len());

    Ok(())
}

/// An epoch is open while it has a snapshot but no `epochs` document.
pub async fn open_epochs(directory: &Directory) -> anyhow::Result<Vec<String>> {
    let sealed: HashSet<String> = directory
        .list(EPOCHS_DIR)
        .await?
        .into_iter()
        .map(|entry| entry.name)
        .collect();

    Ok(directory
        .list(SNAPSHOT_DIR)
        .await?
        .into_iter()
        .map(|entry| entry.name)
        .filter(|epoch| !sealed.contains(epoch))
        .collect())
}
