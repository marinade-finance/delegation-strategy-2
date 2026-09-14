//! One CSV reader for the tables this workspace carries: a header check, blank-cell validation and
//! serde row deserialization, so no caller writes its own.
//!
//! Every table is read the same way: `#` starts a comment, values are trimmed, and a bad row is an
//! error rather than a row that quietly disappears.

use anyhow::{bail, Context, Result};
use serde::de::DeserializeOwned;
use std::path::Path;

/// A column the caller needs. `required` means a blank cell in it is an error.
#[derive(Debug, Clone, Copy)]
pub struct Column {
    pub name: &'static str,
    pub required: bool,
}

pub const fn required(name: &'static str) -> Column {
    Column {
        name,
        required: true,
    }
}

pub const fn optional(name: &'static str) -> Column {
    Column {
        name,
        required: false,
    }
}

/// `label` names the table in every error message.
pub fn parse<T: DeserializeOwned>(text: &str, columns: &[Column], label: &str) -> Result<Vec<T>> {
    let mut reader = csv_reader::ReaderBuilder::new()
        .comment(Some(b'#'))
        .trim(csv_reader::Trim::All)
        .from_reader(text.as_bytes());

    let header = reader
        .headers()
        .with_context(|| format!("{label}: reading the header"))?
        .clone();

    // Columns are matched by name, so their order is the file's business and unknown ones are
    // ignored; what a table cannot survive is a missing column, or a file with no header at all,
    // which would otherwise lose its first row to one.
    for column in columns {
        if !header.iter().any(|name| name == column.name) {
            bail!(
                "{label}: header {:?} has no {} column",
                header.iter().collect::<Vec<_>>(),
                column.name
            );
        }
    }

    let index_of = |name: &str| header.iter().position(|column| column == name);
    let required_indices: Vec<(usize, &str)> = columns
        .iter()
        .filter(|column| column.required)
        .filter_map(|column| index_of(column.name).map(|index| (index, column.name)))
        .collect();

    let mut rows = Vec::new();
    for (offset, record) in reader.records().enumerate() {
        let record = record.with_context(|| format!("{label}: row {}", offset + 1))?;
        // The file's own line, so an error points at the line an editor shows.
        let line = record.position().map_or(offset as u64 + 1, |at| at.line());

        for (index, name) in &required_indices {
            if record.get(*index).unwrap_or_default().is_empty() {
                bail!("{label}:{line} leaves {name} blank");
            }
        }

        rows.push(
            record
                .deserialize(Some(&header))
                .with_context(|| format!("{label}:{line}"))?,
        );
    }

    Ok(rows)
}

/// A table vendored into the binary, overridable by pointing `env_var` at a file.
pub fn load_vendored<T: DeserializeOwned>(
    default: &'static str,
    env_var: &str,
    columns: &[Column],
    label: &str,
) -> Result<Vec<T>> {
    match std::env::var_os(env_var) {
        Some(path) => load_path(Path::new(&path), columns),
        None => parse(default, columns, label),
    }
}

pub fn load_path<T: DeserializeOwned>(path: &Path, columns: &[Column]) -> Result<Vec<T>> {
    let label = path.display().to_string();
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {label}"))?;
    parse(&text, columns, &label)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Deserialize, PartialEq)]
    struct Row {
        name: String,
        epoch: Option<u64>,
    }

    const COLUMNS: [Column; 2] = [required("name"), optional("epoch")];

    fn parse_rows(text: &str) -> Result<Vec<Row>> {
        parse(text, &COLUMNS, "test.csv")
    }

    #[test]
    fn columns_are_matched_by_name_not_position() {
        assert_eq!(
            parse_rows("epoch,name\n979,agave\n").unwrap(),
            vec![Row {
                name: "agave".into(),
                epoch: Some(979)
            }]
        );
    }

    #[test]
    fn an_unknown_column_is_ignored() {
        #[derive(Debug, Deserialize)]
        struct Named {
            name: String,
        }
        let rows: Vec<Named> = parse(
            "name,epoch,note\nagave,979,shipped late\n",
            &COLUMNS,
            "test.csv",
        )
        .unwrap();
        assert_eq!(rows[0].name, "agave");
    }

    #[test]
    fn a_missing_column_is_refused() {
        let err = parse_rows("name\nagave\n").unwrap_err().to_string();
        assert!(err.contains("no epoch column"), "{err}");
    }

    #[test]
    fn a_file_with_no_header_is_refused() {
        // Without the header check the first row would be eaten as one.
        assert!(parse_rows("agave,979\nfiredancer,1019\n").is_err());
    }

    #[test]
    fn comments_and_blank_lines_are_skipped() {
        let text = "# what this file is\n# and where it came from\nname,epoch\n\nagave,979\n\n";
        assert_eq!(parse_rows(text).unwrap().len(), 1);
    }

    #[test]
    fn a_blank_required_cell_is_refused() {
        let err = parse_rows("name,epoch\nagave,979\n,1019\n")
            .unwrap_err()
            .to_string();
        // The file's line, not the row's ordinal.
        assert!(err.contains("test.csv:3"), "{err}");
        assert!(err.contains("leaves name blank"), "{err}");
    }

    #[test]
    fn a_header_with_no_rows_parses_to_nothing() {
        assert!(parse_rows("name,epoch\n").unwrap().is_empty());
    }

    #[test]
    fn empty_input_is_refused() {
        assert!(parse_rows("").is_err());
    }

    #[test]
    fn a_blank_optional_cell_is_none() {
        assert_eq!(parse_rows("name,epoch\nagave,\n").unwrap()[0].epoch, None);
    }

    #[test]
    fn an_unparseable_value_is_refused() {
        let err = parse_rows("name,epoch\nagave,soon\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("test.csv:2"), "{err}");
    }

    #[test]
    fn values_are_trimmed() {
        assert_eq!(
            parse_rows("name,epoch\n  agave ,  979 \n").unwrap(),
            vec![Row {
                name: "agave".into(),
                epoch: Some(979)
            }]
        );
    }

    #[test]
    fn a_short_row_is_refused() {
        assert!(parse_rows("name,epoch\nagave\n").is_err());
    }

    #[test]
    fn the_env_var_replaces_the_vendored_table() {
        // What ops-infra does with OPERATORS_CSV: mounts a file and points the binary at it. The
        // var name and the path are unique to this test, so it shares no state with another.
        let var = "DS_CSV_TEST_OVERRIDE_REPLACES_VENDORED";
        let path = std::env::temp_dir().join(format!("ds-csv-{}-{var}.csv", std::process::id()));
        std::fs::write(&path, "epoch,name\n1019,firedancer\n").unwrap();
        std::env::set_var(var, &path);

        let rows: Vec<Row> =
            load_vendored("name,epoch\nagave,979\n", var, &COLUMNS, "vendored.csv").unwrap();

        std::env::remove_var(var);
        std::fs::remove_file(&path).unwrap();

        assert_eq!(rows[0].name, "firedancer");
    }

    #[test]
    fn the_vendored_table_is_used_when_the_env_var_is_unset() {
        let rows: Vec<Row> = load_vendored(
            "name,epoch\nagave,979\n",
            "DS_CSV_TEST_OVERRIDE_NEVER_SET",
            &COLUMNS,
            "vendored.csv",
        )
        .unwrap();

        assert_eq!(rows[0].name, "agave");
    }

    #[test]
    fn a_missing_file_is_reported_with_its_path() {
        let err = load_path::<Row>(Path::new("/nonexistent/table.csv"), &COLUMNS)
            .unwrap_err()
            .to_string();
        assert!(err.contains("/nonexistent/table.csv"), "{err}");
    }
}
