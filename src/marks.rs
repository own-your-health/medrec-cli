//! A person's `marks.json` (`medrec-marks/v1`): what the household marked on
//! their papers and records that no rule decides. Bills marked paid
//! (`crate::bills`), items marked private or not (`crate::private`) and what
//! they say of a provider (`crate::corrections`). It is a file of its own so
//! that a medrec before 0.9.1, which rewrites `overrides.json` with only the
//! fields it knows, never drops a mark.

use pk_cli_core::CliError;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::model::Person;

pub const SCHEMA: &str = "medrec-marks/v1";

pub fn rel(p: &Person) -> String {
    format!("{}/marks.json", p.folder)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Marks {
    pub schema: String,
    pub person: String,
    /// Bills the household marked paid.
    #[serde(default)]
    pub payments: Vec<crate::bills::Payment>,
    /// The requests whose payments they took back.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub payments_undone: Vec<crate::bills::Undone>,
    /// Items the household marked private, or not, over the rules.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub private: Vec<crate::private::Mark>,
    /// What the household says of the providers their items print.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub providers: Vec<crate::corrections::Correction>,
    /// What they took back of it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub providers_undone: Vec<crate::corrections::Undone>,
    /// What a later medrec writes here, kept as it is.
    #[serde(flatten)]
    pub other: Map<String, Value>,
}

impl Marks {
    pub fn new(person: &str) -> Self {
        Marks {
            schema: SCHEMA.into(),
            person: person.into(),
            payments: Vec::new(),
            payments_undone: Vec::new(),
            private: Vec::new(),
            providers: Vec::new(),
            providers_undone: Vec::new(),
            other: Map::new(),
        }
    }
}

/// How long what was taken back is kept: longer than any request is
/// answered late.
pub const UNDONE_KEEP_SECS: i64 = 90 * 86_400;

/// Why a decision of the household's was not written to a person's
/// `marks.json` or `overrides.json`.
#[derive(Debug)]
pub enum Failed {
    /// It names what is not the person's, or says what cannot be.
    Refused(CliError),
    /// The folder could not be read or written, whatever the error's kind:
    /// a Drive file that is gone is a "not found" too.
    Folder(CliError),
}

impl From<CliError> for Failed {
    fn from(e: CliError) -> Failed {
        Failed::Folder(e)
    }
}

impl From<Failed> for CliError {
    fn from(f: Failed) -> CliError {
        match f {
            Failed::Refused(e) | Failed::Folder(e) => e,
        }
    }
}
