use crate::dto::ValidatorRecord;
use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::sync::OnceLock;

const DEFAULT_CSV: &str = include_str!("../operators.default.csv");

fn registry() -> &'static HashMap<String, String> {
    static REGISTRY: OnceLock<HashMap<String, String>> = OnceLock::new();
    REGISTRY.get_or_init(|| match std::env::var_os("OPERATORS_CSV") {
        Some(path) => {
            let csv = std::fs::read_to_string(&path)
                .unwrap_or_else(|err| panic!("read {path:?} failed: {err}"));
            parse(&csv).unwrap_or_else(|err| panic!("parse {path:?} failed: {err:#}"))
        }
        None => parse(DEFAULT_CSV)
            .unwrap_or_else(|err| panic!("parse operators.default.csv failed: {err:#}")),
    })
}

/// `#` starts a comment; the file groups its rows under one per operator.
fn parse(csv: &str) -> Result<HashMap<String, String>> {
    let mut operators = HashMap::new();
    let mut reader = csv::ReaderBuilder::new()
        .comment(Some(b'#'))
        .from_reader(csv.as_bytes());

    let header = reader.headers().context("reading header")?.clone();
    // Without this a headerless file loses its first row to the header, dropping one validator.
    if header.iter().collect::<Vec<_>>() != ["vote_account", "operator"] {
        bail!("first line must be the vote_account,operator header");
    }

    for record in reader.records() {
        let record = record.context("reading record")?;
        let (Some(vote_account), Some(operator)) = (record.get(0), record.get(1)) else {
            bail!("{record:?} is not a vote_account,operator pair");
        };

        let (vote_account, operator) = (vote_account.trim(), operator.trim());
        if vote_account.is_empty() || operator.is_empty() {
            bail!("{record:?} leaves the vote account or the operator blank");
        }

        operators.insert(vote_account.to_string(), operator.to_string());
    }

    Ok(operators)
}

pub fn operator_of(vote_account: &str) -> Option<&'static str> {
    registry().get(vote_account).map(String::as_str)
}

pub fn stamp_operators<'a>(records: impl IntoIterator<Item = &'a mut ValidatorRecord>) {
    for record in records {
        record.operator = operator_of(&record.vote_account).map(str::to_string);
    }
}
