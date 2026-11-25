//! What an import says of the records its source gave before: how many of them
//! the source does not give now, and the names of what changed in each that it
//! updated. Counts and names, and no value.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use pk_med_records::Skip;
use serde::Serialize;
use serde_json::{Map, Value};

use crate::about;
use crate::import;
use crate::model::{Person, PersonIndex, Provenance, Record};

/// A record that an import updated.
#[derive(Debug, PartialEq, Serialize)]
pub struct Updated {
    pub id: String,
    pub kind: String,
    /// What is other than it was, by name; `fields.` before a field's.
    pub changed: Vec<String>,
}

/// What names no part of what a record says, and what no reader shows.
const APART: [&str; 6] = ["id", "kind", "key", "provenance", "fields_from", "given"];

/// What of these records is other than it was, in the order they are in.
pub fn updated(
    person: &Person,
    before: &[Record],
    now: &[Record],
    ids: &HashSet<String>,
) -> Vec<Updated> {
    let of = now.iter().filter(|r| ids.contains(&r.id));
    of.filter(|r| about::record(person, r).is_none())
        .filter_map(|r| {
            let was = before.iter().find(|b| b.id == r.id)?;
            Some(Updated {
                id: r.id.clone(),
                kind: r.kind.clone(),
                changed: changed(was, r),
            })
        })
        .collect()
}

fn changed(was: &Record, now: &Record) -> Vec<String> {
    let members = |r: &Record| {
        let mut r = r.clone();
        for w in &mut r.words {
            // Which version it is of, and when it was read, says nothing.
            w.version = None;
            w.read = None;
        }
        let fields = std::mem::take(&mut r.fields);
        let mut all = match serde_json::to_value(r) {
            Ok(Value::Object(m)) => m,
            _ => Map::new(),
        };
        for k in APART {
            all.remove(k);
        }
        all.extend(fields.into_iter().map(|(k, v)| (format!("fields.{k}"), v)));
        all
    };
    let (a, b) = (members(was), members(now));
    let names: BTreeSet<&String> = a.keys().chain(b.keys()).collect();
    let other = names.into_iter().filter(|k| a.get(*k) != b.get(*k));
    other.cloned().collect()
}

/// The records of a source that an export of all it has does not have, by kind.
/// `None` where that cannot be told: the folder of the source has imports of
/// another patient, or the export says it left something out and not of which
/// type. A record is not counted where a resource of it may be among those the
/// export left out.
pub fn not_served(
    person: &Person,
    index: &PersonIndex,
    source_dir: &str,
    patient: &str,
    served: &HashSet<String>,
    skipped: &[Skip],
) -> Option<BTreeMap<String, usize>> {
    let mut runs = index.imports.iter();
    if runs.any(|i| i.source_dir == source_dir && i.patient_id != patient) {
        return None;
    }
    let (mut types, mut each) = (HashSet::new(), HashSet::new());
    for s in skipped {
        let of = s.resource_type.as_deref().filter(|t| !t.is_empty());
        let of = of.or_else(|| s.search.as_deref()?.split(['?', '/']).next());
        let of = of.filter(|t| !t.is_empty());
        match (of, s.id.as_deref().filter(|id| !id.is_empty())) {
            (Some(t), Some(id)) => {
                each.insert((t, id.rsplit('/').next().unwrap_or(id)));
            }
            (Some(t), None) => {
                types.insert(t);
            }
            (None, _) => return None,
        }
    }
    let within = format!("{source_dir}/");
    let left_out = |p: &Provenance| {
        let r = p.source_resource.as_ref();
        r.is_some_and(|r| {
            let t = r.resource_type.as_str();
            types.contains(t) || each.contains(&(t, r.id.as_str()))
        })
    };
    let mut out = BTreeMap::new();
    for r in &index.records {
        let paths = r.provenance.iter().filter_map(|p| {
            let path = p.raw_path.as_deref().filter(|f| f.starts_with(&within))?;
            Some((p, import::current_of(path).unwrap_or_else(|| path.into())))
        });
        let (mut any, mut gone) = (false, true);
        for (p, path) in paths {
            any = true;
            gone &= !served.contains(&path) && !left_out(p);
        }
        if any && gone && about::record(person, r).is_none() {
            *out.entry(r.kind.clone()).or_default() += 1;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ImportRun;
    use serde_json::json;

    const DIR: &str = "Robin Example/Records/fakeprov-one";

    fn record(id: &str, kind: &str, of: &[(&str, &str)]) -> Record {
        let provenance = of.iter().map(|(dir, resource)| {
            let (t, rid) = resource.split_once('/').unwrap();
            json!({"source": {"kind": "fhir", "cli": "fakeprov", "organization_id": "one"},
                "source_resource": {"type": t, "id": rid}, "method": "fhir-api",
                "filed_at": "2024-01-02T10:00:00Z", "filed_by": "medrec 0.9.9",
                "raw_path": format!("{dir}/{resource}.json")})
        });
        let provenance: Vec<Value> = provenance.collect();
        serde_json::from_value(json!({"id": id, "kind": kind, "key": id, "display": id,
            "provenance": provenance}))
        .unwrap()
    }

    fn person() -> Person {
        serde_json::from_value(json!({"id": "p_test", "name": "Robin Example",
            "folder": "Robin Example", "created_at": "2024-01-01T00:00:00Z"}))
        .unwrap()
    }

    fn skip(v: Value) -> Skip {
        serde_json::from_value(v).unwrap()
    }

    fn run(dir: &str, patient: &str) -> ImportRun {
        serde_json::from_value(json!({"id": "i_runaaaaaaaaaaaaaaaaa", "source_dir": dir,
            "stream_sha256": "", "stream_bytes": 0, "cli": "fakeprov", "cli_version": "0.0.1",
            "organization": "one", "patient_id": patient, "pulled_at": "", "imported_at": "",
            "counts": {}, "records_added": 0, "records_updated": 0, "records_unchanged": 0,
            "resources_written": 0, "resources_unchanged": 0, "documents_added": 0}))
        .unwrap()
    }

    /// A record is not served where none of the source's resources of it is, and
    /// none of them may be among those the export left out.
    #[test]
    fn what_a_source_does_not_serve_is_counted_by_kind() {
        let mut idx = PersonIndex::new("p_test");
        let other = "Robin Example/Records/fakeprov-two";
        idx.records = vec![
            record("served", "result", &[(DIR, "Observation/a")]),
            record("gone", "result", &[(DIR, "Observation/b")]),
            record(
                "one of two",
                "result",
                &[(DIR, "Observation/c"), (DIR, "Observation/a")],
            ),
            record(
                "another has it",
                "condition",
                &[(DIR, "Condition/d"), (other, "Condition/d")],
            ),
            record("of another", "claim", &[(other, "Claim/e")]),
            record("a version of it", "claim", &[(DIR, "Claim/f~abcdefab")]),
        ];
        let served: HashSet<String> = ["Observation/a", "Claim/f"]
            .map(|r| format!("{DIR}/{r}.json"))
            .into();
        let count = |idx: &PersonIndex, skipped: &[Skip]| {
            let n = not_served(&person(), idx, DIR, "pat-1", &served, skipped);
            n.map(|n| serde_json::to_value(n).unwrap())
        };
        assert_eq!(count(&idx, &[]), Some(json!({"condition": 1, "result": 1})));
        for (left_out, want) in [
            (
                json!({"resource_type": "Observation", "reason": "", "code": "forbidden"}),
                Some(json!({"condition": 1})),
            ),
            (
                json!({"search": "Observation?category=x", "reason": "", "code": "page_cap"}),
                Some(json!({"condition": 1})),
            ),
            (
                json!({"resource_type": "Observation", "id": "b", "reason": "", "code": "too_large"}),
                Some(json!({"condition": 1})),
            ),
            (
                json!({"resource_type": "Binary", "id": "b", "reason": "", "code": "no_file"}),
                Some(json!({"condition": 1, "result": 1})),
            ),
            (json!({"reason": "", "code": "upstream"}), None),
        ] {
            assert_eq!(count(&idx, &[skip(left_out.clone())]), want, "{left_out}");
        }
        idx.imports = vec![run(DIR, "pat-1"), run(other, "pat-2")];
        assert!(count(&idx, &[]).is_some());
        idx.imports.push(run(DIR, "pat-2"));
        assert_eq!(count(&idx, &[]), None);
        // What is about somebody else has no kind.
        idx.imports.clear();
        idx.records[1]
            .fields
            .insert(about::FIELD.into(), "another".into());
        assert_eq!(count(&idx, &[]), Some(json!({"condition": 1})));
    }

    #[test]
    fn what_changed_in_a_record_is_named() {
        let mut was = record("one", "result", &[(DIR, "Observation/a")]);
        was.fields.insert("value".into(), json!({"value": 5}));
        was.fields.insert("note".into(), "as it was".into());
        let mut now = was.clone();
        now.provenance[0].filed_at = "2024-03-02T10:00:00Z".into();
        now.given = vec![Default::default()];
        let ids: HashSet<String> = ["one".to_string(), "new".to_string()].into();
        let names = |now: &Record| {
            let new = record("new", "result", &[(DIR, "Observation/b")]);
            updated(&person(), &[was.clone()], &[now.clone(), new], &ids)
        };
        let of = |changed: &[&str]| Updated {
            id: "one".into(),
            kind: "result".into(),
            changed: changed.iter().map(|s| s.to_string()).collect(),
        };
        // A version that differs in nothing medrec reads.
        assert_eq!(names(&now), [of(&[])]);
        now.status = Some("amended".into());
        now.fields.insert("value".into(), json!({"value": 6}));
        now.fields.remove("note");
        now.fields.insert("unit".into(), "mg".into());
        let want = ["fields.note", "fields.unit", "fields.value", "status"];
        assert_eq!(names(&now), [of(&want)]);
        // Nor is one about somebody else named.
        now.fields.insert(about::FIELD.into(), "another".into());
        assert_eq!(names(&now), []);
        assert!(updated(&person(), &[was.clone()], &[was.clone()], &HashSet::new()).is_empty());
    }
}
