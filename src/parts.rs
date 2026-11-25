//! What is left out of an open item where private items are left out, when
//! something in it is private (docs/data-contract.md, "Private parts"): a
//! claim's private lines, the lines of a bill's text that say an area, and
//! what an event says by them. `private.json` has it, so that every reader
//! shows the same words.

use std::borrow::Cow;
use std::collections::BTreeSet;

use serde_json::{json, Map, Value};

use crate::claims;
use crate::given;
use crate::model::{PersonIndex, Record};
use crate::private;
use crate::services;

/// The places of a claim's private lines, diagnoses and payer notes in their
/// lists.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OfClaim {
    pub lines: BTreeSet<usize>,
    pub diagnoses: BTreeSet<usize>,
    pub notes: BTreeSet<usize>,
}

impl OfClaim {
    /// Those of a claim, by what it shows or by anything a source gave in
    /// their place.
    pub fn of(r: &Record) -> OfClaim {
        let mut out = OfClaim::default();
        for (k, places) in out.by_field() {
            let private: fn(&Value) -> bool = match k {
                "items" => |l| services::line(l).is_some(),
                "diagnoses" => |d| private::diagnosis(d).is_some(),
                _ => |n| n.as_str().is_some_and(|n| private::words(n).is_some()),
            };
            let list = |r: &Record| -> Vec<Value> {
                let list = r.fields.get(k).and_then(Value::as_array);
                list.cloned().unwrap_or_default()
            };
            let shown = list(r);
            let given = given::as_given(r).map(|x| list(&x));
            for list in std::iter::once(shown.clone()).chain(given) {
                let held = list.iter().enumerate().filter(|(_, v)| private(v));
                places.extend(held.map(|(i, _)| i).filter(|i| *i < shown.len()));
            }
        }
        out
    }

    fn by_field(&mut self) -> [(&'static str, &mut BTreeSet<usize>); 3] {
        [
            ("items", &mut self.lines),
            ("diagnoses", &mut self.diagnoses),
            ("payer_notes", &mut self.notes),
        ]
    }

    /// Those of its entry in `private.json`.
    fn read(v: &Value) -> OfClaim {
        let places = |k: &str| -> BTreeSet<usize> {
            let list = v[k].as_array().into_iter().flatten();
            list.filter_map(|i| usize::try_from(i.as_u64()?).ok())
                .collect()
        };
        OfClaim {
            lines: places("lines"),
            diagnoses: places("diagnoses"),
            notes: places("payer_notes"),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty() && self.diagnoses.is_empty() && self.notes.is_empty()
    }

    /// Whether these are all of `other`'s.
    fn holds(&self, other: &OfClaim) -> bool {
        self.lines.is_superset(&other.lines)
            && self.diagnoses.is_superset(&other.diagnoses)
            && self.notes.is_superset(&other.notes)
    }

    /// How much is left out.
    fn says(&self) -> String {
        let of = |n: usize, one: &str, many: &str| {
            (n > 0).then(|| format!("{n} {}", if n == 1 { one } else { many }))
        };
        let all = [
            of(self.lines.len(), "service", "services"),
            of(self.diagnoses.len(), "diagnosis", "diagnoses"),
            of(self.notes.len(), "payer note", "payer notes"),
        ];
        let all: Vec<String> = all.into_iter().flatten().collect();
        let listed = match all.as_slice() {
            [rest @ .., last] if !rest.is_empty() => format!("{} and {last}", rest.join(", ")),
            all => all.concat(),
        };
        format!("{listed} not shown")
    }

    /// The claim without them, and as the lines that are left name it.
    pub fn without(&self, r: &Record) -> Record {
        let mut out = r.clone();
        out.given.clear();
        for (k, places) in self.clone().by_field() {
            without(&mut out.fields, k, places);
        }
        if !self.lines.is_empty() {
            name(&mut out);
        }
        out
    }

    /// Its entry in `private.json`.
    pub fn json(&self, r: &Record) -> Value {
        let left = self.without(r);
        let mut v = json!({"display": left.display, "says": self.says()});
        if let Some(d) = &r.date {
            v["date"] = d.as_str().into();
        }
        let (was, is) = (r.fields.get("category"), left.fields.get("category"));
        if was != is {
            v["category"] = is.cloned().unwrap_or(Value::Null);
        }
        for (k, places) in [
            ("lines", &self.lines),
            ("diagnoses", &self.diagnoses),
            ("payer_notes", &self.notes),
        ] {
            if !places.is_empty() {
                v[k] = json!(places);
            }
        }
        v
    }
}

/// How many services are left out of an event.
pub fn services_not_shown(n: usize) -> String {
    let of = OfClaim {
        lines: (0..n).collect(),
        ..OfClaim::default()
    };
    of.says()
}

/// A claim's fields without some entries of one of them.
fn without(fields: &mut Map<String, Value>, key: &str, places: &BTreeSet<usize>) {
    let Some(Value::Array(list)) = fields.get_mut(key) else {
        return;
    };
    let mut i = 0..;
    list.retain(|_| i.next().is_none_or(|i| !places.contains(&i)));
    if list.is_empty() && !places.is_empty() {
        fields.remove(key);
    }
}

/// Names a claim, and says its category, by the lines it has.
fn name(r: &mut Record) {
    let lines = r.fields.get("items").and_then(Value::as_array);
    let lines = lines.cloned().unwrap_or_default();
    let text = |k: &str| r.fields.get(k).and_then(Value::as_str);
    let names = lines.iter().filter(|l| claims::counted(l));
    let names = names.filter_map(|l| l.pointer("/service/display").and_then(Value::as_str));
    let category = claims::claim_category(&lines, text("type") == Some("pharmacy"));
    let provider = text("provider").map(str::to_string);
    r.display = claims::title(names, category, provider.as_deref())
        .or(provider)
        .unwrap_or_else(|| "Claim".into());
    match category {
        Some(c) => r.fields.insert("category".into(), c.into()),
        None => r.fields.remove("category"),
    };
}

/// A place of a text: where it starts and how long it is, in bytes.
pub type Place = [usize; 2];

/// Where a text's private lines are, without their ends. Words that go over
/// from one line to the next are in both.
pub fn of_text(text: &str) -> Vec<Place> {
    let mut lines: Vec<Place> = Vec::new();
    let mut at = 0;
    for line in text.split(['\n', '\u{c}']) {
        lines.push([at, line.len()]);
        at += line.len() + 1;
    }
    let of = |[start, len]: Place| &text[start..start + len];
    let mut held: BTreeSet<usize> = BTreeSet::new();
    let said: Vec<Option<&str>> = lines.iter().map(|l| services::text(of(*l))).collect();
    held.extend(said.iter().enumerate().filter_map(|(i, a)| a.map(|_| i)));
    for (i, two) in lines.windows(2).enumerate() {
        let both = [of(two[0]).trim(), of(two[1]).trim()];
        let over = said[i].is_none() && said[i + 1].is_none();
        if over && !both.contains(&"") && private::words(&both.join(" ")).is_some() {
            held.extend([i, i + 1]);
        }
    }
    held.into_iter().map(|i| lines[i]).collect()
}

/// What a text says in place of those lines.
fn lines_not_shown(n: usize) -> String {
    match n {
        1 => "1 line not shown".into(),
        n => format!("{n} lines not shown"),
    }
}

/// A text's entry in `private.json`.
pub fn text_json(places: &[Place]) -> Value {
    json!({"text": places, "says": lines_not_shown(places.len())})
}

/// A text without some places of it. Those on lines one after the other are
/// one line that says how many there were. Nothing where they are none of
/// its own.
pub fn text_without(text: &str, places: &[Place]) -> String {
    let mut out = String::with_capacity(text.len());
    let (mut at, mut run) = (0, 0);
    for (i, [start, len]) in places.iter().enumerate() {
        let Some(before) = text.get(at..*start) else {
            return String::new();
        };
        if run == 0 {
            out.push_str(before);
        }
        run += 1;
        at = start + len;
        let next = places.get(i + 1).is_some_and(|[s, _]| *s == at + 1);
        if !next || text.as_bytes().get(at) != Some(&b'\n') {
            out.push_str(&format!("({})", lines_not_shown(run)));
            run = 0;
        }
    }
    out.push_str(text.get(at..).unwrap_or_default());
    out
}

/// What is left out of a person's open items, by their id: the `parts` of
/// their `private.json`.
#[derive(Debug, Clone, Default)]
pub struct Parts {
    by_id: Map<String, Value>,
}

impl Parts {
    /// Of a `private.json`.
    pub fn of(v: &Value) -> Parts {
        Parts {
            by_id: v["parts"].as_object().cloned().unwrap_or_default(),
        }
    }

    pub fn ids(&self) -> impl Iterator<Item = &String> {
        self.by_id.keys()
    }

    pub fn has(&self, id: &str) -> bool {
        self.by_id.contains_key(id)
    }

    /// How much is left out of an item.
    pub fn says(&self, id: &str) -> Option<&str> {
        self.by_id.get(id)?["says"].as_str()
    }

    /// None are left out of an answer that names a person.
    pub fn named(&mut self, person: Option<&str>) {
        if person.is_some() {
            self.by_id.clear();
        }
    }

    pub fn extend(&mut self, other: Parts) {
        self.by_id.extend(other.by_id);
    }

    /// Whether these leave out of an item all that `others` do.
    pub fn leave_out(&self, id: &str, others: &Parts) -> bool {
        let (Some(these), Some(all)) = (self.by_id.get(id), others.by_id.get(id)) else {
            return !others.has(id);
        };
        let texts: BTreeSet<Place> = places(these).into_iter().collect();
        OfClaim::read(these).holds(&OfClaim::read(all))
            && places(all).iter().all(|p| texts.contains(p))
    }

    /// A record as it is shown without its private parts.
    pub fn record<'a>(&self, r: &'a Record) -> Cow<'a, Record> {
        match self.by_id.get(&r.id) {
            Some(of) => Cow::Owned(OfClaim::read(of).without(r)),
            None => Cow::Borrowed(r),
        }
    }

    /// A person's index without them.
    pub fn index(&self, idx: &mut PersonIndex) {
        for r in &mut idx.records {
            if let Cow::Owned(left) = self.record(r) {
                *r = left;
            }
        }
    }

    /// Whether an entry of one of a record's fields is left out of it.
    pub fn leaves_out(&self, id: &str, field: &str, i: usize) -> bool {
        self.by_id.get(id).is_some_and(|of| {
            let mut of = OfClaim::read(of);
            of.by_field()
                .iter()
                .any(|(k, places)| *k == field && places.contains(&i))
        })
    }

    /// Where a document's private lines are in its text.
    pub fn places(&self, id: &str) -> Vec<Place> {
        self.by_id.get(id).map(places).unwrap_or_default()
    }

    /// A document's text without them.
    pub fn text<'a>(&self, id: &str, text: &'a str) -> Cow<'a, str> {
        match self.by_id.get(id) {
            Some(of) => Cow::Owned(text_without(text, &places(of))),
            None => Cow::Borrowed(text),
        }
    }

    /// An event as it is shown without them, and its members' titles.
    pub fn event(&self, e: &mut Value) {
        let Some(id) = e["id"].as_str().map(str::to_string) else {
            return;
        };
        for m in e["members"].as_array_mut().into_iter().flatten() {
            let of = m["record"].as_str().and_then(|id| self.by_id.get(id));
            if let Some(name) = of.and_then(|of| of["display"].as_str()) {
                m["title"] = name.into();
            }
        }
        let (Some(said), Some(e)) = (
            self.by_id.get(&id).and_then(Value::as_object),
            e.as_object_mut(),
        ) else {
            return;
        };
        for (k, v) in said {
            match (k.as_str(), v) {
                ("says", _) => {}
                (_, Value::Null) => drop(e.remove(k)),
                _ => drop(e.insert(k.clone(), v.clone())),
            }
        }
    }
}

/// Where the private lines of a text are, by its entry in `private.json`.
fn places(of: &Value) -> Vec<Place> {
    serde_json::from_value(of["text"].clone()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claim(lines: Value, more: Value) -> Record {
        let mut fields =
            json!({"items": lines, "provider": "Example Pediatrics", "category": "visit"});
        for (k, v) in more.as_object().into_iter().flatten() {
            fields[k] = v.clone();
        }
        serde_json::from_value(json!({
            "id": "r_exampleaaaaaaaaaaaaa", "kind": "claim", "key": "k",
            "display": "Office visit, Brief mood check and 1 more · Example Pediatrics",
            "date": "2024-05-06", "fields": fields, "provenance": [],
        }))
        .unwrap()
    }

    fn line(code: &str, display: &str) -> Value {
        json!({"service": {"system": "http://www.ama-assn.org/go/cpt",
            "code": code, "display": display}, "category": "visit"})
    }

    #[test]
    fn a_claim_is_called_by_the_lines_that_are_left() {
        let r = claim(
            json!([
                line("00000", "Office visit"),
                line("00000", "Depression screening"),
                line("00000", "Flu vaccine")
            ]),
            json!({"diagnoses": [
                {"sequence": 1, "code": {"system": "http://hl7.org/fhir/sid/icd-10-cm", "code": "J02.0"}},
                {"sequence": 2, "code": {"system": "http://hl7.org/fhir/sid/icd-10-cm", "code": "F41.1"}}],
                "payer_notes": ["Paid in full", "Behavioral health is paid apart"]}),
        );
        let of = OfClaim::of(&r);
        assert_eq!(
            of.json(&r),
            json!({"display": "Office visit and Flu vaccine · Example Pediatrics",
                "date": "2024-05-06", "lines": [1], "diagnoses": [1], "payer_notes": [1],
                "says": "1 service, 1 diagnosis and 1 payer note not shown"})
        );
        let left = of.without(&r);
        assert_eq!(left.fields["items"].as_array().unwrap().len(), 2);
        assert_eq!(left.fields["diagnoses"].as_array().unwrap().len(), 1);
        assert_eq!(left.fields["payer_notes"], json!(["Paid in full"]));
        assert_eq!(left.fields["provider"], "Example Pediatrics");
        assert!(OfClaim::of(&left).is_empty());
        // A line the totals leave out names nothing, and is left out too.
        let mut r = claim(
            json!([
                line("00000", "Office visit"),
                line("00000", "Tobacco counseling")
            ]),
            json!({}),
        );
        r.fields["items"][1]["counted"] = false.into();
        let of = OfClaim::of(&r);
        assert_eq!(of.json(&r)["says"], "1 service not shown");
        assert_eq!(of.without(&r).display, "Office visit · Example Pediatrics");
    }

    #[test]
    fn a_text_says_how_many_lines_it_is_without() {
        let text = "Visit\nDepression screening\nPHQ-9\nTotal due\u{c}Anxiety screening\nThanks";
        let places = of_text(text);
        assert_eq!(places, [[6, 20], [27, 5], [43, 17]]);
        assert_eq!(
            text_without(text, &places),
            "Visit\n(2 lines not shown)\nTotal due\u{c}(1 line not shown)\nThanks"
        );
        assert_eq!(text_json(&places)["says"], "3 lines not shown");
        assert_eq!(text_without(text, &[]), text);
        assert_eq!(of_text("Total due\nThanks"), Vec::<[usize; 2]>::new());
    }
}
