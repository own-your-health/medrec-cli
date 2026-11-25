//! The raw files of records: as they are, for a person at the Mac, and the
//! names of their elements with how many of each, which hold no value.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use pk_cli_core::error::CliError;
use pk_med_records::Timestamp;
use serde::Serialize;
use serde_json::Value;

use crate::folder::Folder;
use crate::import;
use crate::model::{Household, Person, Record};
use crate::normalize;
use crate::people;

/// The elements whose codes are words of the format and not a finding: what an
/// amount of a claim is.
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

/// What the versions kept of a raw file changed.
#[derive(Debug, Serialize)]
pub struct Changes {
    pub cli: String,
    pub resource_type: String,
    /// Versions that read.
    pub versions: usize,
    /// Versions that do not, and the current one where it is not there.
    pub unread: usize,
    /// Whether the order of the earlier ones is known, by when their source
    /// says each was made: each is then compared with the one after it. Where
    /// it is not, each is compared with the current one.
    pub ordered: bool,
    pub steps: Vec<Step>,
}

/// The versions kept of a raw file.
#[derive(Default)]
struct Versions {
    /// Oldest first where that is known, and the current one last.
    files: Vec<RawFile>,
    /// The paths of those that do not read, or are not there.
    unread: Vec<String>,
    ordered: bool,
}

/// A record by id, with whose it is.
pub fn record(folder: &Folder, h: &Household, id: &str) -> Result<(Person, Record), CliError> {
    for p in &h.people {
        let idx = folder.whole_index(p).map_err(unnamed)?;
        if let Some(r) = idx.records.into_iter().find(|r| r.id == id) {
            return Ok((p.clone(), r));
        }
    }
    Err(CliError::NotFound(format!("no record `{id}`")))
}

/// Its raw files: of each, the earlier versions, oldest first where that is
/// known, and then the current one.
pub fn files(folder: &Folder, r: &Record) -> Result<Vec<RawFile>, CliError> {
    let mut out = Vec::new();
    for (current, _) in kept(r)? {
        let of = versions(folder, &current)?;
        if let Some(path) = of.unread.first() {
            return Err(CliError::Other(format!(
                "{path} is not there or does not read"
            )));
        }
        out.extend(of.files);
    }
    Ok(out)
}

/// What each version of its raw files changed. Neither it nor an error of it
/// names a file, whose path has a person's name and a source's id.
pub fn changes(folder: &Folder, r: &Record) -> Result<Vec<Changes>, CliError> {
    let mut out = Vec::new();
    for (current, cli) in kept(r)? {
        let of = versions(folder, &current).map_err(unnamed)?;
        let all = &of.files;
        let is_now = |f: &&RawFile| f.version == CURRENT;
        let steps = match (of.ordered, all.iter().find(is_now)) {
            (true, _) => all.windows(2).map(|w| step(&w[0], &w[1])).collect(),
            (false, Some(now)) => {
                let earlier = all.iter().filter(|f| !is_now(f));
                earlier.map(|f| step(f, now)).collect()
            }
            (false, None) => Vec::new(),
        };
        out.push(Changes {
            cli,
            resource_type: all
                .last()
                .map_or_else(String::new, |f| resource_type(&f.resource)),
            versions: all.len(),
            unread: of.unread.len(),
            ordered: of.ordered,
            steps,
        });
    }
    Ok(out)
}

/// The raw files kept of a record, which has one.
fn kept(r: &Record) -> Result<Vec<(String, String)>, CliError> {
    let all = currents(r);
    if all.is_empty() {
        return Err(CliError::NotFound(format!(
            "no raw file is kept of record `{}`",
            r.id
        )));
    }
    Ok(all)
}

/// An error of reading, with the path it can name left out.
fn unnamed(e: CliError) -> CliError {
    const SAYS: &str = "the raw files could not be read; `medrec verify` names what is wrong";
    let says = SAYS.to_string();
    match e.base() {
        CliError::Usage(_) => CliError::Usage(says),
        CliError::Auth(_) => CliError::Auth(
            "the raw files could not be read; sign in to the records folder again \
             (`medrec drive login`)"
                .into(),
        ),
        CliError::NotFound(_) => CliError::NotFound(says),
        CliError::Upstream(_) => CliError::Upstream(says),
        CliError::Keychain(_) => CliError::Keychain(says),
        _ => CliError::Other(says),
    }
}

/// The elements of a record's current raw files.
pub fn fields_of(folder: &Folder, r: &Record) -> Result<Fields, CliError> {
    kept(r)?;
    let mut count = Count::default();
    let mut read = |rel: &str| folder.store.read_text_fresh(rel).map_err(unnamed);
    count.record(r, &mut read)?;
    Ok(count.done())
}

/// The elements of the current raw files of a person's (everyone's by default)
/// records of these kinds (every kind by default), but of what is about
/// somebody else.
pub fn fields(
    folder: &Folder,
    h: &Household,
    person: Option<&str>,
    kinds: &[String],
) -> Result<Fields, CliError> {
    let tmp = pk_cli_drive::private_temp_dir("medrec-fields")?;
    let mut count = Count::default();
    for p in people::selected(h, person)? {
        let idx = folder.index(p).map_err(unnamed)?;
        let of = |r: &&Record| kinds.is_empty() || kinds.contains(&r.kind);
        let chosen: Vec<&Record> = idx.records.iter().filter(of).collect();
        // One copy per folder of raw files, not one call per file.
        let paths = chosen.iter().flat_map(|r| currents(r));
        let dirs: BTreeSet<String> = paths
            .filter_map(|(rel, _)| rel.rsplit_once('/').map(|(d, _)| d.to_string()))
            .collect();
        for d in &dirs {
            if folder.store.exists(d).map_err(unnamed)? {
                let to = tmp.path().join(d);
                folder.store.fetch_tree(d, &to).map_err(unnamed)?;
            }
        }
        for r in chosen {
            count.record(r, &mut |rel| Ok(copy(tmp.path(), rel)))?;
        }
    }
    Ok(count.done())
}

/// The kinds of record.
pub fn kinds() -> impl Iterator<Item = &'static str> {
    normalize::FIELDS.iter().map(|(kind, _)| *kind)
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

/// The versions of a raw file that are kept.
fn versions(folder: &Folder, current: &str) -> Result<Versions, CliError> {
    let (dir, name) = current.rsplit_once('/').unwrap_or(("", current));
    let stem = format!("{}~", name.strip_suffix(".json").unwrap_or(name));
    let mut out = Versions::default();
    let mut there = false;
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
        there |= version == CURRENT;
        let path = format!("{dir}/{}", f.rel);
        let text = folder.store.read_text_fresh(&path)?;
        let resource = text.and_then(|t| serde_json::from_str::<Value>(&t).ok());
        match resource.filter(Value::is_object) {
            Some(resource) => out.files.push(RawFile {
                path,
                version,
                resource,
            }),
            None => out.unread.push(path),
        }
    }
    if !there {
        out.unread.push(current.to_string());
    }
    out.ordered = in_order(&mut out.files) && out.unread.is_empty();
    Ok(out)
}

/// Put versions in their order, oldest first and the current one last. Whether
/// it is known: where an earlier one does not say when it was made, or two say
/// the same, theirs is not.
fn in_order(files: &mut [RawFile]) -> bool {
    let made = |f: &RawFile| {
        let at = f.resource.pointer("/meta/lastUpdated");
        at.and_then(Value::as_str).and_then(Timestamp::parse)
    };
    files.sort_by_cached_key(|f| (f.version == CURRENT, made(f), f.version.clone()));
    let earlier = files.iter().filter(|f| f.version != CURRENT).map(made);
    let earlier: Vec<Option<Timestamp>> = earlier.collect();
    earlier.len() < 2
        || (earlier.iter().all(Option::is_some) && earlier.windows(2).all(|w| w[0] != w[1]))
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

/// The names of the elements that lead to a value, outermost first; of a value
/// in a list, `[]` after the list's.
type Names = Vec<String>;

/// Every value of a resource with its path, a list or an object that is empty
/// too.
fn leaves<'a>(v: &'a Value, path: &mut Names, each: &mut dyn FnMut(&Names, &'a Value)) {
    match v {
        Value::Object(m) if !m.is_empty() => {
            for (k, v) in m {
                path.push(k.clone());
                leaves(v, path, each);
                path.pop();
            }
        }
        Value::Array(l) if !l.is_empty() => {
            path.push(LIST.into());
            l.iter().for_each(|v| leaves(v, path, each));
            path.pop();
        }
        v => each(path, v),
    }
}

const LIST: &str = "[]";

/// A path as it is shown, `item[].net.value`.
fn shown(path: &Names) -> String {
    let mut out = String::new();
    for name in path {
        match name.as_str() {
            LIST => out.push_str(LIST),
            name => {
                let dot = if out.is_empty() { "" } else { "." };
                out.push_str(&format!("{dot}{}", element(name)));
            }
        }
    }
    out
}

/// The word of the format that a value of an element is.
fn word(path: &str, v: &Value) -> Option<String> {
    let is = |w: &&str| path == *w || path.ends_with(&format!(".{w}"));
    if !WORDS.iter().any(is) {
        return None;
    }
    let plain = |w: &&str| {
        w.len() <= 30 && w.chars().all(|c| c.is_ascii_lowercase() || c == '-') && !w.is_empty()
    };
    Some(v.as_str().filter(plain).unwrap_or("?").to_string())
}

fn step(from: &RawFile, to: &RawFile) -> Step {
    let values = |f: &RawFile| {
        let mut out: BTreeMap<(Names, String), isize> = BTreeMap::new();
        leaves(&f.resource, &mut Names::new(), &mut |path, v| {
            *out.entry((path.clone(), v.to_string())).or_default() += 1;
        });
        out
    };
    let mut differ = values(to);
    for (k, n) in values(from) {
        *differ.entry(k).or_default() -= n;
    }
    let mut changed: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for ((path, _), n) in differ.into_iter().filter(|(_, n)| *n != 0) {
        let (added, removed) = changed.entry(shown(&path)).or_default();
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
        leaves(v, &mut Names::new(), &mut |path, v| {
            if v.is_object() || v.is_array() {
                return;
            }
            let path = shown(path);
            if let Some(w) = word(&path, v) {
                *own.entry((path.clone(), Some(w))).or_default() += 1;
            }
            *own.entry((path, None)).or_default() += 1;
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
                "subtotal": [{"category": {"coding": [{"code": "secret-word"}]}}],
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
        for value in ["SECRET", "secret", "12.5"] {
            assert!(!text.contains(value), "{value}: {text}");
        }
        assert_eq!(n(&t.elements, "note", None), None);
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
        // What is empty is something, and so is a name that is not shown.
        let none = file("x", json!({}));
        let empty = file("y", json!({"note": [], "item": {}}));
        let s = step(&none, &empty);
        assert!(!s.reordered);
        assert_eq!(
            s.changed,
            [of("", 0, 1), of("item", 1, 0), of("note", 1, 0)]
        );
        let s = step(
            &file("x", json!({"a b": 1, "c d": 2})),
            &file("y", json!({"a b": 2, "c d": 1})),
        );
        assert!(!s.reordered);
        assert_eq!(s.changed, [of("?", 2, 2)]);
    }

    /// Versions are in the order their source says they were made in, and where
    /// it says none they are in none.
    #[test]
    fn versions_are_in_order_where_one_is_known() {
        let made = |version: &str, at: Option<&str>| {
            let meta = at.map_or(json!({}), |at| json!({ "lastUpdated": at }));
            file(version, json!({ "meta": meta }))
        };
        let order = |all: &[(&str, Option<&str>)]| {
            let mut files: Vec<RawFile> = all.iter().map(|(v, at)| made(v, *at)).collect();
            let known = in_order(&mut files);
            let versions = files.iter().map(|f| f.version.clone());
            (versions.collect::<Vec<_>>().join(" "), known)
        };
        // Of one instant each, whatever their offsets and their names.
        let early = Some("2024-03-01T23:30:00-05:00");
        let late = Some("2024-03-02T05:00:00.5Z");
        let later = Some("2024-03-02T05:00:01Z");
        for (all, want) in [
            (
                vec![("b", late), (CURRENT, later), ("c", early)],
                ("c b current", true),
            ),
            // The current one is last, also where it says it is the oldest.
            (
                vec![(CURRENT, early), ("b", later), ("a", late)],
                ("a b current", true),
            ),
            (vec![("a", None), (CURRENT, later)], ("a current", true)),
            (
                vec![("a", late), ("b", None), (CURRENT, later)],
                ("b a current", false),
            ),
            (
                vec![("b", late), ("a", late), (CURRENT, later)],
                ("a b current", false),
            ),
        ] {
            assert_eq!(order(&all), (want.0.to_string(), want.1), "{all:?}");
        }
    }

    /// A file of two records is counted once.
    #[test]
    fn a_file_is_counted_once() {
        let record = |id: &str| -> Record {
            serde_json::from_value(json!({"id": id, "kind": "claim", "key": id, "display": id,
                "provenance": [{"source": {"kind": "fhir", "cli": "example"},
                    "method": "fhir-api", "filed_at": "2024-01-02T10:00:00Z",
                    "filed_by": "medrec 0.9.9", "raw_path": "a/Claim/one.json"}]}))
            .unwrap()
        };
        let mut c = Count::default();
        let mut read = |_: &str| {
            Ok(Some(
                r#"{"resourceType": "Claim", "id": "one"}"#.to_string(),
            ))
        };
        for id in ["r_one", "r_two"] {
            c.record(&record(id), &mut read).unwrap();
        }
        let f = c.done();
        assert_eq!((f.records, f.unread, f.types[0].files), (2, 0, 1));
    }

    /// An error names no file, and is of the kind it was.
    #[test]
    fn an_error_of_a_count_names_no_file() {
        for e in [
            CliError::Other("SECRET".into()),
            CliError::Auth("SECRET".into()),
            CliError::Upstream("SECRET".into()).with_remedy("SECRET"),
            CliError::NotFound("SECRET".into()),
        ] {
            let exit = e.exit_code();
            let now = unnamed(e);
            assert_eq!(now.exit_code(), exit);
            assert!(!now.to_json().to_string().contains("SECRET"));
        }
    }
}
