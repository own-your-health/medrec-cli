//! What a `medrec reindex` would change in the views, counted before it is run:
//! what its records would make of them against what the folder has. Counts
//! alone, so that they can be shown to anyone: no name, date, amount or id, and
//! no kind of questionnaire that there is one of.

use std::collections::{BTreeMap, BTreeSet};

use pk_cli_core::CliError;
use serde::Serialize;
use serde_json::Value;

use crate::about;
use crate::model::{Document, Person, PersonIndex, Record};
use crate::questionnaires;
use crate::standing::{self, Standings};
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

/// What of a person's views the counts are of; none of those there are not.
#[derive(Default)]
pub(crate) struct Views {
    events: Value,
    bills: Value,
    private: Value,
    standing: Option<Standings>,
}

impl Views {
    /// By what `read` says each of the person's views, in the directory `at`,
    /// is.
    pub fn read(
        at: &str,
        read: impl Fn(&str) -> Result<Option<Value>, CliError>,
    ) -> Result<Views, CliError> {
        let view = |name: &str| Ok::<Value, CliError>(read(name)?.unwrap_or_default());
        let standing = read(standing::FILE)?.map(serde_json::from_value);
        // Not what the parser says: that can quote the view.
        let standing = standing.transpose().map_err(|_| {
            CliError::Other(format!(
                "{at}/{} has another shape than this medrec reads: run this again after `medrec views write --all`",
                standing::FILE
            ))
        })?;
        Ok(Views {
            events: view("events.json")?,
            bills: view("bills.json")?,
            private: view("private.json")?,
            standing,
        })
    }
}

/// A person's views as the last pass made them.
#[derive(Default)]
pub(crate) struct Before {
    /// Which medrec's, none where there are none.
    pub medrec: Option<String>,
    /// They were made from other files than the folder has.
    pub stale: bool,
    pub views: Views,
}

#[derive(Debug, Default, Serialize)]
pub struct Changes {
    /// The medrec that made the views these are against. None where the folder
    /// has none of the person's: then every private item, event and bill is
    /// counted as added.
    pub against: Option<String>,
    /// Those were made from other files than the folder has.
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
    /// The questionnaires of them, by kind, where there are several of one.
    pub questionnaires: Counts,
    /// How many kinds of questionnaire there is one of.
    pub questionnaires_one_of_a_kind: usize,
    /// Private before and open now, by kind.
    pub opened: Counts,
    /// Private before and not there now, by kind: one that is about somebody
    /// else, or an event that is another now.
    pub gone: Counts,
    /// Private by what a source gave alone, by kind.
    pub given: Counts,
    /// Of those added, the ones private by their clinician, by kind.
    pub clinician: Counts,
    /// Of those, held by the day of a practice's paper alone, and through a
    /// clinician linked to one.
    pub clinician_day: usize,
    pub clinician_linked: usize,
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
/// and would be `now`, on the day `today`.
pub(crate) fn of(
    p: &Person,
    was: &PersonIndex,
    now: &PersonIndex,
    before: &Before,
    after: &Views,
    today: &str,
) -> Changes {
    let then = &before.views;
    let shown = about::shown(p, now.clone());
    Changes {
        against: before.medrec.clone(),
        stale: before.stale,
        private: private(was, &shown, &after.events, &then.private, &after.private),
        in_error: in_error(after),
        current: current(was, then, after, today),
        events: lists(&then.events["events"], &after.events["events"], |e| {
            (text(e, &["id"]), amounts(&e["money"], &MONEY))
        }),
        bills: maps(&then.bills["bills"], &after.bills["bills"], &BILLED),
        worth_a_call: lists(
            &then.bills["worth_a_call"],
            &after.bills["worth_a_call"],
            |c| (text(c, &["reason", "bill"]), amounts(c, &ASKED)),
        ),
        not_billed: lists(&then.bills["not_billed"], &after.bills["not_billed"], |n| {
            (text(n, &["event", "provider"]), amounts(n, &SHARE))
        }),
        about_another: about::counted(p, now),
        papers: papers(was, now),
    }
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
                questionnaires::asked_by(r).first().copied()
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
            one(&mut out.added, kind.clone());
            let from = why["from"].as_str().unwrap_or("household");
            one(&mut out.added_by, from.to_string());
            if from == "clinician" {
                one(&mut out.clinician, kind.clone());
                match why["learned"].as_str() {
                    Some("day") => out.clinician_day += 1,
                    Some("linked") => out.clinician_linked += 1,
                    _ => {}
                }
            }
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
    out.questionnaires_one_of_a_kind = alone;
    out
}

fn in_error(after: &Views) -> Counts {
    let of = after.standing.iter().flat_map(|s| &s.in_error);
    let mut out: Counts = of.map(|(k, ids)| (k.clone(), ids.len())).collect();
    let events = after.events["events_in_error"].as_array();
    out.insert("event".into(), events.map_or(0, Vec::len));
    out.retain(|_, n| *n > 0);
    out
}

/// A list of current ones before 0.9.9 was those whose status is no end.
fn current(
    was: &PersonIndex,
    then: &Views,
    now: &Views,
    today: &str,
) -> BTreeMap<&'static str, Moved> {
    let listed = |v: &Views, kind: &str| -> Option<BTreeSet<String>> {
        let there = |_: &str| true;
        let lines = v.standing.as_ref()?.lists(kind, &there, today).current;
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
        let mut asked = counts(&p.questionnaires);
        if let Some(alone) = some(p.questionnaires_one_of_a_kind, "of a kind there is one of") {
            asked = Some(
                asked
                    .into_iter()
                    .chain([alone])
                    .collect::<Vec<_>>()
                    .join(", "),
            );
        }
        let papers = (papers.filed_before > 0).then(|| {
            format!(
                "{}, and another source was found of {}",
                papers.filed_before, papers.source_found
            )
        });
        let of_about = [
            some(about.said, "by what their source says"),
            some(about.by_age, "by the person's age"),
            some(about.listing, "panels and reports that list no other"),
            some(about.documents, "papers"),
        ];
        let of_about: Vec<String> = of_about.into_iter().flatten().collect();
        let mut out = vec![
            ("private, more", counts(&p.added)),
            ("their rules", counts(&p.added_by)),
            ("by their clinician", counts(&p.clinician)),
            (
                "of them by the day of a practice's paper alone",
                some(p.clinician_day, "items"),
            ),
            (
                "of them through a clinician linked to a practice",
                some(p.clinician_linked, "items"),
            ),
            ("their questionnaires", asked),
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
                papers,
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
    use crate::standing::{Is, Standing};
    use serde_json::json;

    const PID: &str = "p_exampleaaaaaaaaaaaaa";

    fn person() -> Person {
        serde_json::from_value(json!({
            "id": PID, "name": "Robin Example", "folder": "Robin Example", "adult": true,
            "created_at": "2024-01-02T00:00:00Z"
        }))
        .unwrap()
    }

    fn index(records: Value) -> PersonIndex {
        let mut idx = PersonIndex::new(PID);
        idx.records = serde_json::from_value(records).unwrap();
        idx
    }

    fn result(id: &str, name: &str) -> Value {
        json!({"id": id, "kind": "result", "key": id, "display": name, "fields": {},
               "provenance": []})
    }

    fn views(private: Value) -> Views {
        Views {
            private,
            ..Views::default()
        }
    }

    /// What is private no more is open where it still is, and else gone.
    #[test]
    fn what_was_private_is_open_or_gone() {
        let was = index(json!([result("r_one", "Sodium"), result("r_two", "Iron")]));
        let now = index(json!([result("r_one", "Sodium")]));
        let why = json!({"area": "household", "from": "household"});
        let before = Before {
            views: views(json!({
                "records": {"r_one": why, "r_two": why}, "events": {"e_one": why}
            })),
            ..Before::default()
        };
        let mut after = views(json!({}));
        after.events = json!({"events": []});
        let c = of(&person(), &was, &now, &before, &after, "2024-06-01");
        assert_eq!(c.private.opened, Counts::from([("result".into(), 1)]));
        let gone = [("event".to_string(), 1), ("result".to_string(), 1)];
        assert_eq!(c.private.gone, Counts::from(gone));
        assert!(c.private.added.is_empty() && c.against.is_none() && !c.stale);
        // An event that is there is open.
        after.events = json!({"events": [{"id": "e_one"}]});
        let c = of(&person(), &was, &now, &before, &after, "2024-06-01");
        assert_eq!(c.private.opened.get("event"), Some(&1));
    }

    /// A kind of questionnaire there is one of is not named, also when that is
    /// the only one.
    #[test]
    fn a_questionnaire_of_a_kind_there_is_one_of_is_not_named() {
        let names = [
            ("r_one", "PHQ-9 total score"),
            ("r_two", "PHQ-2 total score"),
            ("r_three", "GAD-7 total score"),
        ];
        let idx = index(
            names
                .map(|(id, name)| result(id, name))
                .into_iter()
                .collect(),
        );
        let count = |ids: &[&str]| {
            let why = json!({"area": "mental_health", "from": "test"});
            let records: serde_json::Map<String, Value> =
                ids.iter().map(|id| (id.to_string(), why.clone())).collect();
            let after = views(json!({"records": records}));
            of(
                &person(),
                &idx,
                &idx,
                &Before::default(),
                &after,
                "2024-06-01",
            )
            .private
        };
        let all = count(&["r_one", "r_two", "r_three"]);
        assert_eq!(all.added, Counts::from([("result".into(), 3)]));
        assert_eq!(all.questionnaires, Counts::from([("depression".into(), 2)]));
        assert_eq!(all.questionnaires_one_of_a_kind, 1);
        let one = count(&["r_three"]);
        assert!(one.questionnaires.is_empty());
        assert_eq!(one.questionnaires_one_of_a_kind, 1);
    }

    /// What of two lists is in one alone, and what is otherwise in both.
    #[test]
    fn money_moves_by_what_an_amount_is_called() {
        let mut then = views(json!({}));
        then.bills = json!({"bills": {
            "d_one": {"amount_due": "10.00", "left": "10.00"},
            "d_two": {"amount_due": "5.00"}, "d_four": {"amount_due": "7.00"}
        }});
        let mut after = views(json!({}));
        after.bills = json!({"bills": {
            "d_one": {"amount_due": "10.00", "left": "4.00"},
            "d_three": {"amount_due": "5.00"}, "d_four": {"amount_due": "7.00"}
        }});
        let before = Before {
            medrec: Some("0.9.5".into()),
            stale: true,
            views: then,
        };
        let idx = index(json!([]));
        let c = of(&person(), &idx, &idx, &before, &after, "2024-06-01");
        let one = Moved {
            added: 1,
            removed: 1,
            changed: 1,
        };
        assert_eq!(c.bills, one);
        assert_eq!((c.against.as_deref(), c.stale), (Some("0.9.5"), true));
        assert_eq!(c.events, Moved::default());
    }

    /// Whether one has ended by its date is of the day it is asked on.
    #[test]
    fn what_is_current_is_of_the_day() {
        let taken = Standing {
            kind: "medication".into(),
            is: Is::Current,
            line: "~iron".into(),
            until: Some("2024-06-01".into()),
            until_of: None,
            differs: false,
            differs_after: None,
            filled: Vec::new(),
            words: Vec::new(),
            unheard: Vec::new(),
        };
        let in_error = vec!["r_two".to_string(), "r_three".to_string()];
        let standings = Standings {
            schema: "medrec-standing/v1".into(),
            person: PID.into(),
            sources: Vec::new(),
            order: BTreeMap::from([("medication".into(), vec!["r_one".into()])]),
            records: BTreeMap::from([("r_one".into(), taken)]),
            in_error: BTreeMap::from([("result".into(), in_error)]),
            no_problem_list: Vec::new(),
        };
        let after = Views {
            standing: Some(standings),
            ..Views::default()
        };
        let idx = index(json!([]));
        let on = |day: &str| {
            let c = of(&person(), &idx, &idx, &Before::default(), &after, day);
            (
                c.current["medication"].added,
                c.in_error.get("result").copied(),
            )
        };
        assert_eq!(on("2024-05-31"), (1, Some(2)));
        assert_eq!(on("2024-06-02"), (0, Some(2)));
    }

    #[test]
    fn a_version_is_read_of_what_filed_a_paper() {
        assert_eq!(version("medrec 0.9.5"), Some((0, 9, 5)));
        assert!(version("medrec 0.9.10") > version("medrec 0.9.9"));
        assert_eq!(version("medrec"), None);
        assert_eq!(version("another 1.2.3"), None);
    }
}
