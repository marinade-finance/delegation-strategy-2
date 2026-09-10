use crate::directory::Directory;
use crate::docs::{
    epoch_doc_path, merge_into, replace_entries, MevDoc, MevEntry, PriorityFeeDoc,
    PriorityFeeEntry, MEV_DIR, PRIORITY_FEE_DIR,
};
use crate::dto::{
    JitoMevRecord, JitoPriorityFeeRecord, JitoRecord, ValidatorJitoMEVInfo,
    ValidatorJitoPriorityFeeInfo,
};
use chrono::{DateTime, Utc};
use collect::validators_jito::{JitoAccountType, JitoSnapshot};
use log::info;
use rust_decimal::prelude::*;
use serde_yaml;
use std::collections::{HashMap, HashSet};
use structopt::StructOpt;
use tokio_postgres::Client;

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

async fn get_last_validator_info<T, F>(
    psql_client: &Client,
    epochs: u64,
    db_table: &str,
    select_fields: &str,
    row_mapper: F,
) -> anyhow::Result<Vec<T>>
where
    F: Fn(&tokio_postgres::Row) -> anyhow::Result<T>,
{
    let query = format!(
        "WITH cluster AS (
            SELECT MAX(epoch) AS last_epoch
            FROM cluster_info
        ),
        filtered_data AS (
            SELECT
                {select_fields},
                ROW_NUMBER() OVER (PARTITION BY vote_account ORDER BY epoch DESC) AS rn
            FROM {db_table}
            CROSS JOIN cluster
            WHERE epoch > cluster.last_epoch - $1::NUMERIC
        )
        SELECT {select_fields}
        FROM filtered_data
        WHERE rn = 1;"
    );

    let rows = psql_client.query(&query, &[&Decimal::from(epochs)]).await?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row_mapper(&row)?);
    }

    Ok(results)
}

pub async fn get_last_mev_info(
    psql_client: &Client,
    epochs: u64,
) -> anyhow::Result<Vec<JitoMevRecord>> {
    get_last_validator_info(
        psql_client,
        epochs,
        JitoAccountType::MevTipDistribution.db_table_name(),
        "vote_account, mev_commission, epoch",
        |row| {
            Ok(JitoMevRecord {
                epoch: row.get::<_, Decimal>("epoch"),
                mev_commission_bps: row.get::<_, i32>("mev_commission"),
                vote_account: row.get("vote_account"),
            })
        },
    )
    .await
}

async fn get_last_priority_fee_info(
    psql_client: &Client,
    epochs: u64,
) -> anyhow::Result<Vec<JitoPriorityFeeRecord>> {
    get_last_validator_info(
        psql_client,
        epochs,
        JitoAccountType::PriorityFeeDistribution.db_table_name(),
        "vote_account, validator_commission, total_lamports_transferred, epoch",
        |row| {
            Ok(JitoPriorityFeeRecord {
                epoch: row.get::<_, Decimal>("epoch"),
                priority_commission_bps: row.get::<_, i32>("validator_commission"),
                vote_account: row.get("vote_account"),
                total_lamports_transferred: row
                    .get::<_, Decimal>("total_lamports_transferred")
                    .try_into()?,
            })
        },
    )
    .await
}

pub async fn get_last_jito_info(
    psql_client: &Client,
    epochs: u64,
) -> anyhow::Result<Vec<JitoRecord>> {
    let (mev_records, priority_fee_records) = tokio::try_join!(
        get_last_mev_info(psql_client, epochs),
        get_last_priority_fee_info(psql_client, epochs)
    )?;

    // Combine the two records into a single JitoRecord (combine by vote_account and epoch)
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
