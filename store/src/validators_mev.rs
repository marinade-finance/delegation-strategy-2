use crate::dto::{MevRecord, ValidatorMEVInfo};
use crate::utils::*;
use chrono::{DateTime, Utc};
use collect::validators_mev::Snapshot;
use log::info;
use rust_decimal::prelude::*;
use serde_yaml;
use std::collections::{HashMap, HashSet};
use structopt::StructOpt;
use tokio_postgres::types::ToSql;
use tokio_postgres::Client;

#[derive(Debug, StructOpt)]
pub struct StoreMevOptions {
    #[structopt(long = "snapshot-file")]
    snapshot_path: String,
}

const DEFAULT_CHUNK_SIZE: usize = 500;

pub async fn store_mev(
    options: StoreMevOptions,
    mut psql_client: &mut Client,
) -> anyhow::Result<()> {
    info!("Storing MEV snapshot...");

    let snapshot_file = std::fs::File::open(options.snapshot_path)?;
    let snapshot: Snapshot = serde_yaml::from_reader(snapshot_file)?;
    let snapshot_created_at = snapshot.created_at.parse::<DateTime<Utc>>().unwrap();

    let validators_mev: HashMap<_, _> = snapshot
        .validators
        .iter()
        .map(|v| (v.0.clone(), ValidatorMEVInfo::new_from_snapshot(v.1)))
        .collect();
    let snapshot_epoch: i32 = (snapshot.epoch - 1) as i32;
    let snapshot_epoch_slot: Decimal = snapshot.epoch_slot.into();
    let mut updated_identities: HashSet<_> = Default::default();

    info!("Loaded the snapshot");

    for chunk in psql_client
        .query(
            "
        SELECT vote_account
        FROM mev
        WHERE epoch = $1
    ",
            &[&snapshot_epoch],
        )
        .await?
        .chunks(DEFAULT_CHUNK_SIZE)
    {
        let mut query = UpdateQueryCombiner::new(
            "mev".to_string(),
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
            "mev.vote_account = u.vote_account AND mev.epoch = u.epoch".to_string(),
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
                    &snapshot_epoch_slot,
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
                        (6, "NUMERIC".into()),                  // snapshot_epoch_slot
                        (7, "INTEGER".into()),                  // epoch
                        (8, "TIMESTAMP WITH TIME ZONE".into()), // snapshot_created_at
                    ]),
                );
                updated_identities.insert(vote_account.to_string());
            }
        }
        query.execute(&mut psql_client).await?;
        info!(
            "Updated previously existing MEV records: {}",
            updated_identities.len()
        );
    }
    let validators_mev: Vec<_> = validators_mev
        .into_iter()
        .filter(|(vote_account, _)| !updated_identities.contains(vote_account))
        .collect();
    let mut insertions = 0;

    for chunk in validators_mev.chunks(DEFAULT_CHUNK_SIZE) {
        let mut query = InsertQueryCombiner::new(
            "mev".to_string(),
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
                &snapshot_epoch_slot,
                &snapshot_epoch,
                &snapshot_created_at,
            ];
            query.add(&mut params);
        }
        insertions += query.execute(&mut psql_client).await?.unwrap_or(0);
        info!("Stored {} new MEV records", insertions);
    }

    Ok(())
}

pub async fn get_last_mev_info(
    psql_client: &Client,
    epochs: u64,
) -> anyhow::Result<Vec<MevRecord>> {
    let rows = psql_client
        .query(
            "
            WITH cluster AS (
                SELECT MAX(epoch) as last_epoch 
                FROM cluster_info
            ),
            filtered_mev AS (
                SELECT
                    vote_account, 
                    mev_commission,
                    epoch,
                    ROW_NUMBER() OVER (PARTITION BY vote_account ORDER BY epoch DESC) as rn
                FROM mev
                CROSS JOIN cluster
                WHERE epoch > cluster.last_epoch - $1::NUMERIC
            )
            SELECT
                vote_account,
                mev_commission,
                epoch
            FROM filtered_mev
            WHERE rn = 1;",
            &[&Decimal::from(epochs)],
        )
        .await?;

    let mut mev_info: Vec<MevRecord> = vec![];
    for row in rows {
        mev_info.push(MevRecord {
            epoch: row.get::<_, i32>("epoch").try_into()?,
            mev_commission_bps: row.get::<_, i32>("mev_commission").try_into()?,
            vote_account: row.get("vote_account"),
        })
    }

    Ok(mev_info)
}
