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
    /// Records whose raw file is in the folder of another source than they say
    /// they came from, or is another resource than they say.
    pub source_not_its_own: Vec<String>,
    /// Records of which a raw file is not there or does not read.
    pub unread: Vec<String>,
}

impl Against {
    pub fn clean(&self) -> bool {
        self.lists().iter().all(|(_, ids)| ids.is_empty())
    }

    /// Each list, with what it is of.
    pub fn lists(&self) -> [(&'static str, &[String]); 6] {
        [
            (
                "event with a place no source of it gives",
                &self.place_not_given,
            ),
            ("record with more than one place", &self.places_differ),
            ("record with a name no source gives", &self.name_not_given),
            ("record with a day no source gives", &self.day_not_given),
            ("record of another source", &self.source_not_its_own),
            ("record whose raw file does not read", &self.unread),
        ]
    }
}

/// Every person's events against their raw files.
pub fn events(folder: &Folder, h: &Household) -> Result<Vec<Against>, CliError> {
    h.people.iter().map(|p| of(folder, h, p)).collect()
}

fn of(folder: &Folder, h: &Household, p: &Person) -> Result<Against, CliError> {
    let built = events::of_person(folder, h, p)?;
    let idx = folder.index(p)?;
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
        let Some(place) = e["place"].as_str() else {
            continue;
        };
        if matches!(e["place_from"].as_str(), Some("linked" | "alias")) {
            out.places_of_others += 1;
            continue;
        }
        let place = folded(place);
        let given = own.iter().any(|r| {
            texts[r.id.as_str()].contains(&place) || named(h, p, r).any(|n| folded(n) == place)
        });
        let unread = own.iter().any(|r| out.unread.contains(&r.id));
        if let (false, false, Some(id)) = (given, unread, e["id"].as_str()) {
            out.place_not_given.push(id.into());
        }
    }
    Ok(out)
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
        let (mut read, mut unread, mut elsewhere) = (0, false, false);
        for pv in &r.provenance {
            let Some(path) = &pv.raw_path else { continue };
            let path = import::current_of(path).unwrap_or_else(|| path.clone());
            let Some(v) = files.read(&path)? else {
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
                elsewhere |= !is("resourceType", &of.resource_type) || !is("id", &of.id);
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
        let other = |a: &BTreeSet<String>, b: &BTreeSet<String>| {
            !a.iter().any(|x| b.iter().any(|y| events::same_org(x, y)))
        };
        if places.iter().any(|a| places.iter().any(|b| other(a, b))) {
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

/// Where a visit's resource says it was.
fn visited(v: &Value) -> BTreeSet<String> {
    if v["resourceType"] != "Encounter" {
        return BTreeSet::new();
    }
    let at = v["location"].as_array().into_iter().flatten();
    let at = at.map(|l| &l["location"]["display"]);
    let of = std::iter::once(&v["serviceProvider"]["display"]).chain(at);
    of.filter_map(Value::as_str).map(str::to_string).collect()
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
