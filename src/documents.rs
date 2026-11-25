//! `documents merge`: fold a document filed twice into the one it renders
//! again, for copies filed before an import recognized renderings (see
//! [`crate::import::not_a_rendering_of`]).

use pk_cli_core::CliError;
use serde::Serialize;

use crate::folder::Folder;
use crate::given::{self, GivenPaper};
use crate::import::not_a_rendering_of;
use crate::model::{Household, Person, PersonIndex, PersonText, Rendering};

#[derive(Debug, Serialize)]
pub struct Merged {
    /// The document kept.
    pub document: String,
    /// The document merged into it, now gone from the index.
    pub merged: String,
    /// The merged document's file, as it was in the records folder.
    pub path: String,
    /// Whether its file was there to remove. On Drive it goes to the trash.
    pub file_removed: bool,
    /// Records, imaging studies and notes that named the merged document
    /// and now name the kept one.
    pub references_moved: usize,
}

/// A merge every check has passed: [`plan`] reads, [`apply`] writes.
pub struct Plan {
    person: Person,
    index: PersonIndex,
    texts: PersonText,
    duplicate: String,
    into: String,
}

/// Check that `duplicate` can merge into `into`, writing nothing: both
/// filed, for one person, and `duplicate` a rendering of `into` by the rule
/// an import uses.
pub fn plan(folder: &Folder, h: &Household, duplicate: &str, into: &str) -> Result<Plan, CliError> {
    if duplicate == into {
        return Err(CliError::Usage(
            "a document cannot be merged into itself".into(),
        ));
    }
    let (mut dup_at, mut into_at) = (None, None);
    for p in &h.people {
        let index = folder.whole_index(p)?;
        if index.documents.iter().any(|d| d.id == duplicate) {
            dup_at = Some((p.clone(), index.clone()));
        }
        if index.documents.iter().any(|d| d.id == into) {
            into_at = Some((p.clone(), index));
        }
    }
    let missing: Vec<&str> = [(duplicate, dup_at.is_none()), (into, into_at.is_none())]
        .into_iter()
        .filter_map(|(id, gone)| gone.then_some(id))
        .collect();
    if !missing.is_empty() {
        return Err(CliError::NotFound(format!(
            "no document {}; `medrec documents list` shows them",
            missing
                .iter()
                .map(|id| format!("`{id}`"))
                .collect::<Vec<_>>()
                .join(" or ")
        )));
    }
    let (person, index) = into_at.expect("checked above");
    let (dup_person, _) = dup_at.expect("checked above");
    if dup_person.id != person.id {
        return Err(CliError::Usage(format!(
            "`{duplicate}` and `{into}` are filed for different people; nothing was changed"
        )));
    }
    let texts = folder.text(&person)?;
    let find = |id: &str| index.documents.iter().find(|d| d.id == id);
    let (Some(dup), Some(kept)) = (find(duplicate), find(into)) else {
        return Err(CliError::Other(
            "the index changed while it was read; run the merge again".into(),
        ));
    };
    if let Some(why) = not_a_rendering_of(
        kept,
        texts.texts.get(into).map(String::as_str),
        &dup.provenance,
        &dup.media_type,
        texts.texts.get(duplicate).map_or("", String::as_str),
    ) {
        return Err(CliError::Usage(format!(
            "`{duplicate}` is not a rendering of `{into}`: {why}; nothing was changed"
        )));
    }
    Ok(Plan {
        person,
        index,
        texts,
        duplicate: duplicate.to_string(),
        into: into.to_string(),
    })
}

/// Merge as `plan` checked. `duplicate`'s hash, size and fetch time, and
/// its own renderings, go on `into`'s `renderings`, each distinct one once,
/// and what else served its bytes, said and gave of it, on `into`;
/// everything that named it names `into`; its text and index entry go, and
/// then its file. Each step leaves the folder consistent if the next one
/// fails: notes name `into` beside `duplicate` before the index loses
/// `duplicate`, they lose it only after, and the file goes last, so a
/// failure leaves at most an unindexed file for `verify` to report.
pub fn apply(folder: &Folder, plan: Plan) -> Result<Merged, CliError> {
    let Plan {
        person,
        mut index,
        mut texts,
        duplicate,
        into,
    } = plan;
    let mut notes = folder.notes(&person)?;
    let named: Vec<usize> = notes
        .iter()
        .enumerate()
        .filter(|(_, n)| n.about.contains(&duplicate))
        .map(|(i, _)| i)
        .collect();
    for &i in &named {
        if !notes[i].about.contains(&into) {
            notes[i].about.push(into.clone());
        }
    }
    if !named.is_empty() {
        folder.save_notes(&person, &notes)?;
    }

    let dup_at = index
        .documents
        .iter()
        .position(|d| d.id == duplicate)
        .ok_or_else(|| CliError::Other("the index changed; run the merge again".into()))?;
    let dup = index.documents.remove(dup_at);
    let kept = index
        .documents
        .iter_mut()
        .find(|d| d.id == into)
        .ok_or_else(|| CliError::Other("the index changed; run the merge again".into()))?;
    let copy = Rendering {
        sha256: dup.sha256.clone(),
        size: dup.size,
        fetched_at: dup.provenance.fetched_at.clone(),
    };
    for r in std::iter::once(copy).chain(dup.renderings.iter().cloned()) {
        if r.sha256 != kept.sha256 && !kept.renderings.iter().any(|k| k.sha256 == r.sha256) {
            kept.renderings.push(r);
        }
    }
    // Its own provenance is of the kept paper's Binary (`plan`).
    for p in dup.also_from {
        if !kept.provenances().any(|k| k.raw_path == p.raw_path) {
            kept.also_from.push(p);
        }
    }
    for w in &dup.words {
        if !kept.words.iter().any(|k| k.raw_path == w.raw_path) {
            kept.say(w);
        }
    }
    for e in dup.errors {
        if !kept.errors.contains(&e) {
            kept.errors.push(e);
        }
    }
    let called = GivenPaper {
        raw_path: dup.provenance.raw_path,
        organization: dup.organization,
        title: dup.title,
    };
    for g in std::iter::once(called).chain(dup.given) {
        given::keep_paper(kept, g);
    }
    let moved = repoint(&mut index, &duplicate, &into) + named.len();
    texts.texts.remove(&duplicate);
    folder.save_index(&person, &mut index)?;
    folder.save_text(&person, &texts)?;

    if !named.is_empty() {
        for &i in &named {
            notes[i].about.retain(|a| *a != duplicate);
        }
        folder.save_notes(&person, &notes)?;
    }
    let file_removed = folder.store.delete(&dup.path)?;
    Ok(Merged {
        document: into,
        merged: duplicate,
        path: dup.path,
        file_removed,
        references_moved: moved,
    })
}

/// Point every place `index` names the document `from` at `to`: records'
/// `documents`, imaging studies' `report`; a near-duplicate link to `from`
/// goes. How many records and studies changed.
fn repoint(index: &mut PersonIndex, from: &str, to: &str) -> usize {
    let mut moved = 0;
    for r in index.records.iter_mut() {
        if r.documents.iter().any(|d| d == from) {
            r.documents.retain(|d| d != from);
            if !r.documents.iter().any(|d| d == to) {
                r.documents.push(to.to_string());
            }
            moved += 1;
        }
    }
    for s in index.studies.iter_mut() {
        if s.report.as_deref() == Some(from) {
            s.report = Some(to.to_string());
            moved += 1;
        }
    }
    for d in index.documents.iter_mut() {
        d.near_duplicates.retain(|n| n.document != from);
    }
    moved
}
