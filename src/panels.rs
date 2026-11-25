//! What a panel or a report lists, as its source says it
//! (docs/data-contract.md, "Panels"). Membership is never guessed by date or
//! name.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value};

use crate::given;
use crate::import;
use crate::model::{Code, Listed, Provenance, Record};
use crate::providers::local;
use crate::questionnaires;

/// The field of a panel or a report that has the ids of what it lists.
pub const MEMBERS: &str = "members";
/// What a list says of a heading in place of a value.
pub const HEADING: &str = "panel";

/// What a resource lists ([`Record::lists`]): the Observations of it that its
/// own server has.
pub fn lists(r: &Value, p: &Provenance) -> BTreeMap<String, Vec<String>> {
    let listed = observations(r, p.source.fhir_base.as_deref());
    let path = p.raw_path.clone().filter(|_| !listed.is_empty());
    path.map(|path| (path, listed)).into_iter().collect()
}

fn observations(r: &Value, base: Option<&str>) -> Vec<String> {
    let key = match r.get("resourceType").and_then(Value::as_str) {
        Some("DiagnosticReport") => "result",
        Some("Observation") => "hasMember",
        _ => return Vec::new(),
    };
    let listed = r.get(key).and_then(Value::as_array).into_iter().flatten();
    // Before R4.
    let before = r.get("related").and_then(Value::as_array);
    let before = before.into_iter().flatten().filter(|x| {
        key == "hasMember" && x.get("type").and_then(Value::as_str) == Some("has-member")
    });
    let before = before.filter_map(|x| x.get("target"));
    let mut out: Vec<String> = Vec::new();
    for x in listed.chain(before) {
        let reference = x.get("reference").and_then(Value::as_str);
        let Some((ty, id)) = reference.and_then(|t| local(t, base)) else {
            continue;
        };
        let file = format!("{}.json", import::raw_sub_of(ty, id));
        if ty == "Observation" && !out.contains(&file) {
            out.push(file);
        }
    }
    out
}

/// The fields a source gave, without what is made of other records.
pub fn without_links(fields: &Map<String, Value>) -> Map<String, Value> {
    let mut out = fields.clone();
    out.remove(MEMBERS);
    out
}

/// The ids a panel lists.
pub fn members(r: &Record) -> Vec<&str> {
    let ids = r.fields.get(MEMBERS).and_then(Value::as_array);
    ids.into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect()
}

/// Take the ids of what is gone out of what a panel lists.
pub fn without(fields: &mut Map<String, Value>, gone: &BTreeSet<String>) {
    let Some(Value::Array(ids)) = fields.get_mut(MEMBERS) else {
        return;
    };
    ids.retain(|id| !id.as_str().is_some_and(|id| gone.contains(id)));
    if ids.is_empty() {
        fields.remove(MEMBERS);
    }
}

/// Give each panel and report the ids of the results it lists, and each result
/// the questionnaires that list it. An id is of this index, so it is made
/// again on every write of it; a questionnaire is never taken away.
pub fn link(records: &mut [Record]) {
    let mut by_file: BTreeMap<String, usize> = BTreeMap::new();
    for (i, r) in records.iter().enumerate() {
        let files = r.provenance.iter().filter_map(|p| p.raw_path.clone());
        by_file.extend(files.filter(|_| r.kind == "result").map(|f| (f, i)));
    }
    let mut listing: Vec<(usize, Vec<usize>)> = Vec::new();
    for i in 0..records.len() {
        let r = &records[i];
        if !matches!(r.kind.as_str(), "result" | "report") {
            continue;
        }
        let mut of: Vec<usize> = Vec::new();
        // That of its values first.
        let first = import::wrote(r);
        let mut lists: Vec<(&String, &Vec<String>)> = r.lists.iter().collect();
        lists.sort_by_key(|(path, _)| first != Some(path.as_str()));
        for (path, files) in lists {
            let dir = import::source_dir_of(path).unwrap_or_default();
            for file in files {
                let at = by_file.get(&format!("{dir}/{file}")).copied();
                of.extend(at.filter(|at| *at != i && !of.contains(at)));
            }
        }
        let ids: Vec<Value> = of
            .iter()
            .map(|&at| records[at].id.as_str().into())
            .collect();
        match ids.is_empty() {
            true => records[i].fields.remove(MEMBERS),
            false => records[i].fields.insert(MEMBERS.into(), ids.into()),
        };
        if !of.is_empty() {
            listing.push((i, of));
        }
    }
    // A panel that a questionnaire lists passes it on, to any depth.
    loop {
        let mut more = false;
        for (i, of) in &listing {
            let by = asks(&records[*i]);
            for at in of {
                more |= keep(&mut records[*at], by.clone());
            }
        }
        if !more {
            return;
        }
    }
}

/// The questionnaires a panel is of: by its names, as any source gave them,
/// and those that list it.
fn asks(r: &Record) -> Vec<Listed> {
    let named = |r: &Record| Listed {
        display: r.display.clone(),
        code: r.code.clone(),
    };
    let given = given::as_given(r).map(|x| named(&x));
    let names = std::iter::once(named(r)).chain(given);
    let asking = names.filter(|l| questionnaires::of(&l.display, l.code.as_ref()).is_some());
    r.listed_by.iter().cloned().chain(asking).collect()
}

/// Put what a resource, by its raw file, lists on the record it is of, and take
/// it off one it has left. Whether that changes a record.
pub fn said(records: &mut [Record], path: &str, lists: BTreeMap<String, Vec<String>>) -> bool {
    let mut changed = false;
    for r in records {
        let mut paths = r.provenance.iter().filter_map(|p| p.raw_path.as_deref());
        let its = paths.any(|p| p == path);
        let now = lists.get(path).filter(|_| its);
        if r.lists.get(path) != now {
            changed = true;
            match now {
                Some(files) => r.lists.insert(path.to_string(), files.clone()),
                None => r.lists.remove(path),
            };
        }
    }
    changed
}

/// Keep with a result what lists it. Whether one was not kept.
pub fn keep(r: &mut Record, by: Vec<Listed>) -> bool {
    let had = r.listed_by.len();
    for of in by {
        if !r.listed_by.contains(&of) {
            r.listed_by.push(of);
        }
    }
    r.listed_by.len() != had
}

/// Whether a result is a heading: a panel that lists results and has no value
/// of its own.
pub fn heads(r: &Record) -> bool {
    let value = r.fields.get("value").map(crate::views::value_text);
    let own = value.is_some_and(|v| !v.is_empty());
    r.kind == "result" && !own && r.fields.contains_key(MEMBERS)
}

/// The names and codes of the questionnaires that list a result.
pub fn asked_in(r: &Record) -> impl Iterator<Item = (&str, Option<&Code>)> {
    let each = r.listed_by.iter();
    each.map(|l| (l.display.as_str(), l.code.as_ref()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const BASE: &str = "https://north.example.test/fhir";

    fn lists_of(name: &str, files: &[&str]) -> Value {
        match files {
            [] => json!({}),
            _ => json!({format!("Robin Example/Records/north/Observation/{name}.json"): files}),
        }
    }

    fn of(kind: &str, name: &str, display: &str, lists: &[&str]) -> Record {
        serde_json::from_value(json!({
            "id": format!("r_{name}"), "kind": kind, "key": name, "display": display,
            "fields": {}, "lists": lists_of(name, lists), "provenance": [{
                "source": {"kind": "fhir", "cli": "fakeprov"}, "method": "fhir-api",
                "filed_at": "2024-01-02T10:00:00Z", "filed_by": "medrec",
                "raw_path": format!("Robin Example/Records/north/Observation/{name}.json")}]
        }))
        .unwrap()
    }

    /// A resource lists the Observations of its own server, each once.
    #[test]
    fn what_a_resource_lists_is_its_own_servers() {
        let refs =
            |refs: &[&str]| -> Value { refs.iter().map(|r| json!({"reference": r})).collect() };
        let some = refs(&[
            "Observation/a",
            "https://north.example.test/fhir/Observation/b/_history/2",
            "https://south.example.test/fhir/Observation/c",
            "#inside",
            "Observation/a",
            "QuestionnaireResponse/d",
        ]);
        let want = ["Observation/a.json", "Observation/b.json"];
        let report = json!({"resourceType": "DiagnosticReport", "result": some});
        assert_eq!(observations(&report, Some(BASE)), want);
        let panel = json!({"resourceType": "Observation", "hasMember": some});
        assert_eq!(observations(&panel, Some(BASE)), want);
        let before = json!({"resourceType": "Observation", "related": [
            {"type": "has-member", "target": {"reference": "Observation/a"}},
            {"type": "derived-from", "target": {"reference": "Observation/b"}}]});
        assert_eq!(observations(&before, None), ["Observation/a.json"]);
        for other in [
            json!({"resourceType": "Observation", "result": some}),
            json!({"resourceType": "Encounter", "hasMember": some}),
            json!({"resourceType": "Observation", "hasMember": [{"display": "A"}]}),
        ] {
            assert!(observations(&other, Some(BASE)).is_empty(), "{other}");
        }
    }

    /// The ids are of the results that are there, of one source, in the order
    /// they are listed; made again each time, and none where none is there.
    #[test]
    fn the_members_are_the_results_that_are_there() {
        let files = [
            "Observation/b.json",
            "Observation/gone.json",
            "Observation/a.json",
            "Observation/panel.json",
        ];
        let mut all = vec![
            of("result", "a", "One", &[]),
            of("result", "b", "Two", &[]),
            of("result", "panel", "Example panel", &files),
            of("encounter", "gone", "Visit", &[]),
        ];
        let mut south = of("report", "report", "Example report", &[]);
        let path = "Robin Example/Records/south/DiagnosticReport/report.json";
        south.provenance[0].raw_path = Some(path.into());
        south.lists = serde_json::from_value(json!({path: files})).unwrap();
        all.push(south);
        link(&mut all);
        assert_eq!(members(&all[2]), ["r_b", "r_a"]);
        assert!(heads(&all[2]) && !heads(&all[0]));
        assert!(members(&all[4]).is_empty());
        assert!(all.iter().all(|r| r.listed_by.is_empty()));
        // Of two sources, what either lists of its own.
        let north = all[2].clone();
        all[4].provenance.extend(north.provenance);
        all[4].lists.extend(north.lists);
        all[0]
            .provenance
            .extend(of("result", "x", "", &[]).provenance);
        let path = "Robin Example/Records/south/Observation/a.json";
        all[0].provenance[1].raw_path = Some(path.into());
        link(&mut all);
        assert_eq!(members(&all[4]), ["r_a", "r_b", "r_panel"]);
        // One that leaves, or lists another, says so.
        let path = all[4].provenance[1].raw_path.clone().unwrap();
        all[4].provenance.pop();
        assert!(said(&mut all[4..], &path, BTreeMap::new()));
        assert!(!said(&mut all[4..], &path, BTreeMap::new()));
        link(&mut all);
        assert_eq!(members(&all[4]), ["r_a"]);
        // With a value of its own it is a result like any other.
        all[2].fields.insert("value".into(), json!("Seen"));
        assert!(!heads(&all[2]) && without_links(&all[2].fields).len() == 1);
        // One left out of what is shown.
        let gone = BTreeSet::from(["r_b".to_string()]);
        without(&mut all[2].fields, &gone);
        assert_eq!(members(&all[2]), ["r_a"]);
        all.retain(|r| r.kind != "result" || r.id == "r_panel");
        link(&mut all);
        assert!(members(&all[0]).is_empty() && !all[0].fields.contains_key(MEMBERS));
    }

    /// What a questionnaire lists is of it, through any panel of it, by any
    /// name it was given, and stays so.
    #[test]
    fn what_a_questionnaire_lists_is_of_it() {
        let asked = |r: &Record| -> Vec<&'static str> {
            let of = questionnaires::asked_by(r);
            of.iter().map(|q| q.kind).collect()
        };
        let mut all = vec![
            of("result", "a", "One", &[]),
            of("result", "inner", "First part", &["Observation/a.json"]),
            of(
                "result",
                "outer",
                "PHQ-9 panel",
                &["Observation/inner.json"],
            ),
            of("result", "b", "Two", &[]),
            of("result", "named", "Example panel", &["Observation/b.json"]),
        ];
        all[4].given = serde_json::from_value(json!([{"display": "GAD-7"}])).unwrap();
        link(&mut all);
        assert_eq!(asked(&all[0]), ["depression"]);
        assert_eq!(asked(&all[1]), ["depression"]);
        assert_eq!(asked(&all[3]), ["anxiety"]);
        let before = all.clone();
        link(&mut all);
        assert_eq!(all, before);
        // Listed no more, and named otherwise.
        all[1].lists.clear();
        all[2].display = "Example form".into();
        link(&mut all);
        assert_eq!(asked(&all[0]), ["depression"]);
        assert_eq!(asked(&all[1]), ["depression"]);
    }
}
