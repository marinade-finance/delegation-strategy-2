use chrono::{DateTime, Utc};
use collect::whois_service::{IpInfo, WhoisClient};
use log::{info, warn};
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

pub async fn select_unknown_ips(psql_client: &Client) -> anyhow::Result<Vec<String>> {
    // NOT EXISTS rather than NOT IN: a single NULL on the right of NOT IN would discard every row.
    Ok(psql_client
        .query(
            "
        SELECT DISTINCT o.ip
        FROM node_observations o
        WHERE o.ip IS NOT NULL AND o.ip <> '127.0.0.1'
          AND NOT EXISTS (SELECT 1 FROM ip_info i WHERE i.ip = o.ip)
    ",
            &[],
        )
        .await?
        .iter()
        .map(|row| row.get("ip"))
        .collect())
}

pub async fn select_stale_ips(
    psql_client: &Client,
    refresh_limit: i64,
    in_use_days: i32,
) -> anyhow::Result<Vec<String>> {
    // Restricted to addresses still in gossip: an IP the cluster left is not worth paying to recheck.
    Ok(psql_client
        .query(
            "
        SELECT i.ip
        FROM ip_info i
        WHERE EXISTS (
            SELECT 1
            FROM node_observations o
            WHERE o.ip = i.ip AND o.created_at > now() - make_interval(days => $2)
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
    let asns: Vec<Option<i32>> = fetched
        .iter()
        .map(|(_, info)| info.asn.map(|asn| asn as i32))
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
            $2::INTEGER[],
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

    let unknown = select_unknown_ips(psql_client).await?;
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

    let whois_client = WhoisClient::new(params.whois, params.whois_bearer_token);
    // WhoisClient is a blocking reqwest client and this binary runs on tokio, so the whole batch
    // goes to a blocking thread rather than each call fighting the runtime.
    let fetched = tokio::task::spawn_blocking(move || {
        let mut fetched = Vec::with_capacity(ips.len());
        for ip in ips {
            match whois_client.get_ip_info(&ip) {
                Ok(info) => fetched.push((ip, info)),
                // Left unstored on purpose: no row means select_unknown_ips offers it again next run.
                Err(err) => warn!("Couldn't fetch info about IP {ip}: {err}"),
            }
        }
        fetched
    })
    .await?;

    let upserted = upsert_ip_info(psql_client, &fetched, Utc::now()).await?;

    info!("Stored info about {upserted} IPs");

    Ok(())
}
