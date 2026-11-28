//! `medrec reindex`: every claim shaped again from the raw resource filed
//! for it, as this medrec's import shapes it, so claims no source serves any
//! more take the current shape too; what the sources' raw files say of the
//! providers each record prints ([`crate::providers`]); the fields of every
//! condition, allergy, medicine, result and report; what each source says of
//! every record and paper that keeps it; the results that are about somebody else; the other
//! sources that served a paper's bytes; and every bill's and receipt's
//! statement read again from its stored text for the fields it lacks. It reads
//! only the folder.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

use pk_cli_core::CliError;
use serde::Serialize;
use serde_json::{json, Value};

use crate::about;
use crate::changes::{self, Changes};
use crate::claims::without_links;
use crate::folder::{self, Folder};
use crate::given::{self, Given};
use crate::import;
use crate::model::{today, Household, InError, Person, PersonIndex, Provenance, Record, Word};
use crate::normalize;
use crate::panels;
use crate::people;
use crate::store;
use crate::viewfiles;

#[derive(Debug, Default, Serialize)]
pub struct Reindexed {
    /// Nothing was written.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub dry_run: bool,
    /// Of a dry run: every index is in the folder, at its end, as it was read.
    /// One that is not is an error.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fresh: Option<bool>,
    pub people: Vec<PersonReindexed>,
    /// Of a dry run of one person's records: what would change in everyone
    /// else's views, which a run makes again too.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub others: Vec<OtherViews>,
    pub claims: usize,
    pub reshaped: usize,
    pub unchanged: usize,
    /// Claims with no raw file to shape them from: filed before raw files
    /// were kept, or by `record add`.
    pub without_raw: usize,
    /// Bills and receipts whose statement gained fields from their text.
    pub statements: usize,
    /// Papers whose follow-ups or drugs, read from their text, changed
    /// (0.9.12 and later).
    pub paper_fields: usize,
    /// Records whose providers' NPI or address changed.
    pub providers_updated: usize,
    /// Records whose fields by another source, or what a source says of them,
    /// changed, and conditions, allergies and medicines whose fields did.
    pub statuses_updated: usize,
    /// Records that keep something more that a source gave and they do not
    /// show.
    pub given_updated: usize,
    /// Results that were filed with one about somebody else.
    pub results_split: usize,
    /// Results and reports whose values, or what they list, changed: their
    /// fields, status, name, code, date or `lists`.
    pub results_updated: usize,
    /// Records whose links to others are made otherwise: a claim's, a panel's,
    /// and the questionnaires that list a result.
    pub links_updated: usize,
    /// Papers whose sources, or what one says or gave of them, changed.
    pub papers_updated: usize,
    /// Raw files of papers that do not read.
    pub papers_unread: usize,
}

#[derive(Debug, Default, Serialize)]
pub struct PersonReindexed {
    pub person: String,
    pub claims: usize,
    pub reshaped: usize,
    pub unchanged: usize,
    pub without_raw: usize,
    pub statements: usize,
    pub paper_fields: usize,
    pub providers_updated: usize,
    pub statuses_updated: usize,
    pub given_updated: usize,
    pub results_split: usize,
    /// Results and reports whose values, or what they list, changed: their
    /// fields, status, name, code, date or `lists`.
    pub results_updated: usize,
    /// Records whose links to others are made otherwise: a claim's, a panel's,
    /// and the questionnaires that list a result.
    pub links_updated: usize,
    pub papers_updated: usize,
    pub papers_unread: usize,
    /// Of a dry run: what would change in the person's views.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub views: Option<Changes>,
}

/// A person whose records a run would leave, and whose views it would make
/// again.
#[derive(Debug, Serialize)]
pub struct OtherViews {
    pub person: String,
    pub views: Changes,
}

/// Refuse to count when another command wrote a person's index since it was
/// read, with this MD5.
fn as_read_still(folder: &Folder, p: &Person, md5: Option<&str>) -> Result<(), CliError> {
    let rel = folder::index_rel(p);
    if folder.store.md5(&rel)?.as_deref() == md5 {
        return Ok(());
    }
    Err(CliError::Other(format!(
        "{rel} was written elsewhere while this read it, so the counts would be of neither: run this again"
    )))
}

/// The claim's own content, links left out.
fn content(r: &Record) -> impl PartialEq + '_ {
    (
        &r.display,
        &r.status,
        &r.code,
        &r.date,
        without_links(&r.fields),
    )
}

/// What a record says of others, which is made of the index.
fn links(r: &Record) -> Value {
    if r.kind != "claim" {
        return json!([r.fields.get(panels::MEMBERS), r.listed_by]);
    }
    let lines = r.fields.get("items").and_then(Value::as_array);
    let lines = lines.into_iter().flatten().enumerate();
    let lines = lines.filter_map(|(i, l)| Some((i, l.get("records")?)));
    let lines: Vec<(usize, &Value)> = lines.collect();
    json!([r.fields.get("visit"), r.fields.get("results"), lines])
}

/// Shape every claim of `person` (everyone's by default) again from the raw
/// file it is of. A claim the shaping would give another `key` is left as it
/// is: re-keying is an import's to do. Bills and
/// receipts gain the statement fields their text gives and they lack
/// ([`crate::bill::backfill`]). The index is written only when something
/// changed, and never when `dry_run`: then this counts what it would do, and
/// what the views would say otherwise ([`crate::changes`]). The views are
/// brought up to date whether or not an index is written.
pub fn claims(
    folder: &Folder,
    h: &Household,
    person: Option<&str>,
    dry_run: bool,
) -> Result<Reindexed, CliError> {
    let tmp = pk_cli_drive::private_temp_dir("medrec-reindex")?;
    let mut out = Reindexed {
        dry_run,
        ..Reindexed::default()
    };
    let mut as_read: Vec<(&Person, PersonIndex, Option<String>)> = Vec::new();
    let mut would_be: BTreeMap<String, PersonIndex> = BTreeMap::new();
    for p in people::selected(h, person)? {
        let (mut idx, md5) = folder.whole_index_read(p)?;
        if dry_run {
            as_read.push((p, idx.clone(), md5));
        }
        let linked = idx.records.iter().map(|r| (r.id.clone(), links(r)));
        let linked: BTreeMap<String, Value> = linked.collect();
        let raw_of = |r: &Record| r.provenance.iter().find_map(|pr| pr.raw_path.clone());
        // One copy per folder of raw files, not one call per file.
        let dirs: BTreeSet<String> = idx
            .records
            .iter()
            .filter(|r| r.kind == "claim")
            .filter_map(raw_of)
            .filter_map(|rel| rel.rsplit_once('/').map(|(d, _)| d.to_string()))
            .collect();
        for d in &dirs {
            // A path no index can lead out of the copy by.
            if let Some(to) = store::within(tmp.path(), d) {
                folder.store.fetch_tree(d, &to)?;
            }
        }
        let mut row = PersonReindexed {
            person: p.id.clone(),
            ..PersonReindexed::default()
        };
        let sources = sources(&idx);
        fetch(folder, &sources, tmp.path(), &dirs, normalize::WORD_TYPES)?;
        fetch(folder, &sources, tmp.path(), &dirs, RAW_TYPES)?;
        fetch_said(folder, &idx, tmp.path())?;
        let older = fetch_older(folder, &idx, &sources, tmp.path())?;
        row.results_split = apart(&mut idx, &older, tmp.path())?;
        let showed: Vec<Given> = idx.records.iter().map(import::showed).collect();
        row.statuses_updated = statuses(&mut idx, tmp.path())?;
        row.results_updated = results(&mut idx, tmp.path());
        for r in idx.records.iter_mut().filter(|r| r.kind == "claim") {
            row.claims += 1;
            let of = import::writer(r).map(str::to_string);
            let shaped = r
                .provenance
                .iter()
                .find(|pr| match &of {
                    Some(path) => pr.raw_path.as_ref() == Some(path),
                    None => pr.raw_path.is_some(),
                })
                .and_then(|pr| {
                    normalize::record(&read(tmp.path(), pr.raw_path.as_ref()?)?, pr.clone())
                });
            let Some(new) = shaped else {
                row.without_raw += 1;
                continue;
            };
            if new.key != r.key || content(&new) == content(r) {
                row.unchanged += 1;
                continue;
            }
            r.display = new.display;
            r.status = new.status;
            r.code = new.code;
            r.date = new.date;
            r.fields = new.fields;
            row.reshaped += 1;
        }
        // After the fields they are of.
        row.providers_updated = providers(&mut idx, tmp.path())?;
        row.given_updated = given(&mut idx, &showed, &older, tmp.path())?;
        let papers = import::papers_again(folder, h, p, &mut idx, tmp.path())?;
        row.papers_updated = papers.changed;
        row.papers_unread = papers.unread;
        let texts = folder.text(p)?;
        row.statements = crate::bill::backfill(&mut idx, &texts);
        row.paper_fields = crate::paper::backfill(&mut idx, &texts);
        // As a saved index is: this medrec's rules can link it otherwise.
        crate::claims::link(&mut idx.records);
        crate::panels::link(&mut idx.records);
        let other = |r: &&Record| linked.get(&r.id).is_some_and(|was| *was != links(r));
        row.links_updated = idx.records.iter().filter(other).count();
        let changed = [
            row.reshaped,
            row.statements,
            row.paper_fields,
            row.providers_updated,
            row.statuses_updated,
            row.given_updated,
            row.results_split,
            row.results_updated,
            row.links_updated,
            row.papers_updated,
        ];
        if !dry_run && changed.iter().any(|n| *n > 0) {
            folder.save_index(p, &mut idx)?;
        }
        if dry_run {
            would_be.insert(p.id.clone(), idx);
        }
        out.statements += row.statements;
        out.paper_fields += row.paper_fields;
        out.providers_updated += row.providers_updated;
        out.statuses_updated += row.statuses_updated;
        out.given_updated += row.given_updated;
        out.results_split += row.results_split;
        out.results_updated += row.results_updated;
        out.links_updated += row.links_updated;
        out.papers_updated += row.papers_updated;
        out.papers_unread += row.papers_unread;
        out.claims += row.claims;
        out.reshaped += row.reshaped;
        out.unchanged += row.unchanged;
        out.without_raw += row.without_raw;
        out.people.push(row);
    }
    if dry_run {
        count_views(folder, h, as_read, would_be, &mut out)?;
    } else {
        // What another medrec's rules made of the same files is made again too.
        folder.every_view();
    }
    Ok(out)
}

/// Count what would change in everyone's views: in the row of each person
/// whose records were read again, and in `others`. `as_read` is their indexes
/// as the folder has them, and `would_be` as a run would leave them.
fn count_views<'a>(
    folder: &Folder,
    h: &'a Household,
    mut as_read: Vec<(&'a Person, PersonIndex, Option<String>)>,
    mut would_be: BTreeMap<String, PersonIndex>,
    out: &mut Reindexed,
) -> Result<(), CliError> {
    for p in &h.people {
        if !would_be.contains_key(&p.id) {
            let (idx, md5) = folder.whole_index_read(p)?;
            would_be.insert(p.id.clone(), idx.clone());
            as_read.push((p, idx, md5));
        }
    }
    let views = viewfiles::preview(folder, &would_be)?;
    for (p, was, _) in &as_read {
        let (before, after) = (views.before(folder, p)?, views.after(p)?);
        let views = changes::of(p, was, &would_be[&p.id], &before, &after, &today());
        match out.people.iter_mut().find(|row| row.person == p.id) {
            Some(row) => row.views = Some(views),
            None => out.others.push(OtherViews {
                person: p.id.clone(),
                views,
            }),
        }
    }
    for (p, _, md5) in &as_read {
        as_read_still(folder, p, md5.as_deref())?;
    }
    out.fresh = Some(true);
    Ok(())
}

/// The folders of raw files the records are of, one per source.
fn sources(idx: &PersonIndex) -> BTreeSet<String> {
    let of = |pr: &Provenance| import::source_dir_of(pr.raw_path.as_ref()?).map(str::to_string);
    let records = idx.records.iter();
    records.flat_map(|r| &r.provenance).filter_map(of).collect()
}

/// A copy of these sources' raw files of these types, where there is none yet.
fn fetch(
    folder: &Folder,
    sources: &BTreeSet<String>,
    tmp: &Path,
    fetched: &BTreeSet<String>,
    types: &[&str],
) -> Result<(), CliError> {
    for source in sources {
        for ty in types {
            let dir = format!("{source}/{ty}");
            let Some(to) = store::within(tmp, &dir) else {
                continue;
            };
            if !fetched.contains(&dir) && !to.exists() && folder.store.exists(&dir)? {
                folder.store.fetch_tree(&dir, &to)?;
            }
        }
    }
    Ok(())
}

/// Whether a record of another kind than those whose word is always kept
/// keeps one, or would.
fn worded(r: &Record) -> bool {
    r.provenance.len() > 1
        || !r.words.is_empty()
        || !r.errors.is_empty()
        || crate::standing::record_in_error(r)
}

/// A copy of the raw files of the records that keep what their sources say
/// and are not of the types fetched whole.
fn fetch_said(folder: &Folder, idx: &PersonIndex, tmp: &Path) -> Result<(), CliError> {
    let mut wanted: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for r in idx.records.iter().filter(|r| worded(r)) {
        for rel in r.provenance.iter().filter_map(|p| p.raw_path.as_deref()) {
            if let Some((dir, _)) = rel.rsplit_once('/') {
                wanted.entry(dir).or_default().insert(rel);
            }
        }
    }
    for (dir, files) in wanted {
        let to = tmp.join(dir);
        if to.exists() {
            continue;
        }
        let files: Vec<&str> = files.into_iter().collect();
        fetch_files(folder, tmp, dir, &files)?;
    }
    Ok(())
}

/// How many files of one folder are read one by one.
const FEW: usize = 20;

/// The raw files of these sources that earlier versions of the records'
/// resources are kept in, by the current one's, and a copy of them.
fn fetch_older(
    folder: &Folder,
    idx: &PersonIndex,
    sources: &BTreeSet<String>,
    tmp: &Path,
) -> Result<BTreeMap<String, Vec<String>>, CliError> {
    let of = idx.records.iter().flat_map(|r| &r.provenance);
    let known: BTreeSet<&str> = of.filter_map(|p| p.raw_path.as_deref()).collect();
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for source in sources {
        let mut wanted: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for h in folder.store.hashes(source)? {
            let rel = h.rel;
            let Some(current) = import::current_of(&rel).filter(|c| known.contains(c.as_str()))
            else {
                continue;
            };
            let dir = rel
                .rsplit_once('/')
                .map_or(source.as_str(), |(d, _)| d)
                .to_string();
            wanted.entry(dir).or_default().push(rel.clone());
            out.entry(current).or_default().push(rel);
        }
        for (dir, files) in wanted {
            fetch_files(folder, tmp, &dir, &files)?;
        }
    }
    Ok(out)
}

/// A copy of some raw files of one folder. Few are one call each; many are a
/// copy of all of it, and so is what the papers read whole.
fn fetch_files(
    folder: &Folder,
    tmp: &Path,
    dir: &str,
    files: &[impl AsRef<str>],
) -> Result<(), CliError> {
    let Some(to) = store::within(tmp, dir) else {
        return Ok(());
    };
    let missing: Vec<&str> = files
        .iter()
        .map(AsRef::as_ref)
        .filter(|rel| store::within(tmp, rel).is_some_and(|at| !at.exists()))
        .collect();
    let papers = dir.rsplit('/').next().is_some_and(import::describes_papers);
    if missing.len() > FEW || (papers && !missing.is_empty()) {
        return folder.store.fetch_tree(dir, &to);
    }
    for rel in missing {
        if let Some(text) = folder.store.read_text(rel)? {
            crate::store::private_dirs(&to)?;
            crate::write_private(&tmp.join(rel), text.as_bytes())?;
        }
    }
    Ok(())
}

/// What a source gave of the records it states, by every version of its raw
/// files: those filed by an import that did not finish were never read in. One
/// that says it is about somebody else goes apart. Records changed, counted.
pub(crate) fn given_by(
    folder: &Folder,
    idx: &mut PersonIndex,
    source_dir: &str,
) -> Result<usize, CliError> {
    let tmp = pk_cli_drive::private_temp_dir("medrec-given")?;
    let sources = BTreeSet::from([source_dir.to_string()]);
    let mut wanted: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for r in &idx.records {
        let files = r.provenance.iter().filter_map(|p| p.raw_path.as_deref());
        let files: Vec<&str> = files.collect();
        // Another source's of the same record too: it can be what is left.
        let its = |rel: &&str| import::source_dir_of(rel) == Some(source_dir);
        if !files.iter().any(its) {
            continue;
        }
        for rel in files {
            if let Some((dir, _)) = rel.rsplit_once('/') {
                wanted.entry(dir).or_default().insert(rel);
            }
        }
    }
    for (dir, files) in wanted {
        let files: Vec<&str> = files.into_iter().collect();
        fetch_files(folder, tmp.path(), dir, &files)?;
    }
    fetch(folder, &sources, tmp.path(), &BTreeSet::new(), RAW_TYPES)?;
    let older = fetch_older(folder, idx, &sources, tmp.path())?;
    let split = apart(idx, &older, tmp.path())?;
    let showed: Vec<Given> = idx.records.iter().map(import::showed).collect();
    Ok(split + given(idx, &showed, &older, tmp.path())?)
}

/// Each resource about somebody else that was filed as the person's own, or
/// with it in one record, under a key of its own. How many records had one.
fn apart(
    idx: &mut PersonIndex,
    older: &BTreeMap<String, Vec<String>>,
    tmp: &Path,
) -> Result<usize, CliError> {
    let of_file = |file: &str, p: &Provenance| normalize::record(&read(tmp, file)?, p.clone());
    let shape = |p: &Provenance| of_file(p.raw_path.as_ref()?, p);
    // By any version of it, as an import knows it.
    let other = |r: &Record, p: &Provenance| {
        let path = p.raw_path.as_ref()?;
        let mut n = shape(p)?;
        if n.fields.contains_key(about::FIELD) {
            return Some(n);
        }
        let mut before = older.get(path).into_iter().flatten();
        let before = before.find_map(|f| about::of_resource(&of_file(f, p)?, Some(path)));
        let who = before.or_else(|| about::of_resource(r, Some(path)))?;
        n.key = about::key(&n.key, None);
        n.given.push(Given {
            raw_path: Some(path.clone()),
            fields: [(about::FIELD.to_string(), who.into())]
                .into_iter()
                .collect(),
            ..Given::default()
        });
        Some(n)
    };
    let mut moved: Vec<Record> = Vec::new();
    let mut split = 0;
    let mut records = Vec::new();
    for mut r in std::mem::take(&mut idx.records) {
        let apart = r.kind == "result" && !about::keyed(&r);
        let each = r.provenance.iter().filter(|_| apart);
        let mut others: Vec<Record> = each.filter_map(|p| other(&r, p)).collect();
        if others.is_empty() {
            records.push(r);
            continue;
        }
        split += 1;
        let wrote = import::wrote(&r).map(str::to_string);
        let of = |n: &Record, path: &Option<String>| n.provenance[0].raw_path == *path;
        let whole = others.len() == r.provenance.len();
        if whole {
            // The record is theirs, its values and all. Any other's goes.
            let its = others.iter().position(|n| of(n, &wrote)).unwrap_or(0);
            let key = others[its].key.clone();
            for n in others.iter().filter(|n| n.key == key) {
                for g in n.given.clone() {
                    given::keep(&mut r, g);
                }
            }
            others.retain(|n| n.key != key);
            r.key = key;
        }
        let gone = |path: &Option<String>| path.is_some() && others.iter().any(|n| of(n, path));
        r.provenance.retain(|p| !gone(&p.raw_path));
        let again = gone(&wrote);
        for n in &mut others {
            let path = n.provenance[0].raw_path.clone();
            let its = |p: &String| path.as_ref() == Some(p);
            let kept = r.words.iter().filter(|w| w.in_error() && its(&w.raw_path));
            n.words = kept.cloned().collect();
            let kept = r.errors.iter().filter(|e| its(&e.raw_path));
            n.errors = kept.cloned().collect();
            n.listed_by = r.listed_by.clone();
        }
        let left = r.provenance.iter().filter_map(|p| p.raw_path.clone());
        let left: Vec<String> = left.collect();
        r.words.retain(|w| left.contains(&w.raw_path));
        r.errors.retain(|e| left.contains(&e.raw_path));
        r.lists.retain(|path, _| left.contains(path));
        if again {
            import::values_again(&mut r, &mut |p| Ok(shape(p)))?;
        }
        match whole {
            true => moved.push(r),
            false => records.push(r),
        }
        moved.extend(others);
    }
    for n in moved {
        let Some(r) = records.iter_mut().find(|r: &&mut Record| r.key == n.key) else {
            records.push(n);
            continue;
        };
        for g in n.given.iter().cloned().chain([import::showed(&n)]) {
            given::keep(r, g);
        }
        for d in n.documents {
            if !r.documents.contains(&d) {
                r.documents.push(d);
            }
        }
        panels::keep(r, n.listed_by);
        r.lists.extend(n.lists);
        r.provenance.extend(n.provenance);
        r.words.extend(n.words);
        r.errors.extend(n.errors);
    }
    idx.records = records;
    Ok(split)
}

/// What the sources gave of each record that it does not show: by every
/// version of its raw files that is kept, and what it showed before this when
/// one of its sources' does not read. Records changed, counted.
fn given(
    idx: &mut PersonIndex,
    showed: &[Given],
    older: &BTreeMap<String, Vec<String>>,
    tmp: &Path,
) -> Result<usize, CliError> {
    let mut held = Held::new(tmp);
    let mut changed = 0;
    for (r, was) in idx.records.iter_mut().zip(showed) {
        let mut gave: Vec<Given> = Vec::new();
        let mut all_read = true;
        for pr in &r.provenance {
            let Some(rel) = &pr.raw_path else {
                all_read = false;
                continue;
            };
            let versions = older.get(rel).into_iter().flatten();
            for file in std::iter::once(rel).chain(versions) {
                let shaped = read(tmp, file).and_then(|v| {
                    let new = normalize::record(&v, pr.clone())?;
                    Some((v, new))
                });
                let Some((v, mut new)) = shaped.filter(|(_, n)| n.kind == r.kind) else {
                    all_read &= file != rel;
                    continue;
                };
                held.source = import::source_dir_of(rel).unwrap_or_default().to_string();
                held.base = pr.source.fhir_base.clone();
                new.providers = crate::providers::of(&v, &new.fields, &mut held)?.refs;
                gave.push(Given {
                    raw_path: Some(rel.clone()),
                    ..given::gives(&new)
                });
            }
        }
        if !all_read {
            gave.push(was.clone());
        }
        let mut more = false;
        for g in gave {
            more |= given::keep(r, g);
        }
        changed += usize::from(more);
    }
    Ok(changed)
}

/// What each raw file says of the records that keep it, with its history, and
/// the fields of each condition, allergy and medicine again from the raw file
/// they are of. Records changed, counted. One whose `key` that would change
/// keeps its fields, and what a raw file that does not read said stays.
fn statuses(idx: &mut PersonIndex, tmp: &Path) -> Result<usize, CliError> {
    let mut changed = 0;
    for r in &mut idx.records {
        let stands = crate::standing::KINDS.contains(&r.kind.as_str());
        if !stands && !worded(r) {
            continue;
        }
        let mut now = r.clone();
        now.words.clear();
        let mut shapes: Vec<(usize, &String, Record)> = Vec::new();
        let mut all_read = true;
        for (i, pr) in r.provenance.iter().enumerate() {
            let Some(rel) = &pr.raw_path else { continue };
            let had = r.words.iter().find(|w| w.raw_path == *rel);
            let shaped = read(tmp, rel).and_then(|v| normalize::record(&v, pr.clone()));
            let Some(new) = shaped else {
                now.words.extend(had.cloned());
                all_read = false;
                continue;
            };
            for w in &new.words {
                InError::note(&mut now.errors, w, &import::first_read(pr));
                if import::keeps(r, w) {
                    let version = had.and_then(|h| h.version.clone());
                    now.words.push(Word {
                        version,
                        ..w.clone()
                    });
                }
            }
            if new.key == r.key {
                shapes.push((i, rel, new));
            }
        }
        let writers: Vec<&(usize, &String, Record)> = shapes
            .iter()
            .filter(|(i, ..)| import::can_write(&now, *i))
            .collect();
        // The one that wrote the fields, where the record says which. Else
        // the one they agree with, of those import lets write; none does
        // where one is shaped otherwise now, or where one did not read.
        let wrote = match import::writer(&now) {
            Some(path) => writers.iter().find(|(_, p, _)| *p == path),
            None => writers
                .iter()
                .find(|(_, _, n)| agrees(n, r))
                .or_else(|| writers.first().filter(|_| all_read && stands)),
        };
        if let Some((_, path, new)) = wrote {
            let several = r.provenance.len() > 1 && !now.words.is_empty();
            now.fields_from = several.then(|| (*path).clone());
            if stands {
                let in_error = new.words.iter().any(|w| w.in_error());
                let new = match in_error {
                    true => import::over(r, new.clone()),
                    false => new.clone(),
                };
                now.status = new.status;
                now.fields = new.fields;
                // A fill's is its prescription's.
                if let Some(p) = r.fields.get("prescriber") {
                    now.fields.entry("prescriber").or_insert(p.clone());
                }
            }
        }
        let of = |p: &Provenance| {
            let mut of = shapes
                .iter()
                .filter(|(_, rel, _)| p.raw_path.as_ref() == Some(*rel));
            of.next().map(|(_, _, n)| n.clone())
        };
        import::take_from_another(&mut now, &mut |p| Ok(of(p)))?;
        if now != *r {
            *r = now;
            changed += 1;
        }
    }
    Ok(changed)
}

/// The values of each result and report again, from the raw file they are of:
/// one that does not say it is entered in error, and would not give the record
/// another `key`. Of several sources' it is the one the record names, else one
/// its values agree with, else none. And what each of its raw files lists.
/// Records changed, counted.
fn results(idx: &mut PersonIndex, tmp: &Path) -> usize {
    let mut changed = 0;
    let each = idx.records.iter_mut();
    for r in each.filter(|r| matches!(r.kind.as_str(), "result" | "report")) {
        let mut now = r.clone();
        let of = |f: &String| r.provenance.iter().any(|p| p.raw_path.as_ref() == Some(f));
        now.lists.retain(|f, _| of(f));
        let mut shapes: Vec<Option<Record>> = Vec::new();
        for p in &r.provenance {
            let path = p.raw_path.as_ref();
            let shape = path.and_then(|f| normalize::record(&read(tmp, f)?, p.clone()));
            let shape = shape.filter(|n| n.kind == r.kind);
            if let (Some(path), Some(new)) = (path, &shape) {
                now.lists.remove(path);
                now.lists.extend(new.lists.clone());
            }
            shapes.push(shape.filter(|n| {
                let in_error = n.words.iter().any(|w| w.in_error());
                n.key == r.key && !in_error
            }));
        }
        let its = match import::writer(r) {
            _ if shapes.len() == 1 => Some(0),
            Some(path) => {
                let of = |p: &Provenance| p.raw_path.as_deref() == Some(path);
                r.provenance.iter().position(of)
            }
            None => (0..shapes.len()).find(|i| {
                let agree = shapes[*i].as_ref().is_some_and(|n| agrees(n, r));
                agree && import::can_write(r, *i)
            }),
        };
        if let Some(new) = its.and_then(|i| shapes[i].take()) {
            now.display = new.display;
            now.status = new.status;
            now.code = new.code;
            now.date = new.date;
            now.fields = new.fields;
            // Whom it is about by another version, and what it lists, which is
            // made of the index when it is saved.
            for k in [about::FIELD, panels::MEMBERS] {
                if let Some(v) = r.fields.get(k) {
                    now.fields.entry(k).or_insert(v.clone());
                }
            }
        }
        if now != *r {
            *r = now;
            changed += 1;
        }
    }
    changed
}

/// Whether a record's status and fields are of this shape of one of its raw
/// files: it has each, but what is found elsewhere (a prescriber, whom another
/// version says it is about, the members of a panel), and what says that a
/// result's value is a bound, which medrec before 0.9.10 did not read.
fn agrees(shape: &Record, r: &Record) -> bool {
    if r.kind == "claim" {
        return content(shape) == content(r);
    }
    let read = |k: &str| {
        let mut v = shape.fields.get(k)?.clone();
        if let Some(of) = v.as_object_mut().filter(|_| k == "value") {
            of.remove("comparator");
        }
        Some(v)
    };
    let elsewhere = ["prescriber", about::FIELD, panels::MEMBERS];
    let said = |(k, v): (&String, &Value)| {
        let same = shape.fields.get(k) == Some(v) || read(k).as_ref() == Some(v);
        same || elsewhere.contains(&k.as_str())
    };
    shape.status == r.status && r.fields.iter().all(said)
}

/// What each record's raw file says of the providers it prints, and the
/// resources its source filed. Records changed, counted.
fn providers(idx: &mut PersonIndex, tmp: &Path) -> Result<usize, CliError> {
    let mut held = Held::new(tmp);
    let mut changed = 0;
    for r in &mut idx.records {
        let now = import::providers_again(r, &mut |pr| {
            let Some(rel) = &pr.raw_path else {
                return Ok(None);
            };
            let Some(v) = read(tmp, rel) else {
                return Ok(None);
            };
            held.source = import::source_dir_of(rel).unwrap_or_default().to_string();
            held.base = pr.source.fhir_base.clone();
            crate::providers::of(&v, &r.fields, &mut held).map(Some)
        })?;
        if now != r.providers {
            r.providers = now;
            changed += 1;
        }
    }
    Ok(changed)
}

/// A raw file, when it is there and reads.
fn read(tmp: &Path, rel: &str) -> Option<Value> {
    let text = std::fs::read_to_string(store::within(tmp, rel)?).ok()?;
    serde_json::from_str(&text).ok()
}

/// The raw files providers come from: the kinds that print one, and what
/// they point at.
const RAW_TYPES: &[&str] = &[
    "Observation",
    "DiagnosticReport",
    "Encounter",
    "MedicationRequest",
    "MedicationDispense",
    "Immunization",
    "Procedure",
    "ExplanationOfBenefit",
    "Practitioner",
    "PractitionerRole",
    "Organization",
    "Location",
];

struct Held<'a> {
    tmp: &'a Path,
    source: String,
    base: Option<String>,
    read: HashMap<String, Option<Value>>,
}

impl<'a> Held<'a> {
    fn new(tmp: &'a Path) -> Held<'a> {
        Held {
            tmp,
            source: String::new(),
            base: None,
            read: HashMap::new(),
        }
    }
}

impl crate::providers::Resolve for Held<'_> {
    fn base(&self) -> Option<&str> {
        self.base.as_deref()
    }

    fn resource(&mut self, resource_type: &str, id: &str) -> Result<Option<Value>, CliError> {
        let rel = format!(
            "{}/{}.json",
            self.source,
            crate::import::raw_sub_of(resource_type, id)
        );
        if let Some(v) = self.read.get(&rel) {
            return Ok(v.clone());
        }
        let v = read(self.tmp, &rel);
        self.read.insert(rel, v.clone());
        Ok(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A portal's two entries of one condition, and another portal's, as an
    /// index from before 0.9.9 has them: the fields of `wrote`.
    fn earlier(tag: &str, wrote: usize) -> (PersonIndex, std::path::PathBuf) {
        let tmp = std::env::temp_dir().join(format!("medrec-reindex-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let mut shapes: Vec<Record> = Vec::new();
        for (org, id, category) in [
            ("two", "a", "encounter-diagnosis"),
            ("two", "b", "problem-list-item"),
            ("one", "c", "problem-list-item"),
        ] {
            let v = json!({"resourceType": "Condition", "id": id,
                "clinicalStatus": {"coding": [{"code": "active"}]},
                "category": [{"coding": [{"code": category, "display": category}]}],
                "code": {"text": "Sample stiffness"}, "onsetDateTime": "2020-05-01"});
            let rel = format!("{org}/Condition/{id}.json");
            let path = tmp.join(&rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, v.to_string()).unwrap();
            let p: Provenance = serde_json::from_value(json!({
                "source": {"kind": "fhir", "cli": "fakeprov", "organization_id": org},
                "source_resource": {"type": "Condition", "id": id},
                "method": "fhir-api", "filed_at": "2024-01-02T10:00:00Z",
                "filed_by": "medrec 0.9.8", "raw_path": rel}))
            .unwrap();
            shapes.push(normalize::record(&v, p).unwrap());
        }
        let mut r = shapes[wrote].clone();
        r.provenance = shapes.iter().map(|s| s.provenance[0].clone()).collect();
        r.words.clear();
        r.fields.remove("categories");
        let mut idx = PersonIndex::new("p_test");
        idx.records.push(r);
        (idx, tmp)
    }

    /// A result two sources state, with the values of the second, as an index
    /// from before 0.9.10 has it, and one of their raw files as it is now.
    fn earlier_result(tag: &str, second: Value) -> (PersonIndex, std::path::PathBuf) {
        let tmp = std::env::temp_dir().join(format!("medrec-results-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let of = |org: &str, value: Value| {
            let mut v = json!({"resourceType": "Observation", "id": "a", "status": "final",
                "code": {"text": "Example level"}, "effectiveDateTime": "2024-01-01T08:00:00Z"});
            v["valueQuantity"] = value;
            v["status"] = if org == "one" { "final" } else { "corrected" }.into();
            let rel = format!("{org}/Observation/a.json");
            let path = tmp.join(&rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, v.to_string()).unwrap();
            let p: Provenance = serde_json::from_value(json!({
                "source": {"kind": "fhir", "cli": "fakeprov", "organization_id": org},
                "source_resource": {"type": "Observation", "id": "a"},
                "method": "fhir-api", "filed_at": "2024-01-02T10:00:00Z",
                "filed_by": "medrec 0.9.8", "raw_path": rel}))
            .unwrap();
            normalize::record(&v, p).unwrap()
        };
        let one = of("one", json!({"value": 5, "unit": "mg/dL"}));
        let mut r = of("two", second);
        assert_eq!(one.key, r.key);
        r.provenance.insert(0, one.provenance[0].clone());
        r.words.clear();
        r.fields_from = None;
        if let Some(v) = r.fields["value"].as_object_mut() {
            v.remove("comparator");
        }
        let mut idx = PersonIndex::new("p_test");
        idx.records.push(r);
        (idx, tmp)
    }

    /// The values of a result stay those of the source they are of.
    #[test]
    fn a_results_values_come_of_no_other_source() {
        let bound = json!({"value": 5, "unit": "mg/dL", "comparator": "<"});
        let (mut idx, tmp) = earlier_result("bound", bound);
        statuses(&mut idx, &tmp).unwrap();
        assert_eq!(results(&mut idx, &tmp), 1);
        let r = &idx.records[0];
        assert_eq!(r.fields["value"]["comparator"], "<");
        assert_eq!(r.status.as_deref(), Some("corrected"));
        assert_eq!(results(&mut idx, &tmp), 0);
        // Its raw file is not there, or does not read: nothing of the other's.
        let other = json!({"value": 7, "unit": "mg/dL"});
        for (n, left) in [None, Some("{")].into_iter().enumerate() {
            let (mut idx, tmp) = earlier_result(&format!("lost-{n}"), other.clone());
            let file = tmp.join("two/Observation/a.json");
            match left {
                Some(text) => std::fs::write(&file, text).unwrap(),
                None => std::fs::remove_file(&file).unwrap(),
            }
            statuses(&mut idx, &tmp).unwrap();
            assert_eq!(results(&mut idx, &tmp), 0);
            assert_eq!(idx.records[0].fields["value"]["value"], 7);
        }
    }

    /// A version an import filed and did not read in gives the record its
    /// status with its values.
    #[test]
    fn a_results_status_comes_with_its_values() {
        let (mut idx, tmp) = earlier_result("status", json!({"value": 7, "unit": "mg/dL"}));
        idx.records[0].provenance.remove(0);
        let file = tmp.join("two/Observation/a.json");
        let mut v: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        v["status"] = "amended".into();
        v["valueQuantity"]["value"] = 8.into();
        std::fs::write(&file, v.to_string()).unwrap();
        assert_eq!(results(&mut idx, &tmp), 1);
        let r = &idx.records[0];
        assert_eq!(r.status.as_deref(), Some("amended"));
        assert_eq!(r.fields["value"]["value"], 8);
    }

    /// The one it names can write by import: a source's first on the record.
    #[test]
    fn the_writer_is_one_import_lets_write() {
        for (wrote, from) in [(0, "two/Condition/a.json"), (2, "one/Condition/c.json")] {
            let (mut idx, tmp) = earlier(&format!("writer-{wrote}"), wrote);
            assert_eq!(statuses(&mut idx, &tmp).unwrap(), 1);
            let r = &idx.records[0];
            assert_eq!(r.fields_from.as_deref(), Some(from));
            assert_eq!(r.words.len(), 3);
            assert_eq!(statuses(&mut idx, &tmp).unwrap(), 0);
            let _ = std::fs::remove_dir_all(&tmp);
        }
    }

    /// One that names a source's later entry names none: the fields are of
    /// the one they agree with that can write.
    #[test]
    fn a_later_entry_of_a_source_is_no_writer() {
        let (mut idx, tmp) = earlier("later", 2);
        idx.records[0].fields_from = Some("two/Condition/b.json".into());
        assert_eq!(import::writer(&idx.records[0]), None);
        assert_eq!(statuses(&mut idx, &tmp).unwrap(), 1);
        let from = idx.records[0].fields_from.as_deref();
        assert_eq!(from, Some("one/Condition/c.json"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Counts of an index another command wrote meanwhile would be of neither.
    #[test]
    fn an_index_written_meanwhile_is_not_counted() {
        use crate::folder::Access;
        use crate::store::{Location, Reads, Store, DEFAULT_BUDGET};

        let root = crate::testutil::temp_dir("reindex-meanwhile");
        let loc = Location::Dir(root.clone());
        let store = Store::open(&loc, Reads::Fresh, DEFAULT_BUDGET).unwrap();
        folder::create(&store).unwrap();
        let f = Folder::open_at(&loc, Access::Fresh, DEFAULT_BUDGET).unwrap();
        let p: Person = serde_json::from_value(json!({
            "id": "p_exampleaaaaaaaaaaaaa", "name": "Robin Example", "folder": "Robin Example",
            "adult": true, "created_at": "2024-01-02T00:00:00Z"
        }))
        .unwrap();
        // One that is not there is read as none, and stays none.
        let (_, none) = f.whole_index_read(&p).unwrap();
        assert!(none.is_none() && as_read_still(&f, &p, None).is_ok());
        let write = |rev: u64| {
            let mut idx = PersonIndex::new(&p.id);
            idx.rev = rev;
            let text = serde_json::to_string(&idx).unwrap();
            store.write_text(&folder::index_rel(&p), &text).unwrap();
        };
        write(1);
        assert!(as_read_still(&f, &p, None).is_err());
        let (idx, md5) = f.whole_index_read(&p).unwrap();
        assert_eq!(idx.rev, 1);
        assert!(as_read_still(&f, &p, md5.as_deref()).is_ok());
        write(2);
        let e = as_read_still(&f, &p, md5.as_deref()).unwrap_err();
        assert!(e.to_string().contains("run this again"), "{e}");
        std::fs::remove_dir_all(root).unwrap();
    }
}
