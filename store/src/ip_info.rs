use crate::directory::{Directory, Precondition};
use crate::docs::{
    IpInfoDoc, IpInfoEntry, NodeObservationsDoc, IP_INFO_PATH, LIVE_NODE_OBSERVATIONS,
};
use chrono::{DateTime, Duration, Utc};
use clap::Parser;
use collect::whois_service::{BearerToken, IpInfo, WhoisClient};
use log::{info, warn};
use std::collections::HashSet;
use std::net::IpAddr;
use std::sync::Arc;

#[cfg(test)]
#[path = "ip_info_test.rs"]
mod ip_info_test;

#[derive(Debug, Parser)]
pub struct StoreIpInfoParams {
    #[arg(long = "whois", help = "Base URL for whois API.")]
    whois: String,

    #[arg(
        long = "whois-bearer-token",
        help = "Bearer token to be used to fetch data from whois API"
    )]
    whois_bearer_token: Option<BearerToken>,

    #[arg(
        long = "refresh-limit",
        help = "How many already known IPs to re-fetch per run, oldest first.",
        default_value = "21"
    )]
    refresh_limit: usize,

    #[arg(
        long = "in-use-days",
        help = "How recently an IP must have been observed to be worth re-fetching.",
        default_value = "7"
    )]
    in_use_days: i64,
}

/// Small because each entry costs a whois round trip: the point is to write
/// progress often, not to batch the writes.
const WRITE_CHUNK_SIZE: usize = 50;

/// gossip carries whatever a node advertises, and parse_socket_addr does not
/// validate it, so the field can hold a hostname or an unroutable address that
/// whois can only answer nothing about.
pub fn is_worth_looking_up(ip: &str) -> bool {
    match ip.parse::<IpAddr>() {
        Ok(IpAddr::V4(v4)) => {
            !v4.is_private()
                && !v4.is_loopback()
                && !v4.is_link_local()
                && !v4.is_unspecified()
                && !v4.is_broadcast()
                && !v4.is_documentation()
        }
        Ok(IpAddr::V6(v6)) => {
            !v6.is_loopback()
                && !v6.is_unspecified()
                && !v6.is_multicast()
                && !v6.is_unique_local()
                && !v6.is_unicast_link_local()
        }
        Err(_) => false,
    }
}

/// Every address a node advertised and was seen on after `since`: the one it
/// advertises now, and the ones it moved away from recently enough.
pub fn ips_in_use(observations: &NodeObservationsDoc, since: DateTime<Utc>) -> HashSet<String> {
    observations
        .values()
        .flat_map(|state| state.changes.iter().chain([&state.last]))
        .filter(|observation| observation.last_seen_at > since)
        .filter_map(|observation| observation.ip.clone())
        .collect()
}

/// Bounded by the same in-use window as the refresh: without it a first run
/// faces every address the cluster has ever advertised, which is unbounded in
/// history and mostly no longer reachable.
pub fn select_unknown_ips(in_use: &HashSet<String>, info: &IpInfoDoc) -> Vec<String> {
    let mut unknown: Vec<String> = in_use
        .iter()
        .filter(|ip| !info.contains_key(*ip))
        .filter(|ip| is_worth_looking_up(ip))
        .cloned()
        .collect();
    unknown.sort();
    unknown
}

/// The known addresses still in use, oldest answer first. In use by when a
/// node was last seen on it, not by when the node changed: a node that sits
/// still for longer than the window would drop out of the rotation precisely
/// for being stable.
pub fn select_stale_ips(
    in_use: &HashSet<String>,
    info: &IpInfoDoc,
    refresh_limit: usize,
) -> Vec<String> {
    let mut stale: Vec<(&String, &IpInfoEntry)> =
        info.iter().filter(|(ip, _)| in_use.contains(*ip)).collect();
    stale.sort_by_key(|(ip, entry)| (entry.fetched_at, (*ip).clone()));
    stale
        .into_iter()
        .take(refresh_limit)
        .map(|(ip, _)| ip.clone())
        .collect()
}

pub fn apply_fetched(
    info: &mut IpInfoDoc,
    fetched: &[(String, IpInfo)],
    fetched_at: DateTime<Utc>,
) {
    for (ip, answer) in fetched {
        info.insert(
            ip.clone(),
            IpInfoEntry {
                asn: answer.asn.map(i64::from),
                aso: answer.aso.clone(),
                continent: answer.continent.clone(),
                country_iso: answer.country_iso.clone(),
                country: answer.country.clone(),
                city: answer.city.clone(),
                coordinates_lat: answer.coordinates.as_ref().map(|c| c.lat),
                coordinates_lon: answer.coordinates.as_ref().map(|c| c.lon),
                fetched_at,
            },
        );
    }
}

pub async fn store_ip_info(params: StoreIpInfoParams, directory: &Directory) -> anyhow::Result<()> {
    info!("Storing IP info...");

    let observations = directory
        .get::<NodeObservationsDoc>(LIVE_NODE_OBSERVATIONS)
        .await?
        .map(|doc| doc.body)
        .unwrap_or_default();
    let stored = directory.get::<IpInfoDoc>(IP_INFO_PATH).await?;
    let (mut info, mut precondition) = match stored {
        Some(stored) => (stored.body, Precondition::IfMatch(stored.etag)),
        None => (IpInfoDoc::new(), Precondition::Create),
    };

    let in_use = ips_in_use(
        &observations,
        Utc::now() - Duration::days(params.in_use_days),
    );
    let unknown = select_unknown_ips(&in_use, &info);
    let stale = select_stale_ips(&in_use, &info, params.refresh_limit);
    info!(
        "{} IPs never looked up, {} due for a refresh",
        unknown.len(),
        stale.len()
    );

    let ips: Vec<String> = unknown.into_iter().chain(stale).collect();
    if ips.is_empty() {
        info!("Stored info about 0 IPs");
        return Ok(());
    }

    let whois_client = Arc::new(WhoisClient::new(params.whois, params.whois_bearer_token)?);
    let mut written = 0;
    // Written per chunk: a run killed part-way through keeps the lookups it
    // already paid for, instead of discarding the whole batch and starting
    // over next time.
    for chunk in ips.chunks(WRITE_CHUNK_SIZE) {
        let chunk = chunk.to_vec();
        let whois_client = whois_client.clone();
        // WhoisClient is a blocking reqwest client and this binary runs on
        // tokio, so the chunk goes to a blocking thread rather than each call
        // fighting the runtime.
        let fetched =
            tokio::task::spawn_blocking(move || fetch_ip_info(&whois_client, chunk)).await?;
        if fetched.is_empty() {
            continue;
        }

        apply_fetched(&mut info, &fetched, Utc::now());
        let etag = directory.put(IP_INFO_PATH, &info, precondition).await?;
        precondition = Precondition::IfMatch(etag);
        written += fetched.len();
    }

    info!("Stored info about {written} IPs");

    Ok(())
}

/// An address whois could not answer for is left unstored on purpose: with no
/// entry, `select_unknown_ips` offers it again next run.
fn fetch_ip_info(whois_client: &WhoisClient, ips: Vec<String>) -> Vec<(String, IpInfo)> {
    let mut fetched = Vec::with_capacity(ips.len());
    for ip in ips {
        match whois_client.get_ip_info(&ip) {
            Ok(answer) => fetched.push((ip, answer)),
            Err(err) => warn!("Couldn't fetch info about IP {ip}: {err}"),
        }
    }
    fetched
}
