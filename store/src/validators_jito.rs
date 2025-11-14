use crate::dto::{
    JitoMevRecord, JitoPriorityFeeRecord, JitoRecord, ValidatorJitoMEVInfo,
    ValidatorJitoPriorityFeeInfo,
};
use crate::utils::*;
use chrono::{DateTime, Utc};
use collect::validators_jito::{JitoAccountType, JitoSnapshot};
use log::info;
use rust_decimal::prelude::*;
use serde_yaml;
use std::collections::{HashMap, HashSet};
use structopt::StructOpt;
use tokio_postgres::types::ToSql;
use tokio_postgres::Client;

#[derive(Debug, StructOpt)]
pub struct StoreJitoParams {
    #[structopt(long = "snapshot-file")]
    snapshot_path: String,
}

const DEFAULT_CHUNK_SIZE: usize = 500;

pub async fn store_jito(
    params: StoreJitoParams,
    psql_client: &mut Client,
    account_type: JitoAccountType,
) -> anyhow::Result<()> {
    info!("Storing JITO account {account_type} snapshot...");

    let path = params.snapshot_path;
    let snapshot_file = std::fs::File::open(&path)
        .map_err(|e| anyhow::anyhow!("Failed to open snapshot file '{path}': {e}"))?;
    let snapshot: JitoSnapshot = serde_yaml::from_reader(snapshot_file)
        .map_err(|e| anyhow::anyhow!("Failed to parse snapshot file '{path}': {e}",))?;
    let snapshot_created_at: DateTime<Utc> = snapshot.created_at.parse()?;
    let snapshot_loaded_at_slot_index = Decimal::from(snapshot.loaded_at_slot_index);
    let snapshot_epoch = Decimal::from(snapshot.epoch);

    info!(
        "Loaded the snapshot for epoch {}. Snapshot created at {} loaded at epoch {}, slot index {}",
        snapshot_epoch,
        snapshot_created_at,
        snapshot.loaded_at_epoch,
        snapshot_loaded_at_slot_index
    );

    match account_type {
        JitoAccountType::MevTipDistribution => {
            let validators_jito_mev: HashMap<_, _> = snapshot
                .get_mev_validators()
                .iter()
                .map(|v| (v.0.clone(), ValidatorJitoMEVInfo::from_snapshot(v.1)))
                .collect();
            store_mev(
                psql_client,
                snapshot_epoch,
                snapshot_created_at,
                snapshot_loaded_at_slot_index,
                account_type.db_table_name(),
                validators_jito_mev,
            )
            .await
        }
        JitoAccountType::PriorityFeeDistribution => {
            let validators_jito_priority_fee: HashMap<_, _> = snapshot
                .get_priority_fee_validators()
                .iter()
                .map(|v| {
                    (
                        v.0.clone(),
                        ValidatorJitoPriorityFeeInfo::from_snapshot(v.1),
                    )
                })
                .collect();
            store_priority_fee(
                psql_client,
                snapshot_epoch,
                snapshot_created_at,
                snapshot_loaded_at_slot_index,
                account_type.db_table_name(),
                validators_jito_priority_fee,
            )
            .await
        }
    }
}

async fn get_existing_vote_accounts(
    psql_client: &Client,
    db_table: &str,
    snapshot_epoch: Decimal,
) -> anyhow::Result<Vec<tokio_postgres::Row>> {
    let select_query = format!("SELECT vote_account FROM {db_table} WHERE epoch = $1");
    psql_client
        .query(&select_query, &[&snapshot_epoch])
        .await
        .map_err(|e| {
            anyhow::anyhow!(
                "Failed to get existing vote accounts from DB table {db_table} for epoch {snapshot_epoch}: {e} [{e:?}]"
            )
        })
}

async fn store_mev(
    psql_client: &mut Client,
    snapshot_epoch: Decimal,
    snapshot_created_at: DateTime<Utc>,
    snapshot_loaded_at_slot_index: Decimal,
    db_table: &str,
    validators_mev: HashMap<String, ValidatorJitoMEVInfo>,
) -> anyhow::Result<()> {
    let mut updated_identities: HashSet<_> = Default::default();
    info!(
        "Processing snapshot loaded MEV records {}",
        validators_mev.keys().len()
    );
    let existing_vote_accounts =
        get_existing_vote_accounts(psql_client, db_table, snapshot_epoch).await?;
    let mut updates: u64 = 0;

    for chunk in existing_vote_accounts.chunks(DEFAULT_CHUNK_SIZE) {
        let mut query = UpdateQueryCombiner::new(
            db_table.to_string(),
            "
            vote_account = u.vote_account,
            mev_commission = u.mev_commission,
            total_epoch_rewards = u.total_epoch_rewards,
            claimed_epoch_rewards = u.claimed_epoch_rewards,
            total_epoch_claimants = u.total_epoch_claimants,
            epoch_active_claimants = u.epoch_active_claimants,
            epoch_slot = u.epoch_slot,
            epoch = u.epoch
            "
            .to_string(),
            "u(
                vote_account,
                mev_commission,
                total_epoch_rewards,
                claimed_epoch_rewards,
                total_epoch_claimants,
                epoch_active_claimants,
                epoch_slot,
                epoch
            )"
            .to_string(),
            format!("{db_table}.vote_account = u.vote_account AND {db_table}.epoch = u.epoch"),
        );
        for row in chunk {
            let vote_account: &str = row.get("vote_account");

            if let Some(v) = validators_mev.get(vote_account) {
                let mut params: Vec<&(dyn ToSql + Sync)> = vec![
                    &v.vote_account,
                    &v.mev_commission,
                    &v.total_epoch_rewards,
                    &v.claimed_epoch_rewards,
                    &v.total_epoch_claimants,
                    &v.epoch_active_claimants,
                    &snapshot_loaded_at_slot_index,
                    &v.epoch,
                    &snapshot_created_at,
                ];
                query.add(
                    &mut params,
                    HashMap::from_iter([
                        (0, "TEXT".into()),                     // vote_account
                        (1, "INTEGER".into()),                  // mev_commission
                        (2, "NUMERIC".into()),                  // total_epoch_rewards
                        (3, "NUMERIC".into()),                  // claimed_epoch_rewards
                        (4, "INTEGER".into()),                  // total_epoch_claimants
                        (5, "INTEGER".into()),                  // epoch_active_claimants
                        (6, "NUMERIC".into()),                  // snapshot_loaded_at_slot_index
                        (7, "DECIMAL".into()),                  // epoch
                        (8, "TIMESTAMP WITH TIME ZONE".into()), // snapshot_created_at
                    ]),
                );
                updated_identities.insert(vote_account.to_string());
            }
        }
        updates += query.execute(psql_client).await?.unwrap_or(0);
        info!(
            "Trying to update {} previously existing MEV records. SQL updated records: {}",
            updated_identities.len(),
            updates
        );
    }
    let validators_mev_executions: Vec<_> = validators_mev
        .into_iter()
        .filter(|(vote_account, _)| !updated_identities.contains(vote_account))
        .collect();
    let mut insertions = 0;

    for chunk in validators_mev_executions.chunks(DEFAULT_CHUNK_SIZE) {
        let mut query = InsertQueryCombiner::new(
            db_table.to_string(),
            "
        vote_account,
        mev_commission,
        total_epoch_rewards,
        claimed_epoch_rewards,
        total_epoch_claimants,
        epoch_active_claimants,
        epoch_slot,
        epoch,
        created_at
        "
            .to_string(),
        );

        for (vote_account, v) in chunk {
            if updated_identities.contains(vote_account) {
                continue;
            }
            let mut params: Vec<&(dyn ToSql + Sync)> = vec![
                &v.vote_account,
                &v.mev_commission,
                &v.total_epoch_rewards,
                &v.claimed_epoch_rewards,
                &v.total_epoch_claimants,
                &v.epoch_active_claimants,
                &snapshot_loaded_at_slot_index,
                &snapshot_epoch,
                &snapshot_created_at,
            ];
            query.add(&mut params);
        }
        insertions += query.execute(psql_client).await?.unwrap_or(0);
        info!("Inserted new new MEV records {insertions}");
    }

    info!("Stored MEV snapshot: {updates} updated, {insertions} inserted");

    Ok(())
}

async fn store_priority_fee(
    psql_client: &mut Client,
    snapshot_epoch: Decimal,
    snapshot_created_at: DateTime<Utc>,
    snapshot_loaded_at_slot_index: Decimal,
    db_table: &str,
    validators_priority_fee: HashMap<String, ValidatorJitoPriorityFeeInfo>,
) -> anyhow::Result<()> {
    let mut updated_identities: HashSet<_> = Default::default();
    info!(
        "Processing snapshot loaded priority fee records {}",
        validators_priority_fee.keys().len()
    );
    let existing_vote_accounts =
        get_existing_vote_accounts(psql_client, db_table, snapshot_epoch).await?;
    let mut updates: u64 = 0;

    for chunk in existing_vote_accounts.chunks(DEFAULT_CHUNK_SIZE) {
        let mut query = UpdateQueryCombiner::new(
            db_table.to_string(),
            "
            vote_account = u.vote_account,
            validator_commission = u.validator_commission,
            total_lamports_transferred = u.total_lamports_transferred,
            total_epoch_rewards = u.total_epoch_rewards,
            claimed_epoch_rewards = u.claimed_epoch_rewards,
            total_epoch_claimants = u.total_epoch_claimants,
            epoch_active_claimants = u.epoch_active_claimants,
            epoch_slot = u.epoch_slot,
            epoch = u.epoch
            "
            .to_string(),
            "u(
                vote_account,
                validator_commission,
                total_lamports_transferred,
                total_epoch_rewards,
                claimed_epoch_rewards,
                total_epoch_claimants,
                epoch_active_claimants,
                epoch_slot,
                epoch
            )"
            .to_string(),
            format!("{db_table}.vote_account = u.vote_account AND {db_table}.epoch = u.epoch"),
        );
        for row in chunk {
            let vote_account: &str = row.get("vote_account");

            if let Some(v) = validators_priority_fee.get(vote_account) {
                let mut params: Vec<&(dyn ToSql + Sync)> = vec![
                    &v.vote_account,
                    &v.priority_commission,
                    &v.total_lamports_transferred,
                    &v.total_epoch_rewards,
                    &v.claimed_epoch_rewards,
                    &v.total_epoch_claimants,
                    &v.epoch_active_claimants,
                    &snapshot_loaded_at_slot_index,
                    &v.epoch,
                    &snapshot_created_at,
                ];
                query.add(
                    &mut params,
                    HashMap::from_iter([
                        (0, "TEXT".into()),                     // vote_account
                        (1, "INTEGER".into()),                  // validator_commission
                        (2, "NUMERIC".into()),                  // total_lamports_transferred
                        (3, "NUMERIC".into()),                  // total_epoch_rewards
                        (4, "NUMERIC".into()),                  // claimed_epoch_rewards
                        (5, "INTEGER".into()),                  // total_epoch_claimants
                        (6, "INTEGER".into()),                  // epoch_active_claimants
                        (7, "NUMERIC".into()),                  // snapshot_loaded_at_slot_index
                        (8, "NUMERIC".into()),                  // epoch
                        (9, "TIMESTAMP WITH TIME ZONE".into()), // snapshot_created_at
                    ]),
                );
                updated_identities.insert(vote_account.to_string());
            }
        }
        updates += query.execute(psql_client).await?.unwrap_or(0);
        info!(
            "Trying to update {} previously existing priority fee records. SQL updated records: {}",
            updated_identities.len(),
            updates
        );
    }
    let validators_priority_fee_executions: Vec<_> = validators_priority_fee
        .into_iter()
        .filter(|(vote_account, _)| !updated_identities.contains(vote_account))
        .collect();
    let mut insertions = 0;

    for chunk in validators_priority_fee_executions.chunks(DEFAULT_CHUNK_SIZE) {
        let mut query = InsertQueryCombiner::new(
            db_table.to_string(),
            "
        vote_account,
        validator_commission,
        total_lamports_transferred,
        total_epoch_rewards,
        claimed_epoch_rewards,
        total_epoch_claimants,
        epoch_active_claimants,
        epoch_slot,
        epoch,
        created_at
        "
            .to_string(),
        );

        for (vote_account, v) in chunk {
            if updated_identities.contains(vote_account) {
                continue;
            }
            let mut params: Vec<&(dyn ToSql + Sync)> = vec![
                &v.vote_account,
                &v.priority_commission,
                &v.total_lamports_transferred,
                &v.total_epoch_rewards,
                &v.claimed_epoch_rewards,
                &v.total_epoch_claimants,
                &v.epoch_active_claimants,
                &snapshot_loaded_at_slot_index,
                &snapshot_epoch,
                &snapshot_created_at,
            ];
            query.add(&mut params);
        }
        insertions += query.execute(psql_client).await?.unwrap_or(0);
        info!("Inserted new new priority fee records {insertions}");
    }

    info!("Stored priority fee snapshot: {updates} updated, {insertions} inserted");

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
