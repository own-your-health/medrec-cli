//! Many filings in one command (`review file --from`, `intake --from`):
//! lines of JSON, each one filing's values, read and checked before the
//! folder is touched, then filed in order under one folder lock, so the
//! views are brought up to date once, at the end, instead of after each.

use std::io::Read;
use std::path::PathBuf;

use pk_cli_core::CliError;
use serde::de::DeserializeOwned;
use serde::Deserialize;

use crate::intake::{self, Given};

/// The most a list may hold, in bytes.
const MAX_BYTES: u64 = 4 * 1024 * 1024;

/// One `review file --from` line: the item, and the values to file it with.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewLine {
    pub id: String,
    pub person: Option<String>,
    #[serde(rename = "type")]
    pub doc_type: Option<String>,
    pub date: Option<String>,
    pub org: Option<String>,
}

impl ReviewLine {
    pub fn given(&self) -> Given {
        Given {
            person: self.person.clone(),
            doc_type: self.doc_type.clone(),
            date: self.date.clone(),
            organization: self.org.clone(),
            classified_by: None,
        }
    }
}

/// One `intake --from` line: a file, and what is known of it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntakeLine {
    pub file: PathBuf,
    pub person: Option<String>,
    #[serde(rename = "type")]
    pub doc_type: Option<String>,
    pub date: Option<String>,
    pub org: Option<String>,
    pub classified_by: Option<String>,
}

impl IntakeLine {
    /// Its values; `classified_by` the line's, else the command's.
    pub fn given(&self, classified_by: Option<&str>) -> Given {
        Given {
            person: self.person.clone(),
            doc_type: self.doc_type.clone(),
            date: self.date.clone(),
            organization: self.org.clone(),
            classified_by: self
                .classified_by
                .clone()
                .or_else(|| classified_by.map(str::to_string)),
        }
    }
}

/// The lines of a list at `from` (a path, or `-` for standard input), each
/// with its line number: one JSON object per line, blank lines skipped.
pub fn read<T: DeserializeOwned>(from: &str) -> Result<Vec<(usize, T)>, CliError> {
    let mut text = String::new();
    let read = if from == "-" {
        std::io::stdin()
            .take(MAX_BYTES + 1)
            .read_to_string(&mut text)
    } else {
        std::fs::File::open(from).and_then(|f| f.take(MAX_BYTES + 1).read_to_string(&mut text))
    };
    read.map_err(|e| CliError::Usage(format!("cannot read the list {from}: {e}")))?;
    parse(&text)
}

/// The lines of a list's text (`read`).
pub fn parse<T: DeserializeOwned>(text: &str) -> Result<Vec<(usize, T)>, CliError> {
    if text.len() as u64 > MAX_BYTES {
        return Err(CliError::Usage(format!(
            "the list is over {} MiB",
            MAX_BYTES / 1024 / 1024
        )));
    }
    let mut lines = Vec::new();
    for (n, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let v = serde_json::from_str(line)
            .map_err(|e| CliError::Usage(format!("list line {}: {e}", n + 1)))?;
        lines.push((n + 1, v));
    }
    if lines.is_empty() {
        return Err(CliError::Usage("the list has no lines".into()));
    }
    Ok(lines)
}

/// Each line's values checked as the flags would be, before any is filed.
pub fn validate(lines: &[(usize, Given)]) -> Result<(), CliError> {
    for (n, g) in lines {
        intake::validate_given(g)
            .map_err(|e| CliError::Usage(format!("list line {n}: {}", e.message())))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_list_is_one_json_object_per_line_checked_before_anything_is_filed() {
        let lines: Vec<(usize, ReviewLine)> = parse(
            "{\"id\":\"rv_a\",\"person\":\"Robin\",\"type\":\"lab_report\",\"date\":\"2026-03-04\"}\n\n{\"id\":\"rv_b\",\"org\":\"Example Health\"}\n",
        )
        .unwrap();
        assert_eq!(
            lines
                .iter()
                .map(|(n, l)| (*n, l.id.as_str()))
                .collect::<Vec<_>>(),
            [(1, "rv_a"), (3, "rv_b")]
        );
        assert_eq!(
            lines[1].1.given().organization.as_deref(),
            Some("Example Health")
        );

        // An unknown field, a line that is not an object and an empty list
        // are usage errors that name the line.
        let e = parse::<ReviewLine>("{\"id\":\"rv_a\",\"organisation\":\"x\"}").unwrap_err();
        assert!(e.message().starts_with("list line 1:"), "{}", e.message());
        assert!(parse::<ReviewLine>("{\"id\":\"rv_a\"}\nnot json").is_err());
        assert!(parse::<ReviewLine>("\n \n").is_err());

        // Values are checked as the flags are.
        let bad = [
            (1, lines[0].1.given()),
            (
                2,
                Given {
                    doc_type: Some("statement".into()),
                    ..lines[0].1.given()
                },
            ),
        ];
        let e = validate(&bad).unwrap_err();
        assert!(
            e.message()
                .starts_with("list line 2: unknown document type"),
            "{}",
            e.message()
        );

        // An intake line's classifier is its own, else the command's.
        let lines: Vec<(usize, IntakeLine)> =
            parse("{\"file\":\"a.pdf\"}\n{\"file\":\"b.pdf\",\"classified_by\":\"me\"}").unwrap();
        assert_eq!(
            lines[0].1.given(Some("scans")).classified_by.as_deref(),
            Some("scans")
        );
        assert_eq!(
            lines[1].1.given(Some("scans")).classified_by.as_deref(),
            Some("me")
        );
    }
}
