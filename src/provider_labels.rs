//! A person's provider labels, for an answer to look up (`providers.json`,
//! made by `crate::labels`). A label stands beside an item's own words, in a
//! list of its own, and what an answer about the whole household may not say
//! is not in it.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Map, Value};

use crate::folder::Folder;
use crate::labels::{FILE, SCHEMA};
use crate::model::{Household, Person};

/// The fields of a record that print a provider, in the order they are shown.
pub use crate::labels::FIELDS;

/// What is printed in an item, to look its label up by.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Printed {
    pub field: &'static str,
    pub text: String,
    /// The event member that prints it, when the item itself does not.
    pub member: Option<String>,
    /// Whether the item says it by itself, so that the label does not have to.
    pub shown: bool,
}

impl Printed {
    pub fn new(field: &'static str, text: &str) -> Printed {
        Printed {
            field,
            text: text.to_string(),
            member: None,
            shown: true,
        }
    }

    /// Each provider a record's fields print.
    pub fn of_record(fields: &Map<String, Value>) -> Vec<Printed> {
        FIELDS
            .iter()
            .filter_map(|f| Some(Printed::new(f, fields.get(*f)?.as_str()?)))
            .collect()
    }

    /// A claim's provider, in a row that has no fields: its title says it
    /// with the rest.
    pub fn of_claim(fields: &Map<String, Value>) -> Vec<Printed> {
        let provider = fields.get("provider").and_then(Value::as_str);
        let in_title = |p| Printed {
            shown: false,
            ..Printed::new("provider", p)
        };
        provider.map(in_title).into_iter().collect()
    }

    pub fn of_document(organization: Option<&str>) -> Vec<Printed> {
        let of = |o| Printed::new("organization", o);
        organization.map(of).into_iter().collect()
    }

    /// An event's place and clinicians, and in full what its own members
    /// print.
    pub fn of_event(e: &Value, full: bool) -> Vec<Printed> {
        let list = |k: &str| e[k].as_array().into_iter().flatten();
        let mut out: Vec<Printed> = e["place"]
            .as_str()
            .map(|p| Printed::new("place", p))
            .into_iter()
            .collect();
        out.extend(
            list("clinicians")
                .filter_map(Value::as_str)
                .map(|c| Printed::new("clinicians", c)),
        );
        if full {
            let own = list("members").filter(|m| m.get("person").is_none());
            out.extend(own.filter_map(|m| {
                Some(Printed {
                    member: Some(m["record"].as_str()?.to_string()),
                    ..Printed::new("members", m["provider"].as_str()?)
                })
            }));
        }
        out
    }
}

/// What an answer may say.
pub struct Asked<'a> {
    /// About the whole household, rather than one person.
    pub whole: bool,
    /// Whether an item is private.
    pub private: &'a dyn Fn(&str) -> bool,
    /// Whether an item stays out of an answer about the whole household that
    /// does not name it.
    pub held_back: &'a dyn Fn(&str) -> bool,
}

impl Asked<'_> {
    fn hides(&self, id: &str) -> bool {
        self.whole && (self.held_back)(id)
    }
}

#[derive(Debug, Clone, Default)]
struct Part {
    labels: BTreeMap<String, Map<String, Value>>,
    by_name: BTreeMap<String, String>,
    by_item: BTreeMap<String, BTreeMap<String, String>>,
    /// The ids of what the household said.
    said: BTreeSet<String>,
}

impl Part {
    fn of(v: &Value) -> Part {
        let map = |x: &Value| -> BTreeMap<String, String> {
            x.as_object()
                .into_iter()
                .flatten()
                .filter_map(|(k, x)| Some((k.clone(), x.as_str()?.to_string())))
                .collect()
        };
        let object = |k: &str| v[k].as_object().into_iter().flatten();
        Part {
            labels: object("labels")
                .filter_map(|(k, x)| Some((k.clone(), x.as_object()?.clone())))
                .collect(),
            by_name: map(&v["by_name"]),
            by_item: object("by_item")
                .map(|(k, x)| (k.clone(), map(x)))
                .collect(),
            said: v["overrides"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|o| Some(o["id"].as_str()?.to_string()))
                .collect(),
        }
    }

    fn id(&self, item: &str, printed: &str) -> Option<&String> {
        self.by_item
            .get(item)
            .and_then(|names| names.get(printed))
            .or_else(|| self.by_name.get(printed))
    }
}

#[derive(Debug, Clone, Default)]
pub struct Labels {
    open: Part,
    private: Part,
}

impl Labels {
    /// From a `providers.json`.
    pub fn of(v: &Value, person: &str) -> Option<Labels> {
        (v["schema"] == SCHEMA && v["person"] == person).then(|| Labels {
            open: Part::of(v),
            private: Part::of(&v["private"]),
        })
    }

    /// The person's labels as the folder's views have them: none when the
    /// views are older than the files they were made from.
    pub fn read(f: &Folder, h: &Household, p: &Person) -> Labels {
        crate::viewfiles::current_view(f, h, p, FILE)
            .ok()
            .flatten()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .and_then(|v: Value| Labels::of(&v, &p.id))
            .unwrap_or_default()
    }

    /// The labels of what an item prints, as an answer shows them. Two that
    /// print the same and have one label are one.
    pub fn shown(&self, item: &str, printed: &[Printed], asked: &Asked) -> Vec<Map<String, Value>> {
        let mut had = BTreeSet::new();
        let mut out = Vec::new();
        if asked.hides(item) {
            return out;
        }
        for p in printed {
            let at = p.member.as_deref().unwrap_or(item);
            if asked.hides(at) {
                continue;
            }
            let private = (asked.private)(at);
            let part = if private { &self.private } else { &self.open };
            let Some(id) = part.id(at, &p.text) else {
                continue;
            };
            let Some(label) = part.labels.get(id).or_else(|| self.open.labels.get(id)) else {
                continue;
            };
            if !had.insert((&p.text, id)) {
                continue;
            }
            out.extend(self.entry(p, label, asked));
        }
        out
    }

    /// None for a likely one whose every reason is one the answer may not
    /// give.
    fn entry(
        &self,
        p: &Printed,
        label: &Map<String, Value>,
        asked: &Asked,
    ) -> Option<Map<String, Value>> {
        let text = |v: &Value, k: &str| v[k].as_str().map(str::to_string);
        // What gave it: with the ids the answer may not give, none.
        let from = |v: &Value, keys: &[&str]| -> Option<Value> {
            let kind = text(v, "from")?;
            let may = match kind.as_str() {
                "paper" => text(v, "document").is_some_and(|d| !asked.hides(&d)),
                "household" => text(v, "id").is_some_and(|id| {
                    self.open.said.contains(&id)
                        || (!asked.whole && self.private.said.contains(&id))
                }),
                _ => true,
            };
            let mut out = Map::from_iter([("from".to_string(), json!(kind))]);
            for k in keys {
                if let Some(x) = text(v, k) {
                    out.insert(k.to_string(), json!(x));
                }
            }
            may.then_some(Value::Object(out))
        };
        let evidence: Vec<Value> = label
            .get("evidence")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|e| match e["from"].as_str()? {
                "only_match" => from(e, &["state"]),
                "first_name" => from(e, &["state", "printed"]),
                "paper" => from(e, &["document"]),
                "household" => from(e, &["id", "by", "at"]),
                // One a later medrec knows has no words here.
                _ => from(e, &[]),
            })
            .collect();
        // One medrec does not know is likely.
        let confidence = label
            .get("confidence")
            .and_then(Value::as_str)
            .filter(|c| ["household", "sure"].contains(c))
            .unwrap_or("likely");
        if confidence == "likely" && evidence.is_empty() {
            return None;
        }
        let mut out = Map::from_iter([("field".to_string(), json!(p.field))]);
        let mut put = |k: &str, v: Value| {
            if !v.is_null() {
                out.insert(k.to_string(), v);
            }
        };
        if let Some(m) = &p.member {
            put("record", json!(m));
        }
        if p.shown {
            put("printed", json!(p.text));
        }
        for k in ["name", "practice", "role"] {
            put(k, json!(label.get(k).and_then(Value::as_str)));
        }
        // A phone is shown with where it is from, or not at all.
        let phone = label.get("phone").and_then(Value::as_str);
        let whence = label.get("phone_from").and_then(|v| {
            let keys: &[&str] = match v["from"].as_str()? {
                "directory" => &["date"],
                "paper" => &["document"],
                "household" => &["id"],
                _ => return None,
            };
            from(v, keys)
        });
        if let (Some(n), Some(w)) = (phone, whence) {
            put("phone", json!(n));
            put("phone_from", w);
        }
        put("confidence", json!(confidence));
        put("evidence", json!(evidence));
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PERSON: &str = "p_robinaaaaaaaaaaaaaaa";
    const VISIT: &str = "r_visitaaaaaaaaaaaaaaa";
    const TALK: &str = "r_talkaaaaaaaaaaaaaaaa";
    const OPEN: &str = "d_openaaaaaaaaaaaaaaaa";
    const CLOSED: &str = "d_closedaaaaaaaaaaaaaa";

    fn labels() -> Labels {
        let v: Value =
            serde_json::from_str(include_str!("../tests/fixtures/labels/reader.json")).unwrap();
        assert!(Labels::of(&v, "p_otheraaaaaaaaaaaaaaa").is_none());
        Labels::of(&v, PERSON).unwrap()
    }

    fn shown(item: &str, text: &str, whole: bool) -> Vec<Value> {
        listed(item, &[Printed::new("provider", text)], whole)
    }

    fn listed(item: &str, printed: &[Printed], whole: bool) -> Vec<Value> {
        let private = |id: &str| [TALK, CLOSED].contains(&id);
        let asked = Asked {
            whole,
            private: &private,
            held_back: &private,
        };
        labels()
            .shown(item, printed, &asked)
            .into_iter()
            .map(Value::Object)
            .collect()
    }

    #[test]
    fn a_label_is_beside_what_is_printed() {
        assert_eq!(
            shown(VISIT, "J SAMPLE", true),
            [json!({
                "field": "provider", "printed": "J SAMPLE", "name": "Jordan Sample, MD",
                "role": "family doctor", "phone": "555-010-0101",
                "phone_from": {"from": "directory", "date": "2026-03-08"},
                "confidence": "sure", "evidence": [{"from": "npi"}],
            })]
        );
        // Exactly as printed, and one item can have its own.
        assert!(shown(VISIT, "J Sample", true).is_empty());
        assert_eq!(
            shown("r_otheraaaaaaaaaaaaaaa", "J SAMPLE", true)[0]["name"],
            "Jamie Sample, DO"
        );
        let unseen = Printed {
            shown: false,
            ..Printed::new("provider", "J SAMPLE")
        };
        assert!(listed(VISIT, &[unseen], true)[0].get("printed").is_none());
    }

    #[test]
    fn a_private_items_label_is_only_in_an_answer_about_the_person() {
        assert_eq!(shown(TALK, "C EXAMPLE", false)[0]["role"], "therapist");
        assert!(shown(TALK, "C EXAMPLE", true).is_empty());
        // An open item has nothing of what only private ones say.
        assert!(shown(VISIT, "C EXAMPLE", false).is_empty());
        // A private one's label that open items have too is in `labels`.
        assert_eq!(
            shown(TALK, "J SAMPLE", false)[0]["name"],
            "Jordan Sample, MD"
        );
        // An event's member is looked up as itself.
        let member = |record: &str, text: &str| Printed {
            member: Some(record.into()),
            ..Printed::new("members", text)
        };
        let event = [
            Printed::new("place", "J SAMPLE"),
            member(VISIT, "J SAMPLE"),
            member(TALK, "C EXAMPLE"),
        ];
        let of = |whole| -> Vec<(Value, Value)> {
            listed("e_eventaaaaaaaaaaaaaa", &event, whole)
                .into_iter()
                .map(|l| (l["field"].clone(), l["record"].clone()))
                .collect()
        };
        assert_eq!(of(true), [(json!("place"), Value::Null)]);
        assert_eq!(
            of(false),
            [
                (json!("place"), Value::Null),
                (json!("members"), json!(TALK))
            ]
        );
    }

    #[test]
    fn what_an_answer_may_not_say_is_not_why() {
        // Its one paper is private: no label about the household, and one
        // about the person.
        let paper = json!({"from": "paper", "document": CLOSED});
        assert!(shown(VISIT, "R PLACEHOLDER", true).is_empty());
        let about = &shown(VISIT, "R PLACEHOLDER", false)[0];
        assert_eq!(about["evidence"], json!([paper]));
        assert_eq!(about["phone_from"], paper);
        // With another reason it is shown without that one, and without a
        // phone nothing it may name gives.
        let l = &shown(VISIT, "M TEMPLATE", true)[0];
        assert_eq!(
            l["evidence"],
            json!([{"from": "only_match", "state": "FL"}, {"from": "paper", "document": OPEN}])
        );
        assert!(
            l.get("phone").is_none() && l.get("phone_from").is_none(),
            "{l}"
        );
        // What the household said of private items only is not said to be.
        let l = &shown(VISIT, "P DUMMY", true)[0];
        assert_eq!(
            (&l["confidence"], &l["evidence"]),
            (&json!("household"), &json!([]))
        );
        assert!(l.get("phone").is_none());
        let l = &shown(VISIT, "P DUMMY", false)[0];
        assert_eq!(l["evidence"][0]["by"], "Robin Example");
        assert_eq!(l["phone"], "555-010-0166");
    }

    #[test]
    fn what_a_later_medrec_writes_is_read_as_likely() {
        assert_eq!(
            shown(VISIT, "L FILLER", true),
            [json!({
                "field": "provider", "printed": "L FILLER", "name": "Lee Filler",
                "confidence": "likely", "evidence": [{"from": "telepathy"}],
            })]
        );
    }

    #[test]
    fn what_is_printed_is_listed_once_in_a_set_order() {
        let fields = json!({"location": "A", "provider": "B", "code": "C", "facility": ["D"]});
        let of: Vec<(&str, String)> = Printed::of_record(fields.as_object().unwrap())
            .into_iter()
            .map(|p| (p.field, p.text))
            .collect();
        assert_eq!(of, [("provider", "B".into()), ("location", "A".into())]);
        let e = json!({"place": "A", "clinicians": ["B", "C"], "members": [
            {"record": VISIT, "provider": "B"},
            {"record": TALK, "person": "p_otheraaaaaaaaaaaaaaa", "provider": "D"},
            {"document": OPEN, "title": "E"}]});
        let of = |full| -> Vec<(&str, String)> {
            Printed::of_event(&e, full)
                .into_iter()
                .map(|p| (p.field, p.text))
                .collect()
        };
        assert_eq!(
            of(false),
            [
                ("place", "A".to_string()),
                ("clinicians", "B".into()),
                ("clinicians", "C".into())
            ]
        );
        assert_eq!(of(true)[3..], [("members", "B".to_string())]);
    }
}
