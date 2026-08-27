use crate::dto::ValidatorRecord;
use std::collections::HashMap;
use std::sync::OnceLock;

const OPERATORS_CSV: &str = include_str!("../operators.csv");

fn registry() -> &'static HashMap<String, String> {
    static REGISTRY: OnceLock<HashMap<String, String>> = OnceLock::new();
    REGISTRY.get_or_init(|| parse(OPERATORS_CSV))
}

/// `#` starts a comment; the file groups its rows under one per operator.
///
/// Panics if format wrong.
fn parse(csv: &str) -> HashMap<String, String> {
    let mut operators = HashMap::new();
    let mut reader = csv::ReaderBuilder::new()
        .comment(Some(b'#'))
        .from_reader(csv.as_bytes());

    for record in reader.records() {
        let record = record.unwrap_or_else(|err| panic!("operators.csv: {err}"));
        let (Some(vote_account), Some(operator)) = (record.get(0), record.get(1)) else {
            panic!("operators.csv: {record:?} is not a vote_account,operator pair");
        };

        let (vote_account, operator) = (vote_account.trim(), operator.trim());
        assert!(
            !vote_account.is_empty() && !operator.is_empty(),
            "operators.csv: {record:?} leaves the vote account or the operator blank"
        );

        operators.insert(vote_account.to_string(), operator.to_string());
    }

    operators
}

pub fn operator_of(vote_account: &str) -> Option<&'static str> {
    registry().get(vote_account).map(String::as_str)
}

pub fn stamp_operators<'a>(records: impl IntoIterator<Item = &'a mut ValidatorRecord>) {
    for record in records {
        record.operator = operator_of(&record.vote_account).map(str::to_string);
    }
}
