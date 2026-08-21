use crate::dto::ValidatorRecord;
use std::collections::HashMap;
use std::sync::OnceLock;

const OPERATORS_CSV: &str = include_str!("../operators.csv");

/// Injected rather than read from `operators.csv` directly, so the aggregation can be tested on a
/// mapping of its own instead of on whichever rows the file happens to carry.
pub type OperatorLookup = fn(&str) -> Option<&'static str>;

/// `#` starts a comment, so the file can carry provenance next to the rows it explains.
fn reader() -> csv::Reader<&'static [u8]> {
    csv::ReaderBuilder::new()
        .comment(Some(b'#'))
        .from_reader(OPERATORS_CSV.as_bytes())
}

fn registry() -> &'static HashMap<String, String> {
    static REGISTRY: OnceLock<HashMap<String, String>> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        let mut operators = HashMap::new();
        let mut reader = reader();
        for record in reader.records().flatten() {
            let (Some(vote_account), Some(operator)) = (record.get(0), record.get(1)) else {
                continue;
            };
            let (vote_account, operator) = (vote_account.trim(), operator.trim());
            if vote_account.is_empty() || operator.is_empty() {
                continue;
            }
            operators.insert(vote_account.to_string(), operator.to_string());
        }
        operators
    })
}

pub fn operator_of(vote_account: &str) -> Option<&'static str> {
    registry().get(vote_account).map(String::as_str)
}

pub fn stamp_operators<'a>(records: impl IntoIterator<Item = &'a mut ValidatorRecord>) {
    for record in records {
        record.operator = operator_of(&record.vote_account).map(str::to_string);
    }
}