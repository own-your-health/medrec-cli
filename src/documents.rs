//! `documents merge`: fold a document filed twice into the one it renders
//! again, for copies filed before an import recognized renderings (see
//! [`crate::import::not_a_rendering_of`]).

use pk_cli_core::CliError;
use serde::Serialize;

use crate::folder::Folder;
use crate::import::not_a_rendering_of;
use crate::model::{Household, Rendering};

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
    /// Records and notes that named the merged document and now name the
    /// kept one.
    pub references_moved: usize,
}

/// Merge `duplicate` into `into`: both one person's, and `duplicate` a
/// rendering of `into` by the rule an import uses. `duplicate`'s hash,
/// size and fetch time (and its own renderings) go on `into`'s
/// `renderings`; records and notes that named it name `into`; its text and
/// index entry go, and then its file. The index is saved before the file is
/// removed, so a failure leaves an unindexed file for `verify` to report,
/// never an index entry without its file.
pub fn merge(
    folder: &Folder,
    h: &Household,
    duplicate: &str,
    into: &str,
) -> Result<Merged, CliError> {
    if duplicate == into {
        return Err(CliError::Usage(
            "a document cannot be merged into itself".into(),
        ));
    }
    let mut found = None;
    for p in &h.people {
        let index = folder.index(p)?;
        let has = |id: &str| index.documents.iter().any(|d| d.id == id);
        match (has(duplicate), has(into)) {
            (true, true) => {
                found = Some((p.clone(), index));
                break;
            }
            (true, false) | (false, true) => {
                return Err(CliError::Usage(format!(
                    "`{duplicate}` and `{into}` are not both filed for the same person; nothing was changed"
                )))
            }
            (false, false) => {}
        }
    }
    let Some((person, mut index)) = found else {
        return Err(CliError::NotFound(format!(
            "no documents `{duplicate}` and `{into}`; `medrec documents list` shows them"
        )));
    };
    let mut texts = folder.text(&person)?;
    let dup_at = index
        .documents
        .iter()
        .position(|d| d.id == duplicate)
        .unwrap();
    let dup = index.documents[dup_at].clone();
    let kept = index.documents.iter().find(|d| d.id == into).unwrap();
    let dup_text = texts.texts.get(duplicate).cloned().unwrap_or_default();
    if let Some(why) = not_a_rendering_of(
        kept,
        texts.texts.get(into).map(String::as_str),
        &dup.provenance,
        &dup.media_type,
        &dup_text,
    ) {
        return Err(CliError::Usage(format!(
            "`{duplicate}` is not a rendering of `{into}`: {why}; nothing was changed"
        )));
    }

    let mut moved = 0;
    index.documents.remove(dup_at);
    for d in index.documents.iter_mut() {
        if d.id == into {
            let copy = Rendering {
                sha256: dup.sha256.clone(),
                size: dup.size,
                fetched_at: dup.provenance.fetched_at.clone(),
            };
            // Each distinct rendering once.
            for r in std::iter::once(copy).chain(dup.renderings.iter().cloned()) {
                if r.sha256 != d.sha256 && !d.renderings.iter().any(|k| k.sha256 == r.sha256) {
                    d.renderings.push(r);
                }
            }
        }
        d.near_duplicates.retain(|n| n.document != duplicate);
    }
    for r in index.records.iter_mut() {
        if r.documents.iter().any(|d| d == duplicate) {
            r.documents.retain(|d| d != duplicate);
            if !r.documents.iter().any(|d| d == into) {
                r.documents.push(into.to_string());
            }
            moved += 1;
        }
    }
    let mut notes = folder.notes(&person)?;
    let mut notes_changed = false;
    for n in notes.iter_mut() {
        if n.about.iter().any(|a| a == duplicate) {
            n.about.retain(|a| a != duplicate);
            if !n.about.iter().any(|a| a == into) {
                n.about.push(into.to_string());
            }
            notes_changed = true;
            moved += 1;
        }
    }
    texts.texts.remove(duplicate);
    folder.save_index(&person, &mut index)?;
    folder.save_text(&person, &texts)?;
    if notes_changed {
        folder.save_notes(&person, &notes)?;
    }
    let file_removed = folder.store.delete(&dup.path)?;
    Ok(Merged {
        document: into.to_string(),
        merged: duplicate.to_string(),
        path: dup.path,
        file_removed,
        references_moved: moved,
    })
}
