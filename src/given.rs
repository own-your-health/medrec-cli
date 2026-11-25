//! What a source gave of an item that the item does not show
//! (docs/data-contract.md, "Private items"). The rules of what is private
//! read it beside what is shown, so that nothing opens because a source says
//! otherwise later, or takes back what it said.

use std::borrow::Cow;
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::model::{Code, Document, PersonIndex, ProviderRef, Record};
use crate::providers::FIELDS;

/// The field of a claim that is read beside the names.
const DIAGNOSES: &str = "diagnoses";

/// What the rules read of a record, as a source gave it: the parts that were
/// other than those shown.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Given {
    /// The raw file it was read from, where that is known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_path: Option<String>,
    /// The CLI of a source that no longer has the record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cli: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<Code>,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub fields: Map<String, Value>,
    /// The NPI of the provider a field prints.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub npis: BTreeMap<String, String>,
}

/// The same of a paper.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GivenPaper {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// What the rules read of a record.
pub fn of(r: &Record) -> Given {
    let coded = matches!(
        r.kind.as_str(),
        "medication" | "result" | "report" | "condition"
    );
    // A pharmacy claim is named by its medicine.
    let named = (coded && r.kind != "condition") || crate::private::dispensed(r).is_some();
    let mut fields = Map::new();
    for f in FIELDS.iter().chain(&[DIAGNOSES, crate::about::FIELD]) {
        if let Some(v) = r.fields.get(*f) {
            fields.insert(f.to_string(), v.clone());
        }
    }
    let npi = |p: &ProviderRef| Some((p.field.clone(), p.npi.clone()?));
    Given {
        raw_path: None,
        cli: None,
        display: named.then(|| r.display.clone()),
        code: r.code.clone().filter(|_| coded),
        npis: r.providers.iter().filter_map(npi).collect(),
        fields,
    }
}

impl Given {
    fn is_empty(&self) -> bool {
        let Given {
            raw_path: _,
            cli,
            display,
            code,
            fields,
            npis,
        } = self;
        cli.is_none() && display.is_none() && code.is_none() && fields.is_empty() && npis.is_empty()
    }

    /// The parts that `shown` does not have. A provider's NPI goes with the
    /// name it is of.
    fn without(mut self, shown: &Given) -> Given {
        if self.display == shown.display {
            self.display = None;
        }
        if self.code == shown.code {
            self.code = None;
        }
        self.fields.retain(|k, v| shown.fields.get(k) != Some(v));
        let fields = &self.fields;
        self.npis
            .retain(|k, n| fields.contains_key(k) || shown.npis.get(k) != Some(n));
        self
    }

    /// Whether `other` has every part of it.
    fn is_in(&self, other: &Given) -> bool {
        let same = |a: &Option<String>, b: &Option<String>| a.is_none() || a == b;
        same(&self.cli, &other.cli)
            && same(&self.display, &other.display)
            && (self.code.is_none() || self.code == other.code)
            && (self.fields.iter()).all(|(k, v)| other.fields.get(k) == Some(v))
            && (self.npis.iter()).all(|(k, n)| {
                other.npis.get(k) == Some(n) && other.fields.get(k) == self.fields.get(k)
            })
    }
}

/// Keep what a source gave of a record that the record does not show. Whether
/// that was not kept.
pub fn keep(r: &mut Record, given: Given) -> bool {
    let given = given.without(&of(r));
    let known = given.is_empty() || r.given.iter().any(|g| given.is_in(g));
    if !known {
        r.given.push(given);
    }
    !known
}

/// The same of a paper.
pub fn keep_paper(d: &mut Document, mut given: GivenPaper) -> bool {
    given.organization = given
        .organization
        .filter(|o| d.organization.as_ref() != Some(o));
    given.title = given.title.filter(|t| d.title.as_ref() != Some(t));
    let same = |a: &Option<String>, b: &Option<String>| a.is_none() || a == b;
    let mut kept = d.given.iter();
    let known =
        kept.any(|g| same(&given.organization, &g.organization) && same(&given.title, &g.title));
    let known = known || (given.organization.is_none() && given.title.is_none());
    if !known {
        d.given.push(given);
    }
    !known
}

/// A record as each source's given values make it.
pub fn as_given(r: &Record) -> impl Iterator<Item = Record> + '_ {
    r.given.iter().map(|g| {
        let mut x = r.clone();
        x.given.clear();
        if let (Some(cli), Some(p)) = (&g.cli, x.provenance.first_mut()) {
            p.source.cli = Some(cli.clone());
        }
        if let Some(d) = &g.display {
            x.display = d.clone();
        }
        if let Some(c) = &g.code {
            x.code = Some(c.clone());
        }
        // A provider's NPI is of the name it was given with.
        x.providers
            .retain(|p| !g.fields.contains_key(&p.field) && !g.npis.contains_key(&p.field));
        for (k, v) in &g.fields {
            x.fields.insert(k.clone(), v.clone());
        }
        for (k, n) in &g.npis {
            x.providers.push(ProviderRef {
                field: k.clone(),
                npi: Some(n.clone()),
                state: None,
                city: None,
            });
        }
        x
    })
}

/// The same of a paper.
pub fn papers_as_given(d: &Document) -> impl Iterator<Item = Document> + '_ {
    d.given.iter().map(|g| {
        let mut x = d.clone();
        x.given.clear();
        if let Some(o) = &g.organization {
            x.organization = Some(o.clone());
        }
        if let Some(t) = &g.title {
            x.title = Some(t.clone());
        }
        x
    })
}

/// An index with each item as its given values make it beside it, under the
/// id it has: for what reads names alone.
pub fn index(idx: &PersonIndex) -> Cow<'_, PersonIndex> {
    let records: Vec<Record> = idx.records.iter().flat_map(as_given).collect();
    let documents: Vec<Document> = idx.documents.iter().flat_map(papers_as_given).collect();
    if records.is_empty() && documents.is_empty() {
        return Cow::Borrowed(idx);
    }
    let mut out = idx.clone();
    out.records.extend(records);
    out.documents.extend(documents);
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn record(kind: &str, fields: Value) -> Record {
        serde_json::from_value(json!({
            "id": "r_aaaaaaaaaaaaaaaaaaaa", "kind": kind, "key": "k", "display": "Example one",
            "fields": fields, "provenance": [{"source": {"kind": "fhir", "cli": "fakeprov"},
                "method": "fhir-api", "filed_at": "2024-01-02T10:00:00Z", "filed_by": "medrec"}]
        }))
        .unwrap()
    }

    fn given(v: Value) -> Given {
        serde_json::from_value(v).unwrap()
    }

    /// Only what is not shown is kept, once, however it comes again.
    #[test]
    fn what_is_shown_or_kept_is_not_kept_again() {
        let mut r = record(
            "claim",
            json!({"provider": "Example A", "facility": "Example B"}),
        );
        let shown = of(&r);
        assert!(!keep(&mut r, shown));
        let other = given(json!({"fields": {"provider": "Sample C", "facility": "Example B"}}));
        assert!(keep(&mut r, other.clone()));
        assert_eq!(
            serde_json::to_value(&r.given).unwrap(),
            json!([{"fields": {"provider": "Sample C"}}])
        );
        assert!(!keep(&mut r, other));
        // What it shows now is another: what it showed is kept too.
        let was = of(&r);
        r.fields["provider"] = json!("Sample C");
        assert!(keep(&mut r, was));
        assert_eq!(as_given(&r).count(), 2);
    }

    /// An NPI is of the name it came with.
    #[test]
    fn an_npi_goes_with_its_name() {
        let mut r = record("claim", json!({"provider": "Example A"}));
        r.providers =
            serde_json::from_value(json!([{"field": "provider", "npi": "0000000012"}])).unwrap();
        // The same one of another name.
        let g =
            given(json!({"fields": {"provider": "Sample C"}, "npis": {"provider": "0000000012"}}));
        assert!(keep(&mut r, g));
        // A name with none.
        assert!(keep(
            &mut r,
            given(json!({"fields": {"provider": "Sample D"}}))
        ));
        // Another of the same name.
        assert!(keep(
            &mut r,
            given(json!({"npis": {"provider": "0000000020"}}))
        ));
        let npi = |x: &Record| x.providers.iter().find_map(|p| p.npi.clone());
        let name = |x: &Record| x.fields["provider"].as_str().unwrap().to_string();
        let all: Vec<_> = as_given(&r).map(|x| (name(&x), npi(&x))).collect();
        let n = |x: &str| Some(x.to_string());
        assert_eq!(
            all,
            [
                ("Sample C".to_string(), n("0000000012")),
                ("Sample D".to_string(), None),
                ("Example A".to_string(), n("0000000020")),
            ]
        );
        // The index has each beside the record, under its id.
        let idx: PersonIndex = serde_json::from_value(json!({
            "schema": "medrec-index/v1", "person": "p_testaaaaaaaaaaaaaaaa", "rev": 1,
            "imports": [], "documents": [], "studies": [], "records": [r]}))
        .unwrap();
        let of_id = |r: &&Record| r.id == "r_aaaaaaaaaaaaaaaaaaaa";
        assert_eq!(index(&idx).records.iter().filter(of_id).count(), 4);
    }

    /// The CLI of a source that has left the record is one it came from.
    #[test]
    fn a_source_that_left_is_kept() {
        let mut r = record("procedure", json!({}));
        assert!(keep(&mut r, given(json!({"cli": "otherprov"}))));
        assert!(!keep(&mut r, given(json!({"cli": "otherprov"}))));
        let x = as_given(&r).next().unwrap();
        assert_eq!(x.provenance[0].source.cli.as_deref(), Some("otherprov"));
    }
}
