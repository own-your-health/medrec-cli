//! Document intake (PLAN §6): gather, verify, de-duplicate, read, classify,
//! file, and only then remove what was moved.
//!
//! | Source | Policy | Why |
//! |---|---|---|
//! | the inbox (`--inbox`) | move | the inbox is a drop box; a filed document leaves it |
//! | a local file under `~/Downloads` | move | a download is a transient copy |
//! | a file named with `--move` (organize-scans' hand-off) | move | the caller asked |
//! | a Drive folder (`--drive`) | copy | the back catalogue stays where it is |
//! | any other local file | copy | medrec does not delete what it did not receive |
//!
//! `--keep` turns every move into a copy. A symlink is always copied. A
//! moved source is removed only after the filed copy's MD5, as the records
//! folder reports it, matched the source's, and the index that points at it
//! was written and read back.
//!
//! Values given on the command line win over detection. Values given by
//! another classifier (`--classified-by organize-scans`) are checked: when
//! medrec's own detection is certain of something else, the document goes to
//! `_review/` with both answers.

pub mod classify;
pub mod dupes;
pub mod filetype;
pub mod text;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use pk_cli_core::CliError;
use pk_cli_drive::LocalCopy;
use serde::Serialize;
use serde_json::{json, Value};

use crate::config::Config;
use crate::folder::{self, Folder};
use crate::hash;
use crate::ids;
use crate::model::{
    filed_by, now_rfc3339, today, Document, Household, NearDuplicate, Organization, Origin, Person,
    PersonIndex, PersonText, Provenance, ProvenanceSource, ReviewItem, ReviewQueue, TextInfo,
};
use crate::people;
use crate::prompt;
use crate::store::{Hashed, Location, Reads, Store};

use classify::{Classification, Confidence, Known};

/// File extensions considered when listing a folder (the inbox, a Drive
/// folder). Files named on the command line are judged by content alone.
const FOLDER_EXTENSIONS: &[&str] = &[
    "pdf", "png", "jpg", "jpeg", "tif", "tiff", "heic", "heif", "xml",
];

/// Classifiers whose answers medrec takes as given but checks.
pub const CLASSIFIERS: &[&str] = &["organize-scans"];

#[derive(Debug, Clone, Default)]
pub struct Given {
    pub person: Option<String>,
    pub doc_type: Option<String>,
    pub date: Option<String>,
    pub organization: Option<String>,
    /// Who classified it, when not the person at the keyboard.
    pub classified_by: Option<String>,
}

impl Given {
    fn any(&self) -> bool {
        self.person.is_some()
            || self.doc_type.is_some()
            || self.date.is_some()
            || self.organization.is_some()
    }
}

pub struct Request {
    pub files: Vec<PathBuf>,
    pub inbox: Option<Location>,
    pub drive: Option<Location>,
    pub given: Given,
    pub keep: bool,
    /// Move named files too (their caller hands them over).
    pub move_files: bool,
    pub dry_run: bool,
    pub interactive: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Policy {
    Copy,
    Move,
}

enum From {
    Local(PathBuf),
    Folder { store: usize, rel: String },
}

struct Item {
    from: From,
    /// The file name as received, for the index and messages.
    name: String,
    origin: &'static str,
    policy: Policy,
    size: Option<u64>,
    /// The MD5 a folder listing reported, so a duplicate needs no download.
    md5: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Outcome {
    /// The list line it came from (`intake --from`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    pub source: String,
    pub origin: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub person: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub person_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub organization: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub classified_by: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub reasons: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duplicate_of: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub near_duplicates: Vec<NearDuplicate>,
    /// A bill filed before that is the same statement.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub same_statement_as: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<TextInfo>,
    pub source_removed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The error's code (`not_found`, `usage`, ...) when a list line failed
    /// as a command would (`intake --from`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
}

impl Outcome {
    /// A list line that failed as a command would, before its file was
    /// filed (`intake --from`).
    pub fn failed(source: &str, error: &pk_cli_core::CliError) -> Self {
        let mut o = Outcome::new(
            &Item {
                from: From::Local(PathBuf::from(source)),
                name: source.to_string(),
                origin: "file",
                policy: Policy::Copy,
                size: None,
                md5: None,
            },
            "failed",
        );
        o.error = Some(error.message().to_string());
        o.error_code = Some(error.code().to_string());
        o
    }

    fn new(item: &Item, status: &str) -> Self {
        Outcome {
            line: None,
            source: item.name.clone(),
            origin: item.origin.into(),
            status: status.into(),
            person: None,
            person_name: None,
            document: None,
            path: None,
            review: None,
            doc_type: None,
            date: None,
            organization: None,
            confidence: None,
            classified_by: None,
            reasons: Vec::new(),
            duplicate_of: None,
            near_duplicates: Vec::new(),
            same_statement_as: None,
            text: None,
            source_removed: false,
            error: None,
            error_code: None,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub dry_run: bool,
    pub items: Vec<Outcome>,
    pub counts: BTreeMap<String, usize>,
}

/// A document intake has read and classified, ready to file.
pub struct Prepared {
    /// The bytes read, in a private file: what is analyzed and uploaded,
    /// whatever happens to the source afterwards.
    pub local: PathBuf,
    _copy: Option<LocalCopy>,
    _scratch: Option<pk_cli_drive::TempDir>,
    pub kind: filetype::Kind,
    pub sha: String,
    pub md5: String,
    pub size: u64,
    pub extracted: text::Extracted,
    pub signature: Vec<u32>,
    /// What medrec found, before any given value.
    pub detected: Classification,
    /// `detected` with the given values applied.
    pub classification: Classification,
}

/// A signature already filed: (person, document id, signature, type, date).
pub type Signature = (String, String, Vec<u32>, Option<String>, Option<String>);

/// A source to remove once the indexes are written: the item, its outcome,
/// the MD5 of the bytes read from it, and for a duplicate the filed copy's
/// path and MD5.
struct Removable {
    item: usize,
    outcome: usize,
    read_md5: String,
    duplicate_of: Option<(String, String)>,
}

/// Everything already filed that a new document is compared with.
struct Seen {
    /// sha256 and md5 → (person or "review", document or review id, path).
    by_hash: HashMap<String, (String, String, String)>,
    signatures: Vec<Signature>,
}

impl Seen {
    fn add(&mut self, sha: &str, md5: &str, owner: &str, id: &str, path: &str) {
        let v = (owner.to_string(), id.to_string(), path.to_string());
        self.by_hash.insert(sha.to_string(), v.clone());
        self.by_hash.insert(md5.to_string(), v);
    }
}

pub fn run(folder: &Folder, cfg: &Config, req: &Request) -> Result<(Report, bool), CliError> {
    let household = folder.household()?;
    if household.people.is_empty() {
        return Err(CliError::Usage(
            "the records folder has no people yet; add them first with `medrec people add`".into(),
        ));
    }
    let given_person = req
        .given
        .person
        .as_deref()
        .map(|q| people::resolve(&household, q).map(|p| p.id.clone()))
        .transpose()?;
    validate_given(&req.given)?;

    let mut folder_stores: Vec<Store> = Vec::new();
    let items = gather(req, cfg, &mut folder_stores)?;
    if items.is_empty() {
        return Ok((
            Report {
                dry_run: req.dry_run,
                items: Vec::new(),
                counts: BTreeMap::new(),
            },
            true,
        ));
    }

    let mut indexes: BTreeMap<String, PersonIndex> = BTreeMap::new();
    for p in &household.people {
        indexes.insert(p.id.clone(), folder.index(p)?);
    }
    let mut texts: BTreeMap<String, PersonText> = BTreeMap::new();
    let mut queue = folder.review()?;
    let mut seen = Seen {
        by_hash: HashMap::new(),
        signatures: Vec::new(),
    };
    for (pid, idx) in &indexes {
        for d in &idx.documents {
            seen.add(&d.sha256, &d.md5, pid, &d.id, &d.path);
            seen.signatures.push((
                pid.clone(),
                d.id.clone(),
                d.minhash.clone(),
                d.doc_type.clone(),
                d.date.clone(),
            ));
        }
    }
    for q in &queue.items {
        seen.add(&q.sha256, &q.md5, "review", &q.id, &q.path);
    }

    let ctx = Context::new(cfg, &household);
    let mut outcomes = Vec::new();
    // (item, outcome, for a duplicate the filed copy's path and MD5) of
    // sources to remove once the indexes are written.
    let mut removable: Vec<Removable> = Vec::new();
    let mut queue_changed = false;
    let mut failed = false;

    for (n, item) in items.iter().enumerate() {
        let mut out = Outcome::new(item, "failed");
        // A folder listing's MD5 finds a duplicate without a download.
        if let Some((owner, id, path)) =
            item.md5.as_ref().and_then(|m| seen.by_hash.get(m)).cloned()
        {
            mark_duplicate(&mut out, &household, &owner, &id);
            if item.policy == Policy::Move && !req.dry_run {
                let md5 = item.md5.clone().unwrap_or_default();
                removable.push(Removable {
                    item: n,
                    outcome: outcomes.len(),
                    read_md5: md5.clone(),
                    duplicate_of: Some((path, md5)),
                });
            }
            outcomes.push(out);
            continue;
        }
        let prepared = match prepare(
            item,
            &folder_stores,
            &ctx,
            &req.given,
            given_person.as_deref(),
        ) {
            Ok(p) => p,
            Err(Skip(reason)) => {
                out.status = "skipped".into();
                out.error = Some(reason);
                outcomes.push(out);
                continue;
            }
        };
        out.text = Some(text_info(&prepared.extracted));
        if let Some((owner, id, path)) = seen.by_hash.get(&prepared.sha).cloned() {
            mark_duplicate(&mut out, &household, &owner, &id);
            if item.policy == Policy::Move && !req.dry_run {
                removable.push(Removable {
                    item: n,
                    outcome: outcomes.len(),
                    read_md5: prepared.md5.clone(),
                    duplicate_of: Some((path, prepared.md5.clone())),
                });
            }
            outcomes.push(out);
            continue;
        }
        let mut class = prepared.classification.clone();
        let mut reasons = disagreements(&req.given, &class, &prepared.detected, &household);
        if reasons.is_empty() && !class.decided() && req.interactive && !req.dry_run {
            ask(&item.name, &mut class, &household, &household.organizations)?;
        }
        if !class.decided() {
            reasons.extend(class.unsure().into_iter().map(|f| format!("unsure of {f}")));
        }
        describe(&mut out, &household, &class);
        out.classified_by = Some(classified_by(&req.given, &class));
        out.near_duplicates = near_duplicates_in(&seen.signatures, &prepared.signature, &class);
        let file_it = reasons.is_empty();
        out.reasons = reasons.clone();
        if req.dry_run {
            // A later copy in the same batch is a duplicate of this one.
            let owner = if file_it {
                class.person.value().unwrap_or("review").to_string()
            } else {
                "review".to_string()
            };
            let this_run = format!("{} (this run)", item.name);
            seen.add(&prepared.sha, &prepared.md5, &owner, &this_run, &item.name);
            if file_it {
                seen.signatures.push((
                    owner,
                    this_run,
                    prepared.signature.clone(),
                    out.doc_type.clone(),
                    out.date.clone(),
                ));
            }
            out.status = if file_it {
                "would_file"
            } else {
                "would_review"
            }
            .into();
            outcomes.push(out);
            continue;
        }
        let stored = if file_it {
            let pid = class
                .person
                .value()
                .expect("decided means a person")
                .to_string();
            let person = household.person(&pid).expect("a resolved person");
            file_document(
                folder,
                &mut indexes,
                &mut texts,
                person,
                item,
                &prepared,
                &class,
                &out,
            )
            .map(|doc| {
                out.status = "filed".into();
                seen.add(&prepared.sha, &prepared.md5, &pid, &doc.id, &doc.path);
                seen.signatures.push((
                    pid.clone(),
                    doc.id.clone(),
                    prepared.signature.clone(),
                    out.doc_type.clone(),
                    out.date.clone(),
                ));
                out.document = Some(doc.id);
                out.path = Some(doc.path);
                out.same_statement_as = doc.same_statement_as;
            })
        } else {
            hold_for_review(
                folder, &mut queue, item, &prepared, &class, reasons, &req.given,
            )
            .map(|q| {
                out.status = "review".into();
                seen.add(&prepared.sha, &prepared.md5, "review", &q.id, &q.path);
                out.review = Some(q.id);
                out.path = Some(q.path);
                queue_changed = true;
            })
        };
        match stored {
            Ok(()) => {
                if item.policy == Policy::Move {
                    removable.push(Removable {
                        item: n,
                        outcome: outcomes.len(),
                        read_md5: prepared.md5.clone(),
                        duplicate_of: None,
                    });
                }
                outcomes.push(out);
            }
            Err(e) => {
                out.status = "failed".into();
                out.error = Some(e.to_string());
                failed = true;
                outcomes.push(out);
                // Whatever failed (the network, the folder) fails the next
                // file too. What was filed is still indexed below.
                break;
            }
        }
    }

    if !req.dry_run {
        let mut touched: Vec<String> = outcomes
            .iter()
            .filter(|o| o.status == "filed")
            .filter_map(|o| o.person.clone())
            .collect();
        for o in outcomes.iter().filter(|o| o.status == "duplicate") {
            let (Some(pid), Some(id)) = (&o.person, &o.duplicate_of) else {
                continue;
            };
            let idx = indexes.get_mut(pid);
            let held = idx.and_then(|i| i.documents.iter_mut().find(|d| d.id == *id));
            if held.is_some_and(|d| d.held_by_household(&o.origin)) {
                touched.push(pid.clone());
            }
        }
        let write = (|| -> Result<(), CliError> {
            for pid in dedup(touched) {
                let person = household.person(&pid).expect("filed for a known person");
                if let Some(idx) = indexes.get_mut(&pid) {
                    crate::imaging::link_reports(idx);
                    folder.save_index(person, idx)?;
                }
                if let Some(t) = texts.get(&pid) {
                    folder.save_text(person, t)?;
                }
            }
            if queue_changed {
                folder.save_review(&mut queue)?;
            }
            Ok(())
        })();
        if let Err(e) = write {
            for o in outcomes
                .iter_mut()
                .filter(|o| o.status == "filed" || o.status == "review")
            {
                o.status = "failed".into();
                o.error = Some(format!("filed, but the index could not be written: {e}"));
            }
            failed = true;
            removable.clear();
        }
        for Removable {
            item: n,
            outcome: o,
            read_md5,
            duplicate_of,
        } in removable
        {
            let out = &mut outcomes[o];
            // A duplicate's source goes only while the copy already filed
            // is still there with the same bytes.
            if let Some((path, md5)) = duplicate_of {
                if let Err(e) = folder.check_md5(&path, &md5) {
                    out.error = Some(format!(
                        "not removed: the filed copy did not check out: {e}"
                    ));
                    continue;
                }
            }
            match remove_source(&items[n], &folder_stores, &read_md5) {
                Ok(r) => out.source_removed = r,
                Err(e) => {
                    out.error = Some(format!("filed, but the source could not be removed: {e}"))
                }
            }
        }
    }

    let mut counts = BTreeMap::new();
    for o in &outcomes {
        *counts.entry(o.status.clone()).or_insert(0) += 1;
    }
    Ok((
        Report {
            dry_run: req.dry_run,
            items: outcomes,
            counts,
        },
        !failed,
    ))
}

pub fn validate_given(g: &Given) -> Result<(), CliError> {
    if let Some(t) = &g.doc_type {
        if !classify::is_doc_type(t) {
            return Err(CliError::Usage(format!(
                "unknown document type `{t}` (known: {})",
                classify::DOC_TYPES
                    .iter()
                    .map(|d| d.0)
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
    }
    if let Some(d) = &g.date {
        pk_cli_core::dates::parse_iso(d)?;
    }
    if let Some(c) = &g.classified_by {
        if !CLASSIFIERS.contains(&c.as_str()) {
            return Err(CliError::Usage(format!(
                "unknown classifier `{c}` (known: {})",
                CLASSIFIERS.join(", ")
            )));
        }
        if !g.any() {
            return Err(CliError::Usage(
                "--classified-by names who gave --person/--type/--date/--org; give at least one"
                    .into(),
            ));
        }
    }
    Ok(())
}

fn mark_duplicate(out: &mut Outcome, h: &Household, owner: &str, id: &str) {
    out.status = "duplicate".into();
    out.duplicate_of = Some(id.to_string());
    if owner != "review" {
        out.person = Some(owner.to_string());
        out.person_name = Some(h.name_of(owner));
    }
}

fn describe(out: &mut Outcome, h: &Household, class: &Classification) {
    out.person = class.person.value().map(str::to_string);
    out.person_name = out.person.as_deref().map(|p| h.name_of(p));
    out.doc_type = class.doc_type.value().map(str::to_string);
    out.date = class.date.value().map(str::to_string);
    out.organization = class.organization.value().map(str::to_string);
    out.confidence = Some(confidences(class));
}

pub fn confidences(class: &Classification) -> Value {
    let c = |f: &classify::Field| f.proposal.as_ref().map(|p| p.confidence.as_str());
    json!({
        "person": c(&class.person),
        "date": c(&class.date),
        "type": c(&class.doc_type),
        "organization": c(&class.organization),
    })
}

/// `organize-scans`, `hand` (given on the command line or answered in the
/// terminal), or `medrec` (its rules alone).
pub fn classified_by(given: &Given, class: &Classification) -> String {
    if let Some(c) = &given.classified_by {
        return c.clone();
    }
    let any_given = [&class.person, &class.date, &class.doc_type]
        .iter()
        .any(|f| {
            f.proposal
                .as_ref()
                .is_some_and(|p| p.confidence == Confidence::Given)
        });
    if any_given { "hand" } else { "medrec" }.into()
}

/// What another classifier said that medrec's own detection is certain is
/// wrong.
pub fn disagreements(
    given: &Given,
    class: &Classification,
    detected: &Classification,
    household: &Household,
) -> Vec<String> {
    let Some(by) = &given.classified_by else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut check = |field: &str, ours: &classify::Field, theirs: Option<&str>| {
        if let (Some(theirs), Some(p)) = (theirs, ours.proposal.as_ref()) {
            if p.confidence == Confidence::High && p.value != theirs {
                // People by name: an id means nothing to whoever reviews it.
                let name = |v: &str| {
                    if field == "person" {
                        household.name_of(v)
                    } else {
                        v.to_string()
                    }
                };
                out.push(format!(
                    "{by} said {field} `{}`; medrec found `{}` (score {})",
                    name(theirs),
                    name(&p.value),
                    p.score
                ));
            }
        }
    };
    let given_person = given.person.as_ref().and(class.person.value());
    check("person", &detected.person, given_person);
    check("type", &detected.doc_type, given.doc_type.as_deref());
    check("date", &detected.date, given.date.as_deref());
    out
}

fn dedup(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v.dedup();
    v
}

struct Skip(String);

/// What every document in a run is classified against.
pub struct Context {
    pub tools: text::Tools,
    pub opts: text::Options,
    pub people: Vec<Person>,
    pub organizations: Vec<Organization>,
    pub today: String,
    pub max_bytes: u64,
}

impl Context {
    pub fn new(cfg: &Config, h: &Household) -> Self {
        Context {
            tools: text::Tools::from_path(),
            opts: text::Options {
                languages: cfg.ocr_languages(),
                max_ocr_pages: cfg.ocr_max_pages(),
            },
            people: h.people.clone(),
            organizations: h.organizations.clone(),
            today: today(),
            max_bytes: cfg.max_file_bytes(),
        }
    }
}

fn prepare(
    item: &Item,
    stores: &[Store],
    ctx: &Context,
    given: &Given,
    given_person: Option<&str>,
) -> Result<Prepared, Skip> {
    if item.size.is_some_and(|s| s > ctx.max_bytes) {
        return Err(Skip(format!(
            "larger than {} MiB (config max_file_mb)",
            ctx.max_bytes / 1024 / 1024
        )));
    }
    let (local, copy) = match &item.from {
        From::Local(p) => (p.clone(), None),
        From::Folder { store, rel } => {
            let copy = stores[*store]
                .fetch(rel)
                .map_err(|e| Skip(format!("download failed: {e}")))?;
            (copy.path.clone(), Some(copy))
        }
    };
    let bytes = if copy.is_some() {
        read_bounded(&local, ctx.max_bytes)
    } else {
        read_local(
            || modified(&local),
            std::time::SystemTime::now,
            std::thread::sleep,
            || read_bounded(&local, ctx.max_bytes),
            || std::fs::metadata(&local).ok().map(|m| m.len()),
        )
    }
    .map_err(Skip)?;
    let kind = filetype::identify(&bytes).map_err(|e| Skip(e.to_string()))?;
    // A file named on the command line is read once, into a private copy,
    // so its text, its hashes and the upload are one set of bytes even if
    // it is still being written or synced. A folder's file is a download
    // already.
    let (local, scratch) = if copy.is_some() {
        (local, None)
    } else {
        let dir = pk_cli_drive::private_temp_dir("medrec-intake")
            .map_err(|e| Skip(format!("no private temp dir: {e}")))?;
        let name = local
            .file_name()
            .map(|n| n.to_owned())
            .unwrap_or_else(|| "document".into());
        let path = dir.path().join(name);
        crate::write_private(&path, &bytes).map_err(|e| Skip(e.to_string()))?;
        (path, Some(dir))
    };
    let mut p = analyze(ctx, kind, &local, &bytes, &item.name);
    p._copy = copy;
    p._scratch = scratch;
    apply_given(&mut p.classification, given, given_person);
    Ok(p)
}

/// Hashes, text, signature and classification of a file's bytes.
pub fn analyze(
    ctx: &Context,
    kind: filetype::Kind,
    local: &Path,
    bytes: &[u8],
    name: &str,
) -> Prepared {
    let extracted = text::extract(&ctx.tools, kind, local, &ctx.opts);
    let signature = dupes::signature(&extracted.text);
    let known = Known {
        people: &ctx.people,
        organizations: &ctx.organizations,
    };
    let classification = classify::classify(&extracted.text, name, &known, &ctx.today);
    Prepared {
        local: local.to_path_buf(),
        _copy: None,
        _scratch: None,
        kind,
        sha: hash::sha256_hex(bytes),
        md5: hash::md5_hex(bytes),
        size: bytes.len() as u64,
        extracted,
        signature,
        detected: classification.clone(),
        classification,
    }
}

pub fn apply_given(class: &mut Classification, given: &Given, person: Option<&str>) {
    if let Some(p) = person {
        class.person.set_given(p);
    }
    if let Some(t) = &given.doc_type {
        class.doc_type.set_given(t.clone());
    }
    if let Some(d) = &given.date {
        class.date.set_given(d.clone());
    }
    if let Some(o) = &given.organization {
        class.organization.set_given(o.clone());
    }
}

/// How long a file must go unmodified before intake reads it, and how many
/// times it waits for that.
const QUIET: std::time::Duration = std::time::Duration::from_secs(2);
const SETTLE_TRIES: usize = 3;
const STILL_WRITTEN: &str =
    "still being written (modified while intake waited to read it); intake it again once it is complete";

fn modified(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// A local file's bytes: once it has gone [`QUIET`] unmodified ([`settle`]),
/// read until two reads agree with its size ([`read_stable`]), and refused
/// if it was modified while it was read. The clock, the file's modification
/// time and the wait are passed in, so the rules are tested without timing.
fn read_local(
    mtime: impl Fn() -> Option<std::time::SystemTime>,
    now: impl Fn() -> std::time::SystemTime,
    mut sleep: impl FnMut(std::time::Duration),
    read: impl FnMut() -> Result<Vec<u8>, String>,
    size: impl Fn() -> Option<u64>,
) -> Result<Vec<u8>, String> {
    let before = settle(&mtime, &now, &mut sleep, QUIET, SETTLE_TRIES)?;
    let bytes = read_stable(read, size)?;
    if mtime() != before {
        return Err(STILL_WRITTEN.into());
    }
    Ok(bytes)
}

/// Wait until the file has gone `quiet` unmodified, trying `tries` times, so
/// a file written in bursts (a scanner, a hand-off still copying) is read
/// whole. Returns its modification time.
fn settle(
    mtime: &impl Fn() -> Option<std::time::SystemTime>,
    now: &impl Fn() -> std::time::SystemTime,
    sleep: &mut impl FnMut(std::time::Duration),
    quiet: std::time::Duration,
    tries: usize,
) -> Result<Option<std::time::SystemTime>, String> {
    for _ in 0..tries {
        let at = mtime();
        // No metadata: the read that follows says why.
        let Some(t) = at else { return Ok(None) };
        // A time ahead of the clock (skew on a synced folder) cannot be
        // aged, so the whole quiet period is waited.
        let wait = match now().duration_since(t) {
            Ok(age) if age >= quiet => return Ok(at),
            Ok(age) => quiet - age,
            Err(_) => quiet,
        };
        sleep(wait);
        if mtime() == at {
            return Ok(at);
        }
    }
    Err(STILL_WRITTEN.into())
}

/// A local file's bytes, read until two reads in a row agree and are the
/// size the file system gives. A cloud folder's online-only placeholder
/// (a File Provider file) can serve other bytes while it downloads, and a
/// file still being written changes under the read; either is refused
/// rather than filed as bytes the file does not hold.
fn read_stable(
    mut read: impl FnMut() -> Result<Vec<u8>, String>,
    size: impl Fn() -> Option<u64>,
) -> Result<Vec<u8>, String> {
    const READS: usize = 4;
    let mut last = read()?;
    for _ in 1..READS {
        let again = read()?;
        if again == last && size() == Some(again.len() as u64) {
            return Ok(again);
        }
        last = again;
    }
    Err(format!(
        "read differently {READS} times in a row (a placeholder still downloading, or a file \
         still being written); intake it again once it is complete"
    ))
}

fn read_bounded(path: &Path, max: u64) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let f = std::fs::File::open(path).map_err(|e| format!("cannot read: {e}"))?;
    let mut buf = Vec::new();
    f.take(max + 1)
        .read_to_end(&mut buf)
        .map_err(|e| format!("cannot read: {e}"))?;
    if buf.len() as u64 > max {
        return Err(format!(
            "larger than {} MiB (config max_file_mb)",
            max / 1024 / 1024
        ));
    }
    Ok(buf)
}

pub fn text_info(ex: &text::Extracted) -> TextInfo {
    TextInfo {
        chars: ex.text.chars().count(),
        engines: ex.engines.clone(),
        ocr_pages: (ex.ocr_pages > 0).then_some(ex.ocr_pages),
        note: ex.note.clone(),
    }
}

pub fn near_duplicates_in(
    signatures: &[Signature],
    sig: &[u32],
    class: &Classification,
) -> Vec<NearDuplicate> {
    let person = class.person.value();
    let doc_type = class.doc_type.value();
    let date = class.date.value();
    let agrees = |a: Option<&str>, b: Option<&str>| a.is_none() || b.is_none() || a == b;
    let mut out: Vec<NearDuplicate> = signatures
        .iter()
        .filter_map(|(pid, doc, s, t, d)| {
            let sim = dupes::similarity(sig, s);
            let meta = agrees(person, Some(pid))
                && agrees(doc_type, t.as_deref())
                && agrees(date, d.as_deref());
            dupes::should_link(sim, meta).then(|| NearDuplicate {
                document: doc.clone(),
                similarity: (sim * 100.0).round() / 100.0,
            })
        })
        .collect();
    out.sort_by(|a, b| b.similarity.total_cmp(&a.similarity));
    out.truncate(5);
    out
}

/// Ask in the terminal for each field that is not certain.
pub fn ask(
    name: &str,
    class: &mut Classification,
    household: &Household,
    orgs: &[Organization],
) -> Result<(), CliError> {
    eprintln!(
        "\n{name}: medrec is not sure of {}",
        class.unsure().join(", ")
    );
    if !class.person.decided() {
        let options: Vec<(String, String)> = household
            .people
            .iter()
            .map(|p| (p.id.clone(), p.name.clone()))
            .collect();
        let default = class
            .person
            .value()
            .and_then(|v| options.iter().position(|(id, _)| id == v));
        if let Some(p) = prompt::choose("  Whose document is it?", &options, default)? {
            class.person.set_given(p);
        }
    }
    if !class.doc_type.decided() {
        let options: Vec<(String, String)> = classify::DOC_TYPES
            .iter()
            .map(|(c, l, _)| (c.to_string(), l.to_string()))
            .collect();
        let default = class
            .doc_type
            .value()
            .and_then(|v| options.iter().position(|(c, _)| c == v));
        if let Some(t) = prompt::choose("  What kind of document?", &options, default)? {
            class.doc_type.set_given(t);
        }
    }
    if !class.date.decided() {
        if let Some(d) = prompt::ask_date("  Date of service", class.date.value())? {
            class.date.set_given(d);
        }
    }
    if class.organization.proposal.as_ref().map(|p| p.confidence) != Some(Confidence::Given)
        && !class.organization.decided()
    {
        let default = class.organization.value().map(str::to_string);
        let hint = if orgs.is_empty() {
            ""
        } else {
            " (known organizations match by name)"
        };
        match prompt::ask_text(&format!("  Organization{hint}"), default.as_deref())? {
            Some(o) => class.organization.set_given(o),
            None => class.organization.proposal = None,
        }
    }
    Ok(())
}

pub struct Filed {
    pub id: String,
    pub path: String,
    pub same_statement_as: Option<String>,
}

#[allow(clippy::too_many_arguments)]
fn file_document(
    folder: &Folder,
    indexes: &mut BTreeMap<String, PersonIndex>,
    texts: &mut BTreeMap<String, PersonText>,
    person: &Person,
    item: &Item,
    prepared: &Prepared,
    class: &Classification,
    out: &Outcome,
) -> Result<Filed, CliError> {
    let doc_type = class.doc_type.value().unwrap_or("other");
    let date = class.date.value().unwrap_or("undated");
    let name = folder::filed_name(date, doc_type, class.organization.value(), None, &item.name);
    let dir = folder::document_dir(person, doc_type, date);
    let path = folder.put_new_file(&prepared.local, &dir, &name, &prepared.md5)?;
    let mut doc = document_for(
        path.clone(),
        item_origin(item),
        prepared,
        class,
        &out.near_duplicates,
        out.classified_by.clone(),
    );
    let id = doc.id.clone();
    if !texts.contains_key(&person.id) {
        texts.insert(person.id.clone(), folder.text(person)?);
    }
    if !prepared.extracted.text.is_empty() {
        texts
            .get_mut(&person.id)
            .expect("inserted above")
            .texts
            .insert(id.clone(), prepared.extracted.text.clone());
    }
    let index = indexes
        .get_mut(&person.id)
        .ok_or_else(|| CliError::Other(format!("no index loaded for {}", person.id)))?;
    let person_texts = texts.get(&person.id).expect("loaded above");
    crate::bill::annotate(&mut doc, &prepared.extracted.text, index, person_texts);
    let same_statement_as = doc.same_statement_as.clone();
    index.documents.push(doc);
    Ok(Filed {
        id,
        path,
        same_statement_as,
    })
}

fn item_origin(item: &Item) -> Origin {
    Origin {
        kind: item.origin.into(),
        name: Some(item.name.clone()),
        action: match item.policy {
            Policy::Move => "moved",
            Policy::Copy => "copied",
        }
        .into(),
    }
}

/// The index entry for a document filed at `path`.
pub fn document_for(
    path: String,
    origin: Origin,
    prepared: &Prepared,
    class: &Classification,
    near: &[NearDuplicate],
    classified_by: Option<String>,
) -> Document {
    let doc_type = class.doc_type.value().map(str::to_string);
    let filed_at = now_rfc3339();
    Document {
        id: ids::new("d"),
        path: path.clone(),
        sha256: prepared.sha.clone(),
        md5: prepared.md5.clone(),
        size: prepared.size,
        media_type: prepared.kind.media_type().into(),
        pages: prepared.extracted.pages,
        original_name: origin.name.clone(),
        title: None,
        loinc: doc_type
            .as_deref()
            .and_then(classify::doc_type_loinc)
            .map(str::to_string),
        doc_type,
        date: class.date.value().map(str::to_string),
        organization: class.organization.value().map(str::to_string),
        filed_at: filed_at.clone(),
        text: Some(text_info(&prepared.extracted)),
        near_duplicates: near.to_vec(),
        minhash: prepared.signature.clone(),
        renderings: Vec::new(),
        statement: None,
        same_statement_as: None,
        classification: serde_json::to_value(class).ok(),
        provenance: Provenance {
            source: ProvenanceSource {
                kind: if origin.kind == "drive" {
                    "drive"
                } else {
                    "scan"
                }
                .into(),
                cli: None,
                cli_version: None,
                organization_id: None,
                fhir_base: None,
            },
            method: if prepared.extracted.ocr_pages > 0 {
                "scan"
            } else {
                "file"
            }
            .into(),
            source_resource: None,
            fetched_at: None,
            filed_at,
            raw_path: Some(path),
            classified_by,
            derived_from: None,
            filed_by: filed_by(),
        },
        also_from: Vec::new(),
        words: Vec::new(),
        errors: Vec::new(),
        origin,
    }
}

fn hold_for_review(
    folder: &Folder,
    queue: &mut ReviewQueue,
    item: &Item,
    prepared: &Prepared,
    class: &Classification,
    reasons: Vec<String>,
    given: &Given,
) -> Result<Filed, CliError> {
    let name = format!("{}_{}", today(), folder::safe_component(&item.name));
    let path = folder.put_new_file(&prepared.local, folder::REVIEW_DIR, &name, &prepared.md5)?;
    let id = ids::new("q");
    queue.items.push(ReviewItem {
        id: id.clone(),
        path: path.clone(),
        sha256: prepared.sha.clone(),
        md5: prepared.md5.clone(),
        size: prepared.size,
        media_type: prepared.kind.media_type().into(),
        pages: prepared.extracted.pages,
        original_name: item.name.clone(),
        received_at: now_rfc3339(),
        origin: item_origin(item),
        reasons,
        text: prepared.extracted.text.clone(),
        text_info: Some(text_info(&prepared.extracted)),
        minhash: prepared.signature.clone(),
        classification: serde_json::to_value(class).unwrap_or(Value::Null),
        classified_by: given.classified_by.clone(),
    });
    Ok(Filed {
        id,
        path,
        same_statement_as: None,
    })
}

/// Remove a moved source, unless it is not the bytes that were read: one
/// changed since (still being written or synced) stays.
fn remove_source(item: &Item, stores: &[Store], read_md5: &str) -> Result<bool, CliError> {
    let now = match &item.from {
        From::Local(p) if p.is_file() => Some(crate::store::md5_file(p)?),
        From::Local(_) => None,
        From::Folder { store, rel } => {
            let store = &stores[*store];
            match store.md5(rel)? {
                Some(m) => Some(m),
                // A store that reports no MD5 for the file: it is
                // downloaded again and hashed.
                None if store.exists(rel)? => {
                    let copy = store.fetch(rel)?;
                    Some(crate::store::md5_file(&copy.path)?)
                }
                None => None,
            }
        }
    };
    if now.as_deref().is_some_and(|m| m != read_md5) {
        return Err(CliError::Other(
            "it changed after it was read, so it was kept; file it again".into(),
        ));
    }
    match &item.from {
        From::Local(p) => match std::fs::remove_file(p) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(CliError::Other(format!("removing {}: {e}", p.display()))),
        },
        From::Folder { store, rel } => stores[*store].delete(rel),
    }
}

fn gather(req: &Request, cfg: &Config, stores: &mut Vec<Store>) -> Result<Vec<Item>, CliError> {
    let mut items = Vec::new();
    let downloads = std::env::var_os("HOME")
        .map(|h| PathBuf::from(h).join("Downloads"))
        .and_then(|d| d.canonicalize().ok());
    let mut seen_paths = std::collections::HashSet::new();
    for f in &req.files {
        let meta = std::fs::symlink_metadata(f)
            .map_err(|e| CliError::NotFound(format!("{}: {e}", f.display())))?;
        let is_link = meta.file_type().is_symlink();
        let canon = f
            .canonicalize()
            .map_err(|e| CliError::NotFound(format!("{}: {e}", f.display())))?;
        if !canon.is_file() {
            return Err(CliError::Usage(format!("{} is not a file", f.display())));
        }
        if !seen_paths.insert(canon.clone()) {
            return Err(CliError::Usage(format!("{} is named twice", f.display())));
        }
        let in_downloads = downloads.as_ref().is_some_and(|d| canon.starts_with(d));
        let policy = if !is_link && !req.keep && (in_downloads || req.move_files) {
            Policy::Move
        } else {
            Policy::Copy
        };
        let size = canon.metadata().ok().map(|m| m.len());
        items.push(Item {
            name: file_name(&canon),
            from: From::Local(if is_link { canon } else { f.clone() }),
            origin: "file",
            policy,
            size,
            md5: None,
        });
    }
    for (loc, origin, policy) in [
        (
            req.inbox.as_ref(),
            "inbox",
            if req.keep { Policy::Copy } else { Policy::Move },
        ),
        (req.drive.as_ref(), "drive", Policy::Copy),
    ] {
        let Some(loc) = loc else { continue };
        let store = Store::open(loc, Reads::Fresh, cfg.store_budget())?;
        let entries: Vec<Hashed> = store.hashes("")?;
        let idx = stores.len();
        for e in entries {
            let name = e.rel.rsplit('/').next().unwrap_or(&e.rel).to_string();
            if name.starts_with('.') || !has_folder_extension(&name) {
                continue;
            }
            items.push(Item {
                name,
                from: From::Folder {
                    store: idx,
                    rel: e.rel.clone(),
                },
                origin,
                policy,
                size: Some(e.size),
                md5: e.md5,
            });
        }
        stores.push(store);
    }
    Ok(items)
}

fn has_folder_extension(name: &str) -> bool {
    name.rsplit_once('.')
        .map(|(_, ext)| FOLDER_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "document".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("medrec-intake-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn local_item(path: &Path) -> Item {
        Item {
            from: From::Local(path.to_path_buf()),
            name: "statement.pdf".into(),
            origin: "file",
            policy: Policy::Move,
            size: None,
            md5: None,
        }
    }

    #[test]
    fn a_named_file_is_read_once_into_a_private_copy() {
        let dir = scratch("copy");
        let source = dir.join("statement.pdf");
        std::fs::write(&source, crate::pdfgen::text_pdf(&["Example", "first"])).unwrap();
        let cfg = Config::default();
        let ctx = Context {
            tools: text::Tools::from_path(),
            opts: text::Options {
                languages: cfg.ocr_languages(),
                max_ocr_pages: cfg.ocr_max_pages(),
            },
            people: Vec::new(),
            organizations: Vec::new(),
            today: "2026-11-21".into(),
            max_bytes: 1 << 20,
        };
        let Ok(p) = prepare(&local_item(&source), &[], &ctx, &Given::default(), None) else {
            panic!("prepared");
        };
        // The source is rewritten after it was read, as a file still being
        // written or synced is: the upload is still the bytes hashed.
        std::fs::write(&source, crate::pdfgen::text_pdf(&["Example", "second"])).unwrap();
        assert_ne!(p.local, source);
        assert_eq!(crate::store::md5_file(&p.local).unwrap(), p.md5);
        assert_ne!(crate::store::md5_file(&source).unwrap(), p.md5);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_is_taken_only_once_two_reads_agree_with_its_size() {
        let reads = |seq: Vec<&'static [u8]>| {
            let mut it = seq.into_iter();
            move || Ok(it.next().expect("no more reads").to_vec())
        };
        // A placeholder that serves part of itself first.
        let got = read_stable(reads(vec![b"pa", b"partial", b"partial"]), || Some(7));
        assert_eq!(got.unwrap(), b"partial");
        // Two reads agree, but the file system gives another size.
        let got = read_stable(reads(vec![b"abc", b"abc", b"abcdef", b"abcdef"]), || {
            Some(6)
        });
        assert_eq!(got.unwrap(), b"abcdef");
        // A file that never settles is refused.
        let got = read_stable(reads(vec![b"a", b"ab", b"abc", b"abcd"]), || Some(4));
        assert!(got.unwrap_err().contains("read differently"));
    }

    #[test]
    fn a_folder_source_is_removed_only_while_it_has_the_bytes_read() {
        let dir = scratch("folder");
        let store = Store::open(
            &crate::store::Location::Dir(dir.clone()),
            crate::store::Reads::Fresh,
            std::time::Duration::from_secs(5),
        )
        .unwrap();
        let file = dir.join("scan.pdf");
        std::fs::write(&file, b"the bytes read").unwrap();
        let read = crate::store::md5_file(&file).unwrap();
        let item = Item {
            from: From::Folder {
                store: 0,
                rel: "scan.pdf".into(),
            },
            ..local_item(&file)
        };
        let stores = [store];
        std::fs::write(&file, b"the bytes since").unwrap();
        assert!(remove_source(&item, &stores, &read).is_err());
        assert!(file.exists(), "kept");
        std::fs::write(&file, b"the bytes read").unwrap();
        assert!(remove_source(&item, &stores, &read).unwrap());
        assert!(!file.exists(), "removed");
        // Gone already: nothing to remove.
        assert!(!remove_source(&item, &stores, &read).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A file whose modification time is `times[n]` on the nth look, on a
    /// clock that each wait moves forward.
    struct Fake {
        times: Vec<u64>,
        look: std::cell::Cell<usize>,
        clock: std::cell::Cell<std::time::Duration>,
    }

    impl Fake {
        fn new(times: &[u64], at_ms: u64) -> Self {
            Fake {
                times: times.to_vec(),
                look: std::cell::Cell::new(0),
                clock: std::cell::Cell::new(std::time::Duration::from_millis(at_ms)),
            }
        }
        fn mtime(&self) -> Option<std::time::SystemTime> {
            let n = self.look.get();
            self.look.set(n + 1);
            let ms = self.times[n.min(self.times.len() - 1)];
            Some(std::time::UNIX_EPOCH + std::time::Duration::from_millis(ms))
        }
        fn now(&self) -> std::time::SystemTime {
            std::time::UNIX_EPOCH + self.clock.get()
        }
        fn wait(&self, d: std::time::Duration) {
            self.clock.set(self.clock.get() + d);
        }
    }

    #[test]
    fn a_file_still_growing_is_not_read_until_it_goes_quiet() {
        let quiet = std::time::Duration::from_secs(2);
        // Modified again during every wait: still being written. Each try
        // looks before and after its wait.
        let f = Fake::new(&[1000, 2900, 2900, 4800, 4800, 6700], 1500);
        let got = settle(&|| f.mtime(), &|| f.now(), &mut |d| f.wait(d), quiet, 3);
        assert_eq!(got.unwrap_err(), STILL_WRITTEN);
        // Written just now, then left alone: read after one wait.
        let f = Fake::new(&[1000], 1500);
        let got = settle(&|| f.mtime(), &|| f.now(), &mut |d| f.wait(d), quiet, 3);
        assert!(got.is_ok());
        assert_eq!(f.clock.get(), std::time::Duration::from_millis(3000));
        // Old enough already: no wait.
        let f = Fake::new(&[1000], 9000);
        let got = settle(&|| f.mtime(), &|| f.now(), &mut |d| f.wait(d), quiet, 3);
        assert!(got.is_ok());
        assert_eq!(f.clock.get(), std::time::Duration::from_millis(9000));
        // A time ahead of the clock: the whole quiet period, then unchanged.
        let f = Fake::new(&[5000], 1500);
        let got = settle(&|| f.mtime(), &|| f.now(), &mut |d| f.wait(d), quiet, 3);
        assert!(got.is_ok());
        assert_eq!(f.clock.get(), std::time::Duration::from_millis(3500));
    }

    #[test]
    fn a_file_modified_while_it_was_read_is_refused() {
        // Quiet when looked at, then modified before the reads finished,
        // which agreed.
        let f = Fake::new(&[1000, 6000], 9000);
        let got = read_local(
            || f.mtime(),
            || f.now(),
            |d| f.wait(d),
            || Ok(b"same".to_vec()),
            || Some(4),
        );
        assert_eq!(got.unwrap_err(), STILL_WRITTEN);
        let f = Fake::new(&[1000], 9000);
        let got = read_local(
            || f.mtime(),
            || f.now(),
            |d| f.wait(d),
            || Ok(b"same".to_vec()),
            || Some(4),
        );
        assert_eq!(got.unwrap(), b"same");
    }

    #[test]
    fn a_moved_source_that_changed_after_it_was_read_is_kept() {
        let dir = scratch("remove");
        let source = dir.join("statement.pdf");
        std::fs::write(&source, b"the bytes read").unwrap();
        let read = crate::store::md5_file(&source).unwrap();
        std::fs::write(&source, b"the bytes since").unwrap();
        let item = local_item(&source);
        assert!(remove_source(&item, &[], &read).is_err());
        assert!(source.exists(), "kept");
        let now = crate::store::md5_file(&source).unwrap();
        assert!(remove_source(&item, &[], &now).unwrap());
        assert!(!source.exists(), "removed");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
