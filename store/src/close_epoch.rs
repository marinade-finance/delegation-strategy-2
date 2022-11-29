use crate::utils::UpdateQueryCombiner;
use chrono::{DateTime, Utc};
use collect::validators_performance::{ClusterInflation, ValidatorsPerformanceSnapshot};
use log::info;
use rust_decimal::prelude::*;
use serde_yaml;
use std::collections::{HashMap, HashSet};
use structopt::StructOpt;
use tokio_postgres::{types::ToSql, Client};

#[derive(Debug, StructOpt)]
pub struct CloseEpochOptions {
    #[structopt(long = "snapshot-file")]
    snapshot_path: String,
}

const DEFAULT_CHUNK_SIZE: usize = 500;

pub async fn create_epoch_record(
    psql_client: &Client,
    epoch: u64,
    cluster_inflation: ClusterInflation,
) -> anyhow::Result<()> {
    psql_client
        .execute(
            "
        WITH
            epoch_cluster_info AS (
                SELECT
                    MAX(transaction_count) - MIN(transaction_count) transaction_count,
                    MIN(created_at) as start_at,
                    MAX(created_at) as end_at
                FROM cluster_info
                WHERE epoch = $1
            ),
            previous_epoch AS (
                SELECT
                    MAX(end_at) end_at
                FROM epochs
                WHERE epoch = $1 - 1
            )
        INSERT INTO epochs (
            epoch,
            start_at,
            end_at,
            transaction_count,
            supply,
            inflation,
            inflation_taper
        ) SELECT
            $1,
            COALESCE(previous_epoch.end_at, epoch_cluster_info.start_at) start_at,
            epoch_cluster_info.end_at,
            transaction_count,
            $2,
            $3,
            $4
        FROM epoch_cluster_info, previous_epoch
    ",
            &[
                &Decimal::from(epoch),
                &Decimal::from(cluster_inflation.sol_total_supply),
                &cluster_inflation.inflation,
                &cluster_inflation.inflation_taper,
            ],
        )
        .await?;

    Ok(())
}

struct ValidatorUpdateRecord {
    identity: String,
    epoch: Decimal,
    commission_effective: Option<i32>,
    credits: Decimal,
    leader_slots: Decimal,
    blocks_produced: Decimal,
    skip_rate: f64,
    updated_at: DateTime<Utc>,
}

pub async fn close_epoch(
    options: CloseEpochOptions,
    mut psql_client: &mut Client,
) -> anyhow::Result<()> {
    info!("Finalizing validators snapshot...");

    let snapshot_file = std::fs::File::open(options.snapshot_path)?;
    let snapshot: ValidatorsPerformanceSnapshot = serde_yaml::from_reader(snapshot_file)?;
    let snapshot_created_at = snapshot.created_at.parse::<DateTime<Utc>>().unwrap();
    let snapshot_epoch: Decimal = snapshot.epoch.into();
    let rewards = snapshot.rewards.unwrap();

    create_epoch_record(
        &mut psql_client,
        snapshot.epoch,
        snapshot.cluster_inflation.unwrap(),
    )
    .await?;

    let mut updated_identities: HashSet<_> = Default::default();

    info!("Loaded the snapshot");

    let validator_update_records: Vec<_> = snapshot
        .validators
        .iter()
        .map(|(identity, v)| ValidatorUpdateRecord {
            identity: identity.clone(),
            epoch: snapshot_epoch,
            commission_effective: rewards
                .get(identity)
                .map_or(None, |r| r.commission_effective.map(|c| c as i32)),
            credits: v.credits.into(),
            leader_slots: v.leader_slots.into(),
            blocks_produced: v.blocks_produced.into(),
            skip_rate: v.skip_rate,
            updated_at: snapshot_created_at,
        })
        .collect();

    for chunk in validator_update_records.chunks(DEFAULT_CHUNK_SIZE) {
        let mut query = UpdateQueryCombiner::new(
            "validators".to_string(),
            "
            commission_effective = u.commission_effective,
            credits = u.credits,
            leader_slots = u.leader_slots,
            blocks_produced = u.blocks_produced,
            skip_rate = u.skip_rate,
            updated_at = u.updated_at
            "
            .to_string(),
            "u(
                identity,
                epoch,
                commission_effective,
                credits,
                leader_slots,
                blocks_produced,
                skip_rate,
                updated_at
            )"
            .to_string(),
            "validators.identity = u.identity AND validators.epoch = u.epoch".to_string(),
        );
        for v in chunk {
            let mut params: Vec<&(dyn ToSql + Sync)> = vec![
                &v.identity,
                &v.epoch,
                &v.commission_effective,
                &v.credits,
                &v.leader_slots,
                &v.blocks_produced,
                &v.skip_rate,
                &v.updated_at,
            ];
            query.add(
                &mut params,
                HashMap::from_iter([
                    (1, "NUMERIC".into()),                  // epoch
                    (2, "INTEGER".into()),                  // commission_effective
                    (3, "NUMERIC".into()),                  // credits
                    (4, "NUMERIC".into()),                  // leader_slots
                    (5, "NUMERIC".into()),                  // blocks_produced
                    (6, "DOUBLE PRECISION".into()),         // skip_rate
                    (7, "TIMESTAMP WITH TIME ZONE".into()), // updated_at
                ]),
            );
            updated_identities.insert(v.identity.clone());
        }
        query.execute(&mut psql_client).await?;
        info!(
            "Updated previously existing validator records: {}",
            updated_identities.len()
        );
    }

    Ok(())
}
