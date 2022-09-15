use crate::validators::*;
use log::{debug, info};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

#[derive(Deserialize, Clone)]
pub struct Coordinates {
    pub lat: f64,
    pub lon: f64,
}
#[derive(Deserialize, Clone)]
pub struct IpInfo {
    pub asn: u32,
    pub aso: Option<String>,
    pub coordinates: Option<Coordinates>,
    pub continent: Option<String>,
    pub country_iso: Option<String>,
    pub country: Option<String>,
    pub city: Option<String>,
}

pub struct WhoisClient {
    host: String,
    bearer_token: Option<String>,
}
impl WhoisClient {
    pub fn new(host: String, bearer_token: Option<String>) -> Self {
        Self { host, bearer_token }
    }

    pub fn get_ip_info(&self, ip: &String) -> anyhow::Result<IpInfo> {
        debug!("Fetching info about data centers: {}", &ip);
        let client = reqwest::blocking::Client::builder().build()?;
        let body = client
            .get(format!("{}/ip/{}", self.host.clone(), ip))
            .header(
                "Authorization",
                format!(
                    "Bearer {}",
                    self.bearer_token.clone().unwrap_or("none".to_string())
                ),
            )
            .send()?;
        Ok(body.json()?)
    }
}

pub fn get_data_centers(
    whois_client: WhoisClient,
    node_ips: HashMap<String, String>,
) -> anyhow::Result<HashMap<String, (String, IpInfo)>> {
    info!("Fetching info about data centers...");
    let mut data_centers = HashMap::new();

    for (node, ip) in node_ips.iter() {
        let info = whois_client.get_ip_info(ip)?;
        data_centers.insert(node.clone(), (ip.clone(), info));
    }

    info!("Fetched info about data centers...");
    Ok(data_centers)
}
