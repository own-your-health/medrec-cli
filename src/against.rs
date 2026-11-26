//! What a person's events and the records in them say, against the raw files
//! those records are of. The raw files are read as they are, not through what
//! shaped the records, and the answer has counts and ids alone.

use std::collections::{BTreeMap, BTreeSet};

use pk_cli_core::error::CliError;
use serde::Serialize;
use serde_json::Value;

use crate::events;
use crate::folder::Folder;
use crate::import;
use crate::model::{Household, Person, PersonIndex, Record};
use crate::normalize;
use crate::raw;

/// What of a person's events and records is not as their raw files are. Each
/// list has ids.
#[derive(Debug, Default, Serialize, PartialEq, Eq)]
pub struct Against {
    pub person: String,
    /// Their index or events could not be read, so nothing of theirs is
    /// compared (absent otherwise).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub not_compared: bool,
    pub events: usize,
    /// The records in them.
    pub records: usize,
    /// Events whose place is a linked person's or a paper's word, which is not
    /// compared.
    pub places_of_others: usize,
    /// Events whose place is in no raw file of their records, and is no name of
    /// a source of theirs.
    pub place_not_given: Vec<String>,
    /// Records whose raw files give different places, as two visits in one.
    pub places_differ: Vec<String>,
    /// Records whose name, and records whose day, is in none of their raw files.
    pub name_not_given: Vec<String>,
    pub day_not_given: Vec<String>,
    /// Records with a raw file in the folder of another source than they say
    /// they came from.
    pub source_not_its_own: Vec<String>,
    /// Records with a raw file of another resource type or id than they say.
    pub resource_not_its_own: Vec<String>,
    /// Records of which a raw file is not there, does not read, or is in a
    /// folder that could not be copied.
    pub unread: Vec<String>,
}

impl Against {
    pub fn clean(&self) -> bool {
        !self.not_compared && self.lists().iter().all(|(_, ids)| ids.is_empty())
    }

    /// Each list, with what it is of.
    pub fn lists(&self) -> [(&'static str, &[String]); 7] {
        [
            (
                "event with a place no source of it gives",
                &self.place_not_given,
            ),
            ("record with more than one place", &self.places_differ),
            ("record with a name no source gives", &self.name_not_given),
            ("record with a day no source gives", &self.day_not_given),
            (
                "record with a raw file of another source",
                &self.source_not_its_own,
            ),
            (
                "record with a raw file of another resource",
                &self.resource_not_its_own,
            ),
            ("record whose raw file does not read", &self.unread),
        ]
    }
}

/// Every person's events against their raw files. One whose files cannot be
/// read is `not_compared`, and the others are compared all the same.
pub fn events(folder: &Folder, h: &Household) -> Vec<Against> {
    let one = |p: &Person| {
        of(folder, h, p).unwrap_or_else(|_| Against {
            person: p.id.clone(),
            not_compared: true,
            ..Against::default()
        })
    };
    h.people.iter().map(one).collect()
}

fn of(folder: &Folder, h: &Household, p: &Person) -> Result<Against, CliError> {
    // One reading: the events are made of the index they are compared by.
    let idx = folder.index(p)?;
    let built = events::both_of(folder, h, p, &idx)?.events;
    let by_id: BTreeMap<&str, &Record> = idx.records.iter().map(|r| (r.id.as_str(), r)).collect();
    let mut evs: Vec<(&Value, Vec<&Record>)> = Vec::new();
    for e in built["events"].as_array().into_iter().flatten() {
        let members = e["members"].as_array().into_iter().flatten();
        let own = members.filter(|m| m.get("person").is_none());
        let own = own.filter_map(|m| by_id.get(m["record"].as_str()?).copied());
        evs.push((e, own.collect()));
    }
    let mut records: BTreeMap<&str, &Record> = BTreeMap::new();
    records.extend(
        evs.iter()
            .flat_map(|(_, of)| of)
            .map(|r| (r.id.as_str(), *r)),
    );
    let files = raw::Files::of(folder, records.values().copied())?;
    let mut out = Against {
        person: p.id.clone(),
        events: evs.len(),
        records: records.len(),
        ..Against::default()
    };
    let mut texts: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    for (id, r) in &records {
        let said = out.record(r, &idx, &files)?;
        texts.insert(id, said);
    }
    for (e, own) in evs {
        let given = |r: &Record, place: &str| {
            texts[r.id.as_str()].contains(place) || named(h, p, r).any(|n| folded(n) == place)
        };
        match place(e, &own, &out.unread, given) {
            Place::Others => out.places_of_others += 1,
            Place::NotGiven(id) => out.place_not_given.push(id),
            Place::None | Place::Given | Place::Unread => {}
        }
    }
    Ok(out)
}

/// What is so of an event's place.
#[derive(Debug, PartialEq, Eq)]
enum Place {
    /// It has none.
    None,
    /// A linked person's or a paper's word, which is not compared.
    Others,
    /// A raw file of one of its records gives it, or a source's name is it.
    Given,
    /// A raw file of one of its records does not read, so it may be that one.
    Unread,
    /// None gives it: the event's id.
    NotGiven(String),
}

fn place(
    e: &Value,
    own: &[&Record],
    unread: &[String],
    given: impl Fn(&Record, &str) -> bool,
) -> Place {
    let Some(at) = e["place"].as_str() else {
        return Place::None;
    };
    if matches!(e["place_from"].as_str(), Some("linked" | "alias")) {
        return Place::Others;
    }
    let at = folded(at);
    if own.iter().any(|r| given(r, &at)) {
        Place::Given
    } else if own.iter().any(|r| unread.contains(&r.id)) {
        Place::Unread
    } else {
        e["id"]
            .as_str()
            .map_or(Place::None, |id| Place::NotGiven(id.into()))
    }
}

impl Against {
    /// Compare a record with its raw files, and give their texts.
    fn record(
        &mut self,
        r: &Record,
        idx: &PersonIndex,
        files: &raw::Files,
    ) -> Result<BTreeSet<String>, CliError> {
        let mut texts = BTreeSet::new();
        let mut places: Vec<BTreeSet<String>> = Vec::new();
        let (mut read, mut unread, mut elsewhere, mut other) = (0, false, false, false);
        for pv in &r.provenance {
            let Some(path) = &pv.raw_path else { continue };
            let path = import::current_of(path).unwrap_or_else(|| path.clone());
            // A folder that could not be copied is unread, as a file not there is.
            let Some(v) = files.read(&path).ok().flatten() else {
                unread = true;
                continue;
            };
            read += 1;
            let dir = import::source_dir_of(&path);
            let mut runs = idx
                .imports
                .iter()
                .filter(|i| Some(i.source_dir.as_str()) == dir);
            let its = |i: &crate::model::ImportRun| {
                pv.source.cli.as_ref() == Some(&i.cli)
                    && pv.source.organization_id.as_ref() == Some(&i.organization)
            };
            elsewhere |= !runs.any(its);
            if let Some(of) = &pv.source_resource {
                let is = |k: &str, of: &str| of.is_empty() || v[k].as_str().is_none_or(|v| v == of);
                other |= !is("resourceType", &of.resource_type) || !is("id", &of.id);
            }
            let own = visited(&v);
            if !own.is_empty() {
                places.push(own);
            }
            strings(&v, &mut texts);
        }
        if unread {
            self.unread.push(r.id.clone());
        }
        if elsewhere {
            self.source_not_its_own.push(r.id.clone());
        }
        if other {
            self.resource_not_its_own.push(r.id.clone());
        }
        if differ(&places) {
            self.places_differ.push(r.id.clone());
        }
        // One that is not there may be the one that says it.
        if read > 0 && !unread {
            // A claim is named by medrec, for what it paid for.
            let named = r.kind == "claim" || r.display == normalize::default_display(&r.kind);
            if !named && !texts.contains(&folded(&r.display)) {
                self.name_not_given.push(r.id.clone());
            }
            let day = |d: &str| texts.iter().any(|t| t.starts_with(&folded(d)));
            if r.date.as_deref().is_some_and(|d| !day(d)) {
                self.day_not_given.push(r.id.clone());
            }
        }
        Ok(texts)
    }
}

/// The names of the sources a record came from.
fn named<'a>(h: &'a Household, p: &'a Person, r: &'a Record) -> impl Iterator<Item = &'a str> {
    let of = move |s: &&crate::model::Source| {
        let from = |pv: &crate::model::Provenance| {
            s.cli.is_some() && s.cli == pv.source.cli && s.organization == pv.source.organization_id
        };
        s.person == p.id && r.provenance.iter().any(from)
    };
    h.sources
        .iter()
        .filter(of)
        .filter_map(|s| s.organization_name.as_deref())
}

/// Whether two of these resources' places are of no one place: of each pair of
/// resources, none of the names of one is the other's.
fn differ(places: &[BTreeSet<String>]) -> bool {
    let apart = |a: &BTreeSet<String>, b: &BTreeSet<String>| {
        !a.iter().any(|x| b.iter().any(|y| events::same_org(x, y)))
    };
    let mut pairs = places.iter().enumerate();
    pairs.any(|(i, a)| places[i + 1..].iter().any(|b| apart(a, b)))
}

/// Where a visit's resource says it was: the names that can be a place, not a
/// records connection's or one with no word to compare.
fn visited(v: &Value) -> BTreeSet<String> {
    if v["resourceType"] != "Encounter" {
        return BTreeSet::new();
    }
    let at = v["location"].as_array().into_iter().flatten();
    let at = at.map(|l| &l["location"]["display"]);
    let of = std::iter::once(&v["serviceProvider"]["display"]).chain(at);
    let of = of.filter_map(Value::as_str);
    let of = of.filter(|s| !crate::plain::is_connection(s) && !events::org_tokens(s).is_empty());
    of.map(str::to_string).collect()
}

/// Every text in a resource, as [`folded`] has it.
fn strings(v: &Value, out: &mut BTreeSet<String>) {
    match v {
        Value::String(s) => {
            out.insert(folded(s));
        }
        Value::Array(a) => a.iter().for_each(|v| strings(v, out)),
        Value::Object(m) => m.values().for_each(|v| strings(v, out)),
        _ => {}
    }
}

/// A text as it is compared: its letters and digits, in lower case.
fn folded(s: &str) -> String {
    let words = s.split(|c: char| !c.is_alphanumeric());
    let words: Vec<&str> = words.filter(|w| !w.is_empty()).collect();
    words.join(" ").to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn record(id: &str) -> Record {
        serde_json::from_value(json!({
            "id": id, "kind": "encounter", "key": id, "display": "Office visit",
            "fields": {}, "provenance": []
        }))
        .unwrap()
    }

    #[test]
    fn a_place_a_linked_person_or_a_paper_gave_is_not_compared() {
        let (a, b) = (record("r_a"), record("r_b"));
        let own = [&a, &b];
        let gives = |r: &Record, at: &str| r.id == "r_a" && at == "sample clinic";
        let event =
            |place: Value, from: &str| json!({"id": "e_1", "place": place, "place_from": from});
        let unread = ["r_b".to_string()];
        let cases = [
            (event("Elsewhere".into(), "linked"), &[][..], Place::Others),
            (event("Elsewhere".into(), "alias"), &[][..], Place::Others),
            (
                event("Sample Clinic".into(), "portal"),
                &[][..],
                Place::Given,
            ),
            (
                event("Sample Clinic".into(), "portal"),
                &unread[..],
                Place::Given,
            ),
            (
                event("Elsewhere".into(), "portal"),
                &unread[..],
                Place::Unread,
            ),
            (
                event("Elsewhere".into(), "portal"),
                &[][..],
                Place::NotGiven("e_1".into()),
            ),
            (event(Value::Null, "portal"), &[][..], Place::None),
        ];
        for (e, unread, want) in cases {
            assert_eq!(place(&e, &own, unread, gives), want, "{e}");
        }
    }

    #[test]
    fn one_resource_has_one_place_and_a_connection_or_a_number_is_none() {
        let visit = |names: &[&str]| -> BTreeSet<String> {
            let at: Vec<Value> = names[1..]
                .iter()
                .map(|n| json!({"location": {"display": n}}))
                .collect();
            visited(&json!({
                "resourceType": "Encounter",
                "serviceProvider": {"display": names[0]},
                "location": at
            }))
        };
        assert!(visit(&["214"]).is_empty());
        assert!(visit(&["Example Health (all practices)"]).is_empty());
        let clinic = visit(&["Sample Clinic", "Sample Clinic Main"]);
        assert_eq!(clinic.len(), 2);
        assert!(!differ(std::slice::from_ref(&clinic)));
        assert!(!differ(&[clinic.clone(), visit(&["Sample Clinic"])]));
        assert!(differ(&[clinic, visit(&["Elsewhere Hospital"])]));
        assert!(!differ(&[]));
        assert!(
            visited(&json!({"resourceType": "Claim", "provider": {"display": "X"}})).is_empty()
        );
    }
}
