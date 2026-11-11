use crate::{
    common::*, solana_service::solana_client_with_timeout, validators_sandwiches::http_client,
};
use anyhow::Context;
use clap::Parser;
use log::{info, warn};
use serde::{Deserialize, Deserializer, Serialize};
use solana_clock::Epoch;
use std::time::Duration;

/// The `source` of the rows this collector stores; a later in-house collector stores its own.
pub const SOURCE_EXTERNAL: &str = "external";

const DATA_VERSION: u16 = 1;

#[derive(Debug, Parser)]
pub struct SlotTimesParams {
    #[arg(long = "api-url", help = "Base URL of the upstream slot-time API.")]
    api_url: String,

    #[arg(
        long = "rpc-timeout",
        help = "How long to wait for RPC response (seconds).",
        default_value = "300"
    )]
    rpc_timeout: u64,

    #[arg(
        long = "epochs-back",
        help = "How many closed epochs back from the current epoch to (re-)query.",
        default_value = "10"
    )]
    epochs_back: u64,

    #[arg(
        long = "from-epoch",
        help = "Read epochs from this one onwards. Overrides --epochs-back (use for historical backfill)."
    )]
    from_epoch: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ValidatorsSlotTimesSnapshot {
    pub version: u16,
    pub source: String,
    pub from_epoch: Epoch,
    pub loaded_at_epoch: Epoch,
    pub loaded_at_slot_index: u64,
    pub created_at: String,
    pub slot_times: Vec<ValidatorSlotTime>,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct ValidatorSlotTime {
    pub epoch: Epoch,
    pub vote_account: String,
    pub slot_duration_median_ms: f64,
    pub ibrl_score: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct SlotTimeRecord {
    vote_account_pubkey: String,
    #[serde(default, deserialize_with = "number_or_string")]
    slot_duration_median: Option<f64>,
    #[serde(default, deserialize_with = "number_or_string")]
    ibrl_score: Option<f64>,
}

/// The upstream sends some numbers as decimal strings (`"200.00000"`).
fn number_or_string<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<f64>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Value {
        Number(f64),
        Text(String),
    }
    match Option::<Value>::deserialize(deserializer)? {
        None => Ok(None),
        Some(Value::Number(number)) => Ok(Some(number)),
        Some(Value::Text(text)) => text
            .trim()
            .parse()
            .map(Some)
            .map_err(serde::de::Error::custom),
    }
}

fn records_to_slot_times(epoch: Epoch, records: Vec<SlotTimeRecord>) -> Vec<ValidatorSlotTime> {
    records
        .into_iter()
        .filter_map(|record| {
            if !is_base58_pubkey(&record.vote_account_pubkey) {
                warn!(
                    "epoch {epoch}: dropping a record whose vote account {:?} is not a pubkey",
                    record.vote_account_pubkey
                );
                return None;
            }
            Some(ValidatorSlotTime {
                epoch,
                slot_duration_median_ms: record.slot_duration_median?,
                ibrl_score: record.ibrl_score,
                vote_account: record.vote_account_pubkey,
            })
        })
        .collect()
}

/// `None` while the upstream has not published the epoch yet.
fn fetch_epoch(
    client: &reqwest::blocking::Client,
    api_url: &str,
    epoch: Epoch,
) -> anyhow::Result<Option<Vec<SlotTimeRecord>>> {
    let url = format!(
        "{}/validator_rewards/{epoch}",
        api_url.trim_end_matches('/')
    );
    let response = client
        .get(&url)
        .send()
        .with_context(|| format!("requesting {url}"))?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    Ok(Some(
        response
            .error_for_status()
            .with_context(|| format!("requesting {url}"))?
            .json()
            .with_context(|| format!("decoding {url}"))?,
    ))
}

/// Reads `from_epoch..current_epoch`: a running epoch's partial figures must never become evidence.
fn fetch_slot_times(
    client: &reqwest::blocking::Client,
    api_url: &str,
    from_epoch: Epoch,
    current_epoch: Epoch,
) -> anyhow::Result<Vec<ValidatorSlotTime>> {
    let mut slot_times = Vec::new();
    for epoch in from_epoch..current_epoch {
        let Some(records) = fetch_epoch(client, api_url, epoch)? else {
            info!("epoch {epoch} not published upstream yet, stopping");
            break;
        };
        let rows = records_to_slot_times(epoch, records);
        info!("epoch {epoch}: {} validators", rows.len());
        slot_times.extend(rows);
    }
    Ok(slot_times)
}

pub fn collect_validator_slot_times_info(
    common_params: CommonParams,
    slot_times_params: SlotTimesParams,
) -> anyhow::Result<()> {
    info!("Collecting validator slot times snapshot");
    let timeout = Duration::from_secs(slot_times_params.rpc_timeout);
    let rpc_client =
        solana_client_with_timeout(common_params.rpc_url, timeout, common_params.commitment);

    let created_at = chrono::Utc::now();
    let current_epoch_info = rpc_client.get_epoch_info()?;
    let from_epoch = slot_times_params.from_epoch.unwrap_or_else(|| {
        current_epoch_info
            .epoch
            .saturating_sub(slot_times_params.epochs_back)
    });
    info!("Reading slot times from epoch: {from_epoch}");

    let slot_times = fetch_slot_times(
        &http_client()?,
        &slot_times_params.api_url,
        from_epoch,
        current_epoch_info.epoch,
    )?;
    info!("Retrieved {} validator slot time records", slot_times.len());

    serde_yaml::to_writer(
        std::io::stdout(),
        &ValidatorsSlotTimesSnapshot {
            version: DATA_VERSION,
            source: SOURCE_EXTERNAL.into(),
            from_epoch,
            loaded_at_epoch: current_epoch_info.epoch,
            loaded_at_slot_index: current_epoch_info.slot_index,
            created_at: created_at.to_string(),
            slot_times,
        },
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn parse(json: &str) -> Vec<ValidatorSlotTime> {
        records_to_slot_times(1080, serde_json::from_str(json).unwrap())
    }

    #[test]
    fn slot_duration_median_as_a_string_is_parsed() {
        let rows = parse(
            r#"[{"vote_account_pubkey": "5AC692spnjbegP7ttCXJEzUe8S81sLYsqJd8Ae6Zv1xU", "slot_duration_median": "200.00000", "ibrl_score": null}]"#,
        );
        assert_eq!(
            rows,
            vec![ValidatorSlotTime {
                epoch: 1080,
                vote_account: "5AC692spnjbegP7ttCXJEzUe8S81sLYsqJd8Ae6Zv1xU".into(),
                slot_duration_median_ms: 200.0,
                ibrl_score: None,
            }]
        );
    }

    #[test]
    fn slot_duration_median_as_a_number_is_parsed() {
        let rows = parse(
            r#"[{"vote_account_pubkey": "5AC692spnjbegP7ttCXJEzUe8S81sLYsqJd8Ae6Zv1xU", "slot_duration_median": 243.5}]"#,
        );
        assert_eq!(rows[0].slot_duration_median_ms, 243.5);
    }

    #[test]
    fn a_record_without_slot_duration_median_is_skipped() {
        let rows = parse(
            r#"[{"vote_account_pubkey": "5AC692spnjbegP7ttCXJEzUe8S81sLYsqJd8Ae6Zv1xU", "slot_duration_median": null},
                {"vote_account_pubkey": "CzmqDuqEpfnkptuLAcikmJrhCnhFXo8aUBj6Rto1SPAc"},
                {"vote_account_pubkey": "QodirbUG8AZQBWpHhPJPfjj1xg4AaQUZCVVtwT8YfPi", "slot_duration_median": "201.5"}]"#,
        );
        assert_eq!(
            rows.iter()
                .map(|row| row.vote_account.as_str())
                .collect::<Vec<_>>(),
            vec!["QodirbUG8AZQBWpHhPJPfjj1xg4AaQUZCVVtwT8YfPi"]
        );
    }

    #[test]
    fn ibrl_score_is_kept_when_present() {
        let rows = parse(
            r#"[{"vote_account_pubkey": "5AC692spnjbegP7ttCXJEzUe8S81sLYsqJd8Ae6Zv1xU", "slot_duration_median": "200", "ibrl_score": 87.5}]"#,
        );
        assert_eq!(rows[0].ibrl_score, Some(87.5));
    }

    #[test]
    fn a_non_numeric_slot_duration_is_an_error() {
        let result: Result<Vec<SlotTimeRecord>, _> = serde_json::from_str(
            r#"[{"vote_account_pubkey": "5AC692spnjbegP7ttCXJEzUe8S81sLYsqJd8Ae6Zv1xU", "slot_duration_median": "fast"}]"#,
        );
        assert!(result.is_err());
    }

    /// Answers every request with `status` and `body`, and returns the base URL.
    fn stub(responses: Vec<(&'static str, &'static str)>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for (status, body) in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0u8; 4096];
                let _ = stream.read(&mut request).unwrap();
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        url
    }

    #[test]
    fn a_404_means_the_epoch_is_not_ready() {
        let url = stub(vec![("404 Not Found", "")]);
        assert!(fetch_epoch(&http_client().unwrap(), &url, 1080)
            .unwrap()
            .is_none());
    }

    #[test]
    fn a_server_error_is_not_mistaken_for_a_missing_epoch() {
        let url = stub(vec![("500 Internal Server Error", "")]);
        assert!(fetch_epoch(&http_client().unwrap(), &url, 1080).is_err());
    }

    #[test]
    fn reading_stops_at_the_first_unpublished_epoch() {
        let url = stub(vec![
            (
                "200 OK",
                r#"[{"vote_account_pubkey": "5AC692spnjbegP7ttCXJEzUe8S81sLYsqJd8Ae6Zv1xU", "slot_duration_median": "200"}]"#,
            ),
            ("404 Not Found", ""),
        ]);
        let rows = fetch_slot_times(&http_client().unwrap(), &url, 1079, 1082).unwrap();
        assert_eq!(
            rows.iter().map(|row| row.epoch).collect::<Vec<_>>(),
            vec![1079]
        );
    }

    #[test]
    fn a_record_whose_account_is_not_a_pubkey_is_dropped() {
        let rows = parse(
            r#"[{"vote_account_pubkey": "5AC692spnjbegP7ttCXJEzUe8S81sLYsqJd8Ae6Zv1xU", "slot_duration_median": "200"},
                {"vote_account_pubkey": "a,b", "slot_duration_median": "200"},
                {"vote_account_pubkey": "", "slot_duration_median": "200"}]"#,
        );
        assert_eq!(
            rows.iter()
                .map(|row| row.vote_account.as_str())
                .collect::<Vec<_>>(),
            vec!["5AC692spnjbegP7ttCXJEzUe8S81sLYsqJd8Ae6Zv1xU"]
        );
    }
}
