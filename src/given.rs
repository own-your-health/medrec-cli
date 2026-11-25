//! What a source gave of an item that the item does not show
//! (docs/data-contract.md, "Private items"). The rules of what is private
//! read it beside what is shown, so that nothing opens because a source says
//! otherwise later, or takes back what it said.

use std::borrow::Cow;
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::model::{Code, Document, PersonIndex, ProviderRef, Record};
use crate::normalize::COMPONENTS;
use crate::private;
use crate::providers::FIELDS;
use crate::services;

/// The fields of a claim that are read beside the names.
const DIAGNOSES: &str = "diagnoses";
const NOTES: &str = "payer_notes";
const LINES: &str = "items";

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
    /// The name a file it was merged with came with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// What a resource gives of the record it is read as: what that shows, and the
/// parts with no value.
pub fn gives(r: &Record) -> Given {
    let mut out = of(r);
    if !r.absent.is_empty() {
        let absent = r.absent.iter().map(|c| json!({ "code": c }));
        let shown = out.fields.get(COMPONENTS).and_then(Value::as_array);
        let all = shown.into_iter().flatten().cloned().chain(absent);
        out.fields.insert(COMPONENTS.into(), all.collect());
    }
    out
}

/// What the rules read of a record.
pub fn of(r: &Record) -> Given {
    let coded = matches!(
        r.kind.as_str(),
        "medication" | "result" | "report" | "condition" | "procedure"
    );
    // A claim is named by what it paid for.
    let named = (coded && r.kind != "condition") || r.kind == "claim";
    let mut fields = Map::new();
    for f in (FIELDS.iter()).chain(&[DIAGNOSES, NOTES, crate::about::FIELD]) {
        if let Some(v) = r.fields.get(*f) {
            fields.insert(f.to_string(), v.clone());
        }
    }
    if let Some(lines) = r.fields.get(LINES).filter(|_| r.kind == "claim") {
        fields.insert(LINES.into(), services::read_of(lines));
    }
    let parts = r.fields.get(COMPONENTS).and_then(Value::as_array);
    if let Some(parts) = parts.filter(|_| r.kind == "result") {
        let read = |p: &Value| {
            let mut part = json!({"code": p["code"]});
            // A text that says an area, and no other value.
            let text = p["value"].as_str();
            if let Some(t) = text.filter(|t| private::words(t).is_some()) {
                part["value"] = t.into();
            }
            part
        };
        fields.insert(COMPONENTS.into(), parts.iter().map(read).collect());
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
    given.name = given.name.filter(|n| d.original_name.as_ref() != Some(n));
    let is_in = |a: &Option<String>, b: &Option<String>| a.is_none() || a == b;
    let known = [GivenPaper::default()];
    let known = d.given.iter().chain(&known).any(|g| {
        is_in(&given.organization, &g.organization)
            && is_in(&given.title, &g.title)
            && is_in(&given.name, &g.name)
    });
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
        if let Some(n) = &g.name {
            x.original_name = Some(n.clone());
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

    /// The names and codes of a reading's parts are kept, and of their values
    /// the texts that say an area alone.
    #[test]
    fn what_a_readings_parts_were_is_kept() {
        let part = |name: &str, value: Value| json!({"code": {"display": name}, "value": value});
        let parts = |of: [Value; 2]| json!({"components": of, "value": "two"});
        let absent = |r: &Record, name: &str| {
            gives(&Record {
                absent: vec![serde_json::from_value(json!({ "display": name })).unwrap()],
                ..r.clone()
            })
        };
        let one = part("One", 3.into());
        let two = part("Two", "Seen".into());
        let mut r = record("result", parts([one.clone(), two]));
        let says = part("Three", "Chlamydia seen".into());
        let other = absent(&record("result", parts([one, says])), "Five");
        assert!(keep(&mut r, other.clone()));
        assert!(!keep(&mut r, other));
        let want = json!([{"fields": {
            "components": [{"code": {"display": "One"}},
                {"code": {"display": "Three"}, "value": "Chlamydia seen"},
                {"code": {"display": "Five"}}]}}]);
        assert_eq!(serde_json::to_value(&r.given).unwrap(), want);
        let as_was = as_given(&r).next().unwrap();
        let named = crate::questionnaires::told_by(&as_was);
        let named: Vec<&str> = named.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(named, ["Example one", "One", "Three", "Five"]);
        // Its own part with no value is other than what it shows.
        let own = absent(&r, "Four");
        assert!(keep(&mut r, own.clone()) && !keep(&mut r, own.clone()));
        let own = serde_json::to_string(&own).unwrap();
        assert!(!own.contains("Seen") && !own.contains("two"), "{own}");
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
