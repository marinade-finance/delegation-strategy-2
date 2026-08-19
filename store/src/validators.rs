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
pub struct StoreValidatorsParams {
    #[structopt(long = "snapshot-file")]
    snapshot_path: String,
}

const DEFAULT_CHUNK_SIZE: usize = 500;
const DATA_CENTER_CARRY_EPOCHS: u64 = 10;

pub async fn store_validators(
    params: StoreValidatorsParams,
    psql_client: &mut Client,
) -> anyhow::Result<()> {
    info!("Storing validators snapshot...");

    let snapshot_file = std::fs::File::open(params.snapshot_path)?;
    let snapshot: Snapshot = serde_yaml::from_reader(snapshot_file)?;
    let snapshot_created_at: DateTime<Utc> = snapshot.created_at.parse().unwrap();

    let validators: HashMap<_, _> = snapshot
        .validators
        .iter()
        .map(|v| {
            (
                v.vote_account.clone(),
                Validator::new_from_snapshot(v, snapshot.epoch),
            )
        })
        .collect();
    let snapshot_epoch: Decimal = snapshot.epoch.into();
    let mut updated_vote_accounts: HashSet<_> = Default::default();
    let mut unresolved_vote_accounts: Vec<String> = Default::default();

    info!("Loaded the snapshot");

    for chunk in psql_client
        .query(
            "
        SELECT vote_account
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
            node_ip = u.node_ip,
            -- get_data_centers swallows a per-IP whois failure, so only an unresolved lookup on an unchanged IP may keep what is stored; a resolved answer replaces all eight together, since mixing its nulls with the previous data center invents a location nothing observed
            dc_coordinates_lat = CASE WHEN u.dc_resolved OR u.node_ip IS DISTINCT FROM validators.node_ip THEN u.dc_coordinates_lat ELSE validators.dc_coordinates_lat END,
            dc_coordinates_lon = CASE WHEN u.dc_resolved OR u.node_ip IS DISTINCT FROM validators.node_ip THEN u.dc_coordinates_lon ELSE validators.dc_coordinates_lon END,
            dc_continent = CASE WHEN u.dc_resolved OR u.node_ip IS DISTINCT FROM validators.node_ip THEN u.dc_continent ELSE validators.dc_continent END,
            dc_country_iso = CASE WHEN u.dc_resolved OR u.node_ip IS DISTINCT FROM validators.node_ip THEN u.dc_country_iso ELSE validators.dc_country_iso END,
            dc_country = CASE WHEN u.dc_resolved OR u.node_ip IS DISTINCT FROM validators.node_ip THEN u.dc_country ELSE validators.dc_country END,
            dc_city = CASE WHEN u.dc_resolved OR u.node_ip IS DISTINCT FROM validators.node_ip THEN u.dc_city ELSE validators.dc_city END,
            dc_asn = CASE WHEN u.dc_resolved OR u.node_ip IS DISTINCT FROM validators.node_ip THEN u.dc_asn ELSE validators.dc_asn END,
            dc_aso = CASE WHEN u.dc_resolved OR u.node_ip IS DISTINCT FROM validators.node_ip THEN u.dc_aso ELSE validators.dc_aso END,
            commission_advertised = u.commission_advertised,
            version = COALESCE(u.version, validators.version),
            activated_stake = u.activated_stake,
            marinade_stake = u.marinade_stake,
            foundation_stake = u.foundation_stake,
            marinade_native_stake = u.marinade_native_stake,
            institutional_stake = u.institutional_stake,
            self_stake = u.self_stake,
            superminority = u.superminority,
            stake_to_become_superminority = u.stake_to_become_superminority,
            credits = u.credits,
            leader_slots = u.leader_slots,
            blocks_produced = u.blocks_produced,
            skip_rate = u.skip_rate,
            updated_at = u.updated_at,
            info_icon_url = u.info_icon_url,
            client_id = CASE WHEN u.client_id_raw IS NOT NULL THEN u.client_id ELSE validators.client_id END,
            client_id_raw = COALESCE(u.client_id_raw, validators.client_id_raw),
            feature_set = u.feature_set,
            shred_version = u.shred_version,
            gossip_port = u.gossip_port,
            rpc_public = u.rpc_public,
            pubsub_public = u.pubsub_public
            "
            .to_string(),
            "u(
                identity,
                vote_account,
                epoch,
                info_name,
                info_url,
                info_keybase,
                node_ip,
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
                activated_stake,
                marinade_stake,
                foundation_stake,
                marinade_native_stake,
                institutional_stake,
                self_stake,
                superminority,
                stake_to_become_superminority,
                credits,
                leader_slots,
                blocks_produced,
                skip_rate,
                updated_at,
                info_icon_url,
                client_id,
                client_id_raw,
                feature_set,
                shred_version,
                gossip_port,
                rpc_public,
                pubsub_public,
                dc_resolved
            )"
            .to_string(),
            "validators.vote_account = u.vote_account AND validators.epoch = u.epoch".to_string(),
        );
        for row in chunk {
            let vote_account: &str = row.get("vote_account");

            if let Some(v) = validators.get(vote_account) {
                let mut params: Vec<&(dyn ToSql + Sync)> = vec![
                    &v.identity,
                    &v.vote_account,
                    &v.epoch,
                    &v.info_name,
                    &v.info_url,
                    &v.info_keybase,
                    &v.node_ip,
                    &v.dc_coordinates_lat,
                    &v.dc_coordinates_lon,
                    &v.dc_continent,
                    &v.dc_country_iso,
                    &v.dc_country,
                    &v.dc_city,
                    &v.dc_asn,
                    &v.dc_aso,
                    &v.commission_advertised,
                    &v.version,
                    &v.activated_stake,
                    &v.marinade_stake,
                    &v.foundation_stake,
                    &v.marinade_native_stake,
                    &v.institutional_stake,
                    &v.self_stake,
                    &v.superminority,
                    &v.stake_to_become_superminority,
                    &v.credits,
                    &v.leader_slots,
                    &v.blocks_produced,
                    &v.skip_rate,
                    &snapshot_created_at,
                    &v.info_icon_url,
                    &v.client_id,
                    &v.client_id_raw,
                    &v.feature_set,
                    &v.shred_version,
                    &v.gossip_port,
                    &v.rpc_public,
                    &v.pubsub_public,
                    &v.dc_resolved,
                ];
                query.add(
                    &mut params,
                    HashMap::from_iter([
                        (2, "NUMERIC".into()),                   // epoch
                        (7, "DOUBLE PRECISION".into()),          // dc_coordinates_lat
                        (8, "DOUBLE PRECISION".into()),          // dc_coordinates_lon
                        (13, "INTEGER".into()),                  // dc_asn
                        (15, "INTEGER".into()),                  // commission_advertised
                        (17, "NUMERIC".into()),                  // activated_stake
                        (18, "NUMERIC".into()),                  // marinade_stake
                        (19, "NUMERIC".into()),                  // foundation_stake
                        (20, "NUMERIC".into()),                  // marinade_native_stake
                        (21, "NUMERIC".into()),                  // institutional_stake
                        (22, "NUMERIC".into()),                  // selft_stake
                        (23, "BOOL".into()),                     // superminority
                        (24, "NUMERIC".into()),                  // stake_to_become_superminority
                        (25, "NUMERIC".into()),                  // credits
                        (26, "NUMERIC".into()),                  // leader_slots
                        (27, "NUMERIC".into()),                  // blocks_produced
                        (28, "DOUBLE PRECISION".into()),         // skip_rate
                        (29, "TIMESTAMP WITH TIME ZONE".into()), // updated_at
                        (30, "TEXT".into()),                     // icon_url
                        (31, "INTEGER".into()),                  // client_id
                        (32, "TEXT".into()),                     // client_id_raw
                        (33, "BIGINT".into()),                   // feature_set
                        (34, "INTEGER".into()),                  // shred_version
                        (35, "INTEGER".into()),                  // gossip_port
                        (36, "BOOL".into()),                     // rpc_public
                        (37, "BOOL".into()),                     // pubsub_public
                        (38, "BOOL".into()),                     // dc_resolved
                    ]),
                );
                updated_vote_accounts.insert(vote_account.to_string());
                if !v.dc_resolved {
                    unresolved_vote_accounts.push(vote_account.to_string());
                }
            }
        }
        query.execute(psql_client).await?;
        info!(
            "Updated previously existing validator records: {}",
            updated_vote_accounts.len()
        );
    }

    let validators: Vec<_> = validators
        .into_iter()
        .filter(|(vote_account, _validator)| !updated_vote_accounts.contains(vote_account))
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
        node_ip,
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
        activated_stake,
        marinade_stake,
        foundation_stake,
        marinade_native_stake,
        institutional_stake,
        self_stake,
        superminority,
        stake_to_become_superminority,
        credits,
        leader_slots,
        blocks_produced,
        skip_rate,
        uptime_pct,
        uptime,
        downtime,
        updated_at,
        info_icon_url,
        client_id,
        client_id_raw,
        feature_set,
        shred_version,
        gossip_port,
        rpc_public,
        pubsub_public
        "
            .to_string(),
        );

        for (vote_account, v) in chunk {
            if updated_vote_accounts.contains(vote_account) {
                continue;
            }
            let mut params: Vec<&(dyn ToSql + Sync)> = vec![
                &v.identity,
                &v.vote_account,
                &v.epoch,
                &v.info_name,
                &v.info_url,
                &v.info_keybase,
                &v.node_ip,
                &v.dc_coordinates_lat,
                &v.dc_coordinates_lon,
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
                &v.activated_stake,
                &v.marinade_stake,
                &v.foundation_stake,
                &v.marinade_native_stake,
                &v.institutional_stake,
                &v.self_stake,
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
                &v.info_icon_url,
                &v.client_id,
                &v.client_id_raw,
                &v.feature_set,
                &v.shred_version,
                &v.gossip_port,
                &v.rpc_public,
                &v.pubsub_public,
            ];
            query.add(&mut params);
            if !v.dc_resolved {
                unresolved_vote_accounts.push(vote_account.clone());
            }
        }
        insertions += query.execute(psql_client).await?.unwrap_or(0);
        info!("Stored {insertions} new validator records");
    }

    if !unresolved_vote_accounts.is_empty() {
        carry_previous_data_centers(psql_client, &snapshot_epoch, &unresolved_vote_accounts)
            .await?;
    }

    Ok(())
}

// Without this an unresolved lookup drops a location the previous epoch knew: the INSERT branch has no row for the epoch to preserve, and the UPDATE branch preserves whatever an interrupted earlier run left, so the gap would last the whole epoch; matching node_ip is what stops a node that moved from inheriting the old address's data center.
async fn carry_previous_data_centers(
    psql_client: &Client,
    epoch: &Decimal,
    vote_accounts: &[String],
) -> anyhow::Result<()> {
    let carry_window = Decimal::from(DATA_CENTER_CARRY_EPOCHS);
    let carried = psql_client
        .execute(
            "
        UPDATE validators
        SET
            dc_coordinates_lat = previous.dc_coordinates_lat,
            dc_coordinates_lon = previous.dc_coordinates_lon,
            dc_continent = previous.dc_continent,
            dc_country_iso = previous.dc_country_iso,
            dc_country = previous.dc_country,
            dc_city = previous.dc_city,
            dc_asn = previous.dc_asn,
            dc_aso = previous.dc_aso
        FROM (
            -- Keyed per address so a node returning to an earlier one reuses that address's own history: keyed by vote_account alone the newest row wins outright, and a different address there rejects the carry at the outer predicate while a matching older row goes unseen.
            SELECT DISTINCT ON (vote_account, node_ip)
                vote_account,
                node_ip,
                dc_coordinates_lat,
                dc_coordinates_lon,
                dc_continent,
                dc_country_iso,
                dc_country,
                dc_city,
                dc_asn,
                dc_aso
            FROM validators
            -- Bounded because an unbounded epoch < $1 sequentially scans the whole table for a boundary-wide failure, and a location last seen further back than this is no longer good evidence of where the node is now; skipping rows that carry no location at all is what lets the carry step over an epoch some earlier gap left empty instead of propagating that gap forward.
            WHERE epoch < $1 AND epoch >= $1 - $3 AND vote_account = ANY($2)
                AND num_nonnulls(dc_coordinates_lat, dc_coordinates_lon, dc_continent, dc_country_iso, dc_country, dc_city, dc_asn, dc_aso) > 0
            ORDER BY vote_account, node_ip, epoch DESC
        ) previous
        WHERE validators.vote_account = previous.vote_account
            AND validators.epoch = $1
            AND validators.node_ip IS NOT DISTINCT FROM previous.node_ip
            -- An unresolved UPDATE reaches here with whatever the epoch already holds, so restricting the carry to rows that hold nothing is what keeps it from undoing the preserve.
            AND num_nonnulls(validators.dc_coordinates_lat, validators.dc_coordinates_lon, validators.dc_continent, validators.dc_country_iso, validators.dc_country, validators.dc_city, validators.dc_asn, validators.dc_aso) = 0
    ",
            &[epoch, &vote_accounts, &carry_window],
        )
        .await?;
    info!("Carried a previously known data center for {carried} validators");

    Ok(())
}
