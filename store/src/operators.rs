use crate::dto::ValidatorRecord;
use csv::{required, Column};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::OnceLock;

const DEFAULT_CSV: &str = include_str!("../operators.default.csv");

const COLUMNS: [Column; 2] = [required("vote_account"), required("operator")];

#[derive(Deserialize)]
struct OperatorRow {
    vote_account: String,
    operator: String,
}

fn registry() -> &'static HashMap<String, String> {
    static REGISTRY: OnceLock<HashMap<String, String>> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        let rows: Vec<OperatorRow> = csv::load_vendored(
            DEFAULT_CSV,
            "OPERATORS_CSV",
            &COLUMNS,
            "operators.default.csv",
        )
        .unwrap_or_else(|err| panic!("{err:#}"));

        rows.into_iter()
            .map(|row| (row.vote_account, row.operator))
            .collect()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_vendored_operators_parse() {
        // One row of the file, so a broken load is not mistaken for an unmapped validator.
        assert_eq!(
            operator_of("he1iusunGwqrNtafDtLdhsUQDFvo13z9sUa36PauBtk"),
            Some("Helius")
        );
        assert!(operator_of("notAVoteAccount").is_none());
    }
}
