//! What a person's chart holds that is about somebody else
//! (docs/data-contract.md, "About another person"): a screening a parent
//! answers about themselves, filed in their child's chart, and a result its
//! source says is of another. No list, search, total or view has it. A list of
//! the person's counts it, and it is opened by its id at the Mac alone.

use std::collections::BTreeSet;

use pk_cli_core::dates;
use serde_json::Value;

use crate::given;
use crate::model::{Document, Person, PersonIndex, Record};
use crate::questionnaires::{self, Answers};
use crate::standing;

/// The age from which a person answers a screening of themselves.
const ANSWERS_FROM: i64 = 11;

/// The field of a result that says whom its source says it is about.
pub const FIELD: &str = "about";
/// [`FIELD`]'s words.
pub const PARENT: &str = "a parent";
pub const ANOTHER: &str = "another person";

/// Who an item in the person's chart is about, where that is not the person:
/// as it is shown, else as any source gave it.
pub fn record(p: &Person, r: &Record) -> Option<&'static str> {
    if !matches!(r.kind.as_str(), "result" | "report") {
        return None;
    }
    as_is(p, r).or_else(|| given::as_given(r).find_map(|x| as_is(p, &x)))
}

fn as_is(p: &Person, r: &Record) -> Option<&'static str> {
    match r.fields.get(FIELD).and_then(Value::as_str) {
        Some(PARENT) => return Some(PARENT),
        Some(_) => return Some(ANOTHER),
        None => {}
    }
    let fetched = r.provenance.iter().filter_map(|p| p.fetched_at.as_deref());
    let day = r.date.as_deref().or(fetched.min());
    let code = r.code.as_ref();
    let asks = questionnaires::of(&r.display, code).is_some_and(|q| q.answers == Answers::Grown);
    (asks && !lab(r) && too_young(p, day)).then_some(ANOTHER)
}

fn lab(r: &Record) -> bool {
    let category = r.fields.get("category").and_then(Value::as_str);
    category.is_some_and(|c| c.to_ascii_lowercase().starts_with("lab"))
}

/// The same of a paper a source served. One the household filed is whose they
/// say it is.
fn paper(p: &Person, d: &Document) -> Option<&'static str> {
    let served = |d: &Document| d.provenances().all(|p| p.source.cli.is_some());
    let titled = |d: &Document| {
        let of = d.title.as_deref().and_then(|t| questionnaires::of(t, None));
        of.is_some_and(|q| q.answers == Answers::Grown)
    };
    let asks = titled(d) || given::papers_as_given(d).any(|x| titled(&x));
    let fetched = d.provenance.fetched_at.as_deref();
    let day = d.date.as_deref().or(fetched);
    (asks && served(d) && too_young(p, day)).then_some(ANOTHER)
}

/// Whether the person had not reached [`ANSWERS_FROM`] on a day. Not where
/// that cannot be told.
fn too_young(p: &Person, day: Option<&str>) -> bool {
    let of = |s: &str| dates::parse_iso(s.get(..10)?).ok();
    let (Some(born), Some(day)) = (p.birth_date.as_deref().and_then(of), day.and_then(of)) else {
        return false;
    };
    let (y, m, d) = born;
    day < (y + ANSWERS_FROM, m, d)
}

/// The ids of what is about somebody else. A paper that only such records link
/// goes with them.
pub fn ids(p: &Person, idx: &PersonIndex) -> BTreeSet<String> {
    let mut out: BTreeSet<String> = BTreeSet::new();
    for r in &idx.records {
        if record(p, r).is_some() {
            out.insert(r.id.clone());
        }
    }
    for d in &idx.documents {
        let links = |r: &&Record| r.documents.contains(&d.id);
        let mut by = idx.records.iter().filter(links).peekable();
        let only = by.peek().is_some() && by.all(|r| out.contains(&r.id));
        let filed = d.provenances().any(|p| p.source.cli.is_none());
        if paper(p, d).is_some() || (only && !filed) {
            out.insert(d.id.clone());
        }
    }
    out
}

/// What is said of a record opened by its id, where it is about somebody else.
pub fn of_record(p: &Person, r: &Record) -> Option<String> {
    record(p, r).map(|who| says(p, who))
}

/// The same of a paper.
pub fn of_paper(p: &Person, idx: &PersonIndex, id: &str) -> Option<String> {
    ids(p, idx).contains(id).then(|| says(p, ANOTHER))
}

fn says(p: &Person, who: &str) -> String {
    format!("about {who}, in {}'s chart", crate::printable(&p.name))
}

/// The index without it.
pub fn shown(p: &Person, idx: PersonIndex) -> PersonIndex {
    let gone = ids(p, &idx);
    if gone.is_empty() {
        return idx;
    }
    let mut out = standing::without(&idx, &gone).into_owned();
    // A claim names the results of its days, and so do its lines.
    let keep = |of: &mut serde_json::Map<String, Value>, key: &str| {
        let Some(Value::Array(ids)) = of.get_mut(key) else {
            return;
        };
        ids.retain(|id| !id.as_str().is_some_and(|id| gone.contains(id)));
        if ids.is_empty() {
            of.remove(key);
        }
    };
    for r in out.records.iter_mut().filter(|r| r.kind == "claim") {
        keep(&mut r.fields, "results");
        let lines = r.fields.get_mut("items").and_then(Value::as_array_mut);
        for line in lines.into_iter().flatten().filter_map(Value::as_object_mut) {
            keep(line, "records");
        }
    }
    out.about_another = gone.into_iter().collect();
    out
}

/// What a list of one person's says of it: how much, and nothing else. A list
/// of everyone's says nothing: it would not have shown it.
pub fn line(n: usize, person: Option<&str>) -> Option<String> {
    (n > 0 && person.is_some()).then(|| match n {
        1 => "1 item about another person, not shown".to_string(),
        n => format!("{n} items about another person, not shown"),
    })
}
