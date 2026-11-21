//! Provider and insurer import (PLAN §7): run `<cli> records export
//! --patient <id> --ndjson` as a bounded subprocess, read its
//! `records-stream/v1` stream, and file it.
//!
//! Every resource is written verbatim, one file each, under
//! `<Person>/Records/<source>/<ResourceType>/<id>.json`; a resource that
//! changed at the source keeps its previous version beside it as
//! `<id>~<md5 prefix>.json`. A `Binary` document is also filed as a document
//! under `Documents/` (or `Claims/` for an EOB). Nothing is written unless
//! the stream ends with its trailer, the trailer says it is complete and its
//! counts match, and the provider exits 0.
//!
//! Providers are found by profile, not by name: any CLI whose `info --json`
//! lists `health-records/v1` is one.
//!
//! A provider that signs in keeps one login profile per account and lists
//! `login-profiles` in its `info --json` capabilities; medrec passes the one
//! `--from-profile` names as the export's `--profile`, and refuses a provider
//! that signs in without listing it (one from before profiles) rather than
//! run it unprofiled. A provider whose info says no login is needed takes
//! none. One that also reads downloaded files lists `records-files`, and
//! `--from-files <DIR>` runs its export on them with `--files <DIR>` instead
//! of a login, with its `<PREFIX>_PROFILE` left out of its environment.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::{BufReader, Read};
use std::path::PathBuf;
use std::time::Duration;

use base64::Engine;
use md5::Md5;
use pk_cli_config::profile::{ProfileList, ProfileName};
use pk_cli_core::CliError;
use pk_med_records::LOGIN_PROFILES;
pub use pk_med_records::RECORDS_FILES;
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::config::Config;
use crate::folder::{self, Folder};
use crate::hash;
use crate::ids;
use crate::intake::{self, classify, filetype, text};
use crate::model::{
    filed_by, now_rfc3339, Document, Household, Identifier, ImportRun, Organization, Origin,
    Person, PersonIndex, PersonText, Provenance, ProvenanceSource, Record, Rendering, Source,
    SourceResource,
};
use crate::normalize;
use crate::people;
use crate::proc::{which, Bounded};
use crate::stream::{self, Header, Item, RawResource, RecordsReader, Skip};
use crate::version::{self, Filing};

/// Provider CLIs medrec looks for without being told.
pub const KNOWN_PROVIDERS: &[&str] = &[
    "mychart", "kp", "athena", "modmed", "icanotes", "uhc", "caremark", "labcorp", "oneblood",
    "fertemr", "salve",
];
const INFO_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Serialize)]
pub struct Provider {
    pub cli: String,
    #[serde(skip)]
    pub path: PathBuf,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub profiles: Vec<String>,
    /// Its export signs in through a login profile (`--from-profile`).
    pub login_profiles: bool,
    /// Its export also reads downloaded files (`--from-files`).
    pub records_files: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl Provider {
    pub fn usable(&self) -> bool {
        self.status == "ok"
    }
}

/// Ask `<cli> info --json` what it is. `info` is the one command a family
/// CLI answers without reading a credential.
pub fn probe(cli: &str) -> Provider {
    let mut p = Provider {
        cli: cli.into(),
        path: PathBuf::new(),
        status: "not-installed".into(),
        version: None,
        profiles: Vec::new(),
        login_profiles: false,
        records_files: false,
        note: None,
    };
    let Some(path) = which(cli) else {
        return p;
    };
    p.path = path.clone();
    let r = match Bounded::new(&path)
        .args(["info", "--json"])
        .timeout(INFO_TIMEOUT)
        .max_stdout(1024 * 1024)
        .run()
    {
        Ok(r) => r,
        Err(e) => {
            p.status = "no-answer".into();
            p.note = Some(e.to_string());
            return p;
        }
    };
    if r.code != Some(0) {
        p.status = "error".into();
        p.note = Some(format!("`{cli} info --json` exited {}", exit_text(r.code)));
        return p;
    }
    let v: Value = match serde_json::from_slice(&r.stdout) {
        Ok(v) => v,
        Err(_) => {
            p.status = "error".into();
            p.note = Some("`info --json` did not print JSON".into());
            return p;
        }
    };
    if v.get("schema").and_then(Value::as_str) != Some("cli-info/v1") {
        p.status = "error".into();
        p.note = Some("`info --json` is not cli-info/v1".into());
        return p;
    }
    p.version = v.get("version").and_then(Value::as_str).map(str::to_string);
    p.profiles = v
        .get("profiles")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let has = |capability: &str| {
        v.get("capabilities")
            .and_then(Value::as_array)
            .is_some_and(|a| a.iter().any(|c| c.as_str() == Some(capability)))
    };
    p.login_profiles = has(LOGIN_PROFILES);
    p.records_files = has(RECORDS_FILES);
    let signs_in = v
        .pointer("/auth/required")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    p.status = if !p.profiles.iter().any(|x| x == stream::PROFILE) {
        p.note = Some(format!("does not declare {}", stream::PROFILE));
        "not-a-provider".into()
    } else if signs_in && !p.login_profiles {
        p.note = Some(format!(
            "it signs in but predates login profiles (its info lists no `{LOGIN_PROFILES}`); \
             upgrade it"
        ));
        "outdated".into()
    } else {
        "ok".into()
    };
    p
}

/// A family CLI's environment prefix: its name in upper case, `-` as `_`.
fn env_prefix(cli: &str) -> String {
    cli.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}

/// The exit 2 for an import that names no `--from-profile` from a provider
/// that needs one, listing the provider's profiles (`<cli> profiles list
/// --json`, profile-list/v1: a local read, no sign-in).
pub fn missing_profile(provider: &Provider) -> CliError {
    let cli = &provider.cli;
    let files = if provider.records_files {
        ", or its downloaded files: `--from-files <DIR>`"
    } else {
        ""
    };
    let lead = format!("importing from `{cli}` needs its profile: `--from-profile <NAME>`{files}");
    let listed = Bounded::new(&provider.path)
        .args(["profiles", "list", "--json"])
        .timeout(INFO_TIMEOUT)
        .max_stdout(1024 * 1024)
        .run()
        .ok()
        .filter(|r| r.code == Some(0))
        .and_then(|r| serde_json::from_slice::<ProfileList>(&r.stdout).ok())
        .filter(|l| l.schema == ProfileList::SCHEMA);
    let Some(list) = listed else {
        return CliError::Usage(format!(
            "{lead}; `{cli} profiles list --json` did not list its profiles"
        ));
    };
    let names: Vec<String> = list
        .profiles
        .iter()
        .filter_map(|p| ProfileName::parse(&p.name).ok())
        .map(|n| n.to_string())
        .collect();
    if names.is_empty() {
        CliError::Usage(format!("{lead}; `{cli}` has no profile configured")).with_remedy(format!(
            "add one with `{cli} profiles add <NAME>` and sign in"
        ))
    } else {
        CliError::Usage(format!("{lead}; {cli}'s profiles: {}", names.join(", ")))
    }
}

/// Every provider medrec knows of: the known names and the configured ones.
pub fn providers(cfg: &Config) -> Vec<Provider> {
    let mut names: Vec<String> = KNOWN_PROVIDERS.iter().map(|s| s.to_string()).collect();
    for n in cfg.extra_providers() {
        if !names.contains(&n) {
            names.push(n);
        }
    }
    names.iter().map(|n| probe(n)).collect()
}

pub struct Request {
    pub from: String,
    /// The provider's login profile, passed to its export as `--profile`.
    pub from_profile: Option<ProfileName>,
    /// Downloaded files to read instead of a login, passed as `--files`.
    pub files: Option<PathBuf>,
    pub patient: String,
    pub person: Option<String>,
    pub since: Option<String>,
    pub org: Option<String>,
    pub allow_birth_date_mismatch: bool,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub import: String,
    pub person: String,
    pub person_name: String,
    pub cli: String,
    pub cli_version: String,
    pub organization: String,
    pub patient_id: String,
    pub source_dir: String,
    pub resources: u64,
    pub counts: BTreeMap<String, u64>,
    pub skipped: usize,
    /// Types the source served only some of: it withheld the rest (a
    /// `forbidden` skip naming the type, with no id or search). Each type is
    /// passed through [`printable`](crate::printable).
    pub withheld_types: Vec<String>,
    /// The same, by the reason line the source gave.
    pub withheld: Vec<Withheld>,
    /// How many of `skipped` say so, for the human output.
    #[serde(skip)]
    pub withheld_skips: usize,
    pub resources_written: usize,
    pub resources_unchanged: usize,
    pub records_added: usize,
    pub records_updated: usize,
    pub records_unchanged: usize,
    /// Versions of records older (`meta.lastUpdated`) than the ones filed,
    /// which stayed.
    pub records_older: usize,
    pub documents_added: usize,
    pub documents_duplicate: usize,
    /// Documents the source served again with other bytes but the same text
    /// (a rendering it makes on each export): the one filed before stays.
    pub documents_unchanged: usize,
    /// Bills filed that are the same statement as a bill filed before (a
    /// scan the user filed, and the portal's PDF): each points at it.
    pub documents_same_statement: usize,
    pub stream_bytes: u64,
}

/// The provider's bytes, hashed on the way through.
struct Tee<R: Read> {
    inner: R,
    hasher: Sha256,
    bytes: u64,
    /// `import_max_mb`: a stream past this is refused, since an import
    /// holds every resource until the trailer.
    max: u64,
}

impl<R: Read> Read for Tee<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.hasher.update(&buf[..n]);
        self.bytes += n as u64;
        if self.bytes > self.max {
            return Err(std::io::Error::other(format!(
                "the stream is larger than import_max_mb ({} MiB); raise it with \
                 `medrec config set import_max_mb <MiB>` or import with --since",
                self.max / (1024 * 1024)
            )));
        }
        Ok(n)
    }
}

struct Binary {
    id: String,
    content_type: Option<String>,
    bytes: Vec<u8>,
}

/// A resource whose attachment is a Binary in the stream: a
/// DocumentReference (`content[].attachment`), a DiagnosticReport
/// (`presentedForm[]`) or a Media (`content`). The Binary is filed as a
/// document with its date, title and type.
#[derive(Default)]
struct Owner {
    kind: OwnerKind,
    id: String,
    version: Option<String>,
    last_updated: Option<String>,
    /// A DocumentReference's type, or a DiagnosticReport's code (LOINC).
    type_code: Option<String>,
    type_text: Option<String>,
    /// medrec's document type, when the resource says it outright (a
    /// DiagnosticReport's lab or imaging category); otherwise read from
    /// `type_code`.
    doc_type: Option<&'static str>,
    date: Option<String>,
    title: Option<String>,
    /// The attachments' `url`s: how the stream ties a Binary to it.
    urls: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum OwnerKind {
    #[default]
    DocumentReference,
    DiagnosticReport,
    Media,
}

impl OwnerKind {
    fn from_resource_type(t: &str) -> Option<Self> {
        match t {
            "DocumentReference" => Some(Self::DocumentReference),
            "DiagnosticReport" => Some(Self::DiagnosticReport),
            "Media" => Some(Self::Media),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::DocumentReference => "DocumentReference",
            Self::DiagnosticReport => "DiagnosticReport",
            Self::Media => "Media",
        }
    }
}

struct Streamed {
    header: Header,
    trailer: stream::Trailer,
    resources: Vec<RawResource>,
    binaries: Vec<Binary>,
    skipped: Vec<Skip>,
}

pub fn run(
    folder: &Folder,
    cfg: &Config,
    provider: &Provider,
    req: &Request,
) -> Result<Report, CliError> {
    // A person who cannot be resolved fails before the provider runs; the
    // resolution that counts is made again under the lock, below.
    who(&folder.household()?, req)?;
    let max_binary = cfg.max_file_bytes();

    let mut cmd = Bounded::new(&provider.path)
        .args([
            "records",
            "export",
            "--patient",
            req.patient.as_str(),
            "--ndjson",
        ])
        .timeout(cfg.import_timeout())
        .max_stdout(usize::MAX);
    if let Some(s) = &req.since {
        cmd = cmd.args(["--since", s.as_str()]);
    }
    if let Some(o) = &req.org {
        cmd = cmd.args(["--org", o.as_str()]);
    }
    // One source per export: the variable for the other one stays out of
    // the provider's environment.
    let prefix = env_prefix(&req.from);
    if let Some(dir) = &req.files {
        cmd = cmd
            .arg("--files")
            .arg(dir)
            .env_remove(format!("{prefix}_PROFILE"));
    } else if let Some(p) = &req.from_profile {
        cmd = cmd
            .args(["--profile", p.as_str()])
            .env_remove(format!("{prefix}_FILES"));
    }
    let mut child = cmd.stream()?;
    let mut tee = Tee {
        inner: child.stdout(),
        hasher: Sha256::new(),
        bytes: 0,
        max: cfg.import_max_bytes(),
    };
    // Lines stay within records-stream/v1's 64 MiB: a provider skips a
    // larger resource as too_large, so a Binary over about 48 MiB never
    // arrives, whatever max_file_mb says.
    let mut reader = RecordsReader::new(BufReader::with_capacity(256 * 1024, &mut tee));
    let read = read_stream(&mut reader, max_binary);
    let streamed = match read {
        Ok(v) => v,
        Err(e) => {
            let finished = child.finish_within(Duration::from_secs(5));
            return Err(stream_failure(&req.from, e, finished, req.org.as_deref()));
        }
    };
    let (stream_sha, stream_bytes) = (hash::hex(&tee.hasher.finalize()), tee.bytes);
    let finished = child.finish()?;
    if finished.status.code() != Some(0) {
        return Err(provider_exit(
            &req.from,
            finished.status.code(),
            &finished.stderr_tail,
            req.org.as_deref(),
        ));
    }
    let Streamed {
        header,
        trailer,
        resources,
        binaries,
        skipped: skipped_large,
    } = streamed;

    // The folder lock is taken only now, so intakes and other writers carry
    // on while the provider exports, and the household is read again under
    // it: another import or a `people` change may have written it since.
    // Giving up would throw the export away, so it waits as long as an
    // export may run, and it refuses a folder replaced in the meantime.
    let writing = Folder::open_to_write_within(cfg, cfg.import_timeout())?;
    if writing.folder_id != folder.folder_id {
        return Err(CliError::Other(
            "household.json was replaced while this command ran".into(),
        ));
    }
    let folder = &writing;
    let mut household = folder.household()?;
    let (person, known_patients) = match who(&household, req)? {
        Who::Known(person, known) => (*person, known),
        Who::ByStream(elsewhere) => by_stream(&household, &elsewhere, &header, req)?,
    };

    // A re-import without --person files under whoever this patient was
    // filed under before, so the stream must be about a patient filed there.
    if !known_patients.is_empty() && !known_patients.contains(&header.patient.id) {
        return Err(CliError::Usage(format!(
            "{} returned records for a patient medrec has not imported as `{}` before; name \
             whose records they are with --person",
            req.from, req.patient
        )));
    }

    // A stream about someone else must not land in this person's records.
    let source_birth = resources
        .iter()
        .filter(|r| r.resource_type == "Patient")
        .filter_map(|r| r.value().ok())
        .find_map(|v| {
            v.get("birthDate")
                .and_then(Value::as_str)
                .map(str::to_string)
        });
    if let (Some(ours), Some(theirs)) = (&person.birth_date, &source_birth) {
        if ours != theirs && !req.allow_birth_date_mismatch {
            return Err(CliError::Usage(format!(
                "the source's patient has a different birth date than {} in medrec; check --person \
                 (or pass --allow-birth-date-mismatch to import anyway). Nothing was written.",
                person.name
            )));
        }
    }

    let source = source_slug(&header.source.cli, &header.organization.id);
    let source_dir = folder::records_dir(&person, &source);
    let mut index = folder.index(&person)?;
    let mut texts = folder.text(&person)?;
    let imported_at = now_rfc3339();
    let base_prov = Provenance {
        source: ProvenanceSource {
            kind: source_kind(&header).into(),
            cli: Some(header.source.cli.clone()),
            cli_version: Some(header.source.version.clone()),
            organization_id: Some(header.organization.id.clone()),
            fhir_base: Some(
                header
                    .organization
                    .fhir_base
                    .trim_end_matches('/')
                    .to_string(),
            ),
        },
        method: header.method.clone().unwrap_or_else(|| "fhir-api".into()),
        source_resource: None,
        fetched_at: Some(header.pulled_at.clone()),
        filed_at: imported_at.clone(),
        raw_path: None,
        classified_by: None,
        derived_from: None,
        filed_by: filed_by(),
    };
    let written = write_raw(
        folder,
        &source_dir,
        &resources,
        &filed_versions(&index),
        &base_prov,
    )?;

    let mut owners = Vec::new();
    let (mut added, mut updated, mut unchanged, mut older) = (0, 0, 0, 0);
    let mut idless = Idless {
        incoming: resources
            .iter()
            .filter(|r| r.id.is_empty())
            .filter_map(|r| written.paths.get(&raw_sub(r)).cloned())
            .collect(),
        claimed: HashSet::new(),
    };
    let mut prescribers = stream_prescribers(&resources);
    for r in &resources {
        let Ok(v) = r.value() else { continue };
        if let Some(o) = owner(&r.resource_type, &v) {
            owners.push(o);
        }
        let mut prov = base_prov.clone();
        prov.source_resource = source_resource(&v);
        let sub = raw_sub(r);
        prov.raw_path = written.paths.get(&sub).cloned();
        let filing = written
            .filings
            .get(&format!("{source_dir}/{sub}.json"))
            .copied();
        if let Some(mut rec) = normalize::record(&v, prov) {
            let prescription = (r.resource_type == "MedicationDispense"
                && !rec.fields.contains_key("prescriber"))
            .then(|| normalize::path_str(&v, "authorizingPrescription.0.reference"))
            .flatten()
            .and_then(request_id);
            if let Some(id) = prescription {
                if !prescribers.contains_key(id) {
                    // Filed by an earlier import: its own raw file, under
                    // this source.
                    let rel = format!("{source_dir}/{}.json", id_sub("MedicationRequest", id));
                    let named = folder
                        .store
                        .read_text(&rel)?
                        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                        .as_ref()
                        .and_then(requester);
                    prescribers.insert(id.to_string(), named);
                }
                if let Some(Some(name)) = prescribers.get(id) {
                    rec.fields
                        .insert("prescriber".into(), serde_json::json!(name));
                }
            }
            match merge(&mut index, rec, filing, &mut idless) {
                Merge::Added => added += 1,
                Merge::Updated => updated += 1,
                Merge::Unchanged => unchanged += 1,
                Merge::Older => older += 1,
            }
        }
    }
    let eob_docs = eob_document_links(&resources);

    let filed = file_binaries(
        folder,
        cfg,
        &household,
        &mut index,
        &mut texts,
        &person,
        &header,
        &base_prov,
        binaries,
        &owners,
        &eob_docs,
        &written.paths,
    )?;

    let counts = trailer.counts.clone();
    let total: u64 = counts.values().sum();
    let import_id = ids::new("i");
    let mut skipped = trailer.skipped.clone();
    skipped.extend(skipped_large);
    index.imports.push(ImportRun {
        id: import_id.clone(),
        source_dir: source_dir.clone(),
        stream_sha256: stream_sha,
        stream_bytes,
        cli: header.source.cli.clone(),
        cli_version: header.source.version.clone(),
        organization: header.organization.id.clone(),
        patient_id: header.patient.id.clone(),
        pulled_at: header.pulled_at.clone(),
        imported_at,
        counts: counts.clone(),
        skipped: skipped.clone(),
        records_added: added,
        records_updated: updated,
        records_unchanged: unchanged,
        records_older: older,
        resources_written: written.written,
        resources_unchanged: written.unchanged,
        documents_added: filed.added,
    });
    folder.save_index(&person, &mut index)?;
    folder.save_text(&person, &texts)?;

    record_source(&mut household, &person.id, &req.from, &header);
    folder.save_household(&mut household)?;

    Ok(Report {
        import: import_id,
        person: person.id.clone(),
        person_name: person.name.clone(),
        cli: header.source.cli,
        cli_version: header.source.version,
        organization: header.organization.name,
        patient_id: header.patient.id,
        source_dir,
        resources: total,
        counts,
        skipped: skipped.len(),
        withheld_types: withheld_types(&skipped),
        withheld: withheld(&skipped),
        withheld_skips: skipped.iter().filter_map(withheld_type).count(),
        resources_written: written.written,
        resources_unchanged: written.unchanged,
        records_added: added,
        records_updated: updated,
        records_unchanged: unchanged,
        records_older: older,
        documents_added: filed.added,
        documents_duplicate: filed.duplicate,
        documents_unchanged: filed.unchanged,
        documents_same_statement: filed.same_statement,
        stream_bytes,
    })
}

/// Resources of these types the source withheld, for this reason (a line
/// from the server, which carries no record content). Both are
/// [`printable`](crate::printable): the human output shows them as they are.
#[derive(Debug, PartialEq, Serialize)]
pub struct Withheld {
    pub reason: String,
    pub resource_types: Vec<String>,
}

/// A skip saying the source withheld some resources of a type (medical-common
/// DESIGN §1.4): `forbidden`, with a `resource_type` and neither `id` nor
/// `search`. The resources it did serve are in the stream.
fn withheld_type(k: &Skip) -> Option<&str> {
    let withheld =
        k.code == pk_med_records::stream::skip::FORBIDDEN && k.id.is_none() && k.search.is_none();
    k.resource_type
        .as_deref()
        .filter(|t| withheld && !t.is_empty())
}

fn withheld_types(skipped: &[Skip]) -> Vec<String> {
    let types: BTreeSet<String> = skipped
        .iter()
        .filter_map(withheld_type)
        .map(crate::printable)
        .filter(|t| !t.is_empty())
        .collect();
    types.into_iter().collect()
}

/// The withheld types by reason, in the order the reasons first appear.
fn withheld(skipped: &[Skip]) -> Vec<Withheld> {
    let mut out: Vec<(String, BTreeSet<String>)> = Vec::new();
    for k in skipped {
        let Some(ty) = withheld_type(k) else { continue };
        let ty = crate::printable(ty);
        if ty.is_empty() {
            continue;
        }
        let reason = crate::printable(&k.reason);
        let reason = reason.trim();
        match out.iter_mut().find(|(r, _)| r == reason) {
            Some((_, types)) => {
                types.insert(ty);
            }
            None => out.push((reason.to_string(), BTreeSet::from([ty]))),
        }
    }
    out.into_iter()
        .map(|(reason, types)| Withheld {
            reason,
            resource_types: types.into_iter().collect(),
        })
        .collect()
}

/// Whose records, as far as the request says before the export runs.
enum Who {
    /// The person, and without `--person` the patient ids filed for them.
    Known(Box<Person>, Vec<String>),
    /// `--org` names no organization this patient was imported from, but the
    /// CLI did import them from another: possibly the same one under a name
    /// the provider also accepts (`--org sandbox` for its id). The stream
    /// header's organization and patient ids, which a source is filed under,
    /// decide among these.
    ByStream(Vec<Source>),
}

fn who(h: &Household, req: &Request) -> Result<Who, CliError> {
    if req.person.is_none() && req.org.is_some() && matching(h, req, true).is_empty() {
        let elsewhere: Vec<Source> = matching(h, req, false).into_iter().cloned().collect();
        if !elsewhere.is_empty() {
            return Ok(Who::ByStream(elsewhere));
        }
    }
    let (person, known) = resolve_person(h, req)?;
    Ok(Who::Known(Box::new(person.clone()), known))
}

/// [`Who::ByStream`], once the stream's header is read: the source filed
/// under its organization and patient ids. Nothing is written yet, so a
/// refusal leaves the folder as it was.
fn by_stream(
    h: &Household,
    elsewhere: &[Source],
    header: &Header,
    req: &Request,
) -> Result<(Person, Vec<String>), CliError> {
    let hits: Vec<&Source> = elsewhere
        .iter()
        .filter(|s| {
            s.organization.as_deref() == Some(header.organization.id.as_str())
                && s.patient_id.as_deref() == Some(header.patient.id.as_str())
        })
        .collect();
    let Some(first) = hits.first() else {
        return Err(CliError::Usage(format!(
            "medrec has not imported patient `{}` from {} at `{}` before; name whose records \
             they are with --person. Nothing was written.",
            req.patient,
            req.from,
            crate::printable(&header.organization.id)
        )));
    };
    if hits.iter().any(|s| s.person != first.person) {
        return Err(CliError::Usage(format!(
            "patient `{}` from {} was imported for more than one person; name whose records \
             they are with --person. Nothing was written.",
            req.patient, req.from
        )));
    }
    let person = h.person(&first.person).ok_or_else(|| {
        CliError::Other(format!("source points at unknown person {}", first.person))
    })?;
    Ok((person.clone(), vec![header.patient.id.clone()]))
}

/// The sources this CLI filed `--patient` under: by patient id, else by
/// label; under `--org` when `by_org` and it is given.
fn matching<'a>(h: &'a Household, req: &Request, by_org: bool) -> Vec<&'a Source> {
    let from_here = |s: &&Source| {
        s.cli.as_deref() == Some(req.from.as_str())
            && (!by_org || req.org.is_none() || s.organization == req.org)
    };
    let patient = Some(req.patient.as_str());
    let by_id: Vec<&Source> = h
        .sources
        .iter()
        .filter(from_here)
        .filter(|s| s.patient_id.as_deref() == patient)
        .collect();
    if !by_id.is_empty() {
        return by_id;
    }
    h.sources
        .iter()
        .filter(from_here)
        .filter(|s| s.patient_label.as_deref() == patient)
        .collect()
}

/// Whose records: `--person`, or the person this patient was imported for
/// before. `--patient` is whatever the provider takes: the patient's FHIR id,
/// or the provider's own label for them, which its stream header carries.
/// Without `--person`, also the patient ids those imports recorded, one of
/// which the stream must be about.
fn resolve_person<'a>(
    h: &'a Household,
    req: &Request,
) -> Result<(&'a Person, Vec<String>), CliError> {
    if let Some(q) = &req.person {
        return Ok((people::resolve(h, q)?, Vec::new()));
    }
    let matched = matching(h, req, true);
    let Some(first) = matched.first() else {
        return Err(CliError::Usage(format!(
            "medrec has not imported patient `{}` from {} before; name whose records they are \
             with --person",
            req.patient, req.from
        )));
    };
    if matched.iter().any(|s| s.person != first.person) {
        return Err(CliError::Usage(format!(
            "patient `{}` from {} was imported for more than one person; name whose records \
             they are with --person{}",
            req.patient,
            req.from,
            if req.org.is_none() {
                ", or the organization with --org"
            } else {
                ""
            }
        )));
    }
    let person = h.person(&first.person).ok_or_else(|| {
        CliError::Other(format!("source points at unknown person {}", first.person))
    })?;
    let known = matched
        .iter()
        .filter_map(|s| s.patient_id.clone())
        .collect();
    Ok((person, known))
}

fn read_stream<R: std::io::BufRead>(
    reader: &mut RecordsReader<R>,
    max_binary: u64,
) -> Result<Streamed, ReadFailure> {
    let header = reader.header().map_err(ReadFailure::Stream)?.clone();
    let mut resources = Vec::new();
    let mut binaries = Vec::new();
    let mut skipped = Vec::new();
    let trailer = loop {
        match reader.next() {
            Some(Ok(Item::Resource(raw))) => {
                if raw.resource_type == "Binary" {
                    let v = raw
                        .value()
                        .map_err(|e| ReadFailure::Other(format!("Binary {}: {e}", raw.id)))?;
                    let data: Vec<u8> = v
                        .get("data")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .bytes()
                        .filter(|b| !b.is_ascii_whitespace())
                        .collect();
                    // Its size from the base64 length, before decoding: a
                    // Binary over max_file_mb is neither decoded nor kept.
                    if (data.len() as u64 / 4) * 3 > max_binary {
                        skipped.push(Skip::resource(
                            "Binary",
                            raw.id.clone(),
                            pk_med_records::stream::skip::TOO_LARGE,
                            "larger than config max_file_mb; not kept",
                        ));
                        continue;
                    }
                    let bytes = base64::engine::general_purpose::STANDARD
                        .decode(data)
                        .map_err(|_| {
                            ReadFailure::Other(format!("Binary {} carries no valid base64", raw.id))
                        })?;
                    if !bytes.is_empty() {
                        binaries.push(Binary {
                            id: raw.id.clone(),
                            content_type: v
                                .get("contentType")
                                .and_then(Value::as_str)
                                .map(str::to_string),
                            bytes,
                        });
                    }
                }
                resources.push(raw);
            }
            Some(Ok(Item::Trailer(t))) => break t,
            Some(Err(e)) => return Err(ReadFailure::Stream(e)),
            None => return Err(ReadFailure::Stream(stream::StreamError::MissingTrailer)),
        }
    };
    // Read to the end so a line after the trailer is refused.
    if let Some(Err(e)) = reader.next() {
        return Err(ReadFailure::Stream(e));
    }
    Ok(Streamed {
        header,
        trailer,
        resources,
        binaries,
        skipped,
    })
}

#[derive(Debug)]
enum ReadFailure {
    Stream(stream::StreamError),
    Other(String),
}

/// `mychart-nyu-langone`: the CLI and the organization, as a folder name.
pub fn source_slug(cli: &str, org: &str) -> String {
    let s = folder::slug(&format!("{cli} {org}"), 60).replace('_', "-");
    if s.is_empty() {
        "source".into()
    } else {
        s
    }
}

/// A FHIR id as a file name. FHIR R4 caps ids at 64 of `[A-Za-z0-9.-]`, but
/// Epic's unconstrained ids (its default) run longer, up to pk-med-fhir's
/// `MAX_ID_LEN` of 256. A safe name of at most 64 characters is kept as it
/// is; anything else is named by its hash, the same on every run.
fn id_file(id: &str) -> String {
    let ok = !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
        && !id.starts_with('.');
    if ok {
        id.to_string()
    } else {
        format!("id-{}", &hash::sha256_hex(id.as_bytes())[..32])
    }
}

struct Written {
    /// Where each resource went in the records folder, by [`raw_sub`].
    paths: HashMap<String, String>,
    /// How the version served for each raw file already held was filed
    /// over it, by the file's path; a file not there before has none.
    filings: HashMap<String, Filing>,
    written: usize,
    unchanged: usize,
}

/// A resource's raw file under its source, without `.json`: its type and
/// id, or for a resource with no id, its type and its content's digest, so
/// two of them never share a file and one stamped again keeps its own.
fn raw_sub(r: &RawResource) -> String {
    if r.id.is_empty() {
        let digest = version::content_digest(r);
        format!("{}/noid-{}", r.resource_type, &digest[..32])
    } else {
        id_sub(&r.resource_type, &r.id)
    }
}

fn id_sub(resource_type: &str, id: &str) -> String {
    format!("{resource_type}/{}", id_file(id))
}

/// The provenance of the version each raw file the index's records were
/// filed from. Records only: a document's provenance names the resource
/// that describes it, not the Binary its `raw_path` is.
fn filed_versions(index: &PersonIndex) -> HashMap<String, Provenance> {
    index
        .records
        .iter()
        .flat_map(|r| r.provenance.iter())
        .filter_map(|p| Some((p.raw_path.clone()?, p.clone())))
        .collect()
}

/// Write each resource verbatim under `source_dir`, filing it over the
/// version there by medical-common's rule ([`version::decide`]): the same
/// content is left alone or, read later, overwritten in place; other content
/// that is later becomes current, the held version kept beside it; other
/// content that is not later goes beside the current one. One upload for the
/// lot, then every file's MD5 checked.
fn write_raw(
    folder: &Folder,
    source_dir: &str,
    resources: &[RawResource],
    filed: &HashMap<String, Provenance>,
    base: &Provenance,
) -> Result<Written, CliError> {
    let mut existing: HashMap<String, Option<String>> = folder
        .store
        .hashes(source_dir)?
        .into_iter()
        .map(|h| (h.rel, h.md5))
        .collect();
    // A refresh moves the held copy to `<id>~refresh-<tag>.json`, `<tag>`
    // being the start of the new copy's MD5, while that uploads, and removes
    // it after. One still here is from a refresh cut short. It goes when the
    // current copy is the one it made room for. Otherwise it goes back in
    // place, and a current copy that is not that one (an upload that did not
    // read back) is kept beside it as an earlier version.
    let leftovers: Vec<String> = existing
        .keys()
        .filter(|k| k.contains(REFRESHING))
        .cloned()
        .collect();
    for left in leftovers {
        let Some(at) = left.find(REFRESHING) else {
            continue;
        };
        let stem = &left[..at];
        let tag = left[at + REFRESHING.len()..].trim_end_matches(".json");
        let current = format!("{stem}.json");
        let md5 = existing.remove(&left).flatten();
        match existing.get(&current).cloned() {
            Some(Some(m)) if !tag.is_empty() && m.starts_with(tag) => {
                folder.store.delete(&left)?;
                continue;
            }
            Some(unverified) => {
                let sub = stem
                    .strip_prefix(&format!("{source_dir}/"))
                    .unwrap_or(stem)
                    .to_string();
                if let Some(kept) = move_aside(
                    folder,
                    &existing,
                    source_dir,
                    &sub,
                    &current,
                    unverified.clone(),
                )? {
                    existing.insert(kept, unverified);
                }
                existing.remove(&current);
            }
            None => {}
        }
        folder.store.move_no_clobber(&left, &current)?;
        existing.insert(current, md5);
    }
    let staging = pk_cli_drive::private_temp_dir("medrec-import")?;
    let mut paths = HashMap::new();
    let mut filings = HashMap::new();
    let mut staged: Vec<(String, String)> = Vec::new();
    // Held copies moved aside only to be overwritten: removed once the
    // upload reads back.
    let mut overwritten: Vec<String> = Vec::new();
    let mut unchanged = 0;
    let mut latest: HashMap<String, usize> = HashMap::new();
    for (i, r) in resources.iter().enumerate() {
        // A stream carries each resource once; if it repeats one, the last
        // copy wins.
        latest.insert(raw_sub(r), i);
    }
    let mut planned: Vec<(String, usize, String, String, String)> = latest
        .into_iter()
        .map(|(sub, i)| {
            let mut body = resources[i].raw.clone();
            body.push('\n');
            let md5 = hash::md5_hex(body.as_bytes());
            let rel = format!("{source_dir}/{sub}.json");
            (sub, i, rel, body, md5)
        })
        .collect();
    planned.sort();
    // The held copies whose bytes differ are read to compare content: one
    // copy of each type's folder that has any.
    let held_dir = pk_cli_drive::private_temp_dir("medrec-held")?;
    let mut fetched: HashSet<String> = HashSet::new();
    for (_, i, rel, _, md5) in &planned {
        if matches!(existing.get(rel), Some(old) if old.as_deref() != Some(md5.as_str())) {
            let ty = &resources[*i].resource_type;
            if fetched.insert(ty.clone()) {
                folder
                    .store
                    .fetch_tree(&format!("{source_dir}/{ty}"), &held_dir.path().join(ty))?;
            }
        }
    }
    for (sub, i, rel, body, md5) in planned {
        let r = &resources[i];
        paths.insert(sub.clone(), rel.clone());
        let value = r.value().ok();
        let mut served = base.clone();
        served.source_resource = value.as_ref().and_then(source_resource);
        let held_md5 = existing.get(&rel);
        let same_bytes = held_md5.is_some_and(|m| m.as_deref() == Some(md5.as_str()));
        let filing = match held_md5 {
            None => None,
            Some(_) => {
                let held: Option<RawResource> = if same_bytes {
                    Some(r.clone())
                } else {
                    std::fs::read_to_string(held_dir.path().join(format!("{sub}.json")))
                        .ok()
                        .and_then(|t| RawResource::parse(t.trim_end()).ok())
                };
                // A file no record was filed from (a Binary, a Practitioner)
                // has no time in the index: unknown, so other content
                // replaces it.
                let held_prov = filed.get(&rel).cloned().unwrap_or_else(|| {
                    let mut p = base.clone();
                    p.source_resource = None;
                    p.fetched_at = None;
                    p
                });
                Some(match &held {
                    Some(h) => version::decide(h, &held_prov, r, &served),
                    // Unreadable: other content.
                    None if served.supersedes(&held_prov) => Filing::Replaces,
                    None => Filing::Older,
                })
            }
        };
        if let Some(f) = filing {
            filings.insert(rel.clone(), f);
        }
        match filing {
            Some(Filing::Unchanged) => {
                unchanged += 1;
                continue;
            }
            // The same bytes read later: only the provenance moves on.
            Some(Filing::Refreshed) if same_bytes => {
                unchanged += 1;
                continue;
            }
            Some(Filing::Older) => {
                // It goes beside the current version under its own hash.
                let mut tag = md5[..8].to_string();
                let mut aside = format!("{source_dir}/{sub}~{tag}.json");
                match existing.get(&aside) {
                    Some(Some(m)) if *m == md5 => {
                        paths.insert(sub, aside);
                        unchanged += 1;
                        continue;
                    }
                    Some(_) => {
                        tag = md5.clone();
                        aside = format!("{source_dir}/{sub}~{tag}.json");
                        if existing.contains_key(&aside) {
                            paths.insert(sub, aside);
                            unchanged += 1;
                            continue;
                        }
                    }
                    None => {}
                }
                paths.insert(sub.clone(), aside.clone());
                let local = staging.path().join(format!("{sub}~{tag}.json"));
                if let Some(parent) = local.parent() {
                    crate::store::private_dirs(parent)?;
                }
                crate::write_private(&local, body.as_bytes())?;
                staged.push((aside, md5));
                continue;
            }
            Some(Filing::Refreshed) => {
                let room = format!("{source_dir}/{sub}{REFRESHING}{}.json", &md5[..8]);
                folder.store.move_no_clobber(&rel, &room)?;
                overwritten.push(room);
            }
            Some(Filing::Replaces) => {
                let old = held_md5.cloned().flatten();
                move_aside(folder, &existing, source_dir, &sub, &rel, old)?;
            }
            None => {}
        }
        let local = staging.path().join(format!("{sub}.json"));
        if let Some(parent) = local.parent() {
            crate::store::private_dirs(parent)?;
        }
        crate::write_private(&local, body.as_bytes())?;
        staged.push((rel, md5));
    }
    if !staged.is_empty() {
        folder.store.put_tree(staging.path(), source_dir)?;
        let after: HashMap<String, Option<String>> = folder
            .store
            .hashes(source_dir)?
            .into_iter()
            .map(|h| (h.rel, h.md5))
            .collect();
        for (rel, md5) in &staged {
            match after.get(rel) {
                Some(Some(got)) if got == md5 => {}
                _ => {
                    return Err(CliError::Upstream(format!(
                        "{} did not read back with the MD5 that was sent",
                        folder.store.display_path(rel)
                    )))
                }
            }
        }
    }
    for kept in &overwritten {
        folder.store.delete(kept)?;
    }
    Ok(Written {
        paths,
        filings,
        written: staged.len(),
        unchanged,
    })
}

/// The mark of a held copy moved aside only while a refresh uploads.
const REFRESHING: &str = "~refresh-";

/// Move the current copy at `rel` aside before a new one is uploaded
/// (uploads never replace a file), as an earlier version, and return where
/// it went. When those bytes are already kept, from an earlier change back
/// and forth, the copy at `rel` is removed instead.
fn move_aside(
    folder: &Folder,
    existing: &HashMap<String, Option<String>>,
    source_dir: &str,
    sub: &str,
    rel: &str,
    old: Option<String>,
) -> Result<Option<String>, CliError> {
    let tag = old
        .as_deref()
        .and_then(|m| m.get(..8))
        .unwrap_or("prev")
        .to_string();
    let mut kept = format!("{source_dir}/{sub}~{tag}.json");
    let mut n = 2;
    loop {
        match existing.get(&kept) {
            None => {
                folder.store.move_no_clobber(rel, &kept)?;
                return Ok(Some(kept));
            }
            Some(k) if old.is_some() && *k == old => {
                folder.store.delete(rel)?;
                return Ok(None);
            }
            Some(_) => {
                kept = format!("{source_dir}/{sub}~{tag}-{n}.json");
                n += 1;
            }
        }
    }
}

fn stream_failure(
    cli: &str,
    e: ReadFailure,
    finished: Result<crate::proc::Finished, CliError>,
    org: Option<&str>,
) -> CliError {
    // A provider that exited nonzero says why better than a cut stream.
    match finished {
        Ok(f) if f.status.code() != Some(0) => {
            provider_exit(cli, f.status.code(), &f.stderr_tail, org)
        }
        Err(timeout) => timeout,
        _ => {
            let why = match e {
                ReadFailure::Stream(s) => s.to_string(),
                ReadFailure::Other(s) => s,
            };
            CliError::Upstream(format!(
                "{cli}'s records stream was refused: {why}. Nothing was written."
            ))
        }
    }
}

/// A provider's exit code, read as the family contract defines it.
pub fn provider_exit(cli: &str, code: Option<i32>, stderr: &str, org: Option<&str>) -> CliError {
    let line = stderr
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("");
    let line = crate::printable(line);
    let line = line.trim();
    let line = line.strip_prefix("error: ").unwrap_or(line);
    let paren = |s: &str| {
        if s.is_empty() {
            String::new()
        } else {
            format!(" ({s})")
        }
    };
    let why = paren(line);
    match code {
        Some(3) => {
            // The provider's own sign-in command is more exact than medrec's
            // guess (it may name a patient label too), so it replaces it, and
            // the reason is shown without it.
            let (reason, login) = match provider_login(cli, line) {
                Some((reason, login)) => (reason, login),
                None => (
                    line,
                    match org {
                        Some(o) => format!("{cli} auth login --org {o}"),
                        None => format!("{cli} auth login"),
                    },
                ),
            };
            CliError::Auth(format!("{cli} needs a sign-in{}", paren(reason)))
                .with_remedy(format!("run `{login}`, then import again"))
        }
        Some(4) => CliError::NotFound(format!("{cli} does not know that patient{why}")),
        Some(5) => CliError::Upstream(format!(
            "{cli}'s provider did not answer; try again later{why}"
        )),
        // What medrec passes on is mostly the caller's: the profile, the
        // patient, the organization, the date.
        Some(2) => CliError::Usage(format!("{cli} refused the export's arguments{why}")),
        other => CliError::Upstream(format!(
            "{cli} exited {}{why}; nothing was written",
            exit_text(other)
        )),
    }
}

/// A provider's sign-in remedy: the family prints an error's remedy after
/// its message and an em dash (``not signed in — run `kp auth login --org o
/// --label l` ``). The message, and the command when it is the provider's
/// own `auth login`.
fn provider_login<'a>(cli: &str, line: &'a str) -> Option<(&'a str, String)> {
    let (reason, remedy) = line.rsplit_once(" \u{2014} ")?;
    let command = remedy.split('`').nth(1)?.trim();
    let login = format!("{cli} auth login");
    let rest = command.strip_prefix(&login)?;
    (rest.is_empty() || rest.starts_with(' ')).then(|| (reason.trim(), command.to_string()))
}

fn exit_text(code: Option<i32>) -> String {
    code.map(|c| c.to_string())
        .unwrap_or_else(|| "on a signal".into())
}

fn source_resource(r: &Value) -> Option<SourceResource> {
    Some(SourceResource {
        resource_type: r.get("resourceType")?.as_str()?.into(),
        id: r.get("id").and_then(Value::as_str).unwrap_or("").into(),
        version_id: normalize::path_str(r, "meta.versionId").map(str::to_string),
        last_updated: normalize::path_str(r, "meta.lastUpdated").map(str::to_string),
    })
}

#[derive(Debug)]
enum Merge {
    Added,
    Updated,
    Unchanged,
    /// A version older than the one filed, which stays.
    Older,
}

/// File `rec`, normalized from one resource, into `index`. `filing` is how
/// the raw layer filed the version it came from over the one it held
/// ([`write_raw`]), `None` when it held none. `idless` is what this import
/// knows of its resources with no id.
///
/// The same source resource updates its record; the same fact from another
/// source joins it with a second provenance; anything else is new. A record
/// that holds other resources too, whose key this resource no longer
/// shares, lets this one go to a record of its own: a claim folded under
/// the type-and-date key before 0.2.6 splits on its next import.
fn merge(
    index: &mut PersonIndex,
    rec: Record,
    filing: Option<Filing>,
    idless: &mut Idless,
) -> Merge {
    let incoming = rec.provenance[0].clone();
    let incoming_type = incoming
        .source_resource
        .as_ref()
        .map(|s| s.resource_type.clone());
    let incoming_id = incoming
        .source_resource
        .as_ref()
        .filter(|s| !s.id.is_empty())
        .map(|s| (s.resource_type.clone(), s.id.clone()));
    // An id is unique only on its own server: the source is part of it.
    let same_source = |p: &Provenance| {
        p.source.fhir_base == incoming.source.fhir_base
            && p.source.cli == incoming.source.cli
            && p.source.organization_id == incoming.source.organization_id
    };
    let same_resource = |p: &Provenance| {
        incoming_id.is_some()
            && same_source(p)
            && p.source_resource
                .as_ref()
                .map(|s| (s.resource_type.clone(), s.id.clone()))
                == incoming_id
    };
    // A resource with no id is known only by its key: on the record its key
    // names, the provenances of its type from its source with no id are
    // those of the resources like it, each found by its raw file (named by
    // its content).
    let same_idless = |p: &Provenance| {
        incoming_id.is_none()
            && same_source(p)
            && p.source_resource.as_ref().map(|s| s.resource_type.clone()) == incoming_type
            && p.source_resource.as_ref().is_none_or(|s| s.id.is_empty())
    };
    if filing == Some(Filing::Older)
        && index.records.iter().any(|r| {
            r.provenance
                .iter()
                .any(|p| same_resource(p) || (r.key == rec.key && same_idless(p)))
        })
    {
        return Merge::Older;
    }
    if incoming_id.is_some() {
        if let Some(existing) = index
            .records
            .iter_mut()
            .find(|r| r.provenance.iter().any(same_resource))
            .filter(|r| r.key == rec.key || r.provenance.iter().all(same_resource))
        {
            let at = existing.provenance.iter().position(same_resource);
            // The first resource from this source on the record writes its
            // fields; another from it that shares the key keeps only its own
            // provenance, so the two do not write over each other.
            let owner = existing.provenance.iter().position(same_source);
            if let Some(i) = at.filter(|&i| Some(i) != owner) {
                return refresh_provenance(existing, i, incoming, filing);
            }
            return rewrite(existing, rec, incoming, at, filing);
        }
        if let Some(shared) = index
            .records
            .iter_mut()
            .find(|r| r.provenance.iter().any(same_resource))
        {
            shared.provenance.retain(|p| !same_resource(p));
        }
        if let Some(existing) = index.records.iter_mut().find(|r| r.key == rec.key) {
            existing.provenance.push(incoming);
            return Merge::Updated;
        }
        index.records.push(rec);
        return Merge::Added;
    }
    let key = rec.key.clone();
    let Some(existing) = index.records.iter_mut().find(|r| r.key == rec.key) else {
        index.records.push(rec);
        idless.claimed.insert((key, 0));
        return Merge::Added;
    };
    let mine: Vec<usize> = existing
        .provenance
        .iter()
        .enumerate()
        .filter(|(_, p)| same_idless(p))
        .map(|(i, _)| i)
        .collect();
    let free = |i: &usize| !idless.claimed.contains(&(key.clone(), *i));
    // Its own provenance names its raw file. Failing that (a version with
    // other content, or one filed before 0.2.7), it takes the first one that
    // no resource of this import names.
    let own = mine
        .iter()
        .copied()
        .filter(free)
        .find(|&i| {
            incoming.raw_path.is_some() && existing.provenance[i].raw_path == incoming.raw_path
        })
        .or_else(|| {
            mine.iter().copied().filter(free).find(|&i| {
                existing.provenance[i]
                    .raw_path
                    .as_ref()
                    .is_none_or(|p| !idless.incoming.contains(p))
            })
        });
    match own {
        // The first of them writes the record, whatever order they come in,
        // as a resource with an id that shares another's key keeps the first
        // one's fields.
        Some(i) => {
            idless.claimed.insert((key, i));
            if mine.first() == Some(&i) {
                return rewrite(existing, rec, incoming, Some(i), filing);
            }
            refresh_provenance(existing, i, incoming, filing)
        }
        None => {
            idless.claimed.insert((key, existing.provenance.len()));
            existing.provenance.push(incoming);
            Merge::Updated
        }
    }
}

/// Each MedicationRequest's prescriber in the stream, by id. A fill names
/// its prescription as `MedicationRequest/<id>` (medical-common DESIGN
/// §2.1); one filed by an earlier import is read from its own raw file.
fn stream_prescribers(resources: &[RawResource]) -> HashMap<String, Option<String>> {
    resources
        .iter()
        .filter(|r| r.resource_type == "MedicationRequest")
        .filter_map(|r| Some((r.id.clone(), requester(&r.value().ok()?))))
        .collect()
}

/// A MedicationRequest's prescriber: `requester.display` (R4), or
/// `requester.agent.display` (STU3).
fn requester(v: &Value) -> Option<String> {
    normalize::path_str(v, "requester.display")
        .or_else(|| normalize::path_str(v, "requester.agent.display"))
        .map(str::to_string)
}

/// The id in a reference to a MedicationRequest (`MedicationRequest/<id>`,
/// relative or absolute, with or without `/_history/<v>`).
fn request_id(reference: &str) -> Option<&str> {
    let rest = reference.rsplit_once("MedicationRequest/")?.1;
    rest.split('/').next().filter(|id| !id.is_empty())
}

/// What one import knows of its resources with no id: the raw files they
/// were filed to, and which id-less provenances on records they have taken.
#[derive(Default)]
struct Idless {
    incoming: HashSet<String>,
    claimed: HashSet<(String, usize)>,
}

/// Put `incoming` in place of the record's provenance at `i`, leaving its
/// fields to the resource that writes them.
fn refresh_provenance(
    existing: &mut Record,
    i: usize,
    incoming: Provenance,
    filing: Option<Filing>,
) -> Merge {
    match filing {
        Some(Filing::Unchanged) => Merge::Unchanged,
        Some(Filing::Refreshed) => {
            existing.provenance[i] = existing.provenance[i].refreshed_by(&incoming);
            Merge::Unchanged
        }
        _ if existing.provenance[i] == incoming => Merge::Unchanged,
        _ => {
            existing.provenance[i] = incoming;
            Merge::Updated
        }
    }
}

/// Whether two records say the same thing.
fn same_content(a: &Record, b: &Record) -> bool {
    a.key == b.key
        && a.fields == b.fields
        && a.display == b.display
        && a.status == b.status
        && a.code == b.code
        && a.date == b.date
}

/// Write `rec` over the record `existing`, whose provenance at `at` is this
/// resource's own. A version filed new or in place of the held one takes
/// that provenance's place; one read again refreshes it
/// (`Provenance::refreshed_by`, which keeps how it was filed). A record that
/// says the same thing from the same version is unchanged.
fn rewrite(
    existing: &mut Record,
    rec: Record,
    incoming: Provenance,
    at: Option<usize>,
    filing: Option<Filing>,
) -> Merge {
    let changed = !same_content(existing, &rec);
    let new_version = matches!(filing, None | Some(Filing::Replaces))
        && at.is_none_or(|i| existing.provenance[i] != incoming);
    match (filing, at) {
        (Some(Filing::Refreshed), Some(i)) => {
            existing.provenance[i] = existing.provenance[i].refreshed_by(&incoming)
        }
        (None | Some(Filing::Replaces), Some(i)) => existing.provenance[i] = incoming,
        (None | Some(Filing::Refreshed | Filing::Replaces), None) => {
            existing.provenance.push(incoming)
        }
        _ => {}
    }
    if !changed && !new_version {
        return Merge::Unchanged;
    }
    existing.fields = rec.fields;
    existing.display = rec.display;
    existing.status = rec.status;
    existing.code = rec.code;
    existing.date = rec.date;
    existing.key = rec.key;
    Merge::Updated
}

fn owner(resource_type: &str, v: &Value) -> Option<Owner> {
    let date = |paths: &[&str]| {
        paths
            .iter()
            .find_map(|p| normalize::path_str(v, p))
            .map(|d| d.chars().take(10).collect())
    };
    let text = |p: &str| normalize::path_str(v, p).map(str::to_string);
    let urls = |list: &str, url: &str| -> Vec<String> {
        v.get(list)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|c| normalize::path_str(c, url))
            .map(str::to_string)
            .collect()
    };
    let kind = OwnerKind::from_resource_type(resource_type)?;
    let mut o = Owner {
        kind,
        id: v.get("id").and_then(Value::as_str).unwrap_or("").into(),
        version: text("meta.versionId"),
        last_updated: text("meta.lastUpdated"),
        ..Owner::default()
    };
    match kind {
        OwnerKind::DocumentReference => {
            let concept = v
                .get("type")
                .and_then(|t| normalize::codeable(t, &[normalize::LOINC]));
            o.type_code = concept.as_ref().and_then(|c| c.code.clone());
            o.type_text = concept.and_then(|c| c.display);
            o.doc_type = house_doc_type(v);
            o.date = date(&["date", "context.period.start"]);
            o.title = text("description").or_else(|| text("content.0.attachment.title"));
            o.urls = urls("content", "attachment.url");
        }
        OwnerKind::DiagnosticReport => {
            let concept = v
                .get("code")
                .and_then(|c| normalize::codeable(c, &[normalize::LOINC]));
            o.type_code = concept.as_ref().and_then(|c| c.code.clone());
            o.type_text = concept.and_then(|c| c.display);
            o.doc_type = report_doc_type(v);
            o.date = date(&["effectiveDateTime", "effectivePeriod.start", "issued"]);
            o.title = text("code.text")
                .or_else(|| o.type_text.clone())
                .or_else(|| text("presentedForm.0.title"));
            o.urls = urls("presentedForm", "url");
        }
        OwnerKind::Media => {
            o.date = date(&["createdDateTime", "createdPeriod.start", "issued"]);
            o.title = text("content.title")
                .or_else(|| text("modality.text"))
                .or_else(|| text("type.text"));
            o.urls = text("content.url").into_iter().collect();
        }
    }
    Some(o)
}

/// The house document type a DocumentReference names: a `type.coding` in
/// [`DOC_TYPE_SYSTEM`] with a house type's code, or, on a type with no LOINC
/// code, `type.text` `bill` or `receipt` (a billing CLI's statement or
/// receipt). Any other text is a display, and a LOINC code decides.
fn house_doc_type(v: &Value) -> Option<&'static str> {
    let house = |code: &str| {
        classify::DOC_TYPES
            .iter()
            .find(|d| d.0 == code)
            .map(|d| d.0)
    };
    let codings = v
        .pointer("/type/coding")
        .and_then(Value::as_array)
        .into_iter()
        .flatten();
    let mut loinc = false;
    for c in codings {
        match c.get("system").and_then(Value::as_str) {
            Some(DOC_TYPE_SYSTEM) => {
                if let Some(t) = c.get("code").and_then(Value::as_str).and_then(house) {
                    return Some(t);
                }
            }
            Some(normalize::LOINC) => loinc = true,
            _ => {}
        }
    }
    if loinc {
        return None;
    }
    normalize::path_str(v, "type.text")
        .map(|t| t.trim().to_ascii_lowercase())
        .filter(|t| matches!(t.as_str(), "bill" | "receipt"))
        .and_then(|t| house(&t))
}

/// The code system for medrec's house document types on a
/// DocumentReference's `type.coding`.
pub const DOC_TYPE_SYSTEM: &str = "urn:pk-med:doc-type";

/// A DiagnosticReport's category (HL7 v2 table 0074) as medrec's document
/// type: a lab report, or an imaging one.
fn report_doc_type(v: &Value) -> Option<&'static str> {
    const IMAGING: &[&str] = &[
        "RAD", "CT", "MR", "NMR", "NMS", "US", "RUS", "VUS", "CUS", "OUS", "XRC",
    ];
    let codes = v
        .get("category")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|c| c.get("coding").and_then(Value::as_array))
        .flatten()
        .filter_map(|c| c.get("code").and_then(Value::as_str));
    for code in codes {
        let code = code.to_ascii_uppercase();
        if code == "LAB" {
            return Some("lab_report");
        }
        if IMAGING.contains(&code.as_str()) {
            return Some("imaging_report");
        }
    }
    None
}

/// The id a reference to a `resource_type` names: relative (`Binary/<id>`)
/// or absolute (`https://…/Binary/<id>`), with or without
/// `/_history/<version>`. The id's length is not checked: ids past FHIR's
/// 64 characters are read like any other.
fn reference_id<'a>(reference: &'a str, resource_type: &str) -> Option<&'a str> {
    let r = reference.trim_end_matches('/');
    let r = r.rfind("/_history/").map_or(r, |i| &r[..i]);
    let (head, id) = r.rsplit_once('/')?;
    let ty = head.rsplit_once('/').map_or(head, |(_, ty)| ty);
    (ty == resource_type && !id.is_empty()).then_some(id)
}

/// The resource whose attachment is the Binary `binary`: a
/// DocumentReference before a DiagnosticReport or Media, since its type is a
/// document's.
fn owner_of<'a>(owners: &'a [Owner], binary: &str) -> Option<&'a Owner> {
    let names = |o: &&Owner| {
        o.urls
            .iter()
            .any(|u| reference_id(u, "Binary") == Some(binary))
    };
    owners
        .iter()
        .filter(|o| o.kind == OwnerKind::DocumentReference)
        .find(names)
        .or_else(|| owners.iter().find(names))
}

/// DocumentReference ids an ExplanationOfBenefit points at
/// (`supportingInfo[].valueReference`), so those documents file as EOBs
/// under `Claims/`.
fn eob_document_links(resources: &[RawResource]) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for r in resources
        .iter()
        .filter(|r| r.resource_type == "ExplanationOfBenefit")
    {
        let Ok(v) = r.value() else { continue };
        for info in v
            .get("supportingInfo")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(reference) = normalize::path_str(info, "valueReference.reference") {
                if let Some(id) = reference_id(reference, "DocumentReference") {
                    out.insert(id.to_string(), r.id.clone());
                }
            }
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn file_binaries(
    folder: &Folder,
    cfg: &Config,
    household: &Household,
    index: &mut PersonIndex,
    texts: &mut PersonText,
    person: &Person,
    header: &Header,
    base: &Provenance,
    binaries: Vec<Binary>,
    owners: &[Owner],
    eob_docs: &HashMap<String, String>,
    raw_paths: &HashMap<String, String>,
) -> Result<Filed, CliError> {
    let mut filed = Filed::default();
    if binaries.is_empty() {
        return Ok(filed);
    }
    let ctx = intake::Context::new(cfg, household);
    for b in binaries {
        let sha = hash::sha256_hex(&b.bytes);
        // The bytes of a filed document, or of a rendering of one already
        // noted (a portal that serves its last rendering again).
        if index
            .documents
            .iter()
            .any(|d| d.sha256 == sha || d.renderings.iter().any(|r| r.sha256 == sha))
        {
            filed.duplicate += 1;
            continue;
        }
        let kind = filetype::sniff(&b.bytes).or_else(|| {
            b.content_type
                .as_deref()
                .and_then(filetype::Kind::from_media_type)
        });
        let dr = owner_of(owners, &b.id);
        let resource = match dr {
            Some(d) => SourceResource {
                resource_type: d.kind.as_str().into(),
                id: d.id.clone(),
                version_id: d.version.clone(),
                last_updated: d.last_updated.clone(),
            },
            None => SourceResource {
                resource_type: "Binary".into(),
                id: b.id.clone(),
                version_id: None,
                last_updated: None,
            },
        };
        let raw_path = raw_paths.get(&id_sub("Binary", &b.id)).cloned();
        let media_type = kind
            .map(|k| k.media_type().to_string())
            .or_else(|| b.content_type.clone())
            .unwrap_or_else(|| "application/octet-stream".into());
        let claim = dr
            .filter(|d| d.kind == OwnerKind::DocumentReference)
            .and_then(|d| eob_docs.get(&d.id));
        let doc_type = if claim.is_some() {
            "eob".to_string()
        } else {
            dr.and_then(|d| {
                d.doc_type.map(str::to_string).or_else(|| {
                    let c = d.type_code.as_deref()?;
                    classify::DOC_TYPES
                        .iter()
                        .find(|t| t.2 == Some(c))
                        .map(|t| t.0.to_string())
                })
            })
            .unwrap_or_else(|| "other".into())
        };
        let date = dr
            .and_then(|d| d.date.clone())
            .unwrap_or_else(|| header.pulled_at.chars().take(10).collect());
        let ext = kind.map(|k| k.extension()).unwrap_or("bin");
        let title = dr.and_then(|d| d.title.clone().or_else(|| d.type_text.clone()));
        let name = folder::filed_name(
            &date,
            &doc_type,
            Some(&header.organization.name),
            title.as_deref(),
            &format!("{}.{ext}", id_file(&b.id)),
        );
        let dir = folder::document_dir(person, &doc_type, &date);
        let scratch = pk_cli_drive::private_temp_dir("medrec-doc")?;
        let local = scratch.path().join(format!("doc.{ext}"));
        crate::write_private(&local, &b.bytes)?;
        let extracted = match kind {
            Some(k) => text::extract(&ctx.tools, k, &local, &ctx.opts),
            None => text::Extracted::default(),
        };
        let mut prov = base.clone();
        prov.source_resource = Some(resource);
        prov.raw_path = raw_path;
        if let Some(kept) = rendered_again(index, texts, &prov, &media_type, &extracted.text) {
            index.documents[kept].renderings.push(Rendering {
                sha256: sha,
                size: b.bytes.len() as u64,
                fetched_at: prov.fetched_at.clone(),
            });
            filed.unchanged += 1;
            continue;
        }
        let signature = intake::dupes::signature(&extracted.text);
        let md5 = {
            let mut h = Md5::new();
            h.update(&b.bytes);
            hash::hex(&h.finalize())
        };
        let path = folder.put_new_file(&local, &dir, &name, &md5)?;
        let id = ids::new("d");
        if !extracted.text.is_empty() {
            texts.texts.insert(id.clone(), extracted.text.clone());
        }
        let mut doc = Document {
            id: id.clone(),
            path,
            sha256: sha,
            md5,
            size: b.bytes.len() as u64,
            media_type,
            pages: extracted.pages,
            original_name: None,
            title,
            loinc: dr.and_then(|d| d.type_code.clone()),
            doc_type: Some(doc_type),
            date: Some(date),
            organization: Some(header.organization.name.clone()),
            filed_at: base.filed_at.clone(),
            origin: Origin {
                kind: "fhir".into(),
                name: None,
                action: "imported".into(),
            },
            text: Some(intake::text_info(&extracted)),
            near_duplicates: Vec::new(),
            minhash: signature,
            renderings: Vec::new(),
            statement: None,
            same_statement_as: None,
            classification: None,
            provenance: prov,
        };
        crate::bill::annotate(&mut doc, &extracted.text, index, texts);
        if doc.same_statement_as.is_some() {
            filed.same_statement += 1;
        }
        index.documents.push(doc);
        // The claim the EOB document belongs to points back at it.
        if let Some(claim_id) = claim {
            for r in index.records.iter_mut().filter(|r| {
                r.kind == "claim"
                    && r.provenance.iter().any(|p| {
                        p.source_resource.as_ref().is_some_and(|s| {
                            s.resource_type == "ExplanationOfBenefit" && &s.id == claim_id
                        })
                    })
            }) {
                if !r.documents.contains(&id) {
                    r.documents.push(id.clone());
                }
            }
        }
        filed.added += 1;
    }
    Ok(filed)
}

/// What [`file_binaries`] did with a stream's documents.
#[derive(Debug, Default)]
struct Filed {
    added: usize,
    /// The same bytes as a document already filed.
    duplicate: usize,
    /// A rendering of a document already filed: see [`rendered_again`].
    unchanged: usize,
    /// A bill filed as the same statement as one filed before.
    same_statement: usize,
}

/// The document a document from `prov` renders again, if it does: the
/// latest one filed through the same source document
/// ([`same_source_document`]), when it is that one rendered again
/// ([`not_a_rendering_of`]). A portal that generates a summary on each
/// export changes only markup a text reading leaves out (its timestamp and
/// ids). Any other difference, a version that differs, or no text to
/// compare files it as a new document, so an amended one is never lost.
fn rendered_again(
    index: &PersonIndex,
    texts: &PersonText,
    prov: &Provenance,
    media_type: &str,
    text: &str,
) -> Option<usize> {
    let (i, latest) = index
        .documents
        .iter()
        .enumerate()
        .rev()
        .find(|(_, d)| same_source_document(d, prov, media_type))?;
    let filed = texts.texts.get(&latest.id).map(String::as_str);
    not_a_rendering_of(latest, filed, prov, media_type, text)
        .is_none()
        .then_some(i)
}

/// Whether `filed` came through the same source document as one from
/// `prov` in `media_type`: the same CLI and organization, the same source
/// resource and Binary (its raw path), the same media type.
pub fn same_source_document(filed: &Document, prov: &Provenance, media_type: &str) -> bool {
    filed.provenance.source.cli == prov.source.cli
        && filed.provenance.source.organization_id == prov.source.organization_id
        && filed.provenance.raw_path == prov.raw_path
        && filed.media_type == media_type
        && match (&filed.provenance.source_resource, &prov.source_resource) {
            (Some(a), Some(b)) => a.resource_type == b.resource_type && a.id == b.id,
            _ => false,
        }
}

/// Why a document from `prov`, in `media_type`, reading `text`, is not
/// `filed` (whose text is `filed_text`) rendered again; `None` when it is:
/// the same source document, no `meta.versionId` or `lastUpdated` that
/// differs where both have one, and exactly the same text, whitespace
/// aside.
pub fn not_a_rendering_of(
    filed: &Document,
    filed_text: Option<&str>,
    prov: &Provenance,
    media_type: &str,
    text: &str,
) -> Option<&'static str> {
    if !same_source_document(filed, prov, media_type) {
        return Some("it did not come through the same source document, Binary and media type");
    }
    let (Some(theirs), Some(ours)) = (&filed.provenance.source_resource, &prov.source_resource)
    else {
        return Some("it names no source resource");
    };
    // A `lastUpdated` alone does not make another version (DESIGN §1.4):
    // a re-read stamps it again, and the text decides below.
    if matches!((&theirs.version_id, &ours.version_id), (Some(a), Some(b)) if a != b) {
        return Some("its source resource is another version");
    }
    let text = normalized(text);
    if text.is_empty() || filed_text.is_none_or(|t| normalized(t).is_empty()) {
        return Some("it has no text to compare");
    }
    if filed_text.map(normalized).as_deref() != Some(text.as_str()) {
        return Some("its text differs");
    }
    None
}

/// `s` with each run of whitespace as one space, trimmed.
fn normalized(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A source's kind, as medical-common names it: `fhir` for a FHIR API read,
/// else the stream's method (`portal`, `file`).
fn source_kind(header: &Header) -> &str {
    match header.method.as_deref() {
        None | Some("fhir-api") => "fhir",
        Some(m) => m,
    }
}

fn record_source(h: &mut Household, pid: &str, cli: &str, header: &Header) {
    let base = header
        .organization
        .fhir_base
        .trim_end_matches('/')
        .to_string();
    match h.sources.iter_mut().find(|s| {
        s.person == pid
            && s.cli.as_deref() == Some(cli)
            && s.patient_id.as_deref() == Some(header.patient.id.as_str())
            && s.organization.as_deref() == Some(header.organization.id.as_str())
    }) {
        Some(s) => {
            s.last_pull = Some(header.pulled_at.clone());
            if header.patient.label.is_some() {
                s.patient_label = header.patient.label.clone();
            }
        }
        None => h.sources.push(Source {
            id: ids::new("s"),
            kind: source_kind(header).into(),
            person: pid.into(),
            cli: Some(cli.into()),
            organization: Some(header.organization.id.clone()),
            organization_name: Some(header.organization.name.clone()),
            fhir_base: Some(base.clone()),
            patient_id: Some(header.patient.id.clone()),
            patient_label: header.patient.label.clone(),
            last_pull: Some(header.pulled_at.clone()),
        }),
    }
    if !h.organizations.iter().any(|o| {
        o.fhir_base.as_deref() == Some(base.as_str()) || o.name == header.organization.name
    }) {
        h.organizations.push(Organization {
            id: ids::new("g"),
            name: header.organization.name.clone(),
            aliases: Vec::new(),
            fhir_base: Some(base),
        });
    }
    let ident = Identifier {
        source: format!("{cli}:{}", header.organization.id),
        value: header.patient.id.clone(),
        kind: Some("patient".into()),
    };
    if let Some(p) = h.person_mut(pid) {
        if !p.identifiers.contains(&ident) {
            p.identifiers.push(ident);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `merge` as one import of one resource files it: new when no record
    /// holds its resource, unchanged when one holds this provenance, else
    /// replacing the version held.
    fn merge_once(index: &mut PersonIndex, rec: Record) -> Merge {
        let p = &rec.provenance[0];
        let resource = |q: &Provenance| {
            q.source == p.source
                && q.source_resource
                    .as_ref()
                    .map(|s| (&s.resource_type, &s.id))
                    == p.source_resource
                        .as_ref()
                        .map(|s| (&s.resource_type, &s.id))
        };
        let held = index
            .records
            .iter()
            .flat_map(|r| r.provenance.iter())
            .find(|q| resource(q));
        let filing = match held {
            None => None,
            Some(q) if q == p => Some(Filing::Unchanged),
            Some(_) => Some(Filing::Replaces),
        };
        merge(index, rec, filing, &mut Idless::default())
    }

    fn eob_from(id: &str, paid: f64) -> (Value, Provenance) {
        let mut r: Value =
            serde_json::from_str(include_str!("../tests/fixtures/fhir/pharmacy-eob.json")).unwrap();
        r["id"] = serde_json::json!(id);
        r["payment"]["amount"]["value"] = serde_json::json!(paid);
        let prov = Provenance {
            source: crate::model::ProvenanceSource {
                kind: "fhir".into(),
                cli: Some("fakerx".into()),
                cli_version: None,
                organization_id: Some("example-rx".into()),
                fhir_base: None,
            },
            method: "fhir-api".into(),
            source_resource: Some(SourceResource {
                resource_type: "ExplanationOfBenefit".into(),
                id: id.into(),
                version_id: None,
                last_updated: None,
            }),
            fetched_at: None,
            filed_at: "2026-11-19T00:00:00Z".into(),
            raw_path: None,
            classified_by: None,
            derived_from: None,
            filed_by: crate::model::filed_by(),
        };
        (r, prov)
    }

    #[test]
    fn a_resource_with_no_id_is_rewritten_in_place_by_its_key() {
        let mut obs: Value = crate::testdata::STREAM
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .find(|v| v["resourceType"] == "Observation")
            .unwrap();
        obs.as_object_mut().unwrap().remove("id");
        let (_, mut prov) = eob_from("", 0.0);
        prov.source_resource = Some(SourceResource {
            resource_type: "Observation".into(),
            id: String::new(),
            version_id: None,
            last_updated: None,
        });
        let mut index = PersonIndex::new("p_test");
        assert!(matches!(
            merge_once(&mut index, normalize::record(&obs, prov.clone()).unwrap()),
            Merge::Added
        ));
        // Served again: one provenance, unchanged.
        assert!(matches!(
            merge_once(&mut index, normalize::record(&obs, prov.clone()).unwrap()),
            Merge::Unchanged
        ));
        assert_eq!(index.records[0].provenance.len(), 1);
        // Corrected at the source, under the same code and date: rewritten.
        obs["valueQuantity"]["value"] = serde_json::json!(13.4);
        assert!(matches!(
            merge_once(&mut index, normalize::record(&obs, prov).unwrap()),
            Merge::Updated
        ));
        assert_eq!(index.records.len(), 1);
        assert_eq!(index.records[0].provenance.len(), 1);
        assert_eq!(
            index.records[0].fields["value"]["value"],
            serde_json::json!(13.4)
        );
    }

    #[test]
    fn a_prescription_reference_names_its_request() {
        for (reference, id) in [
            ("MedicationRequest/mr-1", Some("mr-1")),
            (
                "https://fhir.example.org/r4/MedicationRequest/mr-1",
                Some("mr-1"),
            ),
            ("MedicationRequest/mr-1/_history/2", Some("mr-1")),
            ("#rx-1", None),
            ("MedicationRequest/", None),
        ] {
            assert_eq!(request_id(reference), id, "{reference}");
        }
    }

    #[test]
    fn two_resources_from_one_source_under_one_key_share_a_record_stably() {
        let a: Value = crate::testdata::STREAM
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .find(|v| v["resourceType"] == "Observation")
            .unwrap();
        // A second reading of the same test on the same day.
        let mut b = a.clone();
        b["id"] = serde_json::json!("obs-0002");
        b["valueQuantity"]["value"] = serde_json::json!(13.9);
        let (_, base) = eob_from("", 0.0);
        let filed = |v: &Value| {
            let mut p = base.clone();
            p.source_resource = source_resource(v);
            normalize::record(v, p).unwrap()
        };
        let mut index = PersonIndex::new("p_test");
        let mut import = |order: [&Value; 2], filing: Option<Filing>| -> Vec<Merge> {
            let mut idless = Idless::default();
            order
                .iter()
                .map(|v| merge(&mut index, filed(v), filing, &mut idless))
                .collect()
        };
        let first = import([&a, &b], None);
        assert!(
            matches!(first[..], [Merge::Added, Merge::Updated]),
            "{first:?}"
        );
        for order in [[&a, &b], [&b, &a], [&a, &b]] {
            let again = import(order, Some(Filing::Unchanged));
            assert!(
                matches!(again[..], [Merge::Unchanged, Merge::Unchanged]),
                "{again:?}"
            );
        }
        assert_eq!(index.records.len(), 1);
        assert_eq!(index.records[0].provenance.len(), 2);
        assert_eq!(
            index.records[0].fields["value"]["value"],
            serde_json::json!(12.5)
        );
    }

    #[test]
    fn the_readme_lists_the_providers_medrec_probes() {
        // The README's provider table is where the docs send a reader for
        // this list, so it names exactly these.
        let readme = include_str!("../README.md");
        let table = readme
            .split("\n## Provider CLIs\n")
            .nth(1)
            .and_then(|s| s.split("\n## ").next())
            .expect("a Provider CLIs section");
        let mut named: Vec<&str> = table
            .lines()
            .filter_map(|l| l.strip_prefix("| [`"))
            .filter_map(|l| l.split('`').next())
            .collect();
        let mut known = KNOWN_PROVIDERS.to_vec();
        named.sort_unstable();
        known.sort_unstable();
        assert_eq!(named, known);
    }

    #[test]
    fn a_document_reference_names_a_house_type_by_its_system_or_a_bills_text() {
        let dr = |ty: Value| serde_json::json!({"resourceType": "DocumentReference", "type": ty});
        let cases = [
            (serde_json::json!({"text": "bill"}), Some("bill")),
            (serde_json::json!({"text": " Receipt "}), Some("receipt")),
            (
                serde_json::json!({"coding": [{"system": DOC_TYPE_SYSTEM, "code": "receipt"}], "text": "Payment"}),
                Some("receipt"),
            ),
            // Another house word is a display, not a type.
            (serde_json::json!({"text": "Letter"}), None),
            // A LOINC code decides over the text.
            (
                serde_json::json!({"coding": [{"system": normalize::LOINC, "code": "34133-9"}], "text": "Bill"}),
                None,
            ),
            (
                serde_json::json!({"coding": [{"system": DOC_TYPE_SYSTEM, "code": "not-a-type"}]}),
                None,
            ),
        ];
        for (ty, want) in cases {
            assert_eq!(house_doc_type(&dr(ty.clone())), want, "{ty}");
        }
    }

    #[test]
    fn an_empty_null_or_absent_id_is_no_id() {
        for line in [
            r#"{"resourceType":"Patient","id":"","name":[{"family":"Testperson"}]}"#,
            r#"{"resourceType":"CareTeam","id":null,"status":"active"}"#,
            r#"{"resourceType":"CareTeam","status":"active"}"#,
        ] {
            let r = RawResource::parse(line).unwrap();
            assert!(r.id.is_empty(), "{line}");
            assert!(raw_sub(&r).contains("/noid-"), "{line}");
        }
    }

    #[test]
    fn two_resources_with_no_id_under_one_key_share_a_record_stably() {
        let mut a: Value = crate::testdata::STREAM
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .find(|v| v["resourceType"] == "Observation")
            .unwrap();
        a.as_object_mut().unwrap().remove("id");
        // Two readings of one test on one day.
        let mut b = a.clone();
        b["valueQuantity"]["value"] = serde_json::json!(13.9);
        let (_, mut prov) = eob_from("", 0.0);
        prov.source_resource = Some(SourceResource {
            resource_type: "Observation".into(),
            id: String::new(),
            version_id: None,
            last_updated: None,
        });
        // Each is filed to a raw file named by its content.
        let filed = |v: &Value, path: &str| {
            let mut p = prov.clone();
            p.raw_path = Some(path.into());
            normalize::record(v, p).unwrap()
        };
        let mut index = PersonIndex::new("p_test");
        let mut import = |order: [usize; 2], filing: Option<Filing>| -> Vec<Merge> {
            let served = [(&a, "noid-a.json"), (&b, "noid-b.json")];
            let mut idless = Idless {
                incoming: served.iter().map(|(_, p)| p.to_string()).collect(),
                claimed: HashSet::new(),
            };
            order
                .iter()
                .map(|&i| {
                    let (v, path) = served[i];
                    merge(&mut index, filed(v, path), filing, &mut idless)
                })
                .collect()
        };
        let first = import([0, 1], None);
        assert!(
            matches!(first[..], [Merge::Added, Merge::Updated]),
            "{first:?}"
        );
        // Served again, in either order, each takes its own provenance back:
        // nothing changes.
        for order in [[0, 1], [1, 0], [1, 0]] {
            let again = import(order, Some(Filing::Unchanged));
            assert!(
                matches!(again[..], [Merge::Unchanged, Merge::Unchanged]),
                "{order:?} {again:?}"
            );
        }
        assert_eq!(
            index.records[0].provenance[0].raw_path.as_deref(),
            Some("noid-a.json")
        );
        assert_eq!(index.records.len(), 1);
        assert_eq!(index.records[0].provenance.len(), 2);
        // The record keeps the first one's value, as with two resources
        // with ids that share a key.
        assert_eq!(
            index.records[0].fields["value"]["value"],
            serde_json::json!(12.5)
        );
    }

    #[test]
    fn a_claim_folded_under_the_old_key_splits_on_its_next_import() {
        let (a, pa) = eob_from("eob-a", 32.5);
        let (b, pb) = eob_from("eob-b", 7.0);
        // As 0.2.5 left it: two same-day claims in one record, keyed by type
        // and date, holding the first one's amounts.
        let mut folded = normalize::record(&a, pa.clone()).unwrap();
        folded.key =
            "claim|http://terminology.hl7.org/CodeSystem/claim-type|pharmacy|2026-03-10".into();
        folded.provenance.push(pb.clone());
        let mut index = PersonIndex::new("p_test");
        index.records.push(folded);

        assert!(matches!(
            merge_once(&mut index, normalize::record(&a, pa.clone()).unwrap()),
            Merge::Added
        ));
        assert!(matches!(
            merge_once(&mut index, normalize::record(&b, pb.clone()).unwrap()),
            Merge::Updated
        ));
        assert_eq!(index.records.len(), 2);
        for (id, paid) in [("eob-a", "32.5"), ("eob-b", "7.0")] {
            let rec = index
                .records
                .iter()
                .find(|r| {
                    r.provenance
                        .iter()
                        .any(|p| p.source_resource.as_ref().unwrap().id == id)
                })
                .unwrap();
            assert_eq!(rec.provenance.len(), 1, "{id}");
            assert_eq!(rec.fields["payment"]["amount"], paid, "{id}");
        }
        // Imported again, both are unchanged.
        assert!(matches!(
            merge_once(&mut index, normalize::record(&a, pa).unwrap()),
            Merge::Unchanged
        ));
        assert!(matches!(
            merge_once(&mut index, normalize::record(&b, pb).unwrap()),
            Merge::Unchanged
        ));

        // Two claims with no id from one source are two records, each by its
        // content, and one served again is unchanged.
        let (mut d, mut pd) = eob_from("", 3.0);
        let (mut e, mut pe) = eob_from("", 4.0);
        for (r, p) in [(&mut d, &mut pd), (&mut e, &mut pe)] {
            r.as_object_mut().unwrap().remove("id");
            p.source_resource.as_mut().unwrap().id = String::new();
        }
        assert!(matches!(
            merge_once(&mut index, normalize::record(&d, pd.clone()).unwrap()),
            Merge::Added
        ));
        assert!(matches!(
            merge_once(&mut index, normalize::record(&e, pe).unwrap()),
            Merge::Added
        ));
        assert!(matches!(
            merge_once(&mut index, normalize::record(&d, pd).unwrap()),
            Merge::Unchanged
        ));
        assert_eq!(index.records.len(), 4);
        assert_eq!(index.records[2].provenance.len(), 1);
        let paid: Vec<&Value> = index.records[2..]
            .iter()
            .map(|r| &r.fields["payment"]["amount"])
            .collect();
        assert_eq!(paid, ["3.0", "4.0"]);

        // The same id from another source is another resource.
        let (c, mut pc) = eob_from("eob-a", 1.0);
        pc.source.cli = Some("otherrx".into());
        assert!(matches!(
            merge_once(&mut index, normalize::record(&c, pc).unwrap()),
            Merge::Added
        ));
        assert_eq!(index.records.len(), 5);
    }

    #[test]
    fn a_providers_environment_prefix_is_its_name_in_upper_case() {
        assert_eq!(env_prefix("uhc"), "UHC");
        assert_eq!(env_prefix("my-chart"), "MY_CHART");
    }

    #[test]
    fn a_binary_over_max_file_mb_is_skipped_and_not_kept() {
        let mut r = RecordsReader::new(crate::testdata::STREAM.as_bytes());
        let all = read_stream(&mut r, u64::MAX).unwrap();
        let binaries = all
            .resources
            .iter()
            .filter(|x| x.resource_type == "Binary")
            .count();
        assert!(binaries > 0);

        let mut r = RecordsReader::new(crate::testdata::STREAM.as_bytes());
        let s = read_stream(&mut r, 1).unwrap();
        assert!(s.binaries.is_empty());
        assert!(!s.resources.iter().any(|x| x.resource_type == "Binary"));
        let too_large = s
            .skipped
            .iter()
            .filter(|k| k.code == pk_med_records::stream::skip::TOO_LARGE)
            .count();
        assert_eq!(too_large, binaries);
    }

    #[test]
    fn a_stream_past_import_max_mb_is_refused() {
        let data = [b'x'; 64];
        let mut t = Tee {
            inner: &data[..],
            hasher: Sha256::new(),
            bytes: 0,
            max: 32,
        };
        let mut out = Vec::new();
        let e = std::io::Read::read_to_end(&mut t, &mut out).unwrap_err();
        assert!(e.to_string().contains("import_max_mb"));
    }

    #[test]
    fn ids_and_sources_become_safe_names() {
        assert_eq!(id_file("abc-1.2"), "abc-1.2");
        assert!(id_file("../etc").starts_with("id-"));
        assert!(id_file(".hidden").starts_with("id-"));
        let at_64 = "e".repeat(64);
        assert_eq!(id_file(&at_64), at_64);
        // Epic's unconstrained ids: past 64, named by a hash that is stable
        // and tells apart ids sharing their first 64 characters.
        let long = format!("{at_64}A1b2-C3d4.");
        let longer = "A1b2-C3d4.".repeat(25) + "e".repeat(6).as_str();
        assert_eq!(longer.len(), 256);
        assert_eq!(id_file(&long), "id-bf3866791d016212a22ac6d16da5b553");
        assert_ne!(id_file(&long), id_file(&format!("{at_64}x")));
        assert!(id_file(&longer).starts_with("id-"));
        assert_eq!(
            source_slug("mychart", "example-health"),
            "mychart-example-health"
        );
    }

    #[test]
    fn references_are_read_relative_absolute_and_versioned_at_any_id_length() {
        let long = "A1b2-C3d4.".repeat(25) + "e".repeat(6).as_str();
        for (reference, ty, id) in [
            ("Binary/bin-1".to_string(), "Binary", Some("bin-1")),
            ("Binary/bin-1/".into(), "Binary", Some("bin-1")),
            (
                "https://fhir.example.org/api/FHIR/R4/Binary/bin-1".into(),
                "Binary",
                Some("bin-1"),
            ),
            ("Binary/bin-1/_history/3".into(), "Binary", Some("bin-1")),
            (
                format!("DocumentReference/{long}"),
                "DocumentReference",
                Some(long.as_str()),
            ),
            (
                format!("https://fhir.example.org/R4/DocumentReference/{long}/_history/1"),
                "DocumentReference",
                Some(long.as_str()),
            ),
            ("DocumentReference/doc-1".into(), "Binary", None),
            ("NotBinary/bin-1".into(), "Binary", None),
            ("Binary/".into(), "Binary", None),
            ("Binary".into(), "Binary", None),
            ("#bin-1".into(), "Binary", None),
        ] {
            assert_eq!(reference_id(&reference, ty), id, "{reference}");
        }

        let doc = Owner {
            kind: OwnerKind::DocumentReference,
            id: long.clone(),
            urls: vec![format!("https://fhir.example.org/R4/Binary/{long}")],
            ..Owner::default()
        };
        let docs = [doc];
        assert!(owner_of(&docs, &long).is_some());
        assert!(owner_of(&docs, &long[..64]).is_none());

        let eob = RawResource::parse(&format!(
            r#"{{"resourceType":"ExplanationOfBenefit","id":"eob-1","supportingInfo":[{{"valueReference":{{"reference":"https://fhir.example.org/R4/DocumentReference/{long}"}}}}]}}"#
        ))
        .unwrap();
        let links = eob_document_links(&[eob]);
        assert_eq!(links.get(&long).map(String::as_str), Some("eob-1"));
    }

    #[test]
    fn a_reimport_finds_the_patient_by_id_or_by_the_providers_label() {
        let h: Household = serde_json::from_value(serde_json::json!({
            "schema": "medrec-household/v1", "format": 1, "folder_id": "f_1", "rev": 1,
            "created_at": "2026-01-01T00:00:00Z",
            "people": [
                {"id": "p_a", "name": "Alex Testperson", "folder": "Alex Testperson",
                 "created_at": "2026-01-01T00:00:00Z"},
                {"id": "p_b", "name": "Robin Testperson", "folder": "Robin Testperson",
                 "created_at": "2026-01-01T00:00:00Z"}
            ],
            "sources": [
                {"id": "s_1", "kind": "portal", "person": "p_a", "cli": "mychart",
                 "organization": "example-health", "patient_id": "eSelf1", "patient_label": "adult1"},
                {"id": "s_2", "kind": "portal", "person": "p_b", "cli": "mychart",
                 "organization": "example-clinic", "patient_id": "eSelf2", "patient_label": "adult1"},
                {"id": "s_3", "kind": "fhir", "person": "p_b", "cli": "fakeprov",
                 "organization": "example-health", "patient_id": "adult1"}
            ]
        }))
        .unwrap();
        let req = |patient: &str, org: Option<&str>, person: Option<&str>| Request {
            from: "mychart".into(),
            from_profile: None,
            files: None,
            patient: patient.into(),
            person: person.map(str::to_string),
            since: None,
            org: org.map(str::to_string),
            allow_birth_date_mismatch: false,
        };
        let who = |r: &Request| {
            resolve_person(&h, r)
                .map(|(p, known)| (p.id.clone(), known))
                .map_err(|e| e.to_string())
        };

        assert_eq!(
            who(&req("eSelf1", None, None)),
            Ok(("p_a".into(), vec!["eSelf1".into()]))
        );
        assert_eq!(
            who(&req("adult1", Some("example-clinic"), None)),
            Ok(("p_b".into(), vec!["eSelf2".into()]))
        );
        // One label at two organizations, for two people: refused.
        let both = who(&req("adult1", None, None)).unwrap_err();
        assert!(
            both.contains("more than one person") && both.contains("--org"),
            "{both}"
        );
        // Another provider's patient id is not this one's.
        assert!(who(&req("nobody", None, None))
            .unwrap_err()
            .contains("has not imported"));
        // --person decides, and leaves the stream's patient unchecked.
        assert_eq!(
            who(&req("adult1", None, Some("Alex"))),
            Ok(("p_a".into(), vec![]))
        );

        // An --org no source was filed under, for a patient filed under
        // another (a name the provider also accepts): the stream decides.
        let alias = req("eSelf1", Some("health"), None);
        let Ok(Who::ByStream(elsewhere)) = super::who(&h, &alias) else {
            panic!("an unknown --org for a known patient waits for the stream")
        };
        let header = |org: &str, patient: &str| -> Header {
            let line = crate::testdata::STREAM.lines().next().unwrap();
            let mut v: Value = serde_json::from_str(line).unwrap();
            v["organization"]["id"] = org.into();
            v["patient"]["id"] = patient.into();
            serde_json::from_value(v).unwrap()
        };
        let picked = by_stream(&h, &elsewhere, &header("example-health", "eSelf1"), &alias)
            .map(|(p, known)| (p.id, known));
        assert_eq!(picked.unwrap(), ("p_a".into(), vec!["eSelf1".into()]));
        // The stream's organization or patient is not one filed: refused.
        for (org, patient) in [("example-clinic", "eSelf1"), ("example-health", "eSelf9")] {
            let e = by_stream(&h, &elsewhere, &header(org, patient), &alias).unwrap_err();
            assert!(e.to_string().contains("Nothing was written"), "{e}");
        }
        // A patient never filed from this CLI still fails before the export.
        assert!(super::who(&h, &req("nobody", Some("health"), None)).is_err());
    }

    #[test]
    fn a_forbidden_skip_naming_only_a_type_marks_it_withheld() {
        use pk_med_records::stream::skip;
        let skipped = [
            Skip::resource(
                "Condition",
                "",
                skip::FORBIDDEN,
                "some categories were left out",
            ),
            Skip::resource("Condition", "", skip::FORBIDDEN, "another withheld line"),
            Skip::resource("AllergyIntolerance", "", skip::FORBIDDEN, "left out"),
            // One resource refused, a search refused, and other codes are
            // not a withheld type.
            Skip::resource("Observation", "obs-0009", skip::FORBIDDEN, "a 403"),
            Skip::search("Procedure?category=x", skip::FORBIDDEN, "scope not granted"),
            Skip::resource("Media", "", skip::UNSUPPORTED, "not offered"),
            Skip::resource("", "", skip::FORBIDDEN, "no type"),
        ];
        assert_eq!(
            withheld_types(&skipped),
            ["AllergyIntolerance", "Condition"]
        );
        assert!(withheld_types(&[]).is_empty());
        assert_eq!(
            withheld(&skipped),
            [
                Withheld {
                    reason: "some categories were left out".into(),
                    resource_types: vec!["Condition".into()],
                },
                Withheld {
                    reason: "another withheld line".into(),
                    resource_types: vec!["Condition".into()],
                },
                Withheld {
                    reason: "left out".into(),
                    resource_types: vec!["AllergyIntolerance".into()],
                },
            ]
        );
        let same = [
            Skip::resource("Observation", "", skip::FORBIDDEN, "outside records "),
            Skip::resource("Condition", "", skip::FORBIDDEN, "outside records"),
            Skip::resource("Condition", "", skip::FORBIDDEN, "outside records"),
        ];
        assert_eq!(
            withheld(&same),
            [Withheld {
                reason: "outside records".into(),
                resource_types: vec!["Condition".into(), "Observation".into()],
            }]
        );
    }

    #[test]
    fn a_report_or_media_gives_its_binary_a_date_title_and_type() {
        let report = serde_json::json!({
            "resourceType": "DiagnosticReport", "id": "rep-1",
            "category": [{"coding": [{"system": "http://terminology.hl7.org/CodeSystem/v2-0074", "code": "LAB"}]}],
            "code": {"coding": [{"system": "http://loinc.org", "code": "58410-2", "display": "CBC panel"}], "text": "CBC"},
            "effectivePeriod": {"start": "2026-03-04T09:45:00Z"},
            "presentedForm": [{"contentType": "application/pdf", "url": "Binary/bin-1"}]
        });
        let o = owner("DiagnosticReport", &report).unwrap();
        assert_eq!(
            (
                o.doc_type,
                o.date.as_deref(),
                o.title.as_deref(),
                o.type_code.as_deref()
            ),
            (
                Some("lab_report"),
                Some("2026-03-04"),
                Some("CBC"),
                Some("58410-2")
            )
        );
        assert_eq!(o.urls, ["Binary/bin-1"]);
        let imaging = serde_json::json!({
            "resourceType": "DiagnosticReport", "id": "rep-2",
            "category": [{"coding": [{"code": "rad"}]}]
        });
        assert_eq!(
            owner("DiagnosticReport", &imaging).unwrap().doc_type,
            Some("imaging_report")
        );

        let media = serde_json::json!({
            "resourceType": "Media", "id": "med-1", "createdDateTime": "2026-03-01T08:00:00Z",
            "modality": {"text": "Photo"},
            "content": {"contentType": "image/png", "url": "https://fhir.example.org/R4/Binary/bin-2"}
        });
        let m = owner("Media", &media).unwrap();
        assert_eq!(
            (m.kind, m.doc_type, m.date.as_deref(), m.title.as_deref()),
            (OwnerKind::Media, None, Some("2026-03-01"), Some("Photo"))
        );
        assert!(owner("Observation", &media).is_none());

        // A Binary that a DocumentReference also names takes its type.
        let doc = Owner {
            kind: OwnerKind::DocumentReference,
            id: "doc-1".into(),
            urls: vec!["Binary/bin-1".into()],
            ..Owner::default()
        };
        let owners = [o, m, doc];
        assert_eq!(
            owner_of(&owners, "bin-1").map(|o| o.kind),
            Some(OwnerKind::DocumentReference)
        );
        assert_eq!(
            owner_of(&owners, "bin-2").map(|o| o.id.as_str()),
            Some("med-1")
        );
        assert!(owner_of(&owners, "bin-3").is_none());
    }

    #[test]
    fn exit_codes_map_to_the_family_contract() {
        let e = provider_exit("x", Some(3), "", Some("o"));
        assert!(e.is_auth());
        assert_eq!(
            e.remedy(),
            Some("run `x auth login --org o`, then import again")
        );

        // The provider's own sign-in command replaces medrec's guess, once.
        let e = provider_exit(
            "kp",
            Some(3),
            "warming up\nerror: not signed in to this organization for this patient \u{2014} run `kp auth login --org sandbox --label adult1`\n",
            Some("sandbox"),
        );
        assert_eq!(e.exit_code(), 3);
        assert_eq!(
            e.to_string(),
            "kp needs a sign-in (not signed in to this organization for this patient) \u{2014} \
             run `kp auth login --org sandbox --label adult1`, then import again"
        );
        assert_eq!(e.to_string().matches("auth login").count(), 1);
        // A remedy that is not this provider's sign-in: medrec's own, and
        // the provider's line kept whole.
        for line in [
            "error: no session \u{2014} run `kp setup`",
            "error: no session \u{2014} run `mychart auth login --org o`",
            "error: no session \u{2014} run `kp auth logins`",
            "error: no session",
        ] {
            let e = provider_exit("kp", Some(3), line, None);
            assert_eq!(
                e.remedy(),
                Some("run `kp auth login`, then import again"),
                "{line}"
            );
            assert!(e.to_string().contains(&line["error: ".len()..]), "{line}");
        }

        assert!(matches!(
            provider_exit("x", Some(4), "", None),
            CliError::NotFound(_)
        ));
        assert!(matches!(
            provider_exit("x", Some(5), "", None),
            CliError::Upstream(_)
        ));
        assert!(
            matches!(provider_exit("x", None, "", None), CliError::Upstream(m) if m.contains("signal"))
        );
    }
}
