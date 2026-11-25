//! `medrec reindex`: every claim shaped again from the raw resource filed
//! for it, as this medrec's import shapes it, so claims no source serves any
//! more take the current shape too; what the sources' raw files say of the
//! providers each record prints ([`crate::providers`]); and every bill's and
//! receipt's statement read again from its stored text for the fields it
//! lacks. It reads only the folder.

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
    pub people: Vec<PersonReindexed>,
    pub claims: usize,
    pub reshaped: usize,
    pub unchanged: usize,
    /// Claims with no raw file to shape them from: filed before raw files
    /// were kept, or by `record add`.
    pub without_raw: usize,
    /// Bills and receipts whose statement gained fields from their text.
    pub statements: usize,
    /// Records whose providers' NPI or state changed.
    pub providers_updated: usize,
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
/// changed.
pub fn claims(folder: &Folder, h: &Household, person: Option<&str>) -> Result<Reindexed, CliError> {
    let tmp = pk_cli_drive::private_temp_dir("medrec-reindex")?;
    let mut out = Reindexed::default();
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
        row.providers_updated = providers(folder, &mut idx, tmp.path(), &dirs)?;
        row.statements = crate::bill::backfill(&mut idx, &folder.text(p)?);
        if row.reshaped > 0 || row.statements > 0 || row.providers_updated > 0 {
            folder.save_index(p, &mut idx)?;
        }
        out.statements += row.statements;
        out.providers_updated += row.providers_updated;
        out.claims += row.claims;
        out.reshaped += row.reshaped;
        out.unchanged += row.unchanged;
        out.without_raw += row.without_raw;
        out.people.push(row);
    }
    Ok(out)
}

/// What each record's raw file says of the providers it prints, and the
/// resources its source filed. Records changed, counted.
fn providers(
    folder: &Folder,
    idx: &mut PersonIndex,
    tmp: &Path,
    fetched: &BTreeSet<String>,
) -> Result<usize, CliError> {
    let sources: BTreeSet<String> = idx
        .records
        .iter()
        .flat_map(|r| &r.provenance)
        .filter_map(|pr| source_of(pr.raw_path.as_ref()?))
        .map(str::to_string)
        .collect();
    let mut held = Held {
        tmp,
        source: String::new(),
        base: None,
        read: HashMap::new(),
    };
    for source in &sources {
        for ty in RAW_TYPES {
            let dir = format!("{source}/{ty}");
            if !fetched.contains(&dir) && folder.store.exists(&dir)? {
                folder.store.fetch_tree(&dir, &tmp.join(&dir))?;
            }
        }
    }
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
