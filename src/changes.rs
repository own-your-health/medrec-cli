//! What a `medrec reindex` would change in the views, counted before it is run:
//! what its records would make of them against what the folder has. Counts
//! alone, so that they can be shown to anyone: no name, date, amount or id, and
//! no kind of questionnaire that there is one of.

use std::collections::{BTreeMap, BTreeSet};

use pk_cli_core::CliError;
use serde::Serialize;
use serde_json::Value;

use crate::about;
use crate::folder::Folder;
use crate::model::{today, Document, Person, PersonIndex, Record};
use crate::questionnaires;
use crate::standing::{self, Standings};
use crate::viewfiles::Preview;
use crate::views;

pub type Counts = BTreeMap<String, usize>;

/// How a list would be otherwise.
#[derive(Debug, Default, PartialEq, Eq, Serialize)]
pub struct Moved {
    pub added: usize,
    pub removed: usize,
    /// In both, with other words or amounts. A list of ids alone has none.
    pub changed: usize,
}

#[derive(Debug, Default, Serialize)]
pub struct Changes {
    /// The medrec that made the views these are against. None where the folder
    /// has none of the person's: then every private item, event and bill is
    /// counted as added.
    pub against: Option<String>,
    /// Those were made from other files than the folder has.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub stale: bool,
    pub private: Private,
    /// Entered in error, by kind.
    pub in_error: Counts,
    /// Conditions, allergies and medicines that a list of current ones has.
    pub current: BTreeMap<&'static str, Moved>,
    /// Events; changed ones have other money.
    pub events: Moved,
    /// Bills; changed ones have another amount due, paid or left.
    pub bills: Moved,
    pub worth_a_call: Moved,
    /// Shares nothing has asked for.
    pub not_billed: Moved,
    pub about_another: about::Counted,
    pub papers: Papers,
}

#[derive(Debug, Default, Serialize)]
pub struct Private {
    /// Open before, or not there, by kind.
    pub added: Counts,
    /// The rules that make them private.
    pub added_by: Counts,
    /// The questionnaires of them, by kind; those of a kind there is one of
    /// together.
    pub questionnaires: Counts,
    /// Private before and open now, by kind.
    pub opened: Counts,
    /// Private before and not there now, by kind: one that is about somebody
    /// else, or an event that is another now.
    pub gone: Counts,
    /// Private by what a source gave alone, by kind.
    pub given: Counts,
    /// Open with a private part, by kind.
    pub parts: Counts,
}

#[derive(Debug, Default, Serialize)]
pub struct Papers {
    /// Filed before medrec kept every source of a paper (0.9.9).
    pub filed_before: usize,
    /// Those of them another source of which was found.
    pub source_found: usize,
}

/// The first medrec that kept every source of a paper.
const KEEPS_SOURCES: (u32, u32, u32) = (0, 9, 9);
/// What the kinds of questionnaire that there is one of are counted as.
const ONE_OF_A_KIND: &str = "one of its kind";
/// What counts as another amount, by list.
const MONEY: [&str; 5] = ["currency", "billed", "allowed", "plan_paid", "you_owe"];
const BILLED: [&str; 6] = [
    "amount_due",
    "claim_you_owe",
    "currency",
    "left",
    "paid_total",
    "settled",
];
const ASKED: [&str; 3] = ["asked", "owed", "currency"];
const SHARE: [&str; 2] = ["you_owe", "currency"];

/// What would change in a person's views, whose index is `was` in the folder
/// and would be `now`.
pub(crate) fn of(
    f: &Folder,
    p: &Person,
    was: &PersonIndex,
    now: &PersonIndex,
    views: &Preview,
) -> Result<Changes, CliError> {
    let made_by = views.made_by(f, p)?;
    let last = |name: &str| views.last_view(f, p, name);
    let view = |name: &str| views.view(p, name);
    let (events, events_then) = (view("events.json"), last("events.json")?);
    let (bills, bills_then) = (view("bills.json"), last("bills.json")?);
    let (standing, standing_then) = (view(standing::FILE), last(standing::FILE)?);
    let shown = about::shown(p, now.clone());
    Ok(Changes {
        against: made_by.map(|(medrec, _)| medrec.to_string()),
        stale: made_by.is_some_and(|(_, current)| !current),
        private: private(
            was,
            &shown,
            &events,
            &last("private.json")?,
            &view("private.json"),
        ),
        in_error: in_error(&standing, &events),
        current: current(was, &standing_then, &standing),
        events: lists(&events_then["events"], &events["events"], |e| {
            (text(e, &["id"]), amounts(&e["money"], &MONEY))
        }),
        bills: maps(&bills_then["bills"], &bills["bills"], &BILLED),
        worth_a_call: lists(&bills_then["worth_a_call"], &bills["worth_a_call"], |c| {
            (text(c, &["reason", "bill"]), amounts(c, &ASKED))
        }),
        not_billed: lists(&bills_then["not_billed"], &bills["not_billed"], |n| {
            (text(n, &["event", "provider"]), amounts(n, &SHARE))
        }),
        about_another: about::counted(p, now),
        papers: papers(was, now),
    })
}

fn text(v: &Value, keys: &[&str]) -> String {
    let of = keys.iter().map(|k| v[*k].as_str().unwrap_or_default());
    of.collect::<Vec<_>>().join("|")
}

fn amounts(v: &Value, keys: &[&str]) -> Vec<Value> {
    keys.iter().map(|k| v[*k].clone()).collect()
}

/// Each of a list by what it is called, with what can be otherwise of it.
fn lists(then: &Value, now: &Value, of: impl Fn(&Value) -> (String, Vec<Value>)) -> Moved {
    let each = |v: &Value| -> BTreeMap<String, Vec<Value>> {
        v.as_array().into_iter().flatten().map(&of).collect()
    };
    moved(&each(then), &each(now))
}

fn maps(then: &Value, now: &Value, keys: &[&str]) -> Moved {
    let each = |v: &Value| -> BTreeMap<String, Vec<Value>> {
        let of = v.as_object().into_iter().flatten();
        of.map(|(id, x)| (id.clone(), amounts(x, keys))).collect()
    };
    moved(&each(then), &each(now))
}

fn moved(then: &BTreeMap<String, Vec<Value>>, now: &BTreeMap<String, Vec<Value>>) -> Moved {
    let without = |a: &BTreeMap<String, Vec<Value>>, b: &BTreeMap<String, Vec<Value>>| {
        a.keys().filter(|k| !b.contains_key(*k)).count()
    };
    let changed = |(k, v): &(&String, &Vec<Value>)| then.get(*k).is_some_and(|was| was != *v);
    Moved {
        added: without(now, then),
        removed: without(then, now),
        changed: now.iter().filter(changed).count(),
    }
}

/// The kinds of a person's items, by id.
struct Kinds<'a> {
    records: BTreeMap<&'a str, &'a Record>,
    documents: BTreeMap<&'a str, &'a Document>,
}

impl<'a> Kinds<'a> {
    fn of(each: &[&'a PersonIndex]) -> Kinds<'a> {
        let records = each.iter().flat_map(|i| &i.records);
        let documents = each.iter().flat_map(|i| &i.documents);
        Kinds {
            records: records.map(|r| (r.id.as_str(), r)).collect(),
            documents: documents.map(|d| (d.id.as_str(), d)).collect(),
        }
    }

    fn of_item(&self, list: &str, id: &str) -> String {
        let record = || self.records.get(id).map_or("record", |r| &r.kind);
        match list {
            "records" => record().to_string(),
            list => list.trim_end_matches('s').replace("studie", "study"),
        }
    }

    /// The kind of a part's item.
    fn of_part(&self, id: &str) -> String {
        match id {
            id if self.records.contains_key(id) => self.of_item("records", id),
            id if id.starts_with("d_") => "document".into(),
            _ => "event".into(),
        }
    }

    fn questionnaire(&self, list: &str, id: &str) -> Option<&'static str> {
        let of = match list {
            "records" => {
                let r = self.records.get(id)?;
                questionnaires::of(&r.display, r.code.as_ref())
            }
            "documents" => questionnaires::of(self.documents.get(id)?.title.as_deref()?, None),
            _ => None,
        };
        of.map(|q| q.kind)
    }
}

const PRIVATE: [&str; 4] = ["records", "documents", "events", "studies"];

fn private(
    was: &PersonIndex,
    shown: &PersonIndex,
    events: &Value,
    then: &Value,
    now: &Value,
) -> Private {
    let kinds = Kinds::of(&[was, shown]);
    let mut there: BTreeSet<&str> = BTreeSet::new();
    there.extend(shown.records.iter().map(|r| r.id.as_str()));
    there.extend(shown.documents.iter().map(|d| d.id.as_str()));
    there.extend(shown.studies.iter().map(|s| s.id.as_str()));
    for list in ["events", "events_in_error"] {
        let of = events[list].as_array().into_iter().flatten();
        there.extend(of.filter_map(|e| e["id"].as_str()));
    }
    let mut out = Private::default();
    let one = |counts: &mut Counts, kind: String| *counts.entry(kind).or_default() += 1;
    for list in PRIVATE {
        for (id, why) in now[list].as_object().into_iter().flatten() {
            let kind = kinds.of_item(list, id);
            if why["given"] == true {
                one(&mut out.given, kind.clone());
            }
            if then[list].get(id).is_some() {
                continue;
            }
            one(&mut out.added, kind);
            let from = why["from"].as_str().unwrap_or("household");
            one(&mut out.added_by, from.to_string());
            if let Some(q) = kinds.questionnaire(list, id) {
                one(&mut out.questionnaires, q.to_string());
            }
        }
        for id in then[list].as_object().into_iter().flat_map(|m| m.keys()) {
            if now[list].get(id).is_none() {
                let to = match there.contains(id.as_str()) {
                    true => &mut out.opened,
                    false => &mut out.gone,
                };
                one(to, kinds.of_item(list, id));
            }
        }
    }
    for id in now["parts"].as_object().into_iter().flat_map(|m| m.keys()) {
        one(&mut out.parts, kinds.of_part(id));
    }
    let alone = out.questionnaires.values().filter(|n| **n == 1).count();
    out.questionnaires.retain(|_, n| *n > 1);
    if alone > 0 {
        out.questionnaires.insert(ONE_OF_A_KIND.into(), alone);
    }
    out
}

fn in_error(standing: &Value, events: &Value) -> Counts {
    let of = standing["in_error"].as_object().into_iter().flatten();
    let count = |v: &Value| v.as_array().map_or(0, Vec::len);
    let mut out: Counts = of.map(|(k, ids)| (k.clone(), count(ids))).collect();
    out.insert("event".into(), count(&events["events_in_error"]));
    out.retain(|_, n| *n > 0);
    out
}

/// A list of current ones before 0.9.9 was those whose status is no end.
fn current(was: &PersonIndex, then: &Value, now: &Value) -> BTreeMap<&'static str, Moved> {
    let today = today();
    let listed = |v: &Value, kind: &str| -> Option<BTreeSet<String>> {
        let st: Standings = serde_json::from_value(v.clone()).ok()?;
        let there = |_: &str| true;
        let lines = st.lists(kind, &there, &today).current;
        Some(lines.into_iter().flat_map(|l| l.records).collect())
    };
    let by_status = |kind: &str| -> BTreeSet<String> {
        let of = was.records.iter().filter(|r| r.kind == kind);
        let open = of.filter(|r| !views::is_ended(r.status.as_deref()));
        open.map(|r| r.id.clone()).collect()
    };
    standing::KINDS
        .iter()
        .map(|kind| {
            let then = listed(then, kind).unwrap_or_else(|| by_status(kind));
            let now = listed(now, kind).unwrap_or_default();
            let moved = Moved {
                added: now.difference(&then).count(),
                removed: then.difference(&now).count(),
                changed: 0,
            };
            (*kind, moved)
        })
        .collect()
}

fn papers(was: &PersonIndex, now: &PersonIndex) -> Papers {
    let sources = |idx: &PersonIndex| -> BTreeMap<String, usize> {
        let of = idx.documents.iter();
        of.map(|d| (d.id.clone(), d.provenances().count()))
            .collect()
    };
    let before = sources(was);
    let filed_before = now
        .documents
        .iter()
        .filter(|d| version(&d.provenance.filed_by).is_none_or(|v| v < KEEPS_SOURCES));
    let mut out = Papers::default();
    for d in filed_before {
        out.filed_before += 1;
        let had = before.get(&d.id).copied().unwrap_or_default();
        out.source_found += usize::from(d.provenances().count() > had);
    }
    out
}

/// The version of `medrec X.Y.Z`.
fn version(filed_by: &str) -> Option<(u32, u32, u32)> {
    let mut of = filed_by.strip_prefix("medrec ")?.split('.');
    let mut next = || of.next()?.parse().ok();
    Some((next()?, next()?, next()?))
}

impl Changes {
    /// As a person reads them, a line for each that is not as it was.
    pub fn lines(&self) -> Vec<String> {
        let counts = |of: &Counts| -> Option<String> {
            let each: Vec<String> = of.iter().map(|(k, n)| format!("{n} {k}")).collect();
            (!each.is_empty()).then(|| each.join(", "))
        };
        let moved = |m: &Moved| -> Option<String> {
            let each = [
                (m.added, "more"),
                (m.removed, "fewer"),
                (m.changed, "otherwise"),
            ];
            let each = each.iter().filter(|(n, _)| *n > 0);
            let each: Vec<String> = each.map(|(n, how)| format!("{n} {how}")).collect();
            (!each.is_empty()).then(|| each.join(", "))
        };
        let p = &self.private;
        let (about, papers) = (&self.about_another, &self.papers);
        let some = |n: usize, what: &str| (n > 0).then(|| format!("{n} {what}"));
        let of_about = [
            some(about.said, "by what their source says"),
            some(about.by_age, "by the person's age"),
            some(about.documents, "papers"),
        ];
        let of_about: Vec<String> = of_about.into_iter().flatten().collect();
        let mut out = vec![
            ("private, more", counts(&p.added)),
            ("their rules", counts(&p.added_by)),
            ("their questionnaires", counts(&p.questionnaires)),
            ("private before, open now", counts(&p.opened)),
            ("private before, not there now", counts(&p.gone)),
            ("private by what a source gave alone", counts(&p.given)),
            ("open with a private part", counts(&p.parts)),
            ("entered in error", counts(&self.in_error)),
        ];
        let current = self.current.iter();
        let current: Vec<(String, Option<String>)> = current
            .map(|(kind, m)| (format!("current, {kind}"), moved(m)))
            .collect();
        out.extend(current.iter().map(|(k, m)| (k.as_str(), m.clone())));
        out.extend([
            ("events", moved(&self.events)),
            ("bills", moved(&self.bills)),
            ("worth a call", moved(&self.worth_a_call)),
            ("not billed yet", moved(&self.not_billed)),
            (
                "about another person",
                (!of_about.is_empty()).then(|| of_about.join(", ")),
            ),
            (
                "papers filed before medrec kept every source of one",
                some(papers.filed_before, "")
                    .map(|n| format!("{n}and another was found of {}", papers.source_found)),
            ),
        ]);
        let each = out.into_iter();
        each.filter_map(|(what, n)| Some(format!("{what}: {}", n?)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_version_is_read_of_what_filed_a_paper() {
        assert_eq!(version("medrec 0.9.5"), Some((0, 9, 5)));
        assert!(version("medrec 0.9.10") > version("medrec 0.9.9"));
        assert_eq!(version("medrec"), None);
        assert_eq!(version("another 1.2.3"), None);
    }
}
