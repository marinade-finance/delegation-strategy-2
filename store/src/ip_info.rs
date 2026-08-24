use chrono::{DateTime, Utc};
use collect::whois_service::{IpInfo, WhoisClient};
use log::{info, warn};
use std::net::IpAddr;
use std::sync::Arc;
use structopt::StructOpt;
use tokio_postgres::Client;

#[derive(Debug, StructOpt)]
pub struct StoreIpInfoParams {
    #[structopt(long = "whois", help = "Base URL for whois API.")]
    whois: String,

    #[structopt(
        long = "whois-bearer-token",
        help = "Bearer token to be used to fetch data from whois API"
    )]
    whois_bearer_token: Option<String>,

    #[structopt(
        long = "refresh-limit",
        help = "How many already known IPs to re-fetch per run, oldest first.",
        default_value = "21"
    )]
    refresh_limit: i64,

    #[structopt(
        long = "in-use-days",
        help = "How recently an IP must have been observed to be worth re-fetching.",
        default_value = "7"
    )]
    in_use_days: i32,
}

// Small because each entry costs a whois round trip: the point is to commit progress often, not to
// batch the writes.
const UPSERT_CHUNK_SIZE: usize = 50;

// gossip carries whatever a node advertises, and parse_socket_addr does not validate it, so the
// column can hold a hostname or an unroutable address that whois can only answer nothing about.
fn is_worth_looking_up(ip: &str) -> bool {
    match ip.parse::<IpAddr>() {
        Ok(IpAddr::V4(v4)) => {
            !v4.is_private()
                && !v4.is_loopback()
                && !v4.is_link_local()
                && !v4.is_unspecified()
                && !v4.is_broadcast()
                && !v4.is_documentation()
        }
        Ok(IpAddr::V6(v6)) => !v6.is_loopback() && !v6.is_unspecified(),
        Err(_) => false,
    }
}

// Bounded by the same in-use window as the refresh: without it a first run faces every address the
// cluster has ever advertised, which is unbounded in history and mostly no longer reachable.
pub async fn select_unknown_ips(
    psql_client: &Client,
    in_use_days: i32,
) -> anyhow::Result<Vec<String>> {
    // NOT EXISTS rather than NOT IN: a single NULL on the right of NOT IN would discard every row.
    Ok(psql_client
        .query(
            "
        SELECT DISTINCT o.ip
        FROM node_observations o
        WHERE o.ip IS NOT NULL
          AND o.last_seen_at > now() - make_interval(days => $1)
          AND NOT EXISTS (SELECT 1 FROM ip_info i WHERE i.ip = o.ip)
    ",
            &[&in_use_days],
        )
        .await?
        .iter()
        .map(|row| row.get("ip"))
        .filter(|ip: &String| is_worth_looking_up(ip))
        .collect())
}

pub async fn select_stale_ips(
    psql_client: &Client,
    refresh_limit: i64,
    in_use_days: i32,
) -> anyhow::Result<Vec<String>> {
    // last_seen_at, not created_at: the latter only moves when a node changes, so a node that sits
    // still for longer than the window would drop out of the rotation precisely for being stable.
    Ok(psql_client
        .query(
            "
        SELECT i.ip
        FROM ip_info i
        WHERE EXISTS (
            SELECT 1
            FROM node_observations o
            WHERE o.ip = i.ip AND o.last_seen_at > now() - make_interval(days => $2)
        )
        ORDER BY i.fetched_at ASC
        LIMIT $1
    ",
            &[&refresh_limit, &in_use_days],
        )
        .await?
        .iter()
        .map(|row| row.get("ip"))
        .collect())
}

pub async fn upsert_ip_info(
    psql_client: &Client,
    fetched: &[(String, IpInfo)],
    fetched_at: DateTime<Utc>,
) -> anyhow::Result<u64> {
    if fetched.is_empty() {
        return Ok(0);
    }

    let ips: Vec<&str> = fetched.iter().map(|(ip, _)| ip.as_str()).collect();
    let asns: Vec<Option<i64>> = fetched
        .iter()
        .map(|(_, info)| info.asn.map(|asn| asn as i64))
        .collect();
    let asos: Vec<Option<&str>> = fetched
        .iter()
        .map(|(_, info)| info.aso.as_deref())
        .collect();
    let continents: Vec<Option<&str>> = fetched
        .iter()
        .map(|(_, info)| info.continent.as_deref())
        .collect();
    let country_isos: Vec<Option<&str>> = fetched
        .iter()
        .map(|(_, info)| info.country_iso.as_deref())
        .collect();
    let countries: Vec<Option<&str>> = fetched
        .iter()
        .map(|(_, info)| info.country.as_deref())
        .collect();
    let cities: Vec<Option<&str>> = fetched
        .iter()
        .map(|(_, info)| info.city.as_deref())
        .collect();
    let lats: Vec<Option<f64>> = fetched
        .iter()
        .map(|(_, info)| info.coordinates.as_ref().map(|c| c.lat))
        .collect();
    let lons: Vec<Option<f64>> = fetched
        .iter()
        .map(|(_, info)| info.coordinates.as_ref().map(|c| c.lon))
        .collect();
    let fetched_ats: Vec<DateTime<Utc>> = vec![fetched_at; fetched.len()];

    Ok(psql_client
        .execute(
            "
        INSERT INTO ip_info (
            ip, asn, aso, continent, country_iso, country, city,
            coordinates_lat, coordinates_lon, fetched_at
        )
        SELECT * FROM UNNEST(
            $1::TEXT[],
            $2::BIGINT[],
            $3::TEXT[],
            $4::TEXT[],
            $5::TEXT[],
            $6::TEXT[],
            $7::TEXT[],
            $8::DOUBLE PRECISION[],
            $9::DOUBLE PRECISION[],
            $10::TIMESTAMP WITH TIME ZONE[]
        )
        ON CONFLICT (ip)
        DO UPDATE SET
            asn = EXCLUDED.asn,
            aso = EXCLUDED.aso,
            continent = EXCLUDED.continent,
            country_iso = EXCLUDED.country_iso,
            country = EXCLUDED.country,
            city = EXCLUDED.city,
            coordinates_lat = EXCLUDED.coordinates_lat,
            coordinates_lon = EXCLUDED.coordinates_lon,
            fetched_at = EXCLUDED.fetched_at
    ",
            &[
                &ips,
                &asns,
                &asos,
                &continents,
                &country_isos,
                &countries,
                &cities,
                &lats,
                &lons,
                &fetched_ats,
            ],
        )
        .await?)
}

pub async fn store_ip_info(
    params: StoreIpInfoParams,
    psql_client: &mut Client,
) -> anyhow::Result<()> {
    info!("Storing IP info...");

    let unknown = select_unknown_ips(psql_client, params.in_use_days).await?;
    let stale = select_stale_ips(psql_client, params.refresh_limit, params.in_use_days).await?;
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
    let mut upserted = 0;
    // Committed per chunk: a run killed part-way through keeps the lookups it already paid for,
    // instead of discarding the whole batch and starting over next time.
    for chunk in ips.chunks(UPSERT_CHUNK_SIZE) {
        let chunk = chunk.to_vec();
        let whois_client = whois_client.clone();
        // WhoisClient is a blocking reqwest client and this binary runs on tokio, so the chunk goes
        // to a blocking thread rather than each call fighting the runtime.
        let fetched = tokio::task::spawn_blocking(move || {
            let mut fetched = Vec::with_capacity(chunk.len());
            for ip in chunk {
                match whois_client.get_ip_info(&ip) {
                    Ok(info) => fetched.push((ip, info)),
                    // Left unstored on purpose: no row means select_unknown_ips offers it again next run.
                    Err(err) => warn!("Couldn't fetch info about IP {ip}: {err}"),
                }
            }
            fetched
        })
        .await?;

        upserted += upsert_ip_info(psql_client, &fetched, Utc::now()).await?;
    }

    info!("Stored info about {upserted} IPs");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routable_addresses_are_looked_up() {
        assert!(is_worth_looking_up("1.1.1.1"));
        assert!(is_worth_looking_up("8.8.8.8"));
        assert!(is_worth_looking_up("2606:4700:4700::1111"));
    }

    #[test]
    fn unroutable_addresses_are_skipped() {
        assert!(!is_worth_looking_up("127.0.0.1"));
        assert!(!is_worth_looking_up("10.0.0.1"));
        assert!(!is_worth_looking_up("172.16.0.1"));
        assert!(!is_worth_looking_up("192.168.1.1"));
        assert!(!is_worth_looking_up("169.254.0.1"));
        assert!(!is_worth_looking_up("0.0.0.0"));
        assert!(!is_worth_looking_up("255.255.255.255"));
        assert!(!is_worth_looking_up("::1"));
    }

    // parse_socket_addr splits on the last colon without validating, so the column can hold this.
    #[test]
    fn a_non_address_is_skipped_rather_than_sent_to_whois() {
        assert!(!is_worth_looking_up("some-host.example.com"));
        assert!(!is_worth_looking_up(""));
    }
}
