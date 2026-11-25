//! The raw files of records: as they are, for a person at the Mac, and the
//! names of their elements with how many of each, which hold no value.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use pk_cli_core::error::CliError;
use serde::Serialize;
use serde_json::Value;

use crate::folder::Folder;
use crate::import;
use crate::model::{Household, Person, Record};
use crate::people;

/// The elements whose codes are a vocabulary of the format, not a finding:
/// what an amount of a claim is.
const WORDS: [&str; 2] = [
    "adjudication[].category.coding[].code",
    "total[].category.coding[].code",
];

/// A raw file.
#[derive(Debug, Serialize)]
pub struct RawFile {
    pub path: String,
    /// `current`, or what its file's name has after the id, of an earlier one.
    pub version: String,
    pub resource: Value,
}

/// How many values an element has, in how many files.
#[derive(Debug, Default, Serialize, PartialEq, Eq)]
pub struct Element {
    pub path: String,
    /// How often a word of the format is its value.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub word: Option<String>,
    pub files: usize,
    pub n: usize,
}

/// The elements of the files of one type from one CLI.
#[derive(Debug, Serialize)]
pub struct Elements {
    pub cli: String,
    pub resource_type: String,
    pub files: usize,
    pub elements: Vec<Element>,
    pub words: Vec<Element>,
}

/// What `medrec records fields` answers.
#[derive(Debug, Default, Serialize)]
pub struct Fields {
    /// Records looked at.
    pub records: usize,
    /// Of those, the ones no raw file is kept of.
    pub without_raw: usize,
    /// Raw files that are not there or do not read.
    pub unread: usize,
    pub types: Vec<Elements>,
}

/// An element whose values differ between two versions.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Change {
    pub path: String,
    pub added: usize,
    pub removed: usize,
}

/// What differs between one version of a raw file and the next.
#[derive(Debug, Serialize)]
pub struct Step {
    pub from: String,
    pub to: String,
    pub changed: Vec<Change>,
    /// The same values in another order.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub reordered: bool,
}

#[derive(Debug, Serialize)]
pub struct Changes {
    pub cli: String,
    pub resource_type: String,
    pub versions: usize,
    pub steps: Vec<Step>,
}

/// A record by id, with whose it is.
pub fn record(folder: &Folder, h: &Household, id: &str) -> Result<(Person, Record), CliError> {
    for p in &h.people {
        let idx = folder.whole_index(p)?;
        if let Some(r) = idx.records.into_iter().find(|r| r.id == id) {
            return Ok((p.clone(), r));
        }
    }
    Err(CliError::NotFound(format!("no record `{id}`")))
}

/// Its raw files: of each that is kept, the earlier versions, oldest first, and
/// then the current one.
pub fn files(folder: &Folder, r: &Record) -> Result<Vec<RawFile>, CliError> {
    let mut out = Vec::new();
    for (current, _) in currents(r) {
        out.extend(versions(folder, &current)?);
    }
    if out.is_empty() {
        return Err(CliError::NotFound(format!(
            "no raw file is kept of record `{}`",
            r.id
        )));
    }
    Ok(out)
}

/// What each version of its raw files changed.
pub fn changes(folder: &Folder, r: &Record) -> Result<Vec<Changes>, CliError> {
    let mut out = Vec::new();
    for (current, cli) in currents(r) {
        let all = versions(folder, &current)?;
        let Some(last) = all.last() else { continue };
        out.push(Changes {
            cli,
            resource_type: resource_type(&last.resource),
            versions: all.len(),
            steps: all.windows(2).map(|w| step(&w[0], &w[1])).collect(),
        });
    }
    Ok(out)
}

/// The elements of a record's raw files, as they are now.
pub fn fields_of(folder: &Folder, r: &Record) -> Result<Fields, CliError> {
    let mut count = Count::default();
    count.record(r, &mut |rel| folder.store.read_text_fresh(rel))?;
    Ok(count.done())
}

/// The elements of the raw files of a person's (everyone's by default) records
/// of a kind (every kind by default), as they are now.
pub fn fields(
    folder: &Folder,
    h: &Household,
    person: Option<&str>,
    kind: Option<&str>,
) -> Result<Fields, CliError> {
    let tmp = pk_cli_drive::private_temp_dir("medrec-fields")?;
    let mut count = Count::default();
    for p in people::selected(h, person)? {
        let idx = folder.whole_index(p)?;
        let mut of = idx.records.iter();
        let chosen: Vec<&Record> = of
            .by_ref()
            .filter(|r| kind.is_none_or(|k| r.kind == k))
            .collect();
        // One copy per folder of raw files, not one call per file.
        let paths = chosen.iter().flat_map(|r| currents(r));
        let dirs: BTreeSet<String> = paths
            .filter_map(|(rel, _)| rel.rsplit_once('/').map(|(d, _)| d.to_string()))
            .collect();
        for d in &dirs {
            if folder.store.exists(d)? {
                folder.store.fetch_tree(d, &tmp.path().join(d))?;
            }
        }
        for r in chosen {
            count.record(r, &mut |rel| Ok(copy(tmp.path(), rel)))?;
        }
    }
    Ok(count.done())
}

fn copy(tmp: &Path, rel: &str) -> Option<String> {
    std::fs::read_to_string(tmp.join(rel)).ok()
}

/// The current raw files of a record, and the CLI each came by.
fn currents(r: &Record) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for p in &r.provenance {
        let Some(rel) = &p.raw_path else { continue };
        let current = import::current_of(rel).unwrap_or_else(|| rel.clone());
        if out.iter().all(|(had, _)| *had != current) {
            out.push((current, p.source.cli.clone().unwrap_or_default()));
        }
    }
    out
}

/// The versions of a raw file that are kept, the current one last.
fn versions(folder: &Folder, current: &str) -> Result<Vec<RawFile>, CliError> {
    let (dir, name) = current.rsplit_once('/').unwrap_or(("", current));
    let stem = format!("{}~", name.strip_suffix(".json").unwrap_or(name));
    let mut out = Vec::new();
    for f in folder.store.list_here(dir)? {
        let earlier = f
            .rel
            .strip_prefix(&stem)
            .and_then(|v| v.strip_suffix(".json"));
        let version = match earlier {
            Some(v) => v.to_string(),
            None if f.rel == name => CURRENT.to_string(),
            None => continue,
        };
        let path = format!("{dir}/{}", f.rel);
        let text = folder.store.read_text_fresh(&path)?;
        let Some(resource) = text.and_then(|t| serde_json::from_str(&t).ok()) else {
            return Err(CliError::Other(format!("{path} does not read")));
        };
        out.push(RawFile {
            path,
            version,
            resource,
        });
    }
    let updated = |f: &RawFile| {
        let at = f.resource.pointer("/meta/lastUpdated");
        at.and_then(Value::as_str).unwrap_or_default().to_string()
    };
    out.sort_by_key(|f| (f.version == CURRENT, updated(f), f.version.clone()));
    Ok(out)
}

const CURRENT: &str = "current";

fn resource_type(v: &Value) -> String {
    let ty = v.get("resourceType").and_then(Value::as_str);
    ty.map(element).unwrap_or_default()
}

/// A name of the format, or `?`: one that could be something else is not shown.
fn element(name: &str) -> String {
    let plain = name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && name.len() <= 40;
    if plain { name } else { "?" }.to_string()
}

/// Every value of a resource with the path of its element, `item[].net.value`.
fn leaves<'a>(v: &'a Value, path: &str, each: &mut dyn FnMut(&str, &'a Value)) {
    match v {
        Value::Object(m) => {
            for (k, v) in m {
                let dot = if path.is_empty() { "" } else { "." };
                leaves(v, &format!("{path}{dot}{}", element(k)), each);
            }
        }
        Value::Array(l) => {
            let path = format!("{path}[]");
            l.iter().for_each(|v| leaves(v, &path, each));
        }
        v => each(path, v),
    }
}

/// The word of the format that a value of an element is.
fn word(path: &str, v: &Value) -> Option<String> {
    if !WORDS.iter().any(|w| path.ends_with(w)) {
        return None;
    }
    let plain = |w: &&str| {
        w.len() <= 30 && w.chars().all(|c| c.is_ascii_lowercase() || c == '-') && !w.is_empty()
    };
    Some(v.as_str().filter(plain).unwrap_or("?").to_string())
}

fn step(from: &RawFile, to: &RawFile) -> Step {
    let values = |f: &RawFile| {
        let mut out: BTreeMap<(String, String), isize> = BTreeMap::new();
        leaves(&f.resource, "", &mut |path, v| {
            *out.entry((path.to_string(), v.to_string())).or_default() += 1;
        });
        out
    };
    let mut differ = values(to);
    for (k, n) in values(from) {
        *differ.entry(k).or_default() -= n;
    }
    let mut changed: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for ((path, _), n) in differ.into_iter().filter(|(_, n)| *n != 0) {
        let (added, removed) = changed.entry(path).or_default();
        match n > 0 {
            true => *added += n.unsigned_abs(),
            false => *removed += n.unsigned_abs(),
        }
    }
    Step {
        from: from.version.clone(),
        to: to.version.clone(),
        reordered: changed.is_empty() && from.resource != to.resource,
        changed: changed
            .into_iter()
            .map(|(path, (added, removed))| Change {
                path,
                added,
                removed,
            })
            .collect(),
    }
}

/// How many of each, by path and word, in how many files.
type Tally = BTreeMap<(String, Option<String>), (usize, usize)>;

#[derive(Default)]
struct Count {
    records: usize,
    without_raw: usize,
    unread: usize,
    seen: BTreeSet<String>,
    types: BTreeMap<(String, String), (usize, Tally)>,
}

impl Count {
    fn record(
        &mut self,
        r: &Record,
        read: &mut dyn FnMut(&str) -> Result<Option<String>, CliError>,
    ) -> Result<(), CliError> {
        self.records += 1;
        let of = currents(r);
        self.without_raw += usize::from(of.is_empty());
        for (rel, cli) in of {
            // A file can be of several records.
            if !self.seen.insert(rel.clone()) {
                continue;
            }
            let text = read(&rel)?;
            let v: Option<Value> = text.and_then(|t| serde_json::from_str(&t).ok());
            match v.filter(Value::is_object) {
                Some(v) => self.file(cli, &v),
                None => self.unread += 1,
            }
        }
        Ok(())
    }

    fn file(&mut self, cli: String, v: &Value) {
        let mut own: BTreeMap<(String, Option<String>), usize> = BTreeMap::new();
        leaves(v, "", &mut |path, v| {
            *own.entry((path.to_string(), None)).or_default() += 1;
            if let Some(w) = word(path, v) {
                *own.entry((path.to_string(), Some(w))).or_default() += 1;
            }
        });
        let (files, tally) = self.types.entry((cli, resource_type(v))).or_default();
        *files += 1;
        for (k, n) in own {
            let of = tally.entry(k).or_default();
            of.0 += 1;
            of.1 += n;
        }
    }

    fn done(self) -> Fields {
        let types = self.types.into_iter();
        Fields {
            records: self.records,
            without_raw: self.without_raw,
            unread: self.unread,
            types: types
                .map(|((cli, resource_type), (files, tally))| {
                    let all = tally.into_iter().map(|((path, word), (files, n))| Element {
                        path,
                        word,
                        files,
                        n,
                    });
                    let (words, elements) = all.partition(|e| e.word.is_some());
                    Elements {
                        cli,
                        resource_type,
                        files,
                        elements,
                        words,
                    }
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn file(version: &str, resource: Value) -> RawFile {
        RawFile {
            path: String::new(),
            version: version.into(),
            resource,
        }
    }

    #[test]
    fn elements_have_no_value() {
        let mut c = Count::default();
        let line = |code: &str, value: f64| json!({"adjudication": [{"category": {"coding": [{"code": code}]}, "amount": {"value": value}}]});
        c.file(
            "example".into(),
            &json!({
                "resourceType": "ExplanationOfBenefit", "id": "SECRET",
                "item": [line("submitted", 12.5), line("not-counted", 1.0), line("SECRET 1", 2.0)],
                "total": [{"category": {"coding": [{"code": "submitted"}]}}],
                "contained": [{"SECRET key": "SECRET"}],
                "note": [],
            }),
        );
        c.file(
            "example".into(),
            &json!({"resourceType": "ExplanationOfBenefit", "item": [line("submitted", 3.0)]}),
        );
        let f = c.done();
        assert_eq!(f.types.len(), 1);
        let t = &f.types[0];
        assert_eq!(
            (t.resource_type.as_str(), t.files),
            ("ExplanationOfBenefit", 2)
        );
        let n = |list: &[Element], path: &str, word: Option<&str>| {
            let mut of = list
                .iter()
                .filter(|e| e.path == path && e.word.as_deref() == word);
            of.next().map(|e| (e.files, e.n))
        };
        let code = "item[].adjudication[].category.coding[].code";
        assert_eq!(n(&t.elements, code, None), Some((2, 4)));
        assert_eq!(n(&t.elements, "contained[].?", None), Some((1, 1)));
        assert_eq!(n(&t.words, code, Some("submitted")), Some((2, 2)));
        assert_eq!(n(&t.words, code, Some("not-counted")), Some((1, 1)));
        assert_eq!(n(&t.words, code, Some("?")), Some((1, 1)));
        let total = "total[].category.coding[].code";
        assert_eq!(n(&t.words, total, Some("submitted")), Some((1, 1)));
        let text = serde_json::to_string(&f).unwrap();
        assert!(!text.contains("SECRET") && !text.contains("12.5"), "{text}");
    }

    #[test]
    fn a_change_names_elements_alone() {
        let a = file(
            "0011aabb",
            json!({"meta": {"versionId": "1"}, "item": [{"n": 1}, {"n": 2}], "status": "active"}),
        );
        let b = file(
            "current",
            json!({"meta": {"versionId": "2"}, "item": [{"n": 2}, {"n": 3}, {"n": 4}], "status": "active"}),
        );
        let s = step(&a, &b);
        let of = |path: &str, added, removed| Change {
            path: path.into(),
            added,
            removed,
        };
        assert_eq!(
            s.changed,
            [of("item[].n", 2, 1), of("meta.versionId", 1, 1)]
        );
        assert!(!s.reordered);
        assert_eq!((s.from.as_str(), s.to.as_str()), ("0011aabb", "current"));
        let c = file("x", json!({"item": [{"n": 1}, {"n": 2}]}));
        let d = file("y", json!({"item": [{"n": 2}, {"n": 1}]}));
        let s = step(&c, &d);
        assert!(s.reordered && s.changed.is_empty());
        assert!(!step(&c, &c).reordered);
    }
}
