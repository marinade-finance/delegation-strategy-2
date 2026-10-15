use std::collections::HashMap;

use clap::Parser;
use log::{info, warn};
use rust_decimal::Decimal;
use serde::Deserialize;
use tokio_postgres::Client;

#[derive(Debug, Parser)]
pub struct CheckVatAdmissionParams {
    /// solana-snapshot-parser `validators.json` of a closed epoch.
    #[arg(long = "validators-json")]
    validators_json: String,
}

#[derive(Debug, Deserialize)]
pub struct ParserValidators {
    pub epoch: u64,
    pub validator_metas: Vec<ParserValidatorMeta>,
}

#[derive(Debug, Deserialize)]
pub struct ParserValidatorMeta {
    pub vote_account: String,
    // The parser writes false only for a missing BLS key, and leaves the key out for any other drop.
    #[serde(default)]
    pub inflation_rewards_admitted: Option<bool>,
}

#[derive(Debug, Default, Clone)]
pub struct StoredVerdict {
    pub vat_admitted: Option<bool>,
    pub vat_admission_reason: Option<String>,
    pub bls_pubkey: Option<String>,
    pub activated_stake: Option<Decimal>,
    pub vote_account_lamports: Option<Decimal>,
    pub pending_delegator_rewards: Option<Decimal>,
}

#[derive(Debug)]
pub struct Mismatch {
    pub vote_account: String,
    pub parser_admitted: Option<bool>,
    pub stored: Option<StoredVerdict>,
}

pub fn compare_vat_admission(
    parser: &[ParserValidatorMeta],
    stored: &HashMap<String, StoredVerdict>,
) -> Vec<Mismatch> {
    parser
        .iter()
        .filter_map(|meta| {
            let verdict = stored.get(&meta.vote_account);
            let agrees = match (meta.inflation_rewards_admitted, verdict) {
                (Some(admitted), Some(verdict)) => verdict.vat_admitted == Some(admitted),
                (None, Some(verdict)) => {
                    verdict.vat_admitted == Some(false)
                        && verdict.vat_admission_reason.as_deref() != Some("no_bls_key")
                }
                (_, None) => false,
            };
            (!agrees).then(|| Mismatch {
                vote_account: meta.vote_account.clone(),
                parser_admitted: meta.inflation_rewards_admitted,
                stored: verdict.cloned(),
            })
        })
        .collect()
}

pub async fn check_vat_admission(
    params: CheckVatAdmissionParams,
    psql_client: &mut Client,
) -> anyhow::Result<()> {
    let parser: ParserValidators =
        serde_json::from_reader(std::fs::File::open(&params.validators_json)?)?;
    let stored: HashMap<String, StoredVerdict> = psql_client
        .query(
            "SELECT vote_account, vat_admitted, vat_admission_reason, bls_pubkey, activated_stake,
                    vote_account_lamports, pending_delegator_rewards
             FROM validators WHERE epoch = $1",
            &[&Decimal::from(parser.epoch)],
        )
        .await?
        .into_iter()
        .map(|row| {
            (
                row.get("vote_account"),
                StoredVerdict {
                    vat_admitted: row.get("vat_admitted"),
                    vat_admission_reason: row.get("vat_admission_reason"),
                    bls_pubkey: row.get("bls_pubkey"),
                    activated_stake: row.get("activated_stake"),
                    vote_account_lamports: row.get("vote_account_lamports"),
                    pending_delegator_rewards: row.get("pending_delegator_rewards"),
                },
            )
        })
        .collect();

    let mismatches = compare_vat_admission(&parser.validator_metas, &stored);
    for mismatch in &mismatches {
        warn!("VAT admission mismatch: {mismatch:?}");
    }
    info!(
        "Epoch {}: {} parser rows, {} DS2 rows, {} mismatches",
        parser.epoch,
        parser.validator_metas.len(),
        stored.len(),
        mismatches.len()
    );
    if !mismatches.is_empty() {
        anyhow::bail!(
            "{} VAT admission mismatches in epoch {}",
            mismatches.len(),
            parser.epoch
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parser(json: &str) -> Vec<ParserValidatorMeta> {
        serde_json::from_str::<ParserValidators>(json)
            .unwrap()
            .validator_metas
    }

    fn verdict(admitted: Option<bool>, reason: Option<&str>) -> StoredVerdict {
        StoredVerdict {
            vat_admitted: admitted,
            vat_admission_reason: reason.map(String::from),
            ..Default::default()
        }
    }

    const FILE: &str = r#"{
        "epoch": 1048,
        "validator_metas": [
            {"vote_account": "admitted", "stake": 5, "inflation_rewards_admitted": true},
            {"vote_account": "no_key", "stake": 5, "inflation_rewards_admitted": false},
            {"vote_account": "dropped", "stake": 5}
        ]
    }"#;

    #[test]
    fn matching_verdicts_give_no_mismatch() {
        let stored = HashMap::from([
            ("admitted".to_string(), verdict(Some(true), None)),
            (
                "no_key".to_string(),
                verdict(Some(false), Some("no_bls_key")),
            ),
            (
                "dropped".to_string(),
                verdict(Some(false), Some("low_balance")),
            ),
        ]);
        assert!(compare_vat_admission(&parser(FILE), &stored).is_empty());
    }

    #[test]
    fn a_different_verdict_or_a_missing_row_is_a_mismatch() {
        let stored = HashMap::from([
            (
                "admitted".to_string(),
                verdict(Some(false), Some("no_stake")),
            ),
            ("no_key".to_string(), verdict(None, None)),
        ]);
        let mismatches: Vec<String> = compare_vat_admission(&parser(FILE), &stored)
            .into_iter()
            .map(|m| m.vote_account)
            .collect();
        assert_eq!(mismatches, ["admitted", "no_key", "dropped"]);
    }

    #[test]
    fn a_parser_drop_for_another_reason_must_not_be_a_missing_key_in_ds2() {
        let stored = HashMap::from([
            ("admitted".to_string(), verdict(Some(true), None)),
            (
                "no_key".to_string(),
                verdict(Some(false), Some("no_bls_key")),
            ),
            (
                "dropped".to_string(),
                verdict(Some(false), Some("no_bls_key")),
            ),
        ]);
        let mismatches = compare_vat_admission(&parser(FILE), &stored);
        assert_eq!(mismatches.len(), 1);
        assert_eq!(mismatches[0].vote_account, "dropped");
    }
}
