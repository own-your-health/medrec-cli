//! A person's `marks.json` (`medrec-marks/v1`): what the household marked on
//! their papers and records that no rule decides. Bills marked paid
//! (`crate::bills`), items marked private or not (`crate::private`) and what
//! they say of a provider (`crate::corrections`). It is a file of its own so
//! that a medrec before 0.9.1, which rewrites `overrides.json` with only the
//! fields it knows, never drops a mark.

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
            private: Vec::new(),
            providers: Vec::new(),
            providers_undone: Vec::new(),
            other: Map::new(),
        }
    }
}
