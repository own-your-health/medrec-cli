//! `medrec reindex`: every claim shaped again from the raw resource filed
//! for it, as this medrec's import shapes it, so claims no source serves any
//! more take the current shape too; what the sources' raw files say of the
//! providers each record prints ([`crate::providers`]); the fields of every
//! condition, allergy and medicine; what each source says of every record and
//! paper that keeps it; the results that are about somebody else; the other
//! sources that served a paper's bytes; and every bill's and receipt's
//! statement read again from its stored text for the fields it lacks. It reads
//! only the folder.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

use pk_cli_core::CliError;
use serde::Serialize;
use serde_json::Value;

use crate::about;
use crate::claims::without_links;
use crate::folder::Folder;
use crate::given::{self, Given};
use crate::import;
use crate::model::{Household, InError, PersonIndex, Provenance, Record, Word};
use crate::normalize;
use crate::people;

#[derive(Debug, Default, Serialize)]
pub struct Reindexed {
    /// Nothing was written.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub dry_run: bool,
    pub people: Vec<PersonReindexed>,
    pub claims: usize,
    pub reshaped: usize,
    pub unchanged: usize,
    /// Claims with no raw file to shape them from: filed before raw files
    /// were kept, or by `record add`.
    pub without_raw: usize,
    /// Bills and receipts whose statement gained fields from their text.
    pub statements: usize,
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
    pub providers_updated: usize,
    pub statuses_updated: usize,
    pub given_updated: usize,
    pub results_split: usize,
    pub papers_updated: usize,
    pub papers_unread: usize,
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

/// Shape every claim of `person` (everyone's by default) again from the raw
/// file it is of. A claim the shaping would give another `key` is left as it
/// is: re-keying is an import's to do. Bills and
/// receipts gain the statement fields their text gives and they lack
/// ([`crate::bill::backfill`]). The index is written only when something
/// changed, and never when `dry_run`: then this counts what it would do.
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
    for p in people::selected(h, person)? {
        let mut idx = folder.whole_index(p)?;
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
            folder.store.fetch_tree(d, &tmp.path().join(d))?;
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
        row.results_split = apart(&mut idx, tmp.path())?;
        let showed: Vec<Given> = idx.records.iter().map(import::showed).collect();
        row.statuses_updated = statuses(&mut idx, tmp.path())?;
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
        row.statements = crate::bill::backfill(&mut idx, &folder.text(p)?);
        let changed = [
            row.reshaped,
            row.statements,
            row.providers_updated,
            row.statuses_updated,
            row.given_updated,
            row.results_split,
            row.papers_updated,
        ];
        if !dry_run && changed.iter().any(|n| *n > 0) {
            folder.save_index(p, &mut idx)?;
        }
        out.statements += row.statements;
        out.providers_updated += row.providers_updated;
        out.statuses_updated += row.statuses_updated;
        out.given_updated += row.given_updated;
        out.results_split += row.results_split;
        out.papers_updated += row.papers_updated;
        out.papers_unread += row.papers_unread;
        out.claims += row.claims;
        out.reshaped += row.reshaped;
        out.unchanged += row.unchanged;
        out.without_raw += row.without_raw;
        out.people.push(row);
    }
    Ok(out)
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
            let to = tmp.join(&dir);
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
    let to = tmp.join(dir);
    let missing: Vec<&str> = files
        .iter()
        .map(AsRef::as_ref)
        .filter(|rel| !tmp.join(rel).exists())
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
/// files: those filed by an import that did not finish were never read in.
/// Records changed, counted.
pub(crate) fn given_by(
    folder: &Folder,
    idx: &mut PersonIndex,
    source_dir: &str,
) -> Result<usize, CliError> {
    let tmp = pk_cli_drive::private_temp_dir("medrec-given")?;
    let sources = BTreeSet::from([source_dir.to_string()]);
    let mut wanted: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    let of = idx.records.iter().flat_map(|r| &r.provenance);
    for rel in of.filter_map(|p| p.raw_path.as_deref()) {
        if import::source_dir_of(rel) != Some(source_dir) {
            continue;
        }
        if let Some((dir, _)) = rel.rsplit_once('/') {
            wanted.entry(dir).or_default().insert(rel);
        }
    }
    for (dir, files) in wanted {
        let files: Vec<&str> = files.into_iter().collect();
        fetch_files(folder, tmp.path(), dir, &files)?;
    }
    fetch(folder, &sources, tmp.path(), &BTreeSet::new(), RAW_TYPES)?;
    let older = fetch_older(folder, idx, &sources, tmp.path())?;
    let showed: Vec<Given> = idx.records.iter().map(import::showed).collect();
    given(idx, &showed, &older, tmp.path())
}

/// Each resource about somebody else that was filed with the person's own in
/// one record, in a record of its own. How many records had one.
fn apart(idx: &mut PersonIndex, tmp: &Path) -> Result<usize, CliError> {
    let shape = |p: &Provenance| {
        let v = read(tmp, p.raw_path.as_ref()?)?;
        normalize::record(&v, p.clone())
    };
    let mut moved: Vec<Record> = Vec::new();
    let mut split = 0;
    for r in &mut idx.records {
        if r.kind != "result" || r.provenance.len() < 2 {
            continue;
        }
        let other = |p: &Provenance| shape(p).filter(|n| n.fields.contains_key(about::FIELD));
        let others: Vec<Record> = r.provenance.iter().filter_map(other).collect();
        if others.is_empty() || others.len() == r.provenance.len() {
            continue;
        }
        split += 1;
        let of = |path: &Option<String>| {
            let mut others = others.iter();
            path.is_some() && others.any(|n| n.provenance[0].raw_path == *path)
        };
        let wrote = of(&import::wrote(r).map(str::to_string));
        r.provenance.retain(|p| !of(&p.raw_path));
        for mut n in others {
            let path = n.provenance[0].raw_path.clone();
            let its = |p: &String| path.as_ref() == Some(p);
            let kept = r.words.iter().filter(|w| w.in_error() && its(&w.raw_path));
            n.words = kept.cloned().collect();
            let kept = r.errors.iter().filter(|e| its(&e.raw_path));
            n.errors = kept.cloned().collect();
            moved.push(n);
        }
        let left: Vec<String> = r
            .provenance
            .iter()
            .filter_map(|p| p.raw_path.clone())
            .collect();
        r.words.retain(|w| left.contains(&w.raw_path));
        r.errors.retain(|e| left.contains(&e.raw_path));
        if wrote {
            import::values_again(r, &mut |p| Ok(shape(p)))?;
        }
    }
    for n in moved {
        match idx.records.iter_mut().find(|r| r.key == n.key) {
            Some(r) => r.provenance.extend(n.provenance),
            None => idx.records.push(n),
        }
    }
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
                    ..given::of(&new)
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

/// Whether a record's status and fields are of this shape of one of its raw
/// files: it has each, but the prescriber import finds elsewhere.
fn agrees(shape: &Record, r: &Record) -> bool {
    if r.kind == "claim" {
        return content(shape) == content(r);
    }
    let said = |(k, v): (&String, &Value)| k == "prescriber" || shape.fields.get(k) == Some(v);
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
    let text = std::fs::read_to_string(tmp.join(rel)).ok()?;
    serde_json::from_str(&text).ok()
}

/// The raw files providers come from: the kinds that print one, and what
/// they point at.
const RAW_TYPES: &[&str] = &[
    "Observation",
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
}
