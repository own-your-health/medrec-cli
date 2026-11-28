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

use std::cell::OnceCell;
use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::time::Duration;

use pk_cli_core::CliError;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::changes::{Before, Views};
use crate::folder::{self, Access, Folder};
use crate::hash::{hex, md5_hex};
use crate::intake::classify::Squashed;
use crate::model::{Household, Person, PersonIndex, PersonText, Record};
use crate::parts::Parts;
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
/// The commit this medrec was built from, empty where it is not known.
const BUILD: &str = env!("BUILD_COMMIT");
/// The oldest medrec whose contract a reader must follow to read these views
/// (`views.json` `reader`). Raise it to the release's version, and say so in
/// its notes, in the release that gives a view a field that changes what a
/// reader must leave out or hold back: such a field cannot be ignored, yet it
/// changes no layout. 0.9.10 gave `private.json` `heading`.
pub const READER: &str = "0.9.10";

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
    crate::standing::FILE,
    SEARCH_PARTS,
];

/// What a search finds of each open item without its private parts.
const SEARCH_PARTS: &str = "search-parts.json";

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
    /// The commit it was built from, where known (0.9.10 and later): views of
    /// another build are made again, as of another version, since a rule can
    /// change between builds of one version.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub build: String,
    /// [`READER`] as it was (0.9.11 and later; empty before).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reader: String,
    /// The MD5 (hex) of `household.json` as the views were made from it.
    pub household: String,
    /// What the ids of provider labels are salted with: random, made once
    /// with the folder's first label (0.9.7 and later).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label_salt: Option<String>,
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
    /// `events.json`, `bills.json`, `private.json`, `search-parts.json`,
    /// `code-names.json` and `providers.json` stale.
    #[serde(default)]
    pub overrides: Option<String>,
    /// The MD5 of the person's `marks.json`, null when there is none
    /// (0.9.1 and later); it makes only `bills.json`, `private.json`,
    /// `search-parts.json`, `code-names.json` and `providers.json` stale.
    #[serde(default)]
    pub marks: Option<String>,
    #[serde(default)]
    pub linked: BTreeMap<String, LinkedFiles>,
    /// The code table these views' names were made with, absent when the
    /// pass that made them had none (0.9.4 and later), whatever names it
    /// carried over. A reader need not check it: a pass on a machine with
    /// another table makes the views again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_table: Option<String>,
    /// The provider directory these views' labels were made with, absent as
    /// `code_table` is (0.9.7 and later). A reader need not check it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directory: Option<String>,
    /// The MD5 of the person's `provider-state.json`, absent when there is
    /// none (0.9.7 and later). `private.json`, `search-parts.json`,
    /// `code-names.json` and `providers.json` are stale when it differs: a
    /// pass writes the file before the views, and one cut short between them
    /// leaves these.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_state: Option<String>,
    /// `standing.json`'s `in_error`: the ids of their records entered in
    /// error, by kind (0.9.9 and later).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub in_error: BTreeMap<String, Vec<String>>,
    /// The ids of what is in their chart and about somebody else, which no
    /// view has (0.9.9 and later): a reader says how many, and no more.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub about_another: Vec<String>,
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
    sync_with(
        f,
        all,
        crate::code_names::titles(),
        crate::nppes::dir().as_deref(),
    )
}

/// [`sync`] on a machine with these code titles, or none, and the provider
/// directories in `directories`.
pub(crate) fn sync_with(
    f: &Folder,
    all: bool,
    titles: Option<crate::tables::Titles>,
    directories: Option<&Path>,
) -> Result<Synced, CliError> {
    let Made {
        h,
        index,
        old,
        old_text,
        held,
        kept,
        files,
        states,
        made_from,
    } = match make(f, all, titles, directories, &BTreeMap::new())? {
        Pass::Made(made) => *made,
        Pass::Stands(people) => {
            return Ok(Synced {
                people,
                ..Synced::default()
            })
        }
    };
    let store = &f.store;
    let mut out = Synced {
        people: h.people.len(),
        people_rebuilt: h.people.len() - kept,
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
    #[cfg(test)]
    tests::before_writing();
    // Before the views made with them: a pass cut short here leaves views
    // the next one makes again.
    for s in &states {
        s.write(store)?;
    }
    out.files_written = upload(store, &uploads)?;
    let index_bytes = json(&index);
    if old_text.as_deref().map(str::as_bytes) != Some(index_bytes.as_slice()) {
        // No reader takes a view of what was private by another machine's
        // state for current.
        for (rel, md5) in &made_from {
            if store.md5(rel)? != *md5 {
                return Err(folder::changed_elsewhere(rel));
            }
        }
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

/// What a pass makes before it writes any of it.
struct Made {
    h: Household,
    index: ViewsIndex,
    old: Option<ViewsIndex>,
    old_text: Option<String>,
    /// The files under [`DIR`], with the MD5s the store has for them.
    held: BTreeMap<String, Option<String>>,
    /// How many people's views stand.
    kept: usize,
    files: BTreeMap<String, Vec<u8>>,
    states: Vec<NewState>,
    /// The state each person's views are made from.
    made_from: Vec<(String, Option<String>)>,
}

enum Pass {
    /// Everyone's views stand, of so many people.
    Stands(usize),
    Made(Box<Made>),
}

/// The views of the folder's files, with these people's indexes in place of
/// those it has.
fn make(
    f: &Folder,
    all: bool,
    titles: Option<crate::tables::Titles>,
    directories: Option<&Path>,
    indexes: &BTreeMap<String, PersonIndex>,
) -> Result<Pass, CliError> {
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
    let current = old.as_ref().filter(|o| {
        let ours = o.medrec == VERSION && o.build == BUILD;
        !all && o.schema == SCHEMA && ours && o.household == household
    });
    // The folder's, for as long as its views are.
    let salt: OnceCell<String> = OnceCell::new();
    if let Some(s) = old
        .as_ref()
        .filter(|o| o.schema == SCHEMA)
        .and_then(|o| o.label_salt.clone())
    {
        let _ = salt.set(s);
    }
    let fresh = |people: BTreeMap<String, PersonViews>| ViewsIndex {
        schema: SCHEMA.into(),
        medrec: VERSION.into(),
        build: BUILD.into(),
        reader: READER.into(),
        household: household.clone(),
        label_salt: salt.get().cloned(),
        people,
    };

    let code_table = titles.as_ref().map(|_| crate::tables::Titles::identity());
    let directory = match directories
        .map(|d| crate::labels::Directory::open(&crate::nppes::path(d, &h.folder_id)))
    {
        Some(Err(crate::nppes::Unused::OtherVersion)) => {
            if !QUIET.load(Ordering::SeqCst) {
                eprintln!(
                    "note: the provider directory on this machine was kept by another version \
                     of medrec, so this one does without it: `medrec tables fetch nppes` \
                     replaces it"
                );
            }
            None
        }
        d => d.and_then(Result::ok),
    };
    let directory_made = directory.as_ref().map(|d| d.made());

    // Whose views stand: their files have the MD5s the views were made from.
    let mut kept: BTreeMap<String, PersonViews> = BTreeMap::new();
    for p in &h.people {
        if let Some(was) = current.and_then(|o| o.people.get(&p.id)) {
            if (code_table.is_none() || was.code_table == code_table)
                // An older directory's pass leaves what a newer one's made.
                && was.directory >= directory_made
                && EVERY_PERSON.iter().all(|f| was.files.contains_key(*f))
                && stands(was, &h, p, |rel| store.md5(rel))?
            {
                kept.insert(p.id.clone(), was.clone());
            }
        }
    }
    let n = h.people.len();
    if kept.len() == n
        && old_text.as_deref().map(str::as_bytes) == Some(json(&fresh(kept.clone())).as_slice())
    {
        return Ok(Pass::Stands(n));
    }
    let held: BTreeMap<String, Option<String>> = store
        .hashes(DIR)?
        .into_iter()
        .map(|x| (x.rel, x.md5))
        .collect();
    // A person missing one of their views, or with one that is not as it was
    // written, has them made again.
    kept.retain(|pid, v| {
        v.files.iter().all(|(name, md5)| {
            held.get(&stored_rel(pid, name, md5))
                .is_some_and(|m| m.as_ref().is_none_or(|m| m == md5))
        })
    });

    let mut people = BTreeMap::new();
    let mut files = BTreeMap::new();
    let mut states: Vec<NewState> = Vec::new();
    let mut made_from: Vec<(String, Option<String>)> = Vec::new();
    // Each person's files read once, whoever's events need them.
    let mut loaded: BTreeMap<String, Loaded> = BTreeMap::new();
    fn load(
        store: &crate::store::Store,
        loaded: &mut BTreeMap<String, Loaded>,
        p: &Person,
        instead: Option<&PersonIndex>,
    ) -> Result<(), CliError> {
        if loaded.contains_key(&p.id) {
            return Ok(());
        }
        let (irel, trel) = (folder::index_rel(p), folder::text_rel(p));
        let itext = store.read_text(&irel)?;
        let ttext = store.read_text(&trel)?;
        let idx: PersonIndex = match (instead, &itext) {
            (Some(idx), _) => idx.clone(),
            (None, Some(t)) => parse(t, &irel)?,
            (None, None) => PersonIndex::new(&p.id),
        };
        if idx.person != p.id {
            return Err(CliError::Other(format!("{irel} names another person")));
        }
        let texts: PersonText = match &ttext {
            Some(t) => parse(t, &trel)?,
            None => PersonText::new(&p.id),
        };
        let idx = crate::about::shown(p, idx);
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
        load(store, &mut loaded, p, indexes.get(&p.id))?;
        for q in &linked {
            load(store, &mut loaded, q, indexes.get(&q.id))?;
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
        let mut events = crate::events::build_both(&crate::events::Input {
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
            events.events["overrides_unreadable"] = Value::Bool(true);
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
        let none = crate::marks::Marks::new(&p.id);
        let read = marks
            .as_ref()
            .and_then(|m| m.as_ref().ok())
            .unwrap_or(&none);
        let payments = read.payments.as_slice();
        let mut bills = crate::bills::build(p, &me.idx, &me.texts, &events.events, payments);
        let private_marks = read.private.as_slice();
        let last = old
            .as_ref()
            .filter(|o| o.schema == SCHEMA)
            .and_then(|o| o.people.get(&p.id));
        // A machine without the directory keeps what another's gave.
        let had = last.is_some_and(|v| v.provider_state.is_some());
        let (before, held) = last_state(store, p, had)?;
        let state = crate::labels::State::of(&me.idx, directory.as_ref(), before);
        let was = held.as_deref().map(|t| md5_hex(t.as_bytes()));
        let write = state.replaces(held.as_deref());
        let provider_state = match &write {
            Some(t) => Some(md5_hex(t.as_bytes())),
            None => was.clone(),
        };
        if let Some(text) = write {
            states.push(NewState {
                rel: crate::labels::state_rel(p),
                unread: held.filter(|t| {
                    !matches!(
                        crate::labels::State::parse(t, &p.id),
                        crate::labels::Last::State(_)
                    )
                }),
                was,
                text,
            });
        }
        let corrections = read.providers.as_slice();
        let mut private = {
            let (idx, texts) = (&me.idx, &me.texts);
            let labelled = crate::labels::areas(idx, &events, &state, corrections);
            crate::private::build(p, idx, texts, &events, read, &labelled)
        };
        if state.lost() {
            private["labels_unreadable"] = Value::Bool(true);
            private["marks_unreadable"] = Value::Bool(true);
        }
        if matches!(marks, Some(Err(()))) {
            bills["marks_unreadable"] = Value::Bool(true);
            private["marks_unreadable"] = Value::Bool(true);
        }
        // So does one without the table the names.
        let before = match (&titles, last) {
            (None, Some(v)) => last_code_names(store, p, v)?,
            _ => None,
        };
        let source = match (&titles, &before) {
            (Some(t), _) => Some(crate::code_names::Source::Table(t)),
            (None, Some(v)) => Some(crate::code_names::Source::Previous(v)),
            (None, None) => None,
        };
        let code_names =
            source.and_then(|s| crate::code_names::build(p, &me.idx, &private, private_marks, &s));
        let now = crate::standing::index(&me.idx);
        let events = events.events;
        let said = crate::labels::Said::of(&now, &events, &state, corrections);
        let lost = events["events_in_error"].as_array().into_iter().flatten();
        let gone: BTreeSet<String> = crate::standing::errors(&me.idx)
            .chain(lost.filter_map(|e| e["id"].as_str()))
            .map(str::to_string)
            .collect();
        let providers = crate::labels::build(&crate::labels::Input {
            person: p,
            index: &now,
            texts: &me.texts,
            events: &events,
            private: &private,
            state: &state,
            said: &said,
            in_error: &gone,
            salt: &|| salt.get_or_init(|| crate::ids::new("s")).clone(),
        });
        let from = EventsFrom {
            bills,
            private,
            events,
            code_names,
            providers,
            provider_state,
            directory: directory_made.clone(),
            // Absent without the table: the names carried are not all a
            // table would give, so a machine with one makes them again.
            code_table: code_table.clone(),
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
        made_from.push((crate::labels::state_rel(p), v.provider_state.clone()));
        people.insert(p.id.clone(), v);
    }
    Ok(Pass::Made(Box::new(Made {
        index: fresh(people),
        h,
        kept: kept.len(),
        old,
        old_text,
        held,
        files,
        states,
        made_from,
    })))
}

/// The views a pass would make, in memory, and those the last one made.
pub(crate) struct Preview {
    h: Household,
    files: BTreeMap<String, Vec<u8>>,
    last: Option<ViewsIndex>,
}

/// What a pass would make of everyone's views with these people's indexes in
/// place of those the folder has. Nothing is written.
pub(crate) fn preview(
    f: &Folder,
    indexes: &BTreeMap<String, PersonIndex>,
) -> Result<Preview, CliError> {
    let titles = crate::code_names::titles();
    let directories = crate::nppes::dir();
    match make(f, true, titles, directories.as_deref(), indexes)? {
        Pass::Made(made) => Ok(Preview {
            h: made.h,
            files: made.files,
            last: match (made.old_text, made.old) {
                (None, _) => None,
                (_, Some(o)) if o.schema == SCHEMA => Some(o),
                // Those had none of the views this counts by.
                (_, Some(o)) if o.schema == SCHEMA_V1 => None,
                (Some(text), _) => return Err(unread(&text)),
            },
        }),
        // Of nobody.
        Pass::Stands(_) => Ok(Preview {
            h: f.household()?,
            files: BTreeMap::new(),
            last: None,
        }),
    }
}

/// Why there is nothing to count against in a `views.json` that does not read
/// as this medrec's. Its own words are not in it.
fn unread(text: &str) -> CliError {
    let of = |s: &str| s.strip_prefix("medrec-views/v")?.parse::<u32>().ok();
    let v: Option<Value> = serde_json::from_str(text).ok();
    let theirs = v.as_ref().and_then(|v| of(v.get("schema")?.as_str()?));
    CliError::Other(match theirs {
        Some(n) if Some(n) > of(SCHEMA) => format!(
            "{INDEX} is a later medrec's (medrec-views/v{n}), so there is nothing this one can count against: upgrade it"
        ),
        _ => format!(
            "{INDEX} is there and does not read, so there is nothing to count against: `medrec views write` makes it again"
        ),
    })
}

impl Preview {
    /// A person's views, as a pass would make them.
    pub fn after(&self, p: &Person) -> Result<Views, CliError> {
        Views::read(&format!("{DIR}/{}", p.id), |name| {
            let rel = format!("{DIR}/{}/{name}", p.id);
            let bytes = self.files.get(&rel);
            let read = bytes.and_then(|b| serde_json::from_slice(b).ok());
            let made = read.ok_or_else(|| CliError::Other(format!("no {rel} was made")));
            made.map(Some)
        })
    }

    /// A person's views, as the last pass made them.
    pub fn before(&self, f: &Folder, p: &Person) -> Result<Before, CliError> {
        let Some(last) = &self.last else {
            return Ok(Before::default());
        };
        let Some(v) = last.people.get(&p.id) else {
            return Ok(Before::default());
        };
        let md5 = |rel: &str| f.store.md5(rel);
        let current = md5(folder::HOUSEHOLD)?.as_ref() == Some(&last.household)
            && stands(v, &self.h, p, md5)?;
        let views = Views::read(&format!("{DIR}/{}", p.id), |name| {
            if !v.files.contains_key(name) {
                return Ok(None);
            }
            let text = last_view(&f.store, p, v, name)?;
            let read = text.and_then(|t| serde_json::from_str(&t).ok());
            let there = read.ok_or_else(|| {
                CliError::Other(format!(
                    "{DIR}/{} has no {name} that {INDEX} names; another command may be writing the views: run this again, or after `medrec views write`",
                    p.id
                ))
            });
            there.map(Some)
        })?;
        Ok(Before {
            medrec: Some(last.medrec.clone()),
            stale: !current,
            views,
        })
    }
}

/// Whether a person's views were made from the files the folder has, under the
/// same household, by what `md5` says each is.
fn stands(
    v: &PersonViews,
    h: &Household,
    p: &Person,
    md5: impl Fn(&str) -> Result<Option<String>, CliError>,
) -> Result<bool, CliError> {
    let own = [
        (folder::index_rel(p), &v.index),
        (folder::text_rel(p), &v.text),
        (crate::events::overrides_rel(p), &v.overrides),
        (crate::marks::rel(p), &v.marks),
        (crate::labels::state_rel(p), &v.provider_state),
    ];
    for (rel, was) in own {
        if md5(&rel)? != *was {
            return Ok(false);
        }
    }
    let linked = crate::events::linked_people(h, p);
    if linked.len() != v.linked.len() {
        return Ok(false);
    }
    for q in linked {
        let now = LinkedFiles {
            index: md5(&folder::index_rel(q))?,
            text: md5(&folder::text_rel(q))?,
        };
        if v.linked.get(&q.id) != Some(&now) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// One of a person's views, when the views are those of the folder's files
/// as they are now; none when it is missing, stale, or another pass's.
pub fn current_view(
    f: &Folder,
    h: &Household,
    p: &Person,
    name: &str,
) -> Result<Option<Vec<u8>>, CliError> {
    let md5 = |rel: &str| -> Result<Option<String>, CliError> {
        Ok(f.store.read_text(rel)?.map(|t| md5_hex(t.as_bytes())))
    };
    let Some(index) = f
        .store
        .read_text(INDEX)?
        .and_then(|t| serde_json::from_str::<ViewsIndex>(&t).ok())
    else {
        return Ok(None);
    };
    if index.schema != SCHEMA || md5(folder::HOUSEHOLD)?.as_deref() != Some(&*index.household) {
        return Ok(None);
    }
    let Some(v) = index.people.get(&p.id) else {
        return Ok(None);
    };
    let Some(want) = v.files.get(name) else {
        return Ok(None);
    };
    if !stands(v, h, p, md5)? {
        return Ok(None);
    }
    let text = f.store.read_text(&stored_rel(&p.id, name, want))?;
    Ok(text.map(String::into_bytes).filter(|b| md5_hex(b) == *want))
}

/// One of the person's views as the last pass wrote it, whatever changed
/// since.
fn last_view(
    store: &crate::store::Store,
    p: &Person,
    v: &PersonViews,
    name: &str,
) -> Result<Option<String>, CliError> {
    let Some(want) = v.files.get(name) else {
        return Ok(None);
    };
    Ok(store
        .read_text(&stored_rel(&p.id, name, want))?
        .filter(|t| md5_hex(t.as_bytes()) == *want))
}

/// The person's `code-names.json` as the last pass wrote it.
fn last_code_names(
    store: &crate::store::Store,
    p: &Person,
    v: &PersonViews,
) -> Result<Option<Value>, CliError> {
    Ok(last_view(store, p, v, crate::code_names::FILE)?
        .and_then(|t| crate::code_names::previous(&t, &p.id)))
}

/// A person's `provider-state.json` to write.
struct NewState {
    rel: String,
    /// The MD5 of the file it is made from, none when there is none.
    was: Option<String>,
    /// What that says, when it does not read.
    unread: Option<String>,
    text: String,
}

impl NewState {
    /// Nothing else says what the file does: one another machine wrote since
    /// it was read stays, and the pass ends. One that did not read stays
    /// beside it.
    fn write(&self, store: &crate::store::Store) -> Result<(), CliError> {
        if store.md5(&self.rel)? != self.was {
            return Err(folder::changed_elsewhere(&self.rel));
        }
        if let Some(text) = &self.unread {
            let aside = self.rel.trim_end_matches(".json");
            let n = &md5_hex(text.as_bytes())[..8];
            folder::write_back(store, &format!("{aside}~unread-{n}.json"), text)?;
        }
        folder::write_back(store, &self.rel, &self.text)
    }
}

/// What the last pass kept of the directory for the person, and the file's
/// text. `had` is whether its views were made with any: a file gone since is
/// not one that never was.
fn last_state(
    store: &crate::store::Store,
    p: &Person,
    had: bool,
) -> Result<(crate::labels::Last, Option<String>), CliError> {
    use crate::labels::{Last, State};
    let rel = crate::labels::state_rel(p);
    let text = match store.read_text(&rel)? {
        // The cache may be from before it was first written.
        None if had => store.read_text_fresh(&rel)?,
        t => t,
    };
    let last = match &text {
        Some(t) => State::parse(t, &p.id),
        None if had => Last::Gone,
        None => Last::None,
    };
    Ok((last, text))
}

/// What is kept of the directory for the person, as the last pass wrote it.
pub fn provider_state(f: &Folder, p: &Person) -> Result<crate::labels::State, CliError> {
    let had = || -> Result<bool, CliError> {
        Ok(f.store
            .read_text(INDEX)?
            .and_then(|t| serde_json::from_str::<ViewsIndex>(&t).ok())
            .filter(|i| i.schema == SCHEMA)
            .and_then(|i| Some(i.people.get(&p.id)?.provider_state.is_some()))
            .unwrap_or(false))
    };
    let last = match last_state(&f.store, p, false)?.0 {
        crate::labels::Last::None if had()? => last_state(&f.store, p, true)?.0,
        last => last,
    };
    Ok(crate::labels::State::last(last, &p.id))
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
/// `organization_id` (`null` when it has none), and its label.
fn slim(r: &Record, from: crate::sources::Sources) -> Value {
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
    if crate::panels::heads(r) {
        m.insert("heading".into(), true.into());
    }
    let says = crate::standing::record_says(from, r);
    if !says.is_empty() {
        m.insert("says".into(), serde_json::json!(says));
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
        keep.insert("label".into(), views::record_source(from, r).into());
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
    standing: CanBeCurrent,
}

/// What `standing.json` can have as current or on hold on some day.
#[derive(Serialize)]
struct CanBeCurrent {
    medications: Vec<summary::Line>,
    conditions: Vec<summary::Line>,
    allergies: Vec<summary::Line>,
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
    /// `code-names.json`, when any code of theirs has a name, and the table
    /// on this machine it was made with.
    pub code_names: Option<Value>,
    pub code_table: Option<String>,
    /// `providers.json`, when any provider of theirs has a label, and the
    /// provider directory on this machine it was made with.
    pub providers: Option<Value>,
    /// The MD5 of the person's `provider-state.json`.
    pub provider_state: Option<String>,
    pub directory: Option<String>,
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
    let st = crate::standing::Standings::of(&h.sources, p, idx);
    put(crate::standing::FILE, json(&st));
    if let Some(v) = &from.code_names {
        put(crate::code_names::FILE, json(v));
    }
    if let Some(v) = &from.providers {
        put(crate::labels::FILE, json(v));
    }

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
    let sources = crate::sources::Sources::of(&h.sources, idx);
    let by_status = |kind: &str| summary::by_status(sources, idx, kind, crate::mcp::SUMMARY_LINES);
    let can_be_current = |kind: &str| summary::can_be_current(sources, idx, &st, kind);
    put(
        "summary.json",
        json(&SummaryView {
            counts: &s.counts,
            sources: &s.sources,
            medications: &by_status("medication"),
            conditions: &by_status("condition"),
            allergies: &by_status("allergy"),
            immunizations: &s.immunizations,
            latest_results: &s.latest_results,
            recent_visits: &s.recent_visits,
            bills: &s.bills,
            standing: CanBeCurrent {
                medications: can_be_current("medication"),
                conditions: can_be_current("condition"),
                allergies: can_be_current("allergy"),
            },
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
        let slims: Vec<Vec<u8>> = list.iter().map(|r| json(&slim(r, sources))).collect();
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
    // newest result, without headings and those entered in error.
    let errors: BTreeSet<&String> = st.errors().collect();
    let mut results: Vec<&Record> = idx
        .records
        .iter()
        .filter(|r| r.kind == "result" && !errors.contains(&r.id))
        .filter(|r| !crate::panels::heads(r))
        .collect();
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
    let mut linked: HashMap<&str, Vec<&Record>> = HashMap::new();
    for r in idx.records.iter().filter(|r| !errors.contains(&r.id)) {
        let mut seen = BTreeSet::new();
        for doc in &r.documents {
            if seen.insert(doc.as_str()) {
                let by = linked.entry(doc.as_str()).or_default();
                if by.len() < 20 {
                    by.push(r);
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
                // Read for what is private, and shown nowhere.
                m.remove("given");
                if let Some(at) = text_at.get(d.id.as_str()) {
                    m.insert("text_at".into(), serde_json::json!(at));
                }
                if let Some(by) = linked.get(d.id.as_str()) {
                    let names: Vec<&str> = by.iter().map(|r| r.display.as_str()).collect();
                    let ids: Vec<&str> = by.iter().map(|r| r.id.as_str()).collect();
                    m.insert("linked".into(), serde_json::json!(names));
                    m.insert("linked_ids".into(), serde_json::json!(ids));
                }
                let says = crate::standing::document_says(sources, d);
                if !says.is_empty() {
                    m.insert("says".into(), serde_json::json!(says));
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
    let listed = views::searchables(idx, texts, &[], true, &Parts::of(&from.private));
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
    let mut parts: BTreeMap<String, Value> = BTreeMap::new();
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
        if let Some(open) = &it.open {
            parts.insert(ord.to_string(), search_part(it, open));
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
    put(SEARCH_PARTS, json(&parts));

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
        code_table: from.code_table,
        directory: from.directory,
        provider_state: from.provider_state,
        in_error: st.in_error,
        about_another: idx.about_another.clone(),
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
/// The views owed by what closed while held: none (0), those of what changed
/// (1), or every person's (2).
static OWED: AtomicU8 = AtomicU8::new(0);

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
        if HOLDS.fetch_sub(1, Ordering::SeqCst) != 1 {
            return;
        }
        let owed = OWED.swap(0, Ordering::SeqCst);
        if owed == 0 || std::thread::panicking() {
            return;
        }
        match Folder::open_at(&self.location, Access::Write, self.budget) {
            Ok(f) => report(sync(&f, owed > 1)),
            Err(e) => report(Err(e)),
        }
    }
}

/// A folder this command wrote, or asked every person's views of (`all`), is
/// closing, still under its lock: its views now, or when the [`Hold`] drops.
pub(crate) fn written(f: &Folder, all: bool) {
    if HOLDS.load(Ordering::SeqCst) > 0 {
        OWED.fetch_max(1 + u8::from(all), Ordering::SeqCst);
        return;
    }
    report(sync(f, all));
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

/// What a search finds of an item without its private parts: what is cut out
/// of its text, and each word that is there fewer times, or elsewhere, than
/// its postings say.
fn search_part(it: &views::Searchable, open: &views::OpenPart) -> Value {
    let count = |text: &str| {
        let mut words: BTreeMap<String, (usize, usize)> = BTreeMap::new();
        for (w, at) in Squashed::new(text).word_starts() {
            words.entry(w).or_insert((0, at)).0 += 1;
        }
        words
    };
    let (hay, left) = (it.hay(), open.hay(it));
    let now = count(&left);
    let cut = open.cuts.iter();
    let cut = cut.filter_map(|[at, len]| hay.get(*at..at.checked_add(*len)?));
    let named = std::iter::once(open.title.as_str());
    let touched = cut.chain(named).flat_map(|text| count(text).into_keys());
    let words: BTreeMap<String, Vec<usize>> = touched
        .map(|w| {
            let of = now.get(&w).map_or(vec![0], |(n, at)| vec![*n, *at]);
            (w, of)
        })
        .collect();
    let mut v = serde_json::json!({"cuts": open.cuts, "words": words});
    if it.lab.is_some() && open.lab.is_none() {
        v["lab"] = Value::Bool(false);
    }
    v
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    /// A reader is never asked for a contract newer than the medrec that
    /// wrote the views.
    #[test]
    fn the_reader_asked_for_is_no_newer_than_this_medrec() {
        let parts = |v: &str| -> Vec<u32> { v.split('.').map(|n| n.parse().unwrap()).collect() };
        assert!(parts(READER) <= parts(VERSION), "{READER} {VERSION}");
    }

    thread_local! {
        static BEFORE_WRITING: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
            const { std::cell::RefCell::new(None) };
    }

    /// What another machine does before this thread's next pass writes.
    pub(crate) fn meanwhile(f: impl FnOnce() + 'static) {
        BEFORE_WRITING.with(|b| *b.borrow_mut() = Some(Box::new(f)));
    }

    pub(super) fn before_writing() {
        if let Some(f) = BEFORE_WRITING.with(|b| b.borrow_mut().take()) {
            f();
        }
    }

    fn days(d: &[Option<&str>]) -> Vec<Option<String>> {
        d.iter().map(|x| x.map(str::to_string)).collect()
    }

    /// Another machine wrote it since it was read: it stays.
    #[test]
    fn a_state_another_machine_wrote_is_not_written_over() {
        use crate::store::{Location, Reads, Store};
        let dir = std::env::temp_dir().join(format!("medrec-state-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let loc = Location::Dir(dir.clone());
        let st = Store::open(&loc, Reads::Fresh, Duration::from_secs(60)).unwrap();
        let path = dir.join("provider-state.json");
        let new = |was: Option<&str>, unread: Option<&str>| NewState {
            rel: "provider-state.json".into(),
            was: was.map(|t| md5_hex(t.as_bytes())),
            unread: unread.map(str::to_string),
            text: "ours\n".into(),
        };
        for (held, state, after) in [
            (Some("theirs"), new(None, None), "theirs"),
            (Some("theirs"), new(Some("read"), None), "theirs"),
            (Some("theirs"), new(Some("read"), Some("read")), "theirs"),
            (Some("read"), new(Some("read"), None), "ours\n"),
            (None, new(None, None), "ours\n"),
        ] {
            let _ = std::fs::remove_file(&path);
            if let Some(t) = held {
                std::fs::write(&path, t).unwrap();
            }
            assert_eq!(state.write(&st).is_ok(), after != "theirs");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), after);
        }
        // One that is gone since is another machine's to have removed.
        std::fs::remove_file(&path).unwrap();
        assert!(new(Some("read"), None).write(&st).is_err());
        assert!(!path.exists());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
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

    /// Two machines writing one folder in turn, one with the table.
    #[test]
    fn a_machine_without_the_table_keeps_names_and_one_with_it_fills_them_in() {
        use crate::tables::Titles;

        let root = std::env::temp_dir().join(format!("medrec-vf-tables-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let loc = crate::store::Location::Dir(root.clone());
        let folder = || Folder::open_at(&loc, Access::Read, Duration::from_secs(60)).unwrap();
        let put = |rel: &str, v: &Value| {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, serde_json::to_vec(v).unwrap()).unwrap();
        };
        const PID: &str = "p_exampleaaaaaaaaaaaaa";
        let mut h = household();
        h.folder_id = "f_exampleaaaaaaaaaaaaa".into();
        h.people[0].id = PID.into();
        put("household.json", &serde_json::to_value(&h).unwrap());
        let condition = |id: &str, code: &str| -> Record {
            serde_json::from_value(serde_json::json!({
                "id": id, "kind": "condition", "key": id, "display": "Source words",
                "fields": {}, "provenance": [],
                "code": {"system": crate::normalize::ICD10, "code": code}
            }))
            .unwrap()
        };
        let records = |list: Vec<Record>| {
            let mut idx = PersonIndex::new(PID);
            idx.records = list;
            put(
                "Robin Testperson/index.json",
                &serde_json::to_value(idx).unwrap(),
            )
        };
        let titles = || {
            Some(Titles::invented(&[
                ("J309", "Sample sneezes"),
                ("R51", "Sample aches"),
            ]))
        };
        let entry = || -> PersonViews {
            let t = std::fs::read_to_string(root.join(INDEX)).unwrap();
            serde_json::from_str::<ViewsIndex>(&t).unwrap().people[PID].clone()
        };
        let names = || -> Vec<String> {
            let Some(md5) = entry().files.get(crate::code_names::FILE).cloned() else {
                return Vec::new();
            };
            let rel = stored_rel(PID, crate::code_names::FILE, &md5);
            let v: Value =
                serde_json::from_str(&std::fs::read_to_string(root.join(rel)).unwrap()).unwrap();
            v["codes"].as_object().unwrap().keys().cloned().collect()
        };
        let rebuilt = |titles| {
            sync_with(&folder(), false, titles, None)
                .unwrap()
                .people_rebuilt
        };

        records(vec![condition("r_a", "J30.9")]);
        assert_eq!(rebuilt(titles()), 1);
        assert_eq!(names(), ["icd10cm|J30.9"]);
        assert_eq!(entry().code_table, Some(Titles::identity()));

        // Without the table: an unchanged person stands, table and all.
        assert_eq!(rebuilt(None), 0);
        assert_eq!(entry().code_table, Some(Titles::identity()));

        // A changed one keeps the names it had, and the new code gets none.
        records(vec![condition("r_a", "J30.9"), condition("r_b", "R51")]);
        assert_eq!(rebuilt(None), 1);
        assert_eq!(names(), ["icd10cm|J30.9"]);
        assert_eq!(entry().code_table, None);

        // With it: made once more, with the name that was missing.
        assert_eq!(rebuilt(titles()), 1);
        assert_eq!(names(), ["icd10cm|J30.9", "icd10cm|R51"]);
        assert_eq!(rebuilt(titles()), 0);
        assert_eq!(rebuilt(None), 0);

        // A lost names file is not carried, and comes back the same way.
        let md5 = entry().files[crate::code_names::FILE].clone();
        std::fs::write(
            root.join(stored_rel(PID, crate::code_names::FILE, &md5)),
            b"{}",
        )
        .unwrap();
        sync_with(&folder(), true, None, None).unwrap();
        assert!(names().is_empty());
        assert_eq!(rebuilt(titles()), 1);
        assert_eq!(names(), ["icd10cm|J30.9", "icd10cm|R51"]);
        let _ = std::fs::remove_dir_all(&root);
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
                code_names: None,
                code_table: None,
                providers: None,
                provider_state: None,
                directory: None,
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

    /// Of any kind, for a reader that opens no other view.
    #[test]
    fn an_entry_names_what_is_entered_in_error() {
        let mut records = history(3);
        let (_, v) = write(&index_of(records.clone()));
        assert!(v.in_error.is_empty());
        assert!(!serde_json::to_value(&v)
            .unwrap()
            .to_string()
            .contains("in_error"));
        records[1].status = Some("entered-in-error".into());
        let (out, v) = write(&index_of(records.clone()));
        assert_eq!(v.in_error["result"], [records[1].id.clone()]);
        let st = out.values().zip(out.keys());
        let st = st
            .filter(|(_, name)| name.ends_with(crate::standing::FILE))
            .map(|(b, _)| serde_json::from_slice::<Value>(b).unwrap())
            .next()
            .unwrap();
        assert_eq!(st["in_error"], serde_json::json!(v.in_error));
    }

    /// Nor are the latest results, a test's series or a document's links of
    /// one.
    #[test]
    fn what_is_entered_in_error_is_in_no_list_made_at_the_write() {
        let mut records = history(3);
        records[0].status = Some("entered-in-error".into());
        for r in &mut records {
            r.documents = vec!["d_exampleaaaaaaaaaaaaaa".into()];
        }
        let mut idx = index_of(records.clone());
        idx.documents = vec![serde_json::from_value(serde_json::json!({
            "id": "d_exampleaaaaaaaaaaaaaa", "path": "x.pdf", "sha256": "0", "md5": "0",
            "size": 1, "media_type": "application/pdf", "origin": {"kind": "file", "action": "copied"},
            "filed_at": "2026-12-28T00:00:00Z",
            "provenance": {"source": {"kind": "manual"}, "method": "manual",
            "filed_at": "2026-12-28T00:00:00Z", "filed_by": "medrec 0.9.9"}}))
        .unwrap()];
        let (out, _) = write(&idx);
        let view = |name: &str| -> Value {
            let (_, b) = out.iter().find(|(rel, _)| rel.ends_with(name)).unwrap();
            serde_json::from_slice(b).unwrap()
        };
        let entries: usize = view("/tests.json")
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["entries"].as_array().unwrap().len())
            .sum();
        assert_eq!(entries, 2);
        let linked = view("/documents.json")[0]["linked"]
            .as_array()
            .unwrap()
            .len();
        assert_eq!(linked, 2);
        let latest = view("/summary.json")["latest_results"].to_string();
        assert!(!latest.contains(&records[0].id), "{latest}");
        // A shard holds it as it holds the rest.
        let shards: String = out
            .iter()
            .filter(|(rel, _)| rel.contains("/records-result-"))
            .map(|(_, b)| String::from_utf8_lossy(b).into_owned())
            .collect();
        assert!(shards.contains(&records[0].id));
    }

    #[test]
    fn a_slim_record_keeps_its_sources_keys_and_its_label() {
        let h: Household = serde_json::from_value(serde_json::json!({
            "schema": "medrec-household/v1", "format": 1, "folder_id": "f_1", "rev": 1,
            "created_at": "2026-01-01T00:00:00Z", "people": [], "organizations": [],
            "sources": [{"id": "s_exampleaaaaaaaaaaaaa", "kind": "fhir", "person": "p_x",
                "cli": "athena", "organization": "900001", "patient_id": "1",
                "organization_name": "Example Pediatrics",
                "fhir_base": "https://athena.example.test/fhir/r4"}]
        }))
        .unwrap();
        let idx = PersonIndex::new("p_x");
        let from = crate::sources::Sources::of(&h.sources, &idx);
        let source = |org: Value| {
            let mut r = result(0, "2026-12-28");
            r.provenance = vec![serde_json::from_value(serde_json::json!({
                "source": {"kind": "fhir", "cli": "athena", "organization_id": org,
                           "fhir_base": "https://athena.example.test/fhir/r4",
                           "cli_version": "0.6.2"},
                "method": "fhir-api", "filed_at": "2026-11-20T10:00:00Z", "filed_by": "medrec 0.9.4"
            }))
            .unwrap()];
            serde_json::to_string(&slim(&r, from)).unwrap()
        };
        // fhir_base, cli, organization_id and label, in that order, nothing
        // else.
        let slimmed = source("900001".into());
        assert!(
            slimmed.ends_with(
                r#""provenance":[{"source":{"fhir_base":"https://athena.example.test/fhir/r4","cli":"athena","organization_id":"900001","label":"Example Pediatrics"}}]}"#
            ),
            "{slimmed}"
        );
        // A source with none says so with null.
        let none = source(Value::Null);
        assert!(
            none.ends_with(
                r#""fhir_base":"https://athena.example.test/fhir/r4","cli":"athena","organization_id":null,"label":"athena"}}]}"#
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
