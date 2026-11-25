//! What an import says of the records its source gave before: how many of them
//! the source does not give now, and the names of what changed in each that it
//! updated. Counts and names, and no value.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use pk_med_records::Skip;
use serde::Serialize;
use serde_json::{Map, Value};

use crate::about;
use crate::import;
use crate::model::{ImportRun, Person, PersonIndex, Provenance, Record};

/// A record that an import updated.
#[derive(Debug, PartialEq, Serialize)]
pub struct Updated {
    pub id: String,
    pub kind: String,
    /// What is other than it was, by name; `fields.` before a field's.
    pub changed: Vec<String>,
}

/// What names no part of what a record says, and what no reader shows.
const APART: [&str; 8] = [
    "id",
    "kind",
    "key",
    "provenance",
    "fields_from",
    "given",
    "lists",
    "listed_by",
];

/// What of these records is other than it was, in the order they are in.
pub fn updated(
    person: &Person,
    before: &[Record],
    now: &PersonIndex,
    ids: &HashSet<String>,
) -> Vec<Updated> {
    let others = about::ids(person, now);
    let of = now.records.iter().filter(|r| ids.contains(&r.id));
    of.filter(|r| !others.contains(&r.id))
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

/// An export through a sign-in of all a source has.
pub struct Export<'a> {
    pub source_dir: &'a str,
    pub patient: &'a str,
    /// Whether its provider also reads downloaded files.
    pub reads_files: bool,
    /// The raw files it has.
    pub served: &'a HashSet<String>,
    pub skipped: &'a [Skip],
}

/// The type a producer reads another with, by its reference: where one of that
/// type was left out, so may what it refers to be.
fn read_with(resource_type: &str) -> Option<&'static str> {
    (resource_type == "MedicationRequest").then_some("MedicationDispense")
}

/// The records of a source that an export does not have, by kind. `None` where
/// that cannot be told: the folder of the source has imports of another patient,
/// or the export says it left something out and not of which type. A record is
/// not counted where a resource of it may be among those the export left out, nor
/// where the version filed of one was not read through a sign-in: downloaded files
/// can have what the source gives no sign-in.
pub fn not_served(
    person: &Person,
    index: &PersonIndex,
    export: &Export,
) -> Option<BTreeMap<String, usize>> {
    let Export {
        source_dir,
        patient,
        reads_files,
        served,
        skipped,
    } = *export;
    let runs = index.imports.iter();
    let runs: Vec<&ImportRun> = runs.filter(|i| i.source_dir == source_dir).collect();
    if runs.iter().any(|i| i.patient_id != patient) {
        return None;
    }
    // A run filed before it was kept which it was of is a sign-in's where the
    // provider has no other.
    let (logins, files): (Vec<&ImportRun>, Vec<&ImportRun>) = runs
        .into_iter()
        .partition(|i| !i.from_files.unwrap_or(reads_files));
    let gave = |p: &Provenance| {
        let at = p.fetched_at.as_deref();
        let of = |runs: &[&ImportRun]| runs.iter().any(|i| Some(i.pulled_at.as_str()) == at);
        of(&logins) && !of(&files)
    };
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
            let with = read_with(t);
            types.contains(t)
                || each.contains(&(t, r.id.as_str()))
                || with.is_some_and(|w| types.contains(w) || each.iter().any(|(t, _)| *t == w))
        })
    };
    let others = about::ids(person, index);
    let mut out = BTreeMap::new();
    for r in &index.records {
        let paths = r.provenance.iter().filter_map(|p| {
            let path = p.raw_path.as_deref().filter(|f| f.starts_with(&within))?;
            Some((p, import::current_of(path).unwrap_or_else(|| path.into())))
        });
        let (mut any, mut gone) = (false, true);
        for (p, path) in paths {
            any = true;
            gone &= !served.contains(&path) && gave(p) && !left_out(p);
        }
        if any && gone && !others.contains(&r.id) {
            *out.entry(r.kind.clone()).or_default() += 1;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const DIR: &str = "Robin Example/Records/fakeprov-one";
    const AT: &str = "2024-01-02T10:00:00Z";

    fn record(id: &str, kind: &str, of: &[(&str, &str)]) -> Record {
        let provenance = of.iter().map(|(dir, resource)| {
            let (t, rid) = resource.split_once('/').unwrap();
            json!({"source": {"kind": "fhir", "cli": "fakeprov", "organization_id": "one"},
                "source_resource": {"type": t, "id": rid}, "method": "fhir-api",
                "fetched_at": AT, "filed_at": AT, "filed_by": "medrec 0.9.9",
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
            "organization": "one", "patient_id": patient, "pulled_at": AT, "imported_at": AT,
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
            record(
                "read with another",
                "medication",
                &[(DIR, "MedicationRequest/g")],
            ),
        ];
        idx.imports = vec![run(DIR, "pat-1"), run(other, "pat-2")];
        let served: HashSet<String> = ["Observation/a", "Claim/f"]
            .map(|r| format!("{DIR}/{r}.json"))
            .into();
        let of = |idx: &PersonIndex, skipped: &[Skip], reads_files: bool| {
            let export = Export {
                source_dir: DIR,
                patient: "pat-1",
                reads_files,
                served: &served,
                skipped,
            };
            not_served(&person(), idx, &export).map(|n| serde_json::to_value(n).unwrap())
        };
        let count = |idx: &PersonIndex, skipped: &[Skip]| of(idx, skipped, false);
        let all = json!({"condition": 1, "medication": 1, "result": 1});
        assert_eq!(count(&idx, &[]), Some(all.clone()));
        for (left_out, want) in [
            (
                json!({"resource_type": "Observation", "reason": "", "code": "forbidden"}),
                Some(json!({"condition": 1, "medication": 1})),
            ),
            (
                json!({"search": "Observation?category=x", "reason": "", "code": "page_cap"}),
                Some(json!({"condition": 1, "medication": 1})),
            ),
            (
                json!({"resource_type": "Observation", "id": "b", "reason": "", "code": "too_large"}),
                Some(json!({"condition": 1, "medication": 1})),
            ),
            (
                json!({"resource_type": "Observation", "id": "Observation/b", "reason": "",
                    "code": "upstream"}),
                Some(json!({"condition": 1, "medication": 1})),
            ),
            // What another type is read with.
            (
                json!({"resource_type": "MedicationDispense", "reason": "", "code": "upstream"}),
                Some(json!({"condition": 1, "result": 1})),
            ),
            (
                json!({"resource_type": "MedicationDispense", "id": "x", "reason": "",
                    "code": "too_large"}),
                Some(json!({"condition": 1, "result": 1})),
            ),
            (
                json!({"resource_type": "Binary", "id": "b", "reason": "", "code": "no_file"}),
                Some(all.clone()),
            ),
            (json!({"reason": "", "code": "upstream"}), None),
        ] {
            assert_eq!(count(&idx, &[skip(left_out.clone())]), want, "{left_out}");
        }
        idx.imports.push(run(DIR, "pat-2"));
        assert_eq!(count(&idx, &[]), None);
        idx.imports.pop();
        // A run that does not say which it was is a sign-in's where the provider
        // has no other.
        assert_eq!(of(&idx, &[], true), Some(json!({})));
        idx.imports[0].from_files = Some(false);
        assert_eq!(of(&idx, &[], true), Some(all));
        // What files gave is not counted, nor what both gave at one time.
        let mut files = run(DIR, "pat-1");
        files.from_files = Some(true);
        idx.imports.push(files);
        assert_eq!(count(&idx, &[]), Some(json!({})));
        idx.imports[2].pulled_at = "2024-01-03T10:00:00Z".into();
        idx.records[1].provenance[0].fetched_at = Some("2024-01-03T10:00:00Z".into());
        let rest = json!({"condition": 1, "medication": 1});
        assert_eq!(count(&idx, &[]), Some(rest.clone()));
        idx.records[1].provenance[0].fetched_at = None;
        assert_eq!(count(&idx, &[]), Some(rest.clone()));
        // What is about somebody else has no kind.
        idx.records[1].provenance[0].fetched_at = Some(AT.into());
        idx.records[1]
            .fields
            .insert(about::FIELD.into(), "another".into());
        assert_eq!(count(&idx, &[]), Some(rest));
    }

    #[test]
    fn what_changed_in_a_record_is_named() {
        let mut was = record("one", "result", &[(DIR, "Observation/a")]);
        was.fields.insert("value".into(), json!({"value": 5}));
        was.fields.insert("note".into(), "as it was".into());
        let word = json!([{"raw_path": format!("{DIR}/Observation/a.json"), "status": "final",
            "version": "1", "read": "2024-01-02"}]);
        was.words = serde_json::from_value(word).unwrap();
        let mut now = was.clone();
        now.words[0].version = Some("2".into());
        now.words[0].read = Some("2024-03-02".into());
        now.provenance[0].filed_at = "2024-03-02T10:00:00Z".into();
        now.given = vec![Default::default()];
        now.lists = BTreeMap::from([(format!("{DIR}/Observation/a.json"), vec![])]);
        let ids: HashSet<String> = ["one".to_string(), "new".to_string()].into();
        let names = |now: &Record| {
            let mut idx = PersonIndex::new("p_test");
            let new = record("new", "result", &[(DIR, "Observation/b")]);
            idx.records = vec![now.clone(), new];
            updated(&person(), &[was.clone()], &idx, &ids)
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
        now.words[0].status = Some("amended".into());
        assert_eq!(names(&now), [of(&[&want[..], &["words"]].concat())]);
        // Nor is one about somebody else named.
        now.fields.insert(about::FIELD.into(), "another".into());
        assert_eq!(names(&now), []);
        let mut idx = PersonIndex::new("p_test");
        idx.records = vec![was.clone()];
        assert!(updated(&person(), &[was.clone()], &idx, &HashSet::new()).is_empty());
    }
}
