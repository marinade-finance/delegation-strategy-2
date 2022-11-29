use crate::dto::Validator;
use crate::utils::{InsertQueryCombiner, UpdateQueryCombiner};
use chrono::{DateTime, Utc};
use collect::validators::Snapshot;
use log::info;
use rust_decimal::prelude::*;
use serde_yaml;
use std::collections::{HashMap, HashSet};
use structopt::StructOpt;
use tokio_postgres::types::ToSql;
use tokio_postgres::Client;

#[derive(Debug, StructOpt)]
pub struct StoreValidatorsOptions {
    #[structopt(long = "snapshot-file")]
    snapshot_path: String,
}

const DEFAULT_CHUNK_SIZE: usize = 500;

pub async fn store_validators(
    options: StoreValidatorsOptions,
    mut psql_client: &mut Client,
) -> anyhow::Result<()> {
    info!("Storing validators snapshot...");

    let snapshot_file = std::fs::File::open(options.snapshot_path)?;
    let snapshot: Snapshot = serde_yaml::from_reader(snapshot_file)?;
    let snapshot_created_at = snapshot.created_at.parse::<DateTime<Utc>>().unwrap();

    let validators: HashMap<_, _> = snapshot
        .validators
        .iter()
        .map(|v| {
            (
                v.identity.clone(),
                Validator::new_from_snapshot(v, snapshot.epoch),
            )
        })
        .collect();
    let snapshot_epoch: Decimal = snapshot.epoch.into();
    let mut updated_identities: HashSet<_> = Default::default();

    info!("Loaded the snapshot");

    for chunk in psql_client
        .query(
            "
        SELECT identity
        FROM validators
        WHERE epoch = $1
    ",
            &[&snapshot_epoch],
        )
        .await?
        .chunks(DEFAULT_CHUNK_SIZE)
    {
        let mut query = UpdateQueryCombiner::new(
            "validators".to_string(),
            "
            identity = u.identity,
            vote_account = u.vote_account,
            epoch = u.epoch,
            info_name = u.info_name,
            info_url = u.info_url,
            info_keybase = u.info_keybase,
            dc_ip = u.dc_ip,
            dc_coordinates_lat = u.dc_coordinates_lat,
            dc_coordinates_lon = u.dc_coordinates_lon,
            dc_continent = u.dc_continent,
            dc_country_iso = u.dc_country_iso,
            dc_country = u.dc_country,
            dc_city = u.dc_city,
            dc_asn = u.dc_asn,
            dc_aso = u.dc_aso,
            commission_advertised = u.commission_advertised,
            version = u.version,
            mnde_votes = u.mnde_votes,
            activated_stake = u.activated_stake,
            marinade_stake = u.marinade_stake,
            decentralizer_stake = u.decentralizer_stake,
            superminority = u.superminority,
            stake_to_become_superminority = u.stake_to_become_superminority,
            credits = u.credits,
            leader_slots = u.leader_slots,
            blocks_produced = u.blocks_produced,
            skip_rate = u.skip_rate,
            updated_at = u.updated_at
            "
            .to_string(),
            "u(
                identity,
                vote_account,
                epoch,
                info_name,
                info_url,
                info_keybase,
                dc_ip,
                dc_coordinates_lat,
                dc_coordinates_lon,
                dc_continent,
                dc_country_iso,
                dc_country,
                dc_city,
                dc_asn,
                dc_aso,
                commission_advertised,
                version,
                mnde_votes,
                activated_stake,
                marinade_stake,
                decentralizer_stake,
                superminority,
                stake_to_become_superminority,
                credits,
                leader_slots,
                blocks_produced,
                skip_rate,
                updated_at
            )"
            .to_string(),
            "validators.identity = u.identity AND validators.epoch = u.epoch".to_string(),
        );
        for row in chunk {
            let identity: &str = row.get("identity");

            if let Some(v) = validators.get(identity) {
                let mut params: Vec<&(dyn ToSql + Sync)> = vec![
                    &v.identity,
                    &v.vote_account,
                    &v.epoch,
                    &v.info_name,
                    &v.info_url,
                    &v.info_keybase,
                    &v.dc_ip,
                    &v.dc_coordinates_lon,
                    &v.dc_coordinates_lat,
                    &v.dc_continent,
                    &v.dc_country_iso,
                    &v.dc_country,
                    &v.dc_city,
                    &v.dc_asn,
                    &v.dc_aso,
                    &v.commission_advertised,
                    &v.version,
                    &v.mnde_votes,
                    &v.activated_stake,
                    &v.marinade_stake,
                    &v.decentralizer_stake,
                    &v.superminority,
                    &v.stake_to_become_superminority,
                    &v.credits,
                    &v.leader_slots,
                    &v.blocks_produced,
                    &v.skip_rate,
                    &snapshot_created_at,
                ];
                query.add(
                    &mut params,
                    HashMap::from_iter([
                        (2, "NUMERIC".into()),                   // epoch
                        (7, "DOUBLE PRECISION".into()),          // dc_coordinates_lat
                        (8, "DOUBLE PRECISION".into()),          // dc_coordinates_lon
                        (13, "INTEGER".into()),                  // dc_asn
                        (15, "INTEGER".into()),                  // commission_advertised
                        (17, "NUMERIC".into()),                  // mnde_votes
                        (18, "NUMERIC".into()),                  // activated_stake
                        (19, "NUMERIC".into()),                  // marinade_stake
                        (20, "NUMERIC".into()),                  // decentralizer_stake
                        (21, "BOOL".into()),                     // superminority
                        (22, "NUMERIC".into()),                  // stake_to_become_superminority
                        (23, "NUMERIC".into()),                  // credits
                        (24, "NUMERIC".into()),                  // leader_slots
                        (25, "NUMERIC".into()),                  // blocks_produced
                        (26, "DOUBLE PRECISION".into()),         // skip_rate
                        (27, "TIMESTAMP WITH TIME ZONE".into()), // updated_at
                    ]),
                );
                updated_identities.insert(identity.to_string());
            }
        }
        query.execute(&mut psql_client).await?;
        info!(
            "Updated previously existing validator records: {}",
            updated_identities.len()
        );
    }

    let validators: Vec<_> = validators
        .into_iter()
        .filter(|(identity, _)| !updated_identities.contains(identity))
        .collect();
    let mut insertions = 0;

    for chunk in validators.chunks(DEFAULT_CHUNK_SIZE) {
        let mut query = InsertQueryCombiner::new(
            "validators".to_string(),
            "
        identity,
        vote_account,
        epoch,
        info_name,
        info_url,
        info_keybase,
        dc_ip,
        dc_coordinates_lat,
        dc_coordinates_lon,
        dc_continent,
        dc_country_iso,
        dc_country,
        dc_city,
        dc_asn,
        dc_aso,
        commission_max_observed,
        commission_min_observed,
        commission_advertised,
        commission_effective,
        version,
        mnde_votes,
        activated_stake,
        marinade_stake,
        decentralizer_stake,
        superminority,
        stake_to_become_superminority,
        credits,
        leader_slots,
        blocks_produced,
        skip_rate,
        uptime_pct,
        uptime,
        downtime,
        updated_at
        "
            .to_string(),
        );

        for (identity, v) in chunk {
            if updated_identities.contains(identity) {
                continue;
            }
            let mut params: Vec<&(dyn ToSql + Sync)> = vec![
                &v.identity,
                &v.vote_account,
                &v.epoch,
                &v.info_name,
                &v.info_url,
                &v.info_keybase,
                &v.dc_ip,
                &v.dc_coordinates_lon,
                &v.dc_coordinates_lat,
                &v.dc_continent,
                &v.dc_country_iso,
                &v.dc_country,
                &v.dc_city,
                &v.dc_asn,
                &v.dc_aso,
                &v.commission_max_observed,
                &v.commission_min_observed,
                &v.commission_advertised,
                &v.commission_effective,
                &v.version,
                &v.mnde_votes,
                &v.activated_stake,
                &v.marinade_stake,
                &v.decentralizer_stake,
                &v.superminority,
                &v.stake_to_become_superminority,
                &v.credits,
                &v.leader_slots,
                &v.blocks_produced,
                &v.skip_rate,
                &v.uptime_pct,
                &v.uptime,
                &v.downtime,
                &snapshot_created_at,
            ];
            query.add(&mut params);
        }
        insertions += query.execute(&mut psql_client).await?.unwrap_or(0);
        info!("Stored {} new validator records", insertions);
    }

    Ok(())
}
