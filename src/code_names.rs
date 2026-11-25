//! The plain names of a person's diagnosis codes
//! (`views/<pid>/code-names.json`, `medrec-code-names/v1`): the ICD-10-CM
//! title of each code their conditions and claims use, for a reader to show
//! with the source's own words, and before them where they are not plain. It
//! only labels items a reader already shows; by itself it lists a person's
//! conditions, so a reader never lists, searches or serves it.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Map, Value};

use crate::folder::Folder;
use crate::model::{Household, Person, PersonIndex, Record};
use crate::normalize::ICD10;
use crate::private::{self, Mark};
use crate::tables::{self, Titles, ICD10CM};

pub const FILE: &str = "code-names.json";
pub const SCHEMA: &str = "medrec-code-names/v1";

/// A code's key: `icd10cm|J30.9`, whatever the source wrote (`J309`,
/// `j30.9`). None for a code of another system (WHO's ICD-10, ICD-10-PCS
/// and ICD-9 are others), or one that cannot be ICD-10-CM.
pub fn key(system: Option<&str>, code: Option<&str>) -> Option<String> {
    if !system?.trim().eq_ignore_ascii_case(ICD10) {
        return None;
    }
    let code = tables::bare(code?);
    if !(3..=7).contains(&code.len()) || !code.starts_with(|c: char| c.is_ascii_alphabetic()) {
        return None;
    }
    let (head, rest) = code.split_at(3);
    Some(match rest {
        "" => format!("{}|{head}", ICD10CM.id),
        _ => format!("{}|{head}.{rest}", ICD10CM.id),
    })
}

/// A condition's own key.
pub fn condition_key(r: &Record) -> Option<String> {
    (r.kind == "condition").then(|| keys(r).pop()).flatten()
}

/// The keys a record's diagnoses have: a condition's code, and each of a
/// claim's diagnoses.
pub fn keys(r: &Record) -> Vec<String> {
    let of = |v: Option<&Value>| {
        let v = v?;
        key(
            v.get("system").and_then(Value::as_str),
            v.get("code").and_then(Value::as_str),
        )
    };
    match r.kind.as_str() {
        "condition" => r
            .code
            .as_ref()
            .and_then(|c| key(c.system.as_deref(), c.code.as_deref()))
            .into_iter()
            .collect(),
        "claim" => r
            .fields
            .get("diagnoses")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|d| of(d.get("code")))
            .collect(),
        _ => Vec::new(),
    }
}

/// Where a pass's names come from.
pub enum Source<'a> {
    /// The table on this machine.
    Table(&'a Titles),
    /// The person's `code-names.json` as it was: a machine without the
    /// table keeps the names another wrote.
    Previous(&'a Value),
}

impl Source<'_> {
    fn table(&self) -> Value {
        match self {
            Source::Table(_) => {
                json!({"id": ICD10CM.id, "version": ICD10CM.version, "date": ICD10CM.date})
            }
            Source::Previous(v) => v["table"].clone(),
        }
    }

    /// The names of `keys`, by key.
    fn names(&self, keys: &BTreeSet<&String>) -> BTreeMap<String, String> {
        match self {
            Source::Table(titles) => {
                let bare = |k: &str| tables::bare(k.split_once('|').map_or("", |(_, c)| c));
                let found = titles.of(&keys.iter().map(|k| bare(k)).collect());
                keys.iter()
                    .filter_map(|k| Some((k.to_string(), found.get(&bare(k))?.clone())))
                    .collect()
            }
            Source::Previous(v) => ["codes", "private_codes"]
                .iter()
                .filter_map(|m| v[*m].as_object())
                .flatten()
                .filter(|(k, _)| keys.contains(k))
                .filter_map(|(k, x)| Some((k.clone(), x["name"].as_str()?.to_string())))
                .collect(),
        }
    }
}

/// The person's `code-names.json`, or none when no code of theirs has a
/// name. `private` is their `private.json`: a code only private items use
/// goes in `private_codes`, and every code does when their `marks.json`
/// does not read.
pub fn build(
    p: &Person,
    idx: &PersonIndex,
    private: &Value,
    marks: &[Mark],
    source: &Source,
) -> Option<Value> {
    let hidden = private.get("marks_unreadable").is_some();
    let items = &private["records"];
    let mut open: BTreeSet<String> = BTreeSet::new();
    let mut closed: BTreeSet<String> = BTreeSet::new();
    for r in &idx.records {
        let kept_private = hidden || items.get(&r.id).is_some();
        // The household said this item is not private.
        let opened = marks
            .iter()
            .rev()
            .find(|m| m.item == r.id)
            .is_some_and(|m| !m.private);
        for k in keys(r) {
            let code = k.split_once('|').map_or("", |(_, c)| c);
            // A claim is private by its principal diagnosis; another of its
            // diagnoses in a private area is held back all the same.
            let private = kept_private
                || (r.kind == "claim" && !opened && private::icd10_area(code).is_some());
            if private {
                closed.insert(k);
            } else {
                open.insert(k);
            }
        }
    }
    let names = source.names(&open.iter().chain(&closed).collect());
    let named = |keys: &mut dyn Iterator<Item = &String>| -> Map<String, Value> {
        keys.filter_map(|k| Some((k.clone(), json!({"name": names.get(k)?}))))
            .collect()
    };
    let codes = named(&mut open.iter());
    let private_codes = named(&mut closed.iter().filter(|k| !open.contains(*k)));
    if codes.is_empty() && private_codes.is_empty() {
        return None;
    }
    Some(json!({
        "schema": SCHEMA,
        "person": p.id,
        "table": source.table(),
        "codes": codes,
        "private_codes": private_codes,
    }))
}

/// A `code-names.json` of this person's, as a pass reads the last one.
pub fn previous(text: &str, person: &str) -> Option<Value> {
    serde_json::from_str(text)
        .ok()
        .filter(|v: &Value| v["schema"] == SCHEMA && v["person"] == person)
}

/// A person's names, for an answer to look up by record.
#[derive(Debug, Clone, Default)]
pub struct Names {
    codes: BTreeMap<String, String>,
    private_codes: BTreeMap<String, String>,
}

impl Names {
    /// From a `code-names.json`.
    pub fn of(v: &Value, person: &str) -> Option<Names> {
        if v["schema"] != SCHEMA || v["person"] != person {
            return None;
        }
        let map = |k: &str| {
            v[k].as_object()
                .into_iter()
                .flatten()
                .filter_map(|(k, x)| Some((k.clone(), x["name"].as_str()?.to_string())))
                .collect()
        };
        Some(Names {
            codes: map("codes"),
            private_codes: map("private_codes"),
        })
    }

    /// The person's names as the folder's views have them: none when the
    /// views are older than the files they were made from.
    pub fn read(f: &Folder, h: &Household, p: &Person) -> Names {
        crate::viewfiles::current_view(f, h, p, FILE)
            .ok()
            .flatten()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .and_then(|v: Value| Names::of(&v, &p.id))
            .unwrap_or_default()
    }

    /// The name of a key. An answer about one person (`whole: false`) has
    /// both maps; one about the household has only the codes an item that
    /// is not private uses, and none for an item that is.
    pub fn name(&self, key: &str, whole: bool, private: bool) -> Option<&str> {
        if whole && private {
            return None;
        }
        self.codes
            .get(key)
            .or_else(|| (!whole).then(|| self.private_codes.get(key)).flatten())
            .map(String::as_str)
    }
}

/// The titles on this machine, if the table is here and whole.
pub fn titles() -> Option<Titles> {
    tables::dir().as_deref().and_then(Titles::load)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(v: Value) -> Record {
        serde_json::from_value(v).unwrap()
    }

    fn condition(id: &str, code: &str) -> Value {
        json!({"id": id, "kind": "condition", "key": id, "display": "Source words", "fields": {},
            "provenance": [], "code": {"system": ICD10, "code": code, "display": "Source words"}})
    }

    fn claim(id: &str, codes: &[&str]) -> Value {
        let d: Vec<Value> = codes
            .iter()
            .enumerate()
            .map(|(i, c)| json!({"sequence": i + 1, "code": {"system": ICD10, "code": c}}))
            .collect();
        json!({"id": id, "kind": "claim", "key": id, "display": "Claim",
            "fields": {"diagnoses": d}, "provenance": []})
    }

    /// The view of these records, by the person's own `private.json`.
    fn view(records: Vec<Value>, marks: &[Mark], unreadable: bool) -> Option<Value> {
        let p: Person = serde_json::from_value(json!({"id": "p_test", "name": "Robin Example",
            "folder": "Robin Example", "created_at": "2026-01-01T00:00:00Z"}))
        .unwrap();
        let mut idx = PersonIndex::new("p_test");
        idx.records = records.into_iter().map(record).collect();
        let events = crate::events::Built {
            events: json!({"schema": crate::events::SCHEMA, "events": []}),
            ..Default::default()
        };
        let mut private = private::build(
            &p,
            &idx,
            &crate::model::PersonText::new("p_test"),
            &events,
            marks,
            &BTreeMap::new(),
        );
        if unreadable {
            private["marks_unreadable"] = Value::Bool(true);
        }
        let titles = Titles::invented(&[
            ("J309", "Sample sneezes"),
            ("F321", "Sample low mood"),
            ("R51", "Sample aches"),
        ]);
        build(&p, &idx, &private, marks, &Source::Table(&titles))
    }

    fn keys_of(v: &Value, map: &str) -> Vec<String> {
        v[map].as_object().unwrap().keys().cloned().collect()
    }

    #[test]
    fn a_key_is_the_code_in_one_form_of_one_system() {
        for (code, want) in [
            ("J30.9", "icd10cm|J30.9"),
            ("j309", "icd10cm|J30.9"),
            (" J30.9 ", "icd10cm|J30.9"),
            ("R51", "icd10cm|R51"),
            ("S72.001A", "icd10cm|S72.001A"),
        ] {
            assert_eq!(
                key(Some(ICD10), Some(code)).as_deref(),
                Some(want),
                "{code}"
            );
        }
        for (system, code) in [
            (None, Some("R51")),
            (Some(ICD10), None),
            (Some(ICD10), Some("")),
            (Some(ICD10), Some("R5")),
            (Some(ICD10), Some("123")),
            (Some(ICD10), Some("J30.90000")),
            (Some("http://hl7.org/fhir/sid/icd-10"), Some("R51")),
            (
                Some("http://www.cms.gov/Medicare/Coding/ICD10"),
                Some("R51"),
            ),
            (Some("http://hl7.org/fhir/sid/icd-9-cm"), Some("R51")),
        ] {
            assert_eq!(key(system, code), None, "{system:?} {code:?}");
        }
    }

    #[test]
    fn a_code_only_private_items_use_is_apart() {
        let v = view(
            vec![
                condition("r_a", "J309"),
                condition("r_b", "F32.1"),
                claim("r_c", &["R51"]),
            ],
            &[],
            false,
        )
        .unwrap();
        assert_eq!(v["schema"], SCHEMA);
        assert_eq!(v["person"], "p_test");
        assert_eq!(v["table"]["version"], "FY2027");
        assert_eq!(keys_of(&v, "codes"), ["icd10cm|J30.9", "icd10cm|R51"]);
        assert_eq!(keys_of(&v, "private_codes"), ["icd10cm|F32.1"]);
        assert_eq!(
            v["codes"]["icd10cm|J30.9"],
            json!({"name": "Sample sneezes"})
        );
    }

    #[test]
    fn another_diagnosis_of_a_claim_in_a_private_area_is_held_back_unless_the_household_opens_it() {
        let records = || vec![claim("r_c", &["J30.9", "F32.1"])];
        let v = view(records(), &[], false).unwrap();
        assert_eq!(keys_of(&v, "codes"), ["icd10cm|J30.9"]);
        assert_eq!(keys_of(&v, "private_codes"), ["icd10cm|F32.1"]);
        let open: Mark = serde_json::from_value(json!({"id": "m_a", "item": "r_c",
            "private": false, "at": "2026-11-24T00:00:00Z"}))
        .unwrap();
        let v = view(records(), &[open], false).unwrap();
        assert_eq!(keys_of(&v, "private_codes"), Vec::<String>::new());
    }

    #[test]
    fn the_households_mark_and_unreadable_marks_decide_as_in_private_json() {
        let hide: Mark = serde_json::from_value(json!({"id": "m_a", "item": "r_a",
            "private": true, "at": "2026-11-24T00:00:00Z"}))
        .unwrap();
        let v = view(vec![condition("r_a", "J30.9")], &[hide], false).unwrap();
        assert_eq!(keys_of(&v, "private_codes"), ["icd10cm|J30.9"]);
        let v = view(vec![condition("r_a", "J30.9")], &[], true).unwrap();
        assert!(keys_of(&v, "codes").is_empty(), "{v}");
        assert_eq!(keys_of(&v, "private_codes"), ["icd10cm|J30.9"]);
    }

    #[test]
    fn a_code_the_table_lacks_has_no_entry_and_no_named_code_means_no_file() {
        assert_eq!(view(vec![condition("r_a", "Z99.89")], &[], false), None);
        assert_eq!(view(vec![], &[], false), None);
        let v = view(
            vec![condition("r_a", "Z99.89"), condition("r_b", "R51")],
            &[],
            false,
        )
        .unwrap();
        assert_eq!(keys_of(&v, "codes"), ["icd10cm|R51"]);
    }

    #[test]
    fn a_household_wide_answer_never_reads_the_private_codes() {
        let v = json!({"schema": SCHEMA, "person": "p_test",
            "codes": {"icd10cm|R51": {"name": "Sample aches"}},
            "private_codes": {"icd10cm|F32.1": {"name": "Sample low mood"}}});
        let n = Names::of(&v, "p_test").unwrap();
        let (open, closed) = ("icd10cm|R51", "icd10cm|F32.1");
        assert_eq!(n.name(open, true, false), Some("Sample aches"));
        assert_eq!(n.name(open, true, true), None);
        assert_eq!(n.name(closed, true, false), None);
        assert_eq!(n.name(closed, true, true), None);
        assert_eq!(n.name(open, false, true), Some("Sample aches"));
        assert_eq!(n.name(closed, false, true), Some("Sample low mood"));
        assert!(Names::of(&v, "p_other").is_none());
        assert!(Names::of(&json!({"schema": "x"}), "p_test").is_none());
    }
}
