//! `medrec reindex`: every claim shaped again from the raw resource filed
//! for it, as this medrec's import shapes it, so claims no source serves any
//! more take the current shape too; what the sources' raw files say of the
//! providers each record prints ([`crate::providers`]); the fields of every
//! condition, allergy and medicine and what each of its sources says of it;
//! and every bill's and receipt's statement read again from its stored text
//! for the fields it lacks. It reads only the folder.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use pk_cli_core::CliError;
use serde::Serialize;
use serde_json::Value;

use crate::claims::without_links;
use crate::folder::Folder;
use crate::model::{Household, PersonIndex, Record};
use crate::normalize;
use crate::people;
use crate::providers::Found;

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
    /// Conditions, allergies and medicines whose fields, or what a source
    /// says of them, changed.
    pub statuses_updated: usize,
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

/// Shape every claim of `person` (everyone's by default) again from its
/// first provenance's raw file. A claim the shaping would give another
/// `key` is left as it is: re-keying is an import's to do. Bills and
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
        let mut idx = folder.index(p)?;
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
        for r in idx.records.iter_mut().filter(|r| r.kind == "claim") {
            row.claims += 1;
            let shaped = r
                .provenance
                .iter()
                .find(|pr| pr.raw_path.is_some())
                .and_then(|pr| {
                    let text =
                        std::fs::read_to_string(tmp.path().join(pr.raw_path.as_ref()?)).ok()?;
                    let v: Value = serde_json::from_str(&text).ok()?;
                    normalize::record(&v, pr.clone())
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
        // Before the providers, which are of the fields.
        fetch(folder, &idx, tmp.path(), &dirs, normalize::WORD_TYPES)?;
        row.statuses_updated = statuses(&mut idx, tmp.path());
        row.providers_updated = providers(folder, &mut idx, tmp.path(), &dirs)?;
        row.statements = crate::bill::backfill(&mut idx, &folder.text(p)?);
        let changed = [
            row.reshaped,
            row.statements,
            row.providers_updated,
            row.statuses_updated,
        ];
        if !dry_run && changed.iter().any(|n| *n > 0) {
            folder.save_index(p, &mut idx)?;
        }
        out.statements += row.statements;
        out.providers_updated += row.providers_updated;
        out.statuses_updated += row.statuses_updated;
        out.claims += row.claims;
        out.reshaped += row.reshaped;
        out.unchanged += row.unchanged;
        out.without_raw += row.without_raw;
        out.people.push(row);
    }
    Ok(out)
}

/// A copy of the raw files of these types, of every source, where there is
/// none yet.
fn fetch(
    folder: &Folder,
    idx: &PersonIndex,
    tmp: &Path,
    fetched: &BTreeSet<String>,
    types: &[&str],
) -> Result<(), CliError> {
    let sources: BTreeSet<&str> = idx
        .records
        .iter()
        .flat_map(|r| &r.provenance)
        .filter_map(|pr| source_of(pr.raw_path.as_ref()?))
        .collect();
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

/// The fields of each condition, allergy and medicine again from the raw file
/// they are of, and what each of its raw files says. Records changed, counted.
/// One whose `key` that would change keeps its fields, and what a raw file
/// that does not read said stays.
fn statuses(idx: &mut PersonIndex, tmp: &Path) -> usize {
    let mut changed = 0;
    for r in &mut idx.records {
        if !["condition", "allergy", "medication"].contains(&r.kind.as_str()) {
            continue;
        }
        let mut now = r.clone();
        now.words.clear();
        let mut shapes: Vec<(&String, Record)> = Vec::new();
        let mut all_read = true;
        for pr in &r.provenance {
            let Some(rel) = &pr.raw_path else { continue };
            let shaped = read(tmp, rel).and_then(|v| normalize::record(&v, pr.clone()));
            let Some(new) = shaped else {
                let had = r.words.iter().find(|w| w.raw_path == *rel);
                now.words.extend(had.cloned());
                all_read = false;
                continue;
            };
            now.words.extend(new.words.iter().cloned());
            if new.key == r.key {
                shapes.push((rel, new));
            }
        }
        // The one that wrote the fields, where the record says which. Else
        // the one they agree with; none does where one is shaped otherwise
        // now, or where one did not read.
        let of = |p: &String| r.provenance.iter().any(|q| q.raw_path.as_ref() == Some(p));
        let wrote = match r.fields_from.as_ref().filter(|p| of(p)) {
            Some(path) => shapes.iter().position(|(p, _)| *p == path),
            None => shapes
                .iter()
                .position(|(_, n)| agrees(n, r))
                .or_else(|| (all_read && !shapes.is_empty()).then_some(0)),
        };
        if let Some((path, new)) = wrote.map(|i| shapes.swap_remove(i)) {
            let several = r.provenance.len() > 1 && !new.words.is_empty();
            now.fields_from = several.then(|| path.clone());
            now.status = new.status;
            now.fields = new.fields;
            // A fill's is its prescription's.
            if let Some(p) = r.fields.get("prescriber") {
                now.fields.entry("prescriber").or_insert(p.clone());
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
/// files: it has each, but the prescriber import finds elsewhere.
fn agrees(shape: &Record, r: &Record) -> bool {
    let said = |(k, v): (&String, &Value)| k == "prescriber" || shape.fields.get(k) == Some(v);
    shape.status == r.status && r.fields.iter().all(said)
}

/// What each record's raw file says of the providers it prints, and the
/// resources its source filed. Records changed, counted.
fn providers(
    folder: &Folder,
    idx: &mut PersonIndex,
    tmp: &Path,
    fetched: &BTreeSet<String>,
) -> Result<usize, CliError> {
    fetch(folder, idx, tmp, fetched, RAW_TYPES)?;
    let mut held = Held {
        tmp,
        source: String::new(),
        base: None,
        read: HashMap::new(),
    };
    let mut changed = 0;
    for r in &mut idx.records {
        // Every source of the record, in the order it names them, so that
        // the first to say something of a provider they all print stands.
        let mut found = Found::default();
        for pr in &r.provenance {
            let Some(rel) = &pr.raw_path else { continue };
            let Some(v) = read(tmp, rel) else {
                found.unresolved.extend(crate::providers::FIELDS);
                continue;
            };
            held.source = source_of(rel).unwrap_or_default().to_string();
            held.base = pr.source.fhir_base.clone();
            found = found.and(crate::providers::of(&v, &r.fields, &mut held)?);
        }
        let now = found.over(&r.providers, &r.fields, &r.fields, false);
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

/// The source folder of `<source>/<Type>/<file>`.
fn source_of(rel: &str) -> Option<&str> {
    let (dir, _) = rel.rsplit_once('/')?;
    dir.rsplit_once('/').map(|(source, _)| source)
}

/// The raw files providers come from: the kinds that print one, and what
/// they point at.
const RAW_TYPES: &[&str] = &[
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
