use crate::dto::ValidatorBlockReward;
use crate::utils::*;
use chrono::{DateTime, Utc};
use collect::validators_block_rewards::ValidatorsBlockRewardsSnapshot;
use log::info;
use rust_decimal::prelude::*;
use serde_yaml;
use std::collections::{HashMap, HashSet};
use structopt::StructOpt;
use tokio_postgres::types::ToSql;
use tokio_postgres::Client;

pub const VALIDATORS_BLOCK_REWARDS_TABLE: &str = "validators_block_rewards";

#[derive(Debug, StructOpt)]
pub struct StoreBlockRewardsParams {
    #[structopt(long = "snapshot-file")]
    snapshot_path: String,
}

const DEFAULT_CHUNK_SIZE: usize = 500;

pub async fn store_block_rewards(
    params: StoreBlockRewardsParams,
    psql_client: &mut Client,
) -> anyhow::Result<()> {
    info!("Storing block rewards snapshot...");

    let path = params.snapshot_path;
    let snapshot_file = std::fs::File::open(&path)
        .map_err(|e| anyhow::anyhow!("Failed to open snapshot block rewards file '{path}': {e}"))?;
    let snapshot: ValidatorsBlockRewardsSnapshot =
        serde_yaml::from_reader(snapshot_file).map_err(|e| {
            anyhow::anyhow!("Failed to parse snapshot block rewards file '{path}': {e}")
        })?;

    let snapshot_created_at = snapshot.created_at.parse::<DateTime<Utc>>()?;
    let snapshot_epoch = Decimal::from(snapshot.epoch);

    info!(
        "Loaded the snapshot for epoch {}. Snapshot created at {} loaded at epoch {}, slot index {}",
        snapshot_epoch,
        snapshot_created_at,
        snapshot.loaded_at_epoch,
        snapshot.loaded_at_slot_index
    );

    let block_rewards: HashMap<_, _> = snapshot
        .block_rewards
        .iter()
        .map(|r| {
            let key = (r.identity_account.clone(), r.vote_account.clone());
            let reward = ValidatorBlockReward::from_snapshot(r, snapshot.epoch);
            (key, reward)
        })
        .collect();

    let mut updated_identities: HashSet<_> = Default::default();

    info!(
        "Processing snapshot loaded block rewards records {}",
        block_rewards.keys().len()
    );

    let existing_records = get_existing_block_rewards(psql_client, snapshot_epoch).await?;
    let mut updates: u64 = 0;

    for chunk in existing_records.chunks(DEFAULT_CHUNK_SIZE) {
        let mut query = UpdateQueryCombiner::new(
            VALIDATORS_BLOCK_REWARDS_TABLE.to_string(),
            "
            identity_account = u.identity_account,
            vote_account = u.vote_account,
            authorized_voter = u.authorized_voter,
            amount = u.amount,
            epoch = u.epoch,
            updated_at = u.updated_at
            "
                .to_string(),
            "u(
                identity_account,
                vote_account,
                authorized_voter,
                amount,
                epoch,
                updated_at
            )"
                .to_string(),
            format!("{VALIDATORS_BLOCK_REWARDS_TABLE}.identity_account = u.identity_account AND {VALIDATORS_BLOCK_REWARDS_TABLE}.vote_account = u.vote_account AND {VALIDATORS_BLOCK_REWARDS_TABLE}.epoch = u.epoch"),
        );

        for row in chunk {
            let identity_account: &str = row.get("identity_account");
            let vote_account: &str = row.get("vote_account");
            let key = (identity_account.to_string(), vote_account.to_string());

            if let Some(reward) = block_rewards.get(&key) {
                let mut params: Vec<&(dyn ToSql + Sync)> = vec![
                    &reward.identity_account,
                    &reward.vote_account,
                    &reward.authorized_voter,
                    &reward.amount,
                    &reward.epoch,
                    &snapshot_created_at,
                ];
                query.add(
                    &mut params,
                    HashMap::from_iter([
                        (0, "TEXT".into()),                     // identity_account
                        (1, "TEXT".into()),                     // vote_account
                        (2, "TEXT".into()),                     // authorized_voter
                        (3, "NUMERIC".into()),                  // amount
                        (4, "NUMERIC".into()),                  // epoch
                        (5, "TIMESTAMP WITH TIME ZONE".into()), // updated_at
                    ]),
                );
                updated_identities.insert(key);
            }
        }
        updates += query.execute(psql_client).await?.unwrap_or(0);
        info!(
            "Trying to update {} previously existing block rewards records. SQL updated records: {}",
            updated_identities.len(),
            updates
        );
    }

    let block_rewards_to_insert: Vec<_> = block_rewards
        .into_iter()
        .filter(|(key, _)| !updated_identities.contains(key))
        .collect();
    let mut insertions = 0;

    for chunk in block_rewards_to_insert.chunks(DEFAULT_CHUNK_SIZE) {
        let mut query = InsertQueryCombiner::new(
            VALIDATORS_BLOCK_REWARDS_TABLE.to_string(),
            "
        identity_account,
        vote_account,
        authorized_voter,
        amount,
        epoch,
        created_at,
        updated_at
        "
            .to_string(),
        );

        for (key, reward) in chunk {
            if updated_identities.contains(key) {
                continue;
            }
            let mut params: Vec<&(dyn ToSql + Sync)> = vec![
                &reward.identity_account,
                &reward.vote_account,
                &reward.authorized_voter,
                &reward.amount,
                &snapshot_epoch,
                &snapshot_created_at,
                &snapshot_created_at,
            ];
            query.add(&mut params);
        }
        insertions += query.execute(psql_client).await?.unwrap_or(0);
        info!("Inserted new block rewards records {insertions}");
    }

    info!("Stored block rewards snapshot: {updates} updated, {insertions} inserted");

    Ok(())
}

async fn get_existing_block_rewards(
    psql_client: &Client,
    snapshot_epoch: Decimal,
) -> anyhow::Result<Vec<tokio_postgres::Row>> {
    let select_query = format!(
        "SELECT identity_account, vote_account FROM {VALIDATORS_BLOCK_REWARDS_TABLE} WHERE epoch = $1"
    );
    psql_client
        .query(&select_query, &[&snapshot_epoch])
        .await
        .map_err(|e| {
            anyhow::anyhow!(
                "Failed to get existing block rewards from DB for epoch {snapshot_epoch}: {e} [{e:?}]"
            )
        })
}
