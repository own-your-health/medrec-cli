//! The views: each person's records cut into small files under `views/`,
//! for a reader that cannot parse a whole index for every request (the
//! hosted connector, which has 10 ms of CPU per request). They are derived
//! from `household.json` and each person's `index.json` and `text.json`.
//! They are written after every command that writes those files, and only
//! files whose bytes change are uploaded (docs/data-contract.md, "Views").
//!
//! Shards and search items are numbered from the oldest end. New records
//! are nearly always the newest, so a pass that adds a few changes the
//! newest shard and the search shards of their words, and little else.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use pk_cli_core::CliError;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::folder::{self, Access, Folder};
use crate::hash::{hex, md5_hex};
use crate::intake::classify::Squashed;
use crate::model::{Household, Person, PersonIndex, PersonText, Record};
use crate::store::Location;
use crate::summary::{self, clip, day, CLIP};
use crate::views::{self, Range};

/// Where the views are, at the top of the records folder.
pub const DIR: &str = "views";
/// What was written, and from which files; written last.
pub const INDEX: &str = "views/views.json";
pub const SCHEMA: &str = "medrec-views/v2";
/// The layout before content-addressed names, written only by the
/// connector's own `npm run views`: its plain-named files are removed.
const SCHEMA_V1: &str = "medrec-views/v1";
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The views every person has. A person whose `views.json` entry lacks
/// one was written by an earlier build of the same version, before that
/// view existed, and gets their views made again.
const EVERY_PERSON: &[&str] = &[
    "summary.json",
    "tests.json",
    "documents.json",
    "events.json",
    "bills.json",
    "private.json",
];

/// A records or timeline shard closes once it holds this many bytes, never
/// inside a day.
const SHARD_BYTES: usize = 48_000;
/// A text pack closes before a text would take it past this; a text is
/// never split.
const PACK_BYTES: usize = 1_000_000;
/// Rows per `search-labs-<n>.json`.
const LAB_ROWS: usize = 256;
/// Items per `search-hay-<n>.bin`.
const HAY_ITEMS: usize = 512;
/// The shard number `names-<kind>.json` and `tests.json` give the undated
/// file.
const UNDATED: i64 = -1;
/// The day the summary is computed for: before every due date, so no bill
/// is past due. Readers work out `past_due` for the day they answer.
const NO_DAY: &str = "0000-00-00";

struct Loaded {
    index_md5: Option<String>,
    text_md5: Option<String>,
    idx: PersonIndex,
    texts: PersonText,
}

/// `views/views.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ViewsIndex {
    pub schema: String,
    /// The medrec that wrote them.
    pub medrec: String,
    /// The MD5 (hex) of `household.json` as the views were made from it.
    pub household: String,
    pub people: BTreeMap<String, PersonViews>,
}

/// One person's entry in `views/views.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PersonViews {
    /// The MD5s (hex) of `index.json` and `text.json` as the views were
    /// made from them; null when the file is absent.
    pub index: Option<String>,
    pub text: Option<String>,
    /// The MD5 of the person's `overrides.json`, null when there is none;
    /// and those of the index and text of each guardian or child whose
    /// records their events read, by person id. Both make only
    /// `events.json`, `bills.json` and `private.json` stale.
    #[serde(default)]
    pub overrides: Option<String>,
    /// The MD5 of the person's `marks.json`, null when there is none
    /// (0.9.1 and later); it makes only `bills.json` and `private.json`
    /// stale.
    #[serde(default)]
    pub marks: Option<String>,
    #[serde(default)]
    pub linked: BTreeMap<String, LinkedFiles>,
    pub row: Row,
    pub records: BTreeMap<String, Run>,
    pub timeline: Run,
    pub search: SearchCounts,
    /// Every file in `views/<person id>/`, by name, with its MD5 (hex): a
    /// reader checks each view it reads against it, so it never mixes two
    /// passes whose files arrived in another order.
    pub files: BTreeMap<String, String>,
}

/// The person's line in `medrec_summary`'s people list, without `notes`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Row {
    pub documents: usize,
    pub records: usize,
    pub studies: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub newest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_import: Option<String>,
}

/// A list's dated shards by number, 0 the oldest, each as its newest day,
/// oldest day and count; and how many items its undated file holds.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Run {
    pub shards: Vec<(String, String, usize)>,
    pub undated: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SearchCounts {
    pub items: usize,
    /// How many `search-labs-<n>.json`.
    pub labs: usize,
}

/// What bringing the views up to date did.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Synced {
    pub people: usize,
    /// People whose views were made again; the rest had no file change.
    pub people_rebuilt: usize,
    /// View files uploaded: new, or under a name the folder lacked.
    pub files_written: usize,
    /// View files made again that the folder already held.
    pub files_unchanged: usize,
    /// View files no view names any more, deleted.
    pub files_removed: usize,
}

/// More view files than this to write, and the command says so first: the
/// first build after an upgrade writes every view, under the folder lock.
const NOTE_FILES: usize = 50;
static QUIET: AtomicBool = AtomicBool::new(false);

/// No notes on stderr from here on (`--quiet`, `--json`).
pub fn quiet() {
    QUIET.store(true, Ordering::SeqCst);
}

/// A view's name in the folder: its name with the first 12 hex of its MD5
/// before the extension (`records-result-7.0a1b2c3d4e5f.json`). A file's
/// bytes never change under its name, so a reader holding an older
/// `views.json` reads that pass's files or finds them gone, never a newer
/// pass's bytes.
pub fn stored_name(name: &str, md5: &str) -> String {
    let tag = &md5[..12.min(md5.len())];
    match name.rsplit_once('.') {
        Some((stem, ext)) => format!("{stem}.{tag}.{ext}"),
        None => format!("{name}.{tag}"),
    }
}

fn stored_rel(pid: &str, name: &str, md5: &str) -> String {
    format!("{DIR}/{pid}/{}", stored_name(name, md5))
}

/// Whether a file name has a stored view's shape, `<stem>.<12 hex>.<json or
/// bin>`: the only files `sync` deletes.
fn is_stored_name(name: &str) -> bool {
    let Some((rest, ext)) = name.rsplit_once('.') else {
        return false;
    };
    let Some((stem, tag)) = rest.rsplit_once('.') else {
        return false;
    };
    matches!(ext, "json" | "bin")
        && !stem.is_empty()
        && tag.len() == 12
        && tag
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Bring `views/` up to date with the folder's files. A person whose
/// `index.json` and `text.json` have the MD5s `views.json` recorded, under
/// the same `household.json` and medrec, keeps their views; `all` makes
/// every person's again. Only views the folder lacks are uploaded, then
/// `views.json`, then the views it no longer names are deleted. Nothing is
/// deleted but files of a stored view's shape in a person's directory.
pub fn sync(f: &Folder, all: bool) -> Result<Synced, CliError> {
    let store = &f.store;
    let household_text = store.read_text(folder::HOUSEHOLD)?.ok_or_else(|| {
        CliError::Other(format!(
            "{} is missing",
            store.display_path(folder::HOUSEHOLD)
        ))
    })?;
    let h: Household = parse(&household_text, folder::HOUSEHOLD)?;
    if let Some(p) = h.people.iter().find(|p| p.folder.eq_ignore_ascii_case(DIR)) {
        return Err(CliError::Other(format!(
            "the person folder `{}` is where medrec writes the views, so this folder has none",
            crate::printable(&p.folder)
        )));
    }
    let household = md5_hex(household_text.as_bytes());
    let old_text = store.read_text(INDEX)?;
    let old: Option<ViewsIndex> = old_text
        .as_deref()
        .and_then(|t| serde_json::from_str(t).ok());
    let current = old
        .as_ref()
        .filter(|o| !all && o.schema == SCHEMA && o.medrec == VERSION && o.household == household);
    let fresh = |people: BTreeMap<String, PersonViews>| ViewsIndex {
        schema: SCHEMA.into(),
        medrec: VERSION.into(),
        household: household.clone(),
        people,
    };

    // Whose views stand: their files have the MD5s the views were made from.
    let mut kept: BTreeMap<String, PersonViews> = BTreeMap::new();
    let linked_md5s = |p: &Person| -> Result<BTreeMap<String, LinkedFiles>, CliError> {
        let mut m = BTreeMap::new();
        for q in crate::events::linked_people(&h, p) {
            m.insert(
                q.id.clone(),
                LinkedFiles {
                    index: store.md5(&folder::index_rel(q))?,
                    text: store.md5(&folder::text_rel(q))?,
                },
            );
        }
        Ok(m)
    };
    for p in &h.people {
        if let Some(was) = current.and_then(|o| o.people.get(&p.id)) {
            if store.md5(&folder::index_rel(p))? == was.index
                && store.md5(&folder::text_rel(p))? == was.text
                && store.md5(&crate::events::overrides_rel(p))? == was.overrides
                && store.md5(&crate::marks::rel(p))? == was.marks
                && linked_md5s(p)? == was.linked
                && EVERY_PERSON.iter().all(|f| was.files.contains_key(*f))
            {
                kept.insert(p.id.clone(), was.clone());
            }
        }
    }
    let n = h.people.len();
    if kept.len() == n
        && old_text.as_deref().map(str::as_bytes) == Some(json(&fresh(kept.clone())).as_slice())
    {
        return Ok(Synced {
            people: n,
            ..Synced::default()
        });
    }
    let held: BTreeMap<String, Option<String>> = store
        .hashes(DIR)?
        .into_iter()
        .map(|x| (x.rel, x.md5))
        .collect();
    // A person missing one of their views has them made again.
    kept.retain(|pid, v| {
        v.files
            .iter()
            .all(|(name, md5)| held.contains_key(&stored_rel(pid, name, md5)))
    });

    let mut people = BTreeMap::new();
    let mut files = BTreeMap::new();
    // Each person's files read once, whoever's events need them.
    let mut loaded: BTreeMap<String, Loaded> = BTreeMap::new();
    fn load(
        store: &crate::store::Store,
        loaded: &mut BTreeMap<String, Loaded>,
        p: &Person,
    ) -> Result<(), CliError> {
        if loaded.contains_key(&p.id) {
            return Ok(());
        }
        let (irel, trel) = (folder::index_rel(p), folder::text_rel(p));
        let itext = store.read_text(&irel)?;
        let ttext = store.read_text(&trel)?;
        let idx: PersonIndex = match &itext {
            Some(t) => parse(t, &irel)?,
            None => PersonIndex::new(&p.id),
        };
        if idx.person != p.id {
            return Err(CliError::Other(format!("{irel} names another person")));
        }
        let texts: PersonText = match &ttext {
            Some(t) => parse(t, &trel)?,
            None => PersonText::new(&p.id),
        };
        let md5 = |t: &Option<String>| t.as_deref().map(|t| md5_hex(t.as_bytes()));
        loaded.insert(
            p.id.clone(),
            Loaded {
                index_md5: md5(&itext),
                text_md5: md5(&ttext),
                idx,
                texts,
            },
        );
        Ok(())
    }
    for p in &h.people {
        if let Some(v) = kept.get(&p.id) {
            people.insert(p.id.clone(), v.clone());
            continue;
        }
        let linked = crate::events::linked_people(&h, p);
        load(store, &mut loaded, p)?;
        for q in &linked {
            load(store, &mut loaded, q)?;
        }
        let orel = crate::events::overrides_rel(p);
        let otext = store.read_text(&orel)?;
        // A decisions file that does not read is left out of the events,
        // not allowed to stop the person's views: the events say so.
        let overrides: Option<Result<crate::events::Overrides, ()>> = otext.as_deref().map(|t| {
            parse::<crate::events::Overrides>(t, &orel)
                .ok()
                .filter(|o| o.person == p.id && o.schema == crate::events::OVERRIDES_SCHEMA)
                .ok_or(())
        });
        let me = &loaded[&p.id];
        let mut events = crate::events::build(&crate::events::Input {
            person: p,
            index: &me.idx,
            texts: &me.texts,
            sources: &h.sources,
            overrides: overrides.as_ref().and_then(|o| o.as_ref().ok()),
            linked: linked
                .iter()
                .map(|q| crate::events::Linked {
                    person: q,
                    index: &loaded[&q.id].idx,
                    texts: Some(&loaded[&q.id].texts),
                })
                .collect(),
            utc_offset: pk_cli_core::dates::local_utc_offset,
        });
        if matches!(overrides, Some(Err(()))) {
            events["overrides_unreadable"] = Value::Bool(true);
        }
        let mrel = crate::marks::rel(p);
        let mtext = store.read_text(&mrel)?;
        // So is a marks file that does not read: the bills say so.
        let marks: Option<Result<crate::marks::Marks, ()>> = mtext.as_deref().map(|t| {
            parse::<crate::marks::Marks>(t, &mrel)
                .ok()
                .filter(|m| m.person == p.id && m.schema == crate::marks::SCHEMA)
                .ok_or(())
        });
        let payments = marks
            .as_ref()
            .and_then(|m| m.as_ref().ok())
            .map(|m| m.payments.as_slice())
            .unwrap_or_default();
        let mut bills = crate::bills::build(p, &me.idx, &events, payments);
        let private_marks = marks
            .as_ref()
            .and_then(|m| m.as_ref().ok())
            .map(|m| m.private.as_slice())
            .unwrap_or_default();
        let mut private = crate::private::build(p, &me.idx, &events, private_marks);
        if matches!(marks, Some(Err(()))) {
            bills["marks_unreadable"] = Value::Bool(true);
            private["marks_unreadable"] = Value::Bool(true);
        }
        let from = EventsFrom {
            bills,
            private,
            events,
            overrides: otext.as_deref().map(|t| md5_hex(t.as_bytes())),
            marks: mtext.as_deref().map(|t| md5_hex(t.as_bytes())),
            linked: linked
                .iter()
                .map(|q| {
                    let l = &loaded[&q.id];
                    (
                        q.id.clone(),
                        LinkedFiles {
                            index: l.index_md5.clone(),
                            text: l.text_md5.clone(),
                        },
                    )
                })
                .collect(),
        };
        let v = person(
            &h,
            p,
            &me.idx,
            &me.texts,
            me.index_md5.clone(),
            me.text_md5.clone(),
            from,
            &mut files,
        )?;
        people.insert(p.id.clone(), v);
    }
    let index = fresh(people);

    let mut out = Synced {
        people: n,
        people_rebuilt: n - kept.len(),
        ..Synced::default()
    };
    let mut uploads = Vec::new();
    for (rel, bytes) in &files {
        let (dir, name) = rel
            .rsplit_once('/')
            .expect("a view is in a person's directory");
        let md5 = md5_hex(bytes);
        let stored = format!("{dir}/{}", stored_name(name, &md5));
        if held.get(&stored).and_then(|m| m.as_deref()) == Some(md5.as_str()) {
            out.files_unchanged += 1;
        } else {
            uploads.push((stored, bytes));
        }
    }
    if uploads.len() > NOTE_FILES && !QUIET.load(Ordering::SeqCst) {
        eprintln!(
            "note: writing {} view files to {}; the first time after an upgrade takes a while",
            uploads.len(),
            store.display_path(DIR)
        );
    }
    out.files_written = upload(store, &uploads)?;
    let index_bytes = json(&index);
    if old_text.as_deref().map(str::as_bytes) != Some(index_bytes.as_slice()) {
        store.write_bytes(INDEX, &index_bytes)?;
    }

    // The views no pass names any more, after `views.json`: in the
    // directory of someone the household or the last views.json names, and
    // of a stored view's shape. Nothing else under views/ is touched.
    let wanted: BTreeSet<String> = index
        .people
        .iter()
        .flat_map(|(pid, v)| {
            v.files
                .iter()
                .map(move |(name, md5)| stored_rel(pid, name, md5))
        })
        .collect();
    let known: BTreeSet<&str> = h
        .people
        .iter()
        .map(|p| p.id.as_str())
        .chain(old.iter().flat_map(|o| o.people.keys().map(String::as_str)))
        .collect();
    let prefix = format!("{DIR}/");
    for rel in held.keys() {
        if wanted.contains(rel) {
            continue;
        }
        let Some((pid, name)) = rel.strip_prefix(&prefix).and_then(|r| r.split_once('/')) else {
            continue;
        };
        if name.contains('/') || !is_stored_name(name) || !known.contains(pid) {
            continue;
        }
        if store.delete(rel)? {
            out.files_removed += 1;
        }
    }
    // A v1 `views.json` named its views plainly; each is removed by the
    // name it gave.
    for (pid, v) in old
        .iter()
        .filter(|o| o.schema == SCHEMA_V1)
        .flat_map(|o| &o.people)
    {
        for name in v.files.keys() {
            let rel = format!("{DIR}/{pid}/{name}");
            if name.contains('/') || wanted.contains(&rel) || !held.contains_key(&rel) {
                continue;
            }
            if store.delete(&rel)? {
                out.files_removed += 1;
            }
        }
    }
    Ok(out)
}

/// Upload every view, stopping at the first error; the caller writes
/// `views.json` only once all are there. On Drive, pk-cli-drive sends a few
/// at a time within its own limit. How many were sent.
fn upload(store: &crate::store::Store, files: &[(String, &Vec<u8>)]) -> Result<usize, CliError> {
    let batch: Vec<(&str, &[u8])> = files
        .iter()
        .map(|(rel, bytes)| (rel.as_str(), bytes.as_slice()))
        .collect();
    store.write_many(&batch)
}

fn parse<T: serde::de::DeserializeOwned>(text: &str, rel: &str) -> Result<T, CliError> {
    serde_json::from_str(text).map_err(|e| CliError::Other(format!("{rel} is not readable: {e}")))
}

/// Compact JSON. Every value here is plain data with string keys.
fn json<T: Serialize + ?Sized>(v: &T) -> Vec<u8> {
    serde_json::to_vec(v).expect("views are plain JSON")
}

/// A JSON array of values already serialized.
fn array<'a>(items: impl IntoIterator<Item = &'a [u8]>) -> Vec<u8> {
    let mut out = vec![b'['];
    for (i, item) in items.into_iter().enumerate() {
        if i > 0 {
            out.push(b',');
        }
        out.extend_from_slice(item);
    }
    out.push(b']');
    out
}

/// A list in newest-first order with its undated items last, cut into
/// shards from its oldest end: shard 0 holds the oldest days, and only the
/// newest shard grows as newer items come. Each shard keeps the list's
/// order, newest first, and closes once it holds [`SHARD_BYTES`], never
/// inside a day. Returns each shard's positions in the list, and the
/// undated ones.
fn cut_from_oldest(days: &[Option<String>], sizes: &[usize]) -> (Vec<Vec<usize>>, Vec<usize>) {
    let dated = days.iter().position(Option::is_none).unwrap_or(days.len());
    let mut groups: Vec<std::ops::Range<usize>> = Vec::new();
    let mut i = 0;
    while i < dated {
        let mut j = i + 1;
        while j < dated && days[j] == days[i] {
            j += 1;
        }
        groups.push(i..j);
        i = j;
    }
    let mut shards = Vec::new();
    let mut run: Vec<std::ops::Range<usize>> = Vec::new();
    let mut bytes = 0;
    for g in groups.into_iter().rev() {
        bytes += sizes[g.clone()].iter().sum::<usize>();
        run.push(g);
        if bytes >= SHARD_BYTES {
            shards.push(run.drain(..).rev().flatten().collect());
            bytes = 0;
        }
    }
    if !run.is_empty() {
        shards.push(run.into_iter().rev().flatten().collect());
    }
    (shards, (dated..days.len()).collect())
}

fn span(days: &[Option<String>], shard: &[usize]) -> (String, String, usize) {
    let at = |i: usize| days[i].clone().unwrap_or_default();
    (at(shard[0]), at(shard[shard.len() - 1]), shard.len())
}

/// A shard's file name: its number, or `undated`.
fn numbered(stem: &str, n: i64) -> String {
    if n == UNDATED {
        format!("{stem}-undated.json")
    } else {
        format!("{stem}-{n}.json")
    }
}

/// A record as the tools need it: its `index.json` fields but `key` and
/// `documents`, with only its first source's `fhir_base`, `cli` and
/// `organization_id` (`null` when it has none).
fn slim(r: &Record) -> Value {
    let mut m = Map::new();
    m.insert("id".into(), r.id.clone().into());
    m.insert("kind".into(), r.kind.clone().into());
    if let Some(d) = &r.date {
        m.insert("date".into(), d.clone().into());
    }
    if let Some(c) = &r.code {
        m.insert(
            "code".into(),
            serde_json::to_value(c).unwrap_or(Value::Null),
        );
    }
    m.insert("display".into(), r.display.clone().into());
    if let Some(s) = &r.status {
        m.insert("status".into(), s.clone().into());
    }
    if !r.fields.is_empty() {
        m.insert("fields".into(), Value::Object(r.fields.clone()));
    }
    let source = r
        .provenance
        .first()
        .and_then(|p| serde_json::to_value(p).ok())
        .and_then(|p| p.get("source").cloned())
        .filter(Value::is_object);
    if let Some(src) = source {
        let mut keep = Map::new();
        for k in ["fhir_base", "cli"] {
            if let Some(v) = src.get(k).filter(|v| v.is_string()) {
                keep.insert(k.into(), v.clone());
            }
        }
        // The sign-in a label comes from; null says the record has none, which
        // a record written before 0.9.4 does not.
        keep.insert(
            "organization_id".into(),
            src.get("organization_id")
                .filter(|v| v.is_string())
                .cloned()
                .unwrap_or(Value::Null),
        );
        m.insert(
            "provenance".into(),
            Value::Array(vec![serde_json::json!({ "source": keep })]),
        );
    }
    Value::Object(m)
}

/// A record kind names files, so it must be a plain word.
fn check_kind(kind: &str) -> Result<(), CliError> {
    if !kind.is_empty()
        && kind
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Ok(());
    }
    Err(CliError::Other(format!(
        "a record's kind `{}` cannot name a views file",
        crate::printable(kind)
    )))
}

fn prefix(word: &str, n: usize) -> String {
    word.chars().take(n).collect()
}

fn offset(n: usize) -> Result<u32, CliError> {
    u32::try_from(n).map_err(|_| CliError::Other("a person's search text passes 4 GiB".into()))
}

/// `summary.json`: `medrec_summary`'s summary without `person` and `as_of`.
#[derive(Serialize)]
struct SummaryView<'a> {
    counts: &'a BTreeMap<String, usize>,
    sources: &'a Vec<summary::SourceRow>,
    medications: &'a summary::Listed<summary::Line>,
    conditions: &'a summary::Listed<summary::Line>,
    allergies: &'a summary::Listed<summary::Line>,
    immunizations: &'a summary::Listed<summary::Vaccine>,
    latest_results: &'a summary::Listed<summary::ResultRow>,
    recent_visits: &'a summary::Listed<summary::Line>,
    bills: &'a summary::Listed<summary::Bill>,
}

/// A timeline event without the person.
#[derive(Serialize)]
struct TimelineEvent<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    date: Option<&'a str>,
    kind: &'a str,
    id: &'a str,
    title: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source: Option<&'a str>,
}

/// A test series in `tests.json`.
#[derive(Serialize)]
struct Series {
    names: Vec<(Option<String>, String, Option<String>)>,
    /// Date, name number, shard, place in the shard, rank among all results.
    entries: Vec<(Option<String>, usize, i64, usize, usize)>,
}

type Name = (Option<String>, String, Option<String>);
/// A name's row in `names-<kind>.json`: the name, and the shards it is in.
type NameRow = (Option<String>, String, Option<String>, Vec<i64>);

fn name_of(r: &Record) -> Name {
    let code = r.code.as_ref();
    (
        code.and_then(|c| c.code.clone()),
        r.display.clone(),
        code.and_then(|c| c.display.clone()),
    )
}

/// A linked person's files as a person's events read them.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct LinkedFiles {
    pub index: Option<String>,
    pub text: Option<String>,
}

/// What a person's `events.json` is made from beyond their own files, and
/// the `bills.json` and `private.json` made from it.
pub(crate) struct EventsFrom {
    pub events: Value,
    pub bills: Value,
    pub private: Value,
    pub overrides: Option<String>,
    pub marks: Option<String>,
    pub linked: BTreeMap<String, LinkedFiles>,
}

/// Every view file of one person into `out`, by path in the folder, and
/// the person's entry in `views.json`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn person(
    h: &Household,
    p: &Person,
    idx: &PersonIndex,
    texts: &PersonText,
    index_md5: Option<String>,
    text_md5: Option<String>,
    from: EventsFrom,
    out: &mut BTreeMap<String, Vec<u8>>,
) -> Result<PersonViews, CliError> {
    let dir = format!("{DIR}/{}", p.id);
    let mut put = |name: &str, bytes: Vec<u8>| {
        out.insert(format!("{dir}/{name}"), bytes);
    };
    put("events.json", json(&from.events));
    put("bills.json", json(&from.bills));
    put("private.json", json(&from.private));

    let s = summary::of_index(
        h,
        p,
        idx,
        0,
        NO_DAY,
        summary::Limits {
            per_list: crate::mcp::SUMMARY_LINES,
        },
        &Default::default(),
    );
    put(
        "summary.json",
        json(&SummaryView {
            counts: &s.counts,
            sources: &s.sources,
            medications: &s.medications,
            conditions: &s.conditions,
            allergies: &s.allergies,
            immunizations: &s.immunizations,
            latest_results: &s.latest_results,
            recent_visits: &s.recent_visits,
            bills: &s.bills,
        }),
    );

    // Records of each kind, in the records tool's order: by day, newest
    // first, then by clipped name, then as the index holds them.
    let mut by_kind: BTreeMap<&str, Vec<&Record>> = BTreeMap::new();
    for r in &idx.records {
        by_kind.entry(r.kind.as_str()).or_default().push(r);
    }
    let mut records = BTreeMap::new();
    let mut place: HashMap<*const Record, (i64, usize)> = HashMap::new();
    for (kind, mut list) in by_kind {
        check_kind(kind)?;
        list.sort_by_cached_key(|r| (Reverse(day(r.date.as_deref())), clip(&r.display, CLIP)));
        let slims: Vec<Vec<u8>> = list.iter().map(|r| json(&slim(r))).collect();
        let days: Vec<Option<String>> = list.iter().map(|r| day(r.date.as_deref())).collect();
        let sizes: Vec<usize> = slims.iter().map(Vec::len).collect();
        let (shards, undated) = cut_from_oldest(&days, &sizes);
        let mut names: Vec<(Name, Vec<i64>)> = Vec::new();
        let mut named: HashMap<Name, usize> = HashMap::new();
        let numbered_shards = shards
            .iter()
            .enumerate()
            .map(|(n, sh)| (n as i64, sh))
            .chain((!undated.is_empty()).then_some((UNDATED, &undated)));
        for (n, shard) in numbered_shards {
            put(
                &numbered(&format!("records-{kind}"), n),
                array(shard.iter().map(|&i| slims[i].as_slice())),
            );
            for (pos, &i) in shard.iter().enumerate() {
                let r = list[i];
                place.insert(std::ptr::from_ref(r), (n, pos));
                let name = name_of(r);
                let at = *named.entry(name.clone()).or_insert_with(|| {
                    names.push((name, Vec::new()));
                    names.len() - 1
                });
                if !names[at].1.contains(&n) {
                    names[at].1.push(n);
                }
            }
        }
        let rows: Vec<NameRow> = names
            .into_iter()
            .map(|((code, display, code_display), shards)| (code, display, code_display, shards))
            .collect();
        put(&format!("names-{kind}.json"), json(&rows));
        records.insert(
            kind.to_string(),
            Run {
                shards: shards.iter().map(|sh| span(&days, sh)).collect(),
                undated: undated.len(),
            },
        );
    }

    // Each test's results, newest first, the series in the order of their
    // newest result.
    let mut results: Vec<&Record> = idx.records.iter().filter(|r| r.kind == "result").collect();
    results.sort_by(|a, b| b.date.cmp(&a.date).then_with(|| a.display.cmp(&b.display)));
    let mut series: Vec<Series> = Vec::new();
    let mut keyed: HashMap<String, usize> = HashMap::new();
    for (rank, r) in results.iter().enumerate() {
        let at = *keyed.entry(summary::series_key(r)).or_insert_with(|| {
            series.push(Series {
                names: Vec::new(),
                entries: Vec::new(),
            });
            series.len() - 1
        });
        let t = &mut series[at];
        let name = name_of(r);
        let ni = match t.names.iter().position(|x| *x == name) {
            Some(i) => i,
            None => {
                t.names.push(name);
                t.names.len() - 1
            }
        };
        let (shard, pos) = place[&std::ptr::from_ref(*r)];
        t.entries.push((r.date.clone(), ni, shard, pos, rank));
    }
    put("tests.json", json(&series));

    // The timeline: documents, records and studies (notes are read as they
    // are), in the records tool's order.
    let mut events = Vec::new();
    views::person_events(h, p, idx, &[], &Range::default(), &|_| true, &mut events);
    views::order_events(&mut events);
    events.sort_by_cached_key(|e| (Reverse(e.date.clone()), clip(&e.title, CLIP)));
    let shown: Vec<Vec<u8>> = events
        .iter()
        .map(|e| {
            json(&TimelineEvent {
                date: e.date.as_deref(),
                kind: &e.kind,
                id: &e.id,
                title: &e.title,
                detail: e.detail.as_deref(),
                source: e.source.as_deref(),
            })
        })
        .collect();
    let days: Vec<Option<String>> = events.iter().map(|e| day(e.date.as_deref())).collect();
    let (shards, undated) = cut_from_oldest(&days, &shown.iter().map(Vec::len).collect::<Vec<_>>());
    for (n, shard) in shards.iter().enumerate() {
        put(
            &numbered("timeline", n as i64),
            array(shard.iter().map(|&i| shown[i].as_slice())),
        );
    }
    if !undated.is_empty() {
        put(
            &numbered("timeline", UNDATED),
            array(undated.iter().map(|&i| shown[i].as_slice())),
        );
    }
    let timeline = Run {
        shards: shards.iter().map(|sh| span(&days, sh)).collect(),
        undated: undated.len(),
    };

    // The documents, with where their text is, and the texts in packs.
    let mut packs: Vec<Vec<u8>> = Vec::new();
    let mut text_at: HashMap<&str, [usize; 4]> = HashMap::new();
    for d in &idx.documents {
        let Some(t) = texts.texts.get(&d.id).filter(|t| !t.is_empty()) else {
            continue;
        };
        let b = t.as_bytes();
        if packs
            .last()
            .is_none_or(|p| !p.is_empty() && p.len() + b.len() > PACK_BYTES)
        {
            packs.push(Vec::new());
        }
        let n = packs.len() - 1;
        let pack = &mut packs[n];
        text_at.insert(&d.id, [n, pack.len(), b.len(), t.chars().count()]);
        pack.extend_from_slice(b);
    }
    let mut linked: HashMap<&str, Vec<&str>> = HashMap::new();
    for r in &idx.records {
        let mut seen = BTreeSet::new();
        for doc in &r.documents {
            if seen.insert(doc.as_str()) {
                let names = linked.entry(doc.as_str()).or_default();
                if names.len() < 20 {
                    names.push(&r.display);
                }
            }
        }
    }
    let documents: Vec<Value> = idx
        .documents
        .iter()
        .map(|d| {
            let mut v = serde_json::to_value(d).unwrap_or(Value::Null);
            if let Value::Object(m) = &mut v {
                if let Some(at) = text_at.get(d.id.as_str()) {
                    m.insert("text_at".into(), serde_json::json!(at));
                }
                if let Some(names) = linked.get(d.id.as_str()) {
                    m.insert("linked".into(), serde_json::json!(names));
                }
            }
            v
        })
        .collect();
    put("documents.json", json(&documents));
    for (n, pack) in packs.into_iter().enumerate() {
        put(&format!("text-{n}.bin"), pack);
    }

    // Search items, numbered in the reverse of medrec's order for hits of
    // equal score (newest first, undated last, then as the files hold
    // them): the newest item has the highest number.
    let listed = views::searchables(idx, texts, &[], true);
    let mut order: Vec<usize> = (0..listed.len()).collect();
    order.sort_by(|&a, &b| listed[b].date.cmp(&listed[a].date));
    order.reverse();
    let mut hay = Vec::new();
    let mut metas = Vec::new();
    let mut hay_at = vec![0u32];
    let mut meta_at = vec![0u32];
    let mut postings: BTreeMap<String, BTreeMap<String, Vec<usize>>> = BTreeMap::new();
    let mut firsts: BTreeMap<String, BTreeMap<String, Vec<usize>>> = BTreeMap::new();
    let mut labs = Vec::new();
    for (ord, &i) in order.iter().enumerate() {
        let it = &listed[i];
        let text = it.hay();
        let mut words: Vec<(String, usize, usize)> = Vec::new();
        let mut seen: HashMap<String, usize> = HashMap::new();
        for (w, at) in Squashed::new(&text).word_starts() {
            match seen.get(&w) {
                Some(&k) => words[k].1 += 1,
                None => {
                    seen.insert(w.clone(), words.len());
                    words.push((w, 1, at));
                }
            }
        }
        for (w, count, first) in words {
            for (into, n) in [(&mut postings, 2), (&mut firsts, 1)] {
                into.entry(hex(prefix(&w, n).as_bytes()))
                    .or_default()
                    .entry(w.clone())
                    .or_default()
                    .extend([ord, count, first]);
            }
        }
        hay.extend_from_slice(text.as_bytes());
        hay_at.push(offset(hay.len())?);
        metas.extend(json(&(it.date.as_deref(), &it.kind, &it.id, &it.title)));
        meta_at.push(offset(metas.len())?);
        if let Some(snippet) = &it.lab {
            labs.push((
                ord,
                it.date.as_deref(),
                &it.kind,
                &it.id,
                &it.title,
                snippet,
            ));
        }
    }
    let n = order.len();
    let mut items = Vec::with_capacity(4 * (1 + 2 * (n + 1)) + metas.len());
    items.extend(offset(n)?.to_le_bytes());
    for at in hay_at.iter().chain(&meta_at) {
        items.extend(at.to_le_bytes());
    }
    items.extend(metas);
    put("search-items.bin", items);
    // The text in packs of a fixed count of items, so a changed item, or a
    // new newest one, rewrites only its own pack.
    for (k, first) in (0..n).step_by(HAY_ITEMS).enumerate() {
        let (a, b) = (
            hay_at[first] as usize,
            hay_at[(first + HAY_ITEMS).min(n)] as usize,
        );
        put(&format!("search-hay-{k}.bin"), hay[a..b].to_vec());
    }
    for (key, words) in &postings {
        put(&format!("search-{key}.json"), json(words));
    }
    for (key, words) in &firsts {
        put(&format!("search-1-{key}.json"), json(words));
    }
    let lab_files = labs.chunks(LAB_ROWS).count();
    for (k, rows) in labs.chunks(LAB_ROWS).enumerate() {
        put(&format!("search-labs-{k}.json"), json(rows));
    }

    let mine = format!("{dir}/");
    let files = out
        .range(mine.clone()..)
        .take_while(|(rel, _)| rel.starts_with(&mine))
        .map(|(rel, bytes)| (rel[mine.len()..].to_string(), md5_hex(bytes)))
        .collect();
    let row = summary::person_row(p, idx, 0);
    Ok(PersonViews {
        index: index_md5,
        text: text_md5,
        overrides: from.overrides,
        marks: from.marks,
        linked: from.linked,
        row: Row {
            documents: row.documents,
            records: row.records,
            studies: row.studies,
            newest: row.newest,
            last_import: row.last_import,
        },
        records,
        timeline,
        search: SearchCounts {
            items: n,
            labs: lab_files,
        },
        files,
    })
}

// ---- when the views are written ------------------------------------------

/// Holds open while a command writes the folder several times.
static HOLDS: AtomicUsize = AtomicUsize::new(0);
/// A folder was written while held, so its views are owed.
static OWED: AtomicBool = AtomicBool::new(false);

/// The views of every folder written while this lives are brought up to
/// date once, when it drops, rather than as each one closes: for a command
/// that files several times (a refresh pass, an import of several members).
#[must_use]
pub struct Hold {
    location: Location,
    budget: Duration,
}

pub fn hold(location: &Location, budget: Duration) -> Hold {
    HOLDS.fetch_add(1, Ordering::SeqCst);
    Hold {
        location: location.clone(),
        budget,
    }
}

impl Drop for Hold {
    fn drop(&mut self) {
        if HOLDS.fetch_sub(1, Ordering::SeqCst) != 1 || !OWED.swap(false, Ordering::SeqCst) {
            return;
        }
        if std::thread::panicking() {
            return;
        }
        match Folder::open_at(&self.location, Access::Write, self.budget) {
            Ok(f) => report(sync(&f, false)),
            Err(e) => report(Err(e)),
        }
    }
}

/// A folder this command wrote is closing, still under its lock: its views
/// now, or when the [`Hold`] drops.
pub(crate) fn written(f: &Folder) {
    if HOLDS.load(Ordering::SeqCst) > 0 {
        OWED.store(true, Ordering::SeqCst);
        return;
    }
    report(sync(f, false));
}

/// A failure leaves the views stale, which readers see from the MD5s; the
/// next write brings them up to date.
fn report(r: Result<Synced, CliError>) {
    if let Err(e) = r {
        eprintln!(
            "medrec: views/ was not brought up to date: {e}. The next write, or `medrec views \
             write`, does it."
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn days(d: &[Option<&str>]) -> Vec<Option<String>> {
        d.iter().map(|x| x.map(str::to_string)).collect()
    }

    #[test]
    fn views_upload_every_file_and_stop_at_the_first_failure() {
        use crate::store::{Location, Reads, Store};
        let dir = std::env::temp_dir().join(format!("medrec-upload-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let st = Store::open(
            &Location::Dir(dir.clone()),
            Reads::Fresh,
            Duration::from_secs(60),
        )
        .unwrap();
        let bytes: Vec<Vec<u8>> = (0..40).map(|i| format!("view {i}").into_bytes()).collect();
        let files: Vec<(String, &Vec<u8>)> = bytes
            .iter()
            .enumerate()
            .map(|(i, b)| (format!("views/p_x/v-{i}.json"), b))
            .collect();
        assert_eq!(upload(&st, &files).unwrap(), 40);
        for (i, b) in bytes.iter().enumerate() {
            assert_eq!(
                &std::fs::read(dir.join(format!("views/p_x/v-{i}.json"))).unwrap(),
                b
            );
        }
        // A file where a directory must go: that upload fails, and so does
        // the whole, so views.json would not be written.
        std::fs::write(dir.join("views/p_y"), "not a directory").unwrap();
        let mut bad = files.clone();
        bad.insert(5, ("views/p_y/v.json".to_string(), &bytes[0]));
        assert!(upload(&st, &bad).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn shards_are_cut_from_the_oldest_day_and_never_inside_a_day() {
        // Newest first, undated last; each item a third of a shard.
        let d = days(&[
            Some("2026-03-03"),
            Some("2026-03-02"),
            Some("2026-03-02"),
            Some("2026-03-01"),
            Some("2026-02-28"),
            None,
            None,
        ]);
        let third = SHARD_BYTES / 3 + 1;
        let (shards, undated) = cut_from_oldest(&d, &[third; 7]);
        // From the oldest: 02-28 and 03-01 (2/3, open), then 03-02's two
        // close it at 4/3, never splitting the day; 03-03 is the open one.
        assert_eq!(shards, vec![vec![1, 2, 3, 4], vec![0]]);
        assert_eq!(undated, vec![5, 6]);
        assert_eq!(
            span(&d, &shards[0]),
            ("2026-03-02".into(), "2026-02-28".into(), 4)
        );
        let (none, all_undated) = cut_from_oldest(&days(&[None]), &[1]);
        assert!(none.is_empty());
        assert_eq!(all_undated, vec![0]);
    }

    fn household() -> Household {
        let mut h = Household::new("f_test");
        h.people.push(
            serde_json::from_value(serde_json::json!({
                "id": "p_test", "name": "Robin Testperson", "folder": "Robin Testperson",
                "created_at": "2026-01-01T00:00:00Z"
            }))
            .unwrap(),
        );
        h
    }

    fn result(n: usize, date: &str) -> Record {
        serde_json::from_value(serde_json::json!({
            "id": format!("r_{n}"), "kind": "result", "key": format!("k_{n}"),
            "date": date, "display": format!("Test {} panel", n % 40),
            "code": {"system": "http://loinc.org", "code": format!("{}-0", n % 40)},
            "fields": {"value": format!("{}.{} mmol/L", n % 9, n % 7)},
            "provenance": []
        }))
        .unwrap()
    }

    fn index_of(records: Vec<Record>) -> PersonIndex {
        let mut idx = PersonIndex::new("p_test");
        idx.records = records;
        idx
    }

    fn write(idx: &PersonIndex) -> (BTreeMap<String, Vec<u8>>, PersonViews) {
        let h = household();
        let mut out = BTreeMap::new();
        let v = person(
            &h,
            &h.people[0],
            idx,
            &PersonText::new("p_test"),
            None,
            None,
            EventsFrom {
                events: serde_json::json!({"schema": crate::events::SCHEMA, "events": []}),
                bills: serde_json::json!({"schema": crate::bills::SCHEMA, "bills": {}}),
                private: serde_json::json!({"schema": crate::private::SCHEMA}),
                overrides: None,
                marks: None,
                linked: BTreeMap::new(),
            },
            &mut out,
        )
        .unwrap();
        (out, v)
    }

    /// Three records a day, going back from 2026-12-28.
    fn history(n: usize) -> Vec<Record> {
        (0..n)
            .map(|i| {
                let k = i / 3;
                let day = format!(
                    "{:04}-{:02}-{:02}",
                    2026 - k / 336,
                    12 - (k / 28) % 12,
                    28 - k % 28
                );
                result(i, &day)
            })
            .collect()
    }

    #[test]
    fn a_slim_record_keeps_the_source_keys_a_label_is_found_by() {
        let source = |org: Value| {
            let mut r = result(0, "2026-12-28");
            r.provenance = vec![serde_json::from_value(serde_json::json!({
                "source": {"kind": "fhir", "cli": "athena", "organization_id": org,
                           "fhir_base": "https://athena.example.test/fhir/r4",
                           "cli_version": "0.6.2"},
                "method": "fhir-api", "filed_at": "2026-11-20T10:00:00Z", "filed_by": "medrec 0.9.4"
            }))
            .unwrap()];
            serde_json::to_string(&slim(&r)).unwrap()
        };
        // fhir_base, cli and organization_id, in that order, nothing else.
        let slimmed = source("900001".into());
        assert!(
            slimmed.ends_with(
                r#""provenance":[{"source":{"fhir_base":"https://athena.example.test/fhir/r4","cli":"athena","organization_id":"900001"}}]}"#
            ),
            "{slimmed}"
        );
        // A source with none says so with null.
        let none = source(Value::Null);
        assert!(
            none.ends_with(
                r#""fhir_base":"https://athena.example.test/fhir/r4","cli":"athena","organization_id":null}}]}"#
            ),
            "{none}"
        );
    }

    #[test]
    fn a_new_newest_result_changes_the_newest_shards_and_little_else() {
        let (before, v) = write(&index_of(history(3000)));
        let shards = v.records["result"].shards.len();
        assert!(shards >= 4, "{shards} shards");
        // Shard 0 is the oldest.
        assert!(v.records["result"].shards[0].1 < v.records["result"].shards[shards - 1].0);

        let mut more = history(3000);
        more.push(result(3000, "2026-12-29"));
        let (after, _) = write(&index_of(more));
        let changed: Vec<&String> = after
            .iter()
            .filter(|(rel, bytes)| before.get(*rel) != Some(*bytes))
            .map(|(rel, _)| rel)
            .collect();
        let dir = "views/p_test";
        for kept in ["records-result-0.json", "timeline-0.json"] {
            assert!(
                !changed.contains(&&format!("{dir}/{kept}")),
                "{kept} changed"
            );
        }
        let newest = format!("{dir}/records-result-{}.json", shards - 1);
        assert!(
            changed.contains(&&newest)
                || after.contains_key(&format!("{dir}/records-result-{shards}.json")),
            "{changed:?}"
        );
        // The single files, and the search shards of its few words.
        assert!(
            changed.len() <= 30,
            "{} of {} files changed: {changed:?}",
            changed.len(),
            after.len()
        );
    }

    #[test]
    fn the_newest_search_item_has_the_highest_number() {
        let mut records = history(10);
        records.push(Record {
            date: None,
            ..result(99, "2000-01-01")
        });
        let (files, v) = write(&index_of(records));
        let items = &files["views/p_test/search-items.bin"];
        let n = u32::from_le_bytes(items[0..4].try_into().unwrap()) as usize;
        assert_eq!(n, v.search.items);
        assert_eq!(n, 11);
        let meta_at = |i: usize| {
            let at = 4 * (1 + (n + 1) + i);
            u32::from_le_bytes(items[at..at + 4].try_into().unwrap()) as usize
        };
        let details = &items[4 * (1 + 2 * (n + 1))..];
        let item = |i: usize| -> Value {
            serde_json::from_slice(&details[meta_at(i)..meta_at(i + 1)]).unwrap()
        };
        // Undated first, the newest last.
        assert_eq!(item(0)[2], "r_99");
        assert_eq!(item(0)[0], Value::Null);
        assert_eq!(item(n - 1)[0], "2026-12-28");
        // Postings by item number, and the byte offset of the word's start
        // in the item's text.
        let shard: BTreeMap<String, Vec<usize>> =
            serde_json::from_slice(&files[&format!("views/p_test/search-{}.json", hex(b"pa"))])
                .unwrap();
        let panel = &shard["panel"];
        assert_eq!(panel.len(), 3 * n);
        assert!(panel
            .chunks(3)
            .map(|c| c[0])
            .collect::<Vec<_>>()
            .is_sorted());
        let hay_at = |i: usize| {
            u32::from_le_bytes(items[4 * (1 + i)..4 * (2 + i)].try_into().unwrap()) as usize
        };
        let hay = &files["views/p_test/search-hay-0.bin"];
        let (ord, first) = (panel[0], panel[2]);
        assert_eq!(&hay[hay_at(ord) + first..hay_at(ord) + first + 5], b"panel");
    }

    #[test]
    fn a_view_is_stored_under_its_md5_and_only_that_shape_is_ever_deleted() {
        let md5 = "0a1b2c3d4e5f60718293a4b5c6d7e8f9";
        assert_eq!(
            stored_name("records-result-7.json", md5),
            "records-result-7.0a1b2c3d4e5f.json"
        );
        assert_eq!(
            stored_name("search-hay-0.bin", md5),
            "search-hay-0.0a1b2c3d4e5f.bin"
        );
        assert!(is_stored_name("records-result-7.0a1b2c3d4e5f.json"));
        for not_a_view in [
            "index.json",
            "notes.md",
            "records-result-7.json",
            "scan.0a1b2c3d4e5f.pdf",
            "x.0A1B2C3D4E5F.json",
            ".0a1b2c3d4e5f.json",
        ] {
            assert!(!is_stored_name(not_a_view), "{not_a_view}");
        }
    }

    #[test]
    fn the_search_text_is_packed_512_items_to_a_file() {
        let (files, v) = write(&index_of(history(1100)));
        let n = v.search.items;
        assert_eq!(n, 1100);
        let items = &files["views/p_test/search-items.bin"];
        let text_at = |i: usize| {
            u32::from_le_bytes(items[4 * (1 + i)..4 * (2 + i)].try_into().unwrap()) as usize
        };
        let packs: Vec<&Vec<u8>> = (0..)
            .map_while(|k| files.get(&format!("views/p_test/search-hay-{k}.bin")))
            .collect();
        assert_eq!(packs.len(), 3);
        for (k, pack) in packs.iter().enumerate() {
            let (first, last) = (512 * k, (512 * (k + 1)).min(n));
            assert_eq!(pack.len(), text_at(last) - text_at(first), "pack {k}");
        }
        // Item 600 is in pack 1, at its offset less the pack's first item's.
        let at = text_at(600) - text_at(512);
        assert!(packs[1][at..].starts_with(b"Test "));
    }

    #[test]
    fn writing_the_same_files_twice_gives_the_same_bytes() {
        let idx = index_of(history(500));
        assert_eq!(write(&idx).0, write(&idx).0);
    }
}
