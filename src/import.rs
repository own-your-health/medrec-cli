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
use std::path::{Path, PathBuf};
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

use crate::about;
use crate::config::Config;
use crate::folder::{self, Folder};
use crate::given::{self, Given, GivenPaper};
use crate::hash;
use crate::ids;
use crate::intake::{self, classify, filetype, text};
use crate::model::{
    filed_by, now_rfc3339, Document, Household, Identifier, ImportRun, InError, Organization,
    Origin, Person, PersonIndex, PersonText, Provenance, ProvenanceSource, ProviderRef, Record,
    Rendering, Source, SourceResource, Word,
};
use crate::normalize;
use crate::people;
use crate::proc::{which, Bounded};
use crate::providers::Found;
use crate::stream::{self, Header, Item, RawResource, RecordsReader, Skip};
use crate::version::{self, Filing};

/// Provider CLIs medrec looks for without being told.
pub const KNOWN_PROVIDERS: &[&str] = &[
    "mychart",
    "kp",
    "athena",
    "modmed",
    "icanotes",
    "uhc",
    "caremark",
    "labcorp",
    "oneblood",
    "fertemr",
    "salve",
    "peryourhealth",
    "flywire",
    "embryooptions",
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
    /// `info.auth.login_hint`: its own sign-in command.
    #[serde(skip)]
    pub login_hint: Option<String>,
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
        login_hint: None,
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
    p.login_hint = v
        .pointer("/auth/login_hint")
        .and_then(Value::as_str)
        .map(str::to_string);
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
    /// Raw files of papers that do not read ([`Papers::unread`]).
    pub papers_unread: usize,
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
    /// Its own word of the paper.
    status: Option<String>,
    /// A DocumentReference's, of the paper itself.
    doc_status: Option<String>,
    /// Not later than the version filed, which is the current one.
    older: bool,
}

impl Owner {
    fn resource_key(&self) -> (&str, &str) {
        (self.kind.as_str(), &self.id)
    }

    fn word(&self, raw_path: &str, read: Option<String>) -> Word {
        Word {
            raw_path: raw_path.into(),
            status: self.status.clone(),
            verification: self.doc_status.clone(),
            read,
            ..Word::default()
        }
    }

    fn in_error(&self) -> bool {
        self.word("", None).in_error()
    }

    /// The title it gives a paper.
    fn says_title(&self) -> Option<String> {
        self.title.clone().or_else(|| self.type_text.clone())
    }

    fn resource(&self) -> SourceResource {
        SourceResource {
            resource_type: self.kind.as_str().into(),
            id: self.id.clone(),
            version_id: self.version.clone(),
            last_updated: self.last_updated.clone(),
        }
    }

    /// Whether an attachment of it is the Binary `id`.
    fn names(&self, id: &str) -> bool {
        let mut urls = self.urls.iter();
        urls.any(|u| reference_id(u, "Binary") == Some(id))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum OwnerKind {
    #[default]
    DocumentReference,
    DiagnosticReport,
    Media,
}

impl OwnerKind {
    const ALL: [OwnerKind; 3] = [Self::DocumentReference, Self::DiagnosticReport, Self::Media];

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

/// The prefix of a spool's directory under the system temp dir:
/// `medrec-spool-<pid>-<nanos>-<seq>` (`pk_cli_drive::private_temp_dir`).
const SPOOL_PREFIX: &str = "medrec-spool";
/// The file in a spool's directory naming the pid of the medrec that owns it.
const SPOOL_OWNER: &str = "owner.pid";

/// A provider's export, kept on this machine (an owner-only `0600` file in
/// a `0700` temporary directory) until it is filed, and removed when it is
/// dropped. `medrec refresh` exports every due source first and files them
/// after, so a short website session serves every member before uploads to
/// the folder use it up.
pub struct Spooled {
    dir: pk_cli_drive::TempDir,
}

impl Spooled {
    fn path(&self) -> PathBuf {
        self.dir.path().join("stream.ndjson")
    }
}

/// Run the provider's export into a spool: nothing in the folder is
/// written. Its exit is checked here, its stream when it is filed
/// ([`file_spooled`]).
pub fn export(
    folder: &Folder,
    cfg: &Config,
    provider: &Provider,
    req: &Request,
) -> Result<Spooled, CliError> {
    who(&folder.household()?, req)?;
    sweep_spools(&std::env::temp_dir());
    let dir = pk_cli_drive::private_temp_dir(SPOOL_PREFIX)?;
    // The owner, for `sweep_spools`, which reads this rather than the
    // directory's name.
    std::fs::write(dir.path().join(SPOOL_OWNER), std::process::id().to_string())
        .map_err(|e| CliError::Other(format!("creating a spool: {e}")))?;
    let spooled = Spooled { dir };
    let mut file = {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(spooled.path())
            .map_err(|e| CliError::Other(format!("creating a spool file: {e}")))?
    };
    let mut child = export_command(cfg, provider, req).stream()?;
    let mut tee = Tee {
        inner: child.stdout(),
        hasher: Sha256::new(),
        bytes: 0,
        max: cfg.import_max_bytes(),
    };
    if let Err(e) = std::io::copy(&mut tee, &mut file) {
        let finished = child.finish_within(Duration::from_secs(5));
        return Err(stream_failure(
            &req.from,
            ReadFailure::Other(e.to_string()),
            finished,
            req.org.as_deref(),
        ));
    }
    let finished = child.finish()?;
    if finished.status.code() != Some(0) {
        return Err(provider_exit(
            &req.from,
            finished.status.code(),
            &finished.stderr_tail,
            req.org.as_deref(),
        ));
    }
    Ok(spooled)
}

/// Remove the spools a killed medrec left in `dir`: this user's, whose
/// owner (`owner.pid`) is gone, which have had no owner file for a minute,
/// or which are a day old. A command that ends removes its own; one stopped
/// by a signal or a logout cannot.
pub fn sweep_spools(dir: &Path) {
    use std::os::unix::fs::MetadataExt;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    // SAFETY: getuid has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    let prefix = format!("{SPOOL_PREFIX}-");
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with(&prefix) {
            continue;
        }
        let Ok(meta) = std::fs::symlink_metadata(entry.path()) else {
            continue;
        };
        if !meta.is_dir() || meta.uid() != uid {
            continue;
        }
        let age = meta.modified().ok().and_then(|m| m.elapsed().ok());
        let older = |secs: u64| age.is_some_and(|a| a > Duration::from_secs(secs));
        let owner: Option<i32> = std::fs::read_to_string(entry.path().join(SPOOL_OWNER))
            .ok()
            .and_then(|p| p.trim().parse().ok());
        let gone = match owner {
            Some(p) if p == std::process::id() as i32 => false,
            Some(p) if p > 0 => {
                // SAFETY: signal 0 delivers nothing; kill only checks that
                // the process exists. errno is read straight after the call,
                // before anything else can set it. EPERM (a live process of
                // another user) counts as not gone.
                let probe = unsafe { libc::kill(p, 0) };
                probe != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
            }
            // The owner file is written as the spool is made, so one
            // without it a minute on was cut short in the making.
            _ => older(60),
        };
        let old = older(24 * 3600);
        if gone || old {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// File an export [`export`] spooled, as [`run`] files one it streams.
pub fn file_spooled(
    folder: &Folder,
    cfg: &Config,
    req: &Request,
    spooled: Spooled,
) -> Result<Report, CliError> {
    let file = std::fs::File::open(spooled.path())
        .map_err(|e| CliError::Other(format!("reading the spooled export: {e}")))?;
    let mut tee = Tee {
        inner: file,
        hasher: Sha256::new(),
        bytes: 0,
        max: cfg.import_max_bytes(),
    };
    let mut reader = RecordsReader::new(BufReader::with_capacity(256 * 1024, &mut tee));
    let streamed =
        read_stream(&mut reader, cfg.max_file_bytes()).map_err(|e| refused(&req.from, e))?;
    let digest = (hash::hex(&tee.hasher.finalize()), tee.bytes);
    file_streamed(folder, cfg, req, streamed, digest)
}

/// The provider's `records export` for a request.
fn export_command(cfg: &Config, provider: &Provider, req: &Request) -> Bounded {
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
    cmd
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
    let mut child = export_command(cfg, provider, req).stream()?;
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
    let digest = (hash::hex(&tee.hasher.finalize()), tee.bytes);
    let finished = child.finish()?;
    if finished.status.code() != Some(0) {
        return Err(provider_exit(
            &req.from,
            finished.status.code(),
            &finished.stderr_tail,
            req.org.as_deref(),
        ));
    }
    file_streamed(folder, cfg, req, streamed, digest)
}

/// Everything after the export: the folder's lock, the person, the raw
/// layer, the records and the documents.
fn file_streamed(
    folder: &Folder,
    cfg: &Config,
    req: &Request,
    streamed: Streamed,
    (stream_sha, stream_bytes): (String, u64),
) -> Result<Report, CliError> {
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
    let mut index = folder.whole_index(&person)?;
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
        left: HashSet::new(),
    };
    let mut prescribers = stream_prescribers(&resources);
    let mut held = Held {
        folder,
        source_dir: &source_dir,
        base: base_prov.source.fhir_base.clone(),
        stream: stream_providers(&resources),
        read: HashMap::new(),
        listed: HashMap::new(),
    };
    for r in &resources {
        let Ok(v) = r.value() else { continue };
        let mut prov = base_prov.clone();
        prov.source_resource = source_resource(&v);
        let sub = raw_sub(r);
        prov.raw_path = written.paths.get(&sub).cloned();
        let filing = written
            .filings
            .get(&format!("{source_dir}/{sub}.json"))
            .copied();
        if let Some(mut o) = owner(&r.resource_type, &v) {
            o.older = filing == Some(Filing::Older);
            owners.push(o);
        }
        if let Some(mut rec) = normalize::record(&v, prov) {
            for w in &mut rec.words {
                w.version = Some(version::content_digest(r)[..16].into());
            }
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
            let found = crate::providers::of(&v, &rec.fields, &mut held)?;
            match merge(&mut index, rec, found, filing, &mut idless) {
                Merge::Added => added += 1,
                Merge::Updated => updated += 1,
                Merge::Unchanged => unchanged += 1,
                Merge::Older => older += 1,
            }
        }
    }
    take_from_others(folder, &mut index)?;
    for r in &mut index.records {
        if idless.left.contains(&r.id) {
            values_again(r, &mut |p| {
                let v = read_raw(folder, p)?;
                Ok(v.and_then(|v| normalize::record(&v, p.clone())))
            })?;
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
    let mut papers = paper_words(folder, &mut index, &owners, &base_prov, &source_dir)?;
    // Before its raw files are read again, as one of the person's.
    record_source(&mut household, &person.id, &req.from, &header);
    if written.cut_short {
        // Versions were filed that no import read in.
        crate::reindex::given_by(folder, &mut index, &source_dir)?;
    }
    if papers.changed > 0 || written.cut_short {
        // Its other sources' words, and those an earlier medrec did not keep.
        let tmp = pk_cli_drive::private_temp_dir("medrec-papers")?;
        papers = papers_again(folder, &household, &person, &mut index, tmp.path())?;
    }

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
    folder.save_household(&mut household)?;
    if written.written > 0 || written.cut_short {
        // One that stays has the next import read the raw files again.
        let _ = folder.store.delete(&unfinished(&source_dir));
    }

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
        papers_unread: papers.unread,
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
    /// Whether an earlier import of the source did not finish.
    cut_short: bool,
}

/// What marks that an import of a source files raw files, until what they give
/// is in the person's index.
fn unfinished(source_dir: &str) -> String {
    format!("{source_dir}/importing.json")
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

/// `Type/<file name>` of the raw file a resource is filed as, without
/// `.json`.
pub(crate) fn raw_sub_of(resource_type: &str, id: &str) -> String {
    id_sub(resource_type, id)
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
    let mark = unfinished(source_dir);
    let cut_short = existing.contains_key(&mark);
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
                // replaces it. One that describes papers stays current over
                // what is older than its own time.
                let held_prov = filed.get(&rel).cloned().unwrap_or_else(|| {
                    let mut p = base.clone();
                    p.fetched_at = None;
                    p.source_resource = held
                        .as_ref()
                        .filter(|_| describes_papers(&r.resource_type))
                        .and_then(|h| source_resource(&h.value().ok()?));
                    let (its, new) = (p.current_at(), served.current_at());
                    if !matches!((its, new), (Some(its), Some(new)) if its > new) {
                        p.source_resource = None;
                    }
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
        if !cut_short {
            let at = serde_json::json!({"started_at": base.filed_at});
            folder.store.write_text(&mark, &format!("{at}\n"))?;
        }
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
        cut_short,
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
        _ => refused(cli, e),
    }
}

/// A stream medrec could not read, from a provider that exited 0.
fn refused(cli: &str, e: ReadFailure) -> CliError {
    let why = match e {
        ReadFailure::Stream(s) => s.to_string(),
        ReadFailure::Other(s) => s,
    };
    CliError::Upstream(format!(
        "{cli}'s records stream was refused: {why}. Nothing was written."
    ))
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

/// The provider's own words in the message [`provider_exit`] gives a sign-in
/// that ended: empty where it gave none.
pub fn provider_words<'a>(cli: &str, message: &'a str) -> &'a str {
    let own = message.strip_prefix(cli);
    match own.and_then(|m| m.strip_prefix(" needs a sign-in")) {
        Some(rest) => rest
            .strip_prefix(" (")
            .and_then(|r| r.strip_suffix(')'))
            .unwrap_or(rest),
        None => message,
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
///
/// The record keeps what each of its sources says, whichever of them writes
/// its fields.
fn merge(
    index: &mut PersonIndex,
    mut rec: Record,
    found: Found,
    filing: Option<Filing>,
    idless: &mut Idless,
) -> Merge {
    let word = rec.words.pop();
    // Once about somebody else, whatever a later version says.
    let of = &rec.provenance[0];
    let mut held = index.records.iter();
    let held = held.find(|r| r.provenance.iter().any(|p| one_resource(p, of)));
    let was = held.filter(|r| about::was(r, of.raw_path.as_deref()));
    if let Some(r) = was.filter(|_| !rec.fields.contains_key(about::FIELD)) {
        rec.key = match r.provenance.len() {
            1 => r.key.clone(),
            _ => format!("{}{}", rec.key, about::KEY),
        };
    }
    let came = Came::of(index, &rec, &found, word.as_ref());
    let merged = merge_fields(index, rec, word.as_ref(), found, filing, idless);
    let mut changed = came.keep(index);
    if matches!(merged, Merge::Older) {
        return merged;
    }
    changed |= word.is_some_and(|w| said(index, &w));
    match merged {
        Merge::Unchanged if changed => Merge::Updated,
        m => m,
    }
}

/// What a resource gives, and what the records it can be of showed before it
/// is merged: what of both a record then does not show is kept with it
/// ([`crate::given`]).
struct Came {
    of: Provenance,
    /// The raw file of the current version of its resource.
    raw: Option<String>,
    key: String,
    gives: Given,
    /// Each by its place in the index: what it showed, unless that is known
    /// to be of the same version, and whether the resource was of it.
    was: Vec<(usize, Option<Given>, bool)>,
}

impl Came {
    fn of(index: &PersonIndex, rec: &Record, found: &Found, word: Option<&Word>) -> Came {
        let of = rec.provenance[0].clone();
        let raw = current(of.raw_path.as_ref());
        let gives = given::of(&Record {
            providers: found.refs.clone(),
            ..rec.clone()
        });
        // Its raw file can be this version's and the record not yet: an
        // import was cut short between them.
        let read_in = |r: &Record| {
            let mut said = r.words.iter();
            let had = word.and_then(|w| said.find(|x| x.raw_path == w.raw_path));
            let version = had.and_then(|x| x.version.as_ref());
            version.is_some() && version == word.and_then(|w| w.version.as_ref())
        };
        let shows = |r: &Record| {
            let was = showed(r);
            let again = was.raw_path.is_some() && was.raw_path == raw && read_in(r);
            (!again).then_some(was)
        };
        let has = |r: &Record| r.provenance.iter().any(|p| one_resource(p, &of));
        let records = index.records.iter().enumerate();
        Came {
            was: records
                .filter(|(_, r)| r.key == rec.key || has(r))
                .map(|(i, r)| (i, shows(r), has(r)))
                .collect(),
            key: rec.key.clone(),
            gives: Given {
                raw_path: raw.clone(),
                ..gives
            },
            raw,
            of,
        }
    }

    /// The record the resource is of now.
    fn place(&self, index: &PersonIndex) -> Option<usize> {
        let at = |of: &dyn Fn(&Record) -> bool| index.records.iter().position(of);
        let has = |r: &Record| r.provenance.iter().any(|p| one_resource(p, &self.of));
        at(&has).or_else(|| at(&|r| r.key == self.key))
    }

    /// Whether a record keeps something more.
    fn keep(self, index: &mut PersonIndex) -> bool {
        let Some(now) = self.place(index) else {
            return false;
        };
        let mut changed = false;
        // What it gave moves with it to the record it is of now. What another
        // gave of the record it was of is as good as given by it: nothing
        // opens because it left.
        let mut moved: Vec<Given> = vec![self.gives];
        for (i, was, had) in self.was {
            let r = &mut index.records[i];
            if had && i != now {
                let shown = was.clone().unwrap_or_else(|| showed(r));
                let all = r.given.iter().cloned().chain([shown]);
                moved.extend(all.map(|g| Given {
                    raw_path: self.raw.clone(),
                    ..g
                }));
                let cli = |p: &Provenance| p.source.cli == self.of.source.cli;
                if !r.provenance.iter().any(cli) {
                    let left = Given {
                        raw_path: self.raw.clone(),
                        cli: self.of.source.cli.clone(),
                        ..Given::default()
                    };
                    changed |= given::keep(r, left);
                }
            }
            if let Some(was) = was {
                changed |= given::keep(r, was);
            }
        }
        let r = &mut index.records[now];
        for g in moved {
            changed |= given::keep(r, g);
        }
        changed
    }
}

/// Whether two provenances are of one resource: by its id, or, of those with
/// none, by its raw file.
fn one_resource(a: &Provenance, b: &Provenance) -> bool {
    fn of(p: &Provenance) -> Option<&SourceResource> {
        p.source_resource.as_ref().filter(|r| !r.id.is_empty())
    }
    let same = match (of(a), of(b)) {
        (Some(x), Some(y)) => x.id == y.id && x.resource_type == y.resource_type,
        (None, None) => a.raw_path.is_some() && a.raw_path == b.raw_path,
        _ => false,
    };
    let (s, t) = (&a.source, &b.source);
    same && s.fhir_base == t.fhir_base && s.cli == t.cli && s.organization_id == t.organization_id
}

/// What a record shows, with the raw file it is of, where that is known.
pub(crate) fn showed(r: &Record) -> Given {
    Given {
        raw_path: match r.provenance.as_slice() {
            [only] => only.raw_path.clone(),
            _ => r.fields_from.clone(),
        },
        ..given::of(r)
    }
}

/// Put a resource's word on the record it is of, with its history, and take
/// both off one it has left. Whether that changes a record.
fn said(index: &mut PersonIndex, word: &Word) -> bool {
    let of = |p: &Provenance| p.raw_path.as_ref() == Some(&word.raw_path);
    let mut changed = false;
    let mut history = Vec::new();
    for r in index
        .records
        .iter_mut()
        .filter(|r| !r.provenance.iter().any(of))
    {
        let had = r.words.len();
        r.words.retain(|w| w.raw_path != word.raw_path);
        changed |= r.words.len() != had;
        let (moved, left) = std::mem::take(&mut r.errors)
            .into_iter()
            .partition(|e| e.raw_path == word.raw_path);
        r.errors = left;
        history.extend::<Vec<InError>>(moved);
    }
    changed |= !history.is_empty();
    for r in &mut index.records {
        let Some(p) = r.provenance.iter().find(|p| of(p)) else {
            continue;
        };
        let at = first_read(p);
        if keeps(r, word) {
            changed |= r.say(word);
        }
        r.errors.append(&mut history);
        changed |= InError::note(&mut r.errors, word, &at);
    }
    changed
}

/// Whether a record keeps a source's word: a condition, an allergy or a
/// prescription always does, and another where several sources state it, or
/// one says or said it is entered in error.
pub(crate) fn keeps(r: &Record, word: &Word) -> bool {
    crate::standing::stands(r)
        || r.provenance.len() > 1
        || word.in_error()
        || r.words.iter().any(|w| w.raw_path == word.raw_path)
}

/// When a version was first read, as its provenance says: when it was
/// fetched, or when it was filed where that is the earlier (one read again
/// since was fetched since).
pub(crate) fn first_read(p: &Provenance) -> String {
    let at = |s: &str| crate::refresh::time::parse(s);
    match p.fetched_at.as_deref() {
        Some(f) if at(f) < at(&p.filed_at) || at(&p.filed_at).is_none() => f.into(),
        _ => p.filed_at.clone(),
    }
}

/// [`merge`], but for what each source says.
fn merge_fields(
    index: &mut PersonIndex,
    mut rec: Record,
    word: Option<&Word>,
    found: Found,
    filing: Option<Filing>,
    idless: &mut Idless,
) -> Merge {
    rec.providers = found.refs.clone();
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
    let in_error = word.is_some_and(Word::in_error);
    if incoming_id.is_some() {
        // One entered in error can come with little of what it said: it stays
        // on the record it was of.
        if let Some(existing) = index
            .records
            .iter_mut()
            .find(|r| r.provenance.iter().any(same_resource))
            .filter(|r| in_error || r.key == rec.key || r.provenance.iter().all(same_resource))
        {
            let at = existing.provenance.iter().position(same_resource);
            let (rec, found) = withdrawn(existing, rec, found, in_error);
            let filing = match not_read_in(existing, word) {
                true => Some(Filing::Replaces),
                false => filing,
            };
            // Another resource from this source that shares the key keeps
            // only its own provenance, so the two do not write over each
            // other.
            if let Some(i) = at.filter(|&i| !can_write(existing, i)) {
                return refresh_provenance(existing, i, incoming, filing);
            }
            let alone = existing.provenance.iter().all(same_resource);
            if let Some(i) = at.filter(|&i| {
                in_error && !alone && (existing.key != rec.key || stands_beside(existing, i))
            }) {
                return refresh_provenance(existing, i, incoming, filing);
            }
            let of_several = !existing.provenance.iter().all(same_source);
            if let Some(i) = at.filter(|_| of_several && !writes(existing, word, filing)) {
                return refresh_shared(existing, rec, found, i, incoming, filing);
            }
            return rewrite(existing, rec, found, incoming, at, filing, word.is_some());
        }
        if let Some(shared) = index
            .records
            .iter_mut()
            .find(|r| r.provenance.iter().any(same_resource))
        {
            let left = shared.provenance.iter().find(|p| same_resource(p));
            let left = left.and_then(|p| p.raw_path.as_deref());
            if about::of_resource(&rec, left).is_some() && wrote(shared) == left {
                idless.left.insert(shared.id.clone());
            }
            shared.provenance.retain(|p| !same_resource(p));
        }
        if let Some(existing) = index.records.iter_mut().find(|r| r.key == rec.key) {
            joined(existing, word);
            existing.provenance.push(incoming);
            if !in_error {
                existing.providers = states_too(existing, &rec, found);
            }
            return Merge::Updated;
        }
        index.records.push(rec);
        return Merge::Added;
    }
    let key = rec.key.clone();
    if rec.fields.contains_key(about::FIELD) {
        // Filed with another's under their key, it goes to its own.
        let its = |p: &Provenance| same_idless(p) && p.raw_path == incoming.raw_path;
        for r in index.records.iter_mut().filter(|r| r.key != key) {
            let Some(i) = r.provenance.iter().position(its) else {
                continue;
            };
            if r.provenance.len() == 1 {
                r.key.clone_from(&key);
                continue;
            }
            if wrote(r) == incoming.raw_path.as_deref() {
                idless.left.insert(r.id.clone());
            }
            r.provenance.remove(i);
        }
    }
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
            // A raw file filed by an import that was then cut short.
            let filing = match &existing.provenance[i].raw_path {
                Some(p) if incoming.raw_path.as_ref().is_some_and(|q| p != q) => {
                    Some(Filing::Replaces)
                }
                _ => filing,
            };
            if can_write(existing, i) {
                if in_error && stands_beside(existing, i) {
                    return refresh_provenance(existing, i, incoming, filing);
                }
                let of_several = !existing.provenance.iter().all(same_source);
                if of_several && !writes(existing, word, filing) {
                    return refresh_shared(existing, rec, found, i, incoming, filing);
                }
                let worded = word.is_some();
                let (rec, found) = withdrawn(existing, rec, found, in_error);
                return rewrite(existing, rec, found, incoming, Some(i), filing, worded);
            }
            refresh_provenance(existing, i, incoming, filing)
        }
        None => {
            idless.claimed.insert((key, existing.provenance.len()));
            joined(existing, word);
            existing.provenance.push(incoming);
            if !in_error {
                existing.providers = states_too(existing, &rec, found);
            }
            Merge::Updated
        }
    }
}

/// The resources a provider reference can lead to, from this stream or the
/// raw files earlier imports filed for the same source.
struct Held<'a> {
    folder: &'a Folder,
    source_dir: &'a str,
    base: Option<String>,
    stream: HashMap<(String, String), Value>,
    read: HashMap<String, Option<Value>>,
    /// The raw files each type's folder holds, listed once.
    listed: HashMap<String, HashSet<String>>,
}

impl crate::providers::Resolve for Held<'_> {
    fn base(&self) -> Option<&str> {
        self.base.as_deref()
    }

    fn resource(&mut self, resource_type: &str, id: &str) -> Result<Option<Value>, CliError> {
        let key = (resource_type.to_string(), id.to_string());
        if let Some(v) = self.stream.get(&key) {
            return Ok(Some(v.clone()));
        }
        let sub = id_sub(resource_type, id);
        if let Some(v) = self.read.get(&sub) {
            return Ok(v.clone());
        }
        // A type's folder is listed once, so a source that never serves a
        // Practitioner costs one call and not one per reference.
        if !self.listed.contains_key(resource_type) {
            let dir = format!("{}/{resource_type}", self.source_dir);
            let names = self
                .folder
                .store
                .list_here(&dir)?
                .into_iter()
                .filter_map(|e| e.rel.rsplit('/').next().map(str::to_string))
                .collect();
            self.listed.insert(resource_type.into(), names);
        }
        let v = if self.listed[resource_type].contains(&format!("{}.json", id_file(id))) {
            self.folder
                .store
                .read_text(&format!("{}/{sub}.json", self.source_dir))?
                .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        } else {
            None
        };
        self.read.insert(sub, v.clone());
        Ok(v)
    }
}

/// The providers and prescriptions in the stream, by type and id.
fn stream_providers(resources: &[RawResource]) -> HashMap<(String, String), Value> {
    resources
        .iter()
        .filter(|r| {
            crate::providers::TYPES.contains(&r.resource_type.as_str())
                || r.resource_type == "MedicationRequest"
        })
        .filter_map(|r| Some(((r.resource_type.clone(), r.id.clone()), r.value().ok()?)))
        .collect()
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
    crate::providers::printed(v, "prescriber").map(str::to_string)
}

/// The id in a reference to a MedicationRequest (`MedicationRequest/<id>`,
/// relative or absolute, with or without `/_history/<v>`).
fn request_id(reference: &str) -> Option<&str> {
    let rest = reference.rsplit_once("MedicationRequest/")?.1;
    rest.split('/').next().filter(|id| !id.is_empty())
}

/// What one import knows of its resources with no id: the raw files they
/// were filed to, and which id-less provenances on records they have taken.
/// And the records whose values are of a resource that left them.
#[derive(Default)]
struct Idless {
    incoming: HashSet<String>,
    claimed: HashSet<(String, usize)>,
    left: HashSet<String>,
}

/// A version that says a record is entered in error takes no value from it:
/// it can come with little of what it was of. Its fields stand over the same
/// of the record's.
fn withdrawn(had: &Record, rec: Record, found: Found, in_error: bool) -> (Record, Found) {
    if !in_error {
        return (rec, found);
    }
    let found = Found {
        refs: had.providers.clone(),
        ..Found::default()
    };
    (over(had, rec), found)
}

/// What `rec`, entered in error, gives over what the record had.
pub(crate) fn over(had: &Record, rec: Record) -> Record {
    let mut fields = had.fields.clone();
    fields.extend(rec.fields);
    Record {
        key: had.key.clone(),
        date: had.date.clone().or(rec.date),
        code: had.code.clone().or(rec.code),
        display: had.display.clone(),
        fields,
        ..rec
    }
}

/// Whether a version is another than the one last read into the record,
/// though its raw file is there: an import was cut short between them. One
/// whose version is not known (a record an earlier medrec saved) is told by
/// what it says.
fn not_read_in(existing: &Record, word: Option<&Word>) -> bool {
    let Some(word) = word else { return false };
    let had = existing.words.iter().find(|w| w.raw_path == word.raw_path);
    had.is_some_and(|had| match (&had.version, &word.version) {
        (Some(a), Some(b)) => a != b,
        _ => {
            let version = word.version.clone();
            let had = Word {
                version,
                ..had.clone()
            };
            had != *word
        }
    })
}

/// Whether a resource other than that of the provenance at `i` does not say
/// the record is entered in error.
fn stands_beside(r: &Record, i: usize) -> bool {
    let mut others = r.provenance.iter().enumerate().filter(|(j, _)| *j != i);
    others.any(|(_, p)| !is_error(r, p))
}

/// Whether the word of a record's provenance is that it is entered in error.
fn is_error(r: &Record, p: &Provenance) -> bool {
    let path = p.raw_path.as_ref();
    let word = r.words.iter().find(|w| path == Some(&w.raw_path));
    word.is_some_and(Word::in_error)
}

/// Whether a version writes the fields of a record other sources state too.
/// One with a word does where the raw file that wrote them is its own, not
/// known or gone, and where the version is new. So each served again as it
/// was writes nothing over another.
fn writes(existing: &Record, word: Option<&Word>, filing: Option<Filing>) -> bool {
    let Some(word) = word else { return true };
    writer(existing).is_none_or(|w| w == word.raw_path)
        || matches!(filing, None | Some(Filing::Replaces))
}

/// The raw file a record's values are of, as far as that is known: the only
/// one, the one it names, else that of the first resource import lets write.
pub(crate) fn wrote(r: &Record) -> Option<&str> {
    if let [only] = r.provenance.as_slice() {
        return only.raw_path.as_deref();
    }
    let first = (0..r.provenance.len()).find(|i| can_write(r, *i));
    writer(r).or_else(|| r.provenance[first?].raw_path.as_deref())
}

/// Takes a record's values from the first of its resources that reads, those
/// import lets write before others. Where none does, they stay those of a
/// resource about somebody else, and the record is about them.
pub(crate) fn values_again(
    r: &mut Record,
    read: &mut dyn FnMut(&Provenance) -> Result<Option<Record>, CliError>,
) -> Result<(), CliError> {
    let mut order: Vec<usize> = (0..r.provenance.len()).collect();
    order.sort_by_key(|&i| !can_write(r, i));
    for i in order {
        let p = r.provenance[i].clone();
        let Some(new) = read(&p)?.filter(|n| n.key == r.key) else {
            continue;
        };
        r.fields = new.fields;
        r.display = new.display;
        r.status = new.status;
        r.code = new.code;
        r.date = new.date;
        let several = r.provenance.len() > 1 && !r.words.is_empty();
        r.fields_from = p.raw_path.filter(|_| several);
        return Ok(());
    }
    let mut given = Given::default();
    given
        .fields
        .insert(about::FIELD.into(), about::ANOTHER.into());
    r.given.push(given);
    Ok(())
}

/// The raw file a record's fields are of, where one that can write them has
/// it.
pub(crate) fn writer(r: &Record) -> Option<&str> {
    let named = r.fields_from.as_deref()?;
    (0..r.provenance.len())
        .any(|i| r.provenance[i].raw_path.as_deref() == Some(named) && can_write(r, i))
        .then_some(named)
}

/// Whether import lets the resource of a record's provenance write its
/// fields: the first of its source on the record does, and of those with no
/// id the first of its type. One that says it is entered in error comes after
/// those that do not.
pub(crate) fn can_write(r: &Record, i: usize) -> bool {
    let p = &r.provenance[i];
    let same_source = |q: &Provenance| {
        q.source.fhir_base == p.source.fhir_base
            && q.source.cli == p.source.cli
            && q.source.organization_id == p.source.organization_id
    };
    let of = |q: &Provenance| q.source_resource.as_ref().map(|s| s.resource_type.clone());
    let idless = |q: &Provenance| q.source_resource.as_ref().is_none_or(|s| s.id.is_empty());
    let like = |q: &Provenance| same_source(q) && (!idless(p) || (idless(q) && of(q) == of(p)));
    let stands = r.provenance.iter().position(|q| like(q) && !is_error(r, q));
    stands.or_else(|| r.provenance.iter().position(like)) == Some(i)
}

/// Whether a record's values are of a resource that says it is entered in
/// error, or of one not known, and it has another that does not say so and
/// one that does.
fn of_an_error(r: &Record) -> bool {
    let errors = r.provenance.iter().filter(|p| is_error(r, p)).count();
    if errors == 0 || errors == r.provenance.len() {
        return false;
    }
    let Some(path) = &r.fields_from else {
        return true;
    };
    let mut named = r.provenance.iter();
    named
        .find(|p| p.raw_path.as_ref() == Some(path))
        .is_none_or(|p| is_error(r, p))
}

/// Take a record's values from another of its resources, where they are of
/// one that says it is entered in error ([`of_an_error`]): the first that
/// reads and that import lets write, else the first that reads. Whether one
/// did.
pub(crate) fn take_from_another(
    r: &mut Record,
    read: &mut dyn FnMut(&Provenance) -> Result<Option<Record>, CliError>,
) -> Result<bool, CliError> {
    if !of_an_error(r) {
        return Ok(false);
    }
    let mut standing: Vec<usize> = (0..r.provenance.len())
        .filter(|&i| !is_error(r, &r.provenance[i]))
        .collect();
    standing.sort_by_key(|&i| !can_write(r, i));
    for i in standing {
        let p = r.provenance[i].clone();
        let Some(mut new) = read(&p)?.filter(|n| n.key == r.key) else {
            continue;
        };
        // A fill's is its prescription's.
        if let Some(of) = r.fields.get("prescriber") {
            new.fields.entry("prescriber").or_insert(of.clone());
        }
        let was = showed(r);
        r.fields = new.fields;
        r.display = new.display;
        r.status = new.status;
        r.code = new.code;
        r.date = new.date;
        r.fields_from = p.raw_path;
        given::keep(r, was);
        return Ok(true);
    }
    Ok(false)
}

/// What a record's resources say of the providers it prints, the first to say
/// something of one standing: those that do not say it is entered in error,
/// or all where every one does. `found` is what one says, where its raw file
/// reads. What the record had stands in for what is not read, but where a
/// resource that does not speak may have given it.
pub(crate) fn providers_again(
    r: &Record,
    found: &mut dyn FnMut(&Provenance) -> Result<Option<Found>, CliError>,
) -> Result<Vec<ProviderRef>, CliError> {
    let errors = r.provenance.iter().filter(|p| is_error(r, p)).count();
    let in_part = errors > 0 && errors < r.provenance.len();
    let mut all = Found::default();
    for p in &r.provenance {
        if p.raw_path.is_none() || (in_part && is_error(r, p)) {
            continue;
        }
        match found(p)? {
            Some(of) => all = all.and(of),
            None => all.unresolved.extend(crate::providers::FIELDS),
        }
    }
    let had = if in_part { &[][..] } else { &r.providers };
    Ok(all.over(had, &r.fields, &r.fields, false))
}

/// The resource of a provenance, from its raw file.
fn read_raw(folder: &Folder, p: &Provenance) -> Result<Option<Value>, CliError> {
    let text = match &p.raw_path {
        Some(rel) => folder.store.read_text(rel)?,
        None => None,
    };
    Ok(text.and_then(|t| serde_json::from_str(&t).ok()))
}

/// [`take_from_another`] and [`providers_again`] for every record of a person
/// that a resource says, or said, is entered in error, from the raw files
/// here now.
fn take_from_others(folder: &Folder, index: &mut PersonIndex) -> Result<(), CliError> {
    let read = |p: &Provenance| read_raw(folder, p);
    for r in &mut index.records {
        take_from_another(r, &mut |p| {
            Ok(read(p)?.and_then(|v| normalize::record(&v, p.clone())))
        })?;
        if r.errors.is_empty() || r.provenance.len() == 1 {
            continue;
        }
        let was = showed(r);
        r.providers = providers_again(r, &mut |p| {
            let Some(v) = read(p)? else { return Ok(None) };
            let dir = p.raw_path.as_deref().and_then(source_dir_of);
            let mut held = Held {
                folder,
                source_dir: dir.unwrap_or_default(),
                base: p.source.fhir_base.clone(),
                stream: HashMap::new(),
                read: HashMap::new(),
                listed: HashMap::new(),
            };
            crate::providers::of(&v, &r.fields, &mut held).map(Some)
        })?;
        given::keep(r, was);
    }
    Ok(())
}

/// The source folder of `<source>/<Type>/<file>`.
pub(crate) fn source_dir_of(rel: &str) -> Option<&str> {
    let (dir, _) = rel.rsplit_once('/')?;
    dir.rsplit_once('/').map(|(source, _)| source)
}

/// Another resource joins the record: where one source alone stated it, the
/// first of its own wrote the fields. Where the first kept no word, its
/// status is that word.
fn joined(existing: &mut Record, word: Option<&Word>) {
    if let (Some(_), [only], []) = (word, &existing.provenance[..], &existing.words[..]) {
        if let Some(raw_path) = only
            .raw_path
            .clone()
            .filter(|_| !crate::standing::stands(existing))
        {
            existing.words.push(Word {
                raw_path,
                status: existing.status.clone(),
                ..Word::default()
            });
        }
    }
    let of_one = (0..existing.provenance.len()).filter(|i| can_write(existing, *i));
    if word.is_some() && existing.fields_from.is_none() && of_one.count() == 1 {
        existing.fields_from = existing.provenance[0].raw_path.clone();
    }
}

/// A version read again of a record other sources state too: its provenance,
/// and what it adds of the providers.
fn refresh_shared(
    existing: &mut Record,
    rec: Record,
    found: Found,
    i: usize,
    incoming: Provenance,
    filing: Option<Filing>,
) -> Merge {
    let providers = states_too(existing, &rec, found);
    let changed = providers != existing.providers;
    existing.providers = providers;
    match refresh_provenance(existing, i, incoming, filing) {
        Merge::Unchanged if changed => Merge::Updated,
        m => m,
    }
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
            replace(existing, i, incoming);
            Merge::Updated
        }
    }
}

/// Put a later version's provenance in place of the one at `i`. What the
/// record keeps by its raw file follows it: one with no id has another.
fn replace(existing: &mut Record, i: usize, incoming: Provenance) {
    let had = std::mem::replace(&mut existing.provenance[i], incoming).raw_path;
    let now = existing.provenance[i].raw_path.clone();
    let (Some(had), Some(now)) = (had, now) else {
        return;
    };
    let errors = existing.errors.iter_mut().map(|e| &mut e.raw_path);
    let words = existing.words.iter_mut().map(|w| &mut w.raw_path);
    let given = existing.given.iter_mut();
    let given = given.filter_map(|g| g.raw_path.as_mut());
    let from = existing.fields_from.as_mut();
    for path in errors.chain(words).chain(given).chain(from) {
        if *path == had {
            path.clone_from(&now);
        }
    }
}

/// Whether two records say the same thing.
/// A claim's links (`claims::link`) follow from other records and are left
/// out.
fn same_content(a: &Record, b: &Record) -> bool {
    let fields = if a.kind == "claim" {
        crate::claims::without_links(&a.fields) == crate::claims::without_links(&b.fields)
    } else {
        a.fields == b.fields
    };
    a.key == b.key
        && fields
        && a.display == b.display
        && a.status == b.status
        && a.code == b.code
        && a.date == b.date
        && a.providers == b.providers
}

/// What `existing` says of its providers once `rec`, the same fact from
/// another resource, adds what `found` says of those both print.
fn states_too(existing: &Record, rec: &Record, mut found: Found) -> Vec<ProviderRef> {
    found
        .refs
        .retain(|r| rec.fields.get(&r.field) == existing.fields.get(&r.field));
    found.over(
        &existing.providers,
        &existing.fields,
        &existing.fields,
        true,
    )
}

/// Write `rec` over the record `existing`, whose provenance at `at` is this
/// resource's own. A version filed new or in place of the held one takes
/// that provenance's place; one read again refreshes it
/// (`Provenance::refreshed_by`, which keeps how it was filed). A record that
/// says the same thing from the same version is unchanged.
fn rewrite(
    existing: &mut Record,
    mut rec: Record,
    found: Found,
    incoming: Provenance,
    at: Option<usize>,
    filing: Option<Filing>,
    worded: bool,
) -> Merge {
    let path = incoming.raw_path.clone();
    let shared = existing.provenance.len() > usize::from(at.is_some());
    rec.providers = found.over(&existing.providers, &existing.fields, &rec.fields, shared);
    let changed = !same_content(existing, &rec);
    let new_version = matches!(filing, None | Some(Filing::Replaces))
        && at.is_none_or(|i| existing.provenance[i] != incoming);
    match (filing, at) {
        (Some(Filing::Refreshed), Some(i)) => {
            existing.provenance[i] = existing.provenance[i].refreshed_by(&incoming)
        }
        (None | Some(Filing::Replaces), Some(i)) => replace(existing, i, incoming),
        (None | Some(Filing::Refreshed | Filing::Replaces), None) => {
            existing.provenance.push(incoming)
        }
        _ => {}
    }
    existing.fields_from = path.filter(|_| worded && existing.provenance.len() > 1);
    if !changed && !new_version {
        return Merge::Unchanged;
    }
    existing.fields = rec.fields;
    existing.providers = rec.providers;
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
        status: text("status"),
        doc_status: text("docStatus").filter(|_| kind == OwnerKind::DocumentReference),
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

/// The resource whose attachment is the Binary `binary` ([`first_owner`]).
fn owner_of<'a>(owners: &'a [Owner], binary: &str) -> Option<&'a Owner> {
    first_owner(owners.iter().filter(|o| o.names(binary)))
}

/// Of those that describe one paper, the one it is filed by: one that does
/// not say it is entered in error, and a DocumentReference before a
/// DiagnosticReport or Media, since its type is a document's.
fn first_owner<'a>(owners: impl Iterator<Item = &'a Owner>) -> Option<&'a Owner> {
    owners.min_by_key(|o| (o.in_error(), o.kind != OwnerKind::DocumentReference))
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
        let dr = owner_of(owners, &b.id);
        let resource = match dr {
            Some(d) => d.resource(),
            None => SourceResource {
                resource_type: "Binary".into(),
                id: b.id.clone(),
                version_id: None,
                last_updated: None,
            },
        };
        let mut prov = base.clone();
        prov.source_resource = Some(resource);
        prov.raw_path = raw_paths.get(&id_sub("Binary", &b.id)).cloned();
        // The bytes of a filed document, or of a rendering of one already
        // noted (a portal that serves its last rendering again).
        if let Some(d) = index.documents.iter_mut().find(|d| has_bytes(d, &sha)) {
            filed.duplicate += 1;
            let of = Some(&header.organization.name);
            given::keep_paper(d, paper_given(dr, of, prov.raw_path.as_ref()));
            if !d.provenances().any(|p| p.raw_path == prov.raw_path) {
                d.also_from.push(prov);
            }
            continue;
        }
        let kind = filetype::sniff(&b.bytes).or_else(|| {
            b.content_type
                .as_deref()
                .and_then(filetype::Kind::from_media_type)
        });
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
        let title = dr.and_then(Owner::says_title);
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
        if let Some(kept) = rendered_again(index, texts, &prov, &media_type, &extracted.text) {
            let d = &mut index.documents[kept];
            let of = Some(&header.organization.name);
            given::keep_paper(d, paper_given(dr, of, prov.raw_path.as_ref()));
            d.renderings.push(Rendering {
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
            also_from: Vec::new(),
            words: Vec::new(),
            errors: Vec::new(),
            given: Vec::new(),
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

/// What describes the papers of one source.
struct Describes<'a> {
    by_file: HashMap<String, Vec<&'a Owner>>,
    by_resource: HashMap<(&'a str, &'a str), &'a Owner>,
}

impl<'a> Describes<'a> {
    fn new(owners: impl Iterator<Item = &'a Owner> + Clone, source_dir: &str) -> Self {
        let mut out = Describes {
            by_file: HashMap::new(),
            by_resource: HashMap::new(),
        };
        // A stream carries each once; if it repeats one, the last copy wins.
        // One with no id is known by the Binary it names only.
        for o in owners.clone().filter(|o| !o.id.is_empty()) {
            out.by_resource.insert(o.resource_key(), o);
        }
        for o in owners {
            let last = out.by_resource.get(&o.resource_key());
            if last.is_some_and(|last| !std::ptr::eq(*last, o)) {
                continue;
            }
            for id in o.urls.iter().filter_map(|u| reference_id(u, "Binary")) {
                let file = binary_file(source_dir, id);
                out.by_file.entry(file).or_default().push(o);
            }
        }
        out
    }

    /// What describes a paper by one of its Binaries: the resource it was
    /// filed by, and those that name the Binary.
    fn all(&self, p: &Provenance) -> Vec<&'a Owner> {
        let resource = p.source_resource.as_ref();
        let by = resource.and_then(|r| {
            let key = (r.resource_type.as_str(), r.id.as_str());
            self.by_resource.get(&key)
        });
        let naming = p.raw_path.as_ref().and_then(|f| self.by_file.get(f));
        let naming = naming.into_iter().flatten();
        by.into_iter().chain(naming).copied().collect()
    }

    /// The first of them ([`first_owner`]).
    fn first(&self, p: &Provenance) -> Option<&'a Owner> {
        first_owner(self.all(p).into_iter())
    }
}

/// What a source gives of a paper by one of its Binaries: its own name, where
/// it is another than the one that filed the paper, and the title.
fn paper_given(o: Option<&Owner>, source: Option<&String>, raw: Option<&String>) -> GivenPaper {
    GivenPaper {
        raw_path: current(raw),
        organization: source.cloned(),
        title: o.and_then(Owner::says_title),
    }
}

/// The raw file of a source's Binary.
fn binary_file(source_dir: &str, id: &str) -> String {
    format!("{source_dir}/{}.json", id_sub("Binary", id))
}

/// Put what a source says of a paper, by one of its Binaries, on it. Whether
/// that changes it.
fn paper_said(d: &mut Document, word: &Word, at: &str) -> bool {
    let keeps = !d.also_from.is_empty()
        || word.in_error()
        || d.words.iter().any(|w| w.raw_path == word.raw_path);
    let said = keeps && d.say(word);
    InError::note(&mut d.errors, word, at) || said
}

/// What was read of papers.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct Papers {
    pub changed: usize,
    /// Raw files that do not read: Binaries, and resources that describe
    /// papers. While one of these does not, its source newly says of no paper
    /// that it is entered in error.
    pub unread: usize,
}

/// What the stream says of the papers of its source, those whose Binary it
/// does not carry included. A source says a paper is entered in error, by one
/// of its Binaries, when everything it has that describes it there does: what
/// earlier imports filed counts where the stream does not carry it, or carries
/// an older version of it. Counts those it says so of and did not.
fn paper_words(
    folder: &Folder,
    index: &mut PersonIndex,
    owners: &[Owner],
    base: &Provenance,
    source_dir: &str,
) -> Result<Papers, CliError> {
    let read = first_read(base);
    let carried = Describes::new(owners.iter().filter(|o| !o.older), source_dir);
    let older = Describes::new(owners.iter().filter(|o| o.older), source_dir);
    let mut earlier: Option<FiledOwners> = None;
    let mut out = Papers::default();
    for d in &mut index.documents {
        let ours: Vec<Provenance> = papers_of(d, source_dir).cloned().collect();
        for p in ours {
            for o in carried.all(&p).into_iter().chain(older.all(&p)) {
                given::keep_paper(d, paper_given(Some(o), None, p.raw_path.as_ref()));
            }
            let mut first = carried.first(&p);
            let ask = match first {
                Some(o) => o.in_error(),
                None => older.first(&p).is_some(),
            };
            if ask {
                let filed = match &mut earlier {
                    Some(filed) => filed,
                    none => none.insert(filed_owners(folder, source_dir, owners)?),
                };
                out.unread = filed.unread;
                let also = Describes::new(filed.owners.iter(), source_dir).first(&p);
                first = first_owner(first.into_iter().chain(also));
            }
            let Some(first) = first else {
                continue;
            };
            let path = p.raw_path.as_deref().unwrap_or_default();
            let word = first.word(path, Some(read.clone()));
            if newly_in_error(d, &word) {
                if out.unread > 0 {
                    continue;
                }
                out.changed += 1;
            }
            paper_said(d, &word, &read);
        }
    }
    Ok(out)
}

/// Whether a word says a paper is entered in error and the one it takes the
/// place of does not.
fn newly_in_error(d: &Document, word: &Word) -> bool {
    let had = d.words.iter().find(|w| w.raw_path == word.raw_path);
    word.in_error() && !had.is_some_and(Word::in_error)
}

/// The provenances of a paper that a source's raw Binaries are of.
fn papers_of<'a>(d: &'a Document, source_dir: &'a str) -> impl Iterator<Item = &'a Provenance> {
    d.provenances().filter(move |p| {
        let path = p.raw_path.as_deref().unwrap_or_default();
        path.strip_prefix(source_dir)
            .is_some_and(|rest| rest.starts_with("/Binary/"))
    })
}

/// What a source's raw files have that describes papers.
#[derive(Default)]
struct FiledOwners {
    owners: Vec<Owner>,
    /// Earlier versions.
    before: Vec<Owner>,
    /// Those that do not read.
    unread: usize,
}

/// What earlier imports of a source filed that describes papers, but for what
/// the stream carries the current version of. A resource with no id is never
/// the same as another.
fn filed_owners(
    folder: &Folder,
    source_dir: &str,
    carried: &[Owner],
) -> Result<FiledOwners, CliError> {
    let tmp = pk_cli_drive::private_temp_dir("medrec-papers")?;
    let mut out = owners_in(folder, source_dir, tmp.path())?;
    let here: HashSet<(&str, &str)> = carried
        .iter()
        .filter(|o| !o.id.is_empty() && !o.older)
        .map(Owner::resource_key)
        .collect();
    out.owners.retain(|o| !here.contains(&o.resource_key()));
    Ok(out)
}

/// A source's resources that describe a paper, by their raw files, which
/// `tmp` takes a copy of.
fn owners_in(folder: &Folder, source_dir: &str, tmp: &Path) -> Result<FiledOwners, CliError> {
    let copied = |e| CliError::Other(format!("reading a copy of the raw files: {e}"));
    let mut out = FiledOwners::default();
    for kind in OwnerKind::ALL {
        let dir = format!("{source_dir}/{}", kind.as_str());
        let to = tmp.join(&dir);
        if !to.exists() && folder.store.exists(&dir)? {
            folder.store.fetch_tree(&dir, &to)?;
        }
        if !to.exists() {
            continue;
        }
        let mut files = Vec::new();
        for f in std::fs::read_dir(&to).map_err(copied)? {
            files.push(f.map_err(copied)?.path());
        }
        files.retain(|f| f.extension().is_some_and(|x| x == "json"));
        files.sort();
        for f in files {
            let name = f.file_name().map(|n| n.to_string_lossy());
            let current = name.is_some_and(|n| is_current(&n));
            let v: Option<Value> = serde_json::from_slice(&std::fs::read(f).map_err(copied)?).ok();
            match v {
                Some(v) if current => out.owners.extend(owner(kind.as_str(), &v)),
                Some(v) => out.before.extend(owner(kind.as_str(), &v)),
                None => out.unread += usize::from(current),
            }
        }
    }
    Ok(out)
}

/// Whether resources of a type describe papers.
pub(crate) fn describes_papers(resource_type: &str) -> bool {
    OwnerKind::from_resource_type(resource_type).is_some()
}

/// Whether a raw file's name is that of a resource's current version.
fn is_current(name: &str) -> bool {
    name.ends_with(".json") && !name.contains('~')
}

/// The raw file of the current version of what a raw file keeps.
fn current(raw: Option<&String>) -> Option<String> {
    raw.map(|rel| current_of(rel).unwrap_or_else(|| rel.clone()))
}

/// The raw file of the current version of what a raw file keeps an earlier
/// one of.
pub(crate) fn current_of(rel: &str) -> Option<String> {
    let (dir, name) = rel.rsplit_once('/')?;
    let (id, _) = name.strip_suffix(".json")?.split_once('~')?;
    Some(format!("{dir}/{id}.json"))
}

/// What the raw files say of every paper of a person; and the Binaries with a
/// paper's bytes that medrec before 0.9.9 did not keep, of its own source or
/// another: the raw ones no paper is of. `tmp` takes the copies.
pub(crate) fn papers_again(
    folder: &Folder,
    household: &Household,
    person: &Person,
    index: &mut PersonIndex,
    tmp: &Path,
) -> Result<Papers, CliError> {
    let now = now_rfc3339();
    let mut changed: BTreeSet<String> = BTreeSet::new();
    let mut unread = 0;
    let mut sources: Vec<(String, Option<String>, FiledOwners)> = Vec::new();
    for (dir, mut of) in papers_sources(household, person, index) {
        if of.filed_at.is_empty() {
            of.filed_at = now.clone();
        }
        let filed = owners_in(folder, &dir, tmp)?;
        let describes = Describes::new(filed.owners.iter(), &dir);
        let binaries = unknown_binaries(folder, index, &dir, &of)?;
        unread += filed.unread + binaries.unread;
        for (sha, mut p) in binaries.found {
            let Some(d) = index.documents.iter_mut().find(|d| has_bytes(d, &sha)) else {
                continue;
            };
            if let Some(o) = describes.first(&p) {
                p.source_resource = Some(o.resource());
            }
            d.also_from.push(p);
            changed.insert(d.id.clone());
        }
        sources.push((dir, of.fetched_at, filed));
    }
    for (dir, latest, filed) in sources {
        let describes = Describes::new(filed.owners.iter(), &dir);
        let before: Vec<(Describes, &Owner)> = filed
            .before
            .iter()
            .map(|o| (Describes::new(std::iter::once(o), &dir), o))
            .collect();
        let called = household.sources.iter().find_map(|s| {
            let of = source_slug(s.cli.as_deref()?, s.organization.as_deref()?);
            let here = s.person == person.id && folder::records_dir(person, &of) == dir;
            s.organization_name.as_ref().filter(|_| here)
        });
        for d in &mut index.documents {
            let ours: Vec<Provenance> = papers_of(d, &dir).cloned().collect();
            for p in ours {
                let raw = p.raw_path.as_ref();
                let of = called.filter(|_| d.provenance.raw_path != p.raw_path);
                let mut gave = vec![paper_given(None, of, raw)];
                let earlier = before.iter().filter(|(by, _)| by.first(&p).is_some());
                let described = describes.all(&p).into_iter();
                let described = described.chain(earlier.map(|(_, o)| *o));
                gave.extend(described.map(|o| paper_given(Some(o), None, raw)));
                for g in gave {
                    if given::keep_paper(d, g) {
                        changed.insert(d.id.clone());
                    }
                }
                let Some(o) = describes.first(&p) else {
                    continue;
                };
                let path = p.raw_path.as_deref().unwrap_or_default();
                let had = d.words.iter().find(|w| w.raw_path == path);
                // One not kept was read when its source last was.
                let read = match had {
                    Some(w) => w.read.clone(),
                    None => latest.clone(),
                };
                let at = read.as_ref().filter(|_| had.is_none()).unwrap_or(&now);
                let word = o.word(path, read.clone());
                if filed.unread > 0 && newly_in_error(d, &word) {
                    continue;
                }
                if paper_said(d, &word, at) {
                    changed.insert(d.id.clone());
                }
            }
        }
    }
    Ok(Papers {
        changed: changed.len(),
        unread,
    })
}

/// Each source folder of a person, with the provenance of what it filed: the
/// times are those of its latest import, where all of them are one patient's.
fn papers_sources(
    household: &Household,
    person: &Person,
    index: &PersonIndex,
) -> BTreeMap<String, Provenance> {
    let mut out = BTreeMap::new();
    for s in household.sources.iter().filter(|s| s.person == person.id) {
        let (Some(cli), Some(org)) = (&s.cli, &s.organization) else {
            continue;
        };
        let dir = folder::records_dir(person, &source_slug(cli, org));
        let mut runs = index.imports.iter().filter(|r| r.source_dir == dir);
        let patients: BTreeSet<&str> = runs.clone().map(|r| r.patient_id.as_str()).collect();
        let latest = runs.next_back();
        let read = latest.filter(|_| patients.len() == 1);
        let method = match s.kind.as_str() {
            "fhir" => "fhir-api",
            m => m,
        };
        out.entry(dir.clone()).or_insert_with(|| Provenance {
            source: ProvenanceSource {
                kind: s.kind.clone(),
                cli: Some(cli.clone()),
                cli_version: latest.map(|r| r.cli_version.clone()),
                organization_id: Some(org.clone()),
                fhir_base: s.fhir_base.clone(),
            },
            method: method.into(),
            source_resource: None,
            fetched_at: read.map(|r| r.pulled_at.clone()),
            filed_at: read.map(|r| r.imported_at.clone()).unwrap_or_default(),
            raw_path: None,
            classified_by: None,
            derived_from: None,
            filed_by: filed_by(),
        });
    }
    out
}

/// The raw Binaries of a source no paper is of.
#[derive(Default)]
struct Unknown {
    /// By their bytes' digest.
    found: Vec<(String, Provenance)>,
    /// Those that do not read.
    unread: usize,
}

fn unknown_binaries(
    folder: &Folder,
    index: &PersonIndex,
    dir: &str,
    of: &Provenance,
) -> Result<Unknown, CliError> {
    let known: HashSet<&str> = index
        .documents
        .iter()
        .flat_map(|d| d.provenances())
        .filter_map(|p| p.raw_path.as_deref())
        .collect();
    let binaries = format!("{dir}/Binary");
    let mut out = Unknown::default();
    if !folder.store.exists(&binaries)? {
        return Ok(out);
    }
    for file in folder.store.list(&binaries)? {
        let rel = format!("{binaries}/{}", file.rel);
        let name = rel.rsplit('/').next().unwrap_or_default();
        if known.contains(rel.as_str()) || !is_current(name) {
            continue;
        }
        let Some(text) = folder.store.read_text(&rel)? else {
            continue;
        };
        let Some(read) = binary_bytes(&text) else {
            out.unread += 1;
            continue;
        };
        let Some((id, sha)) = read else { continue };
        out.found.push((
            sha,
            Provenance {
                source_resource: Some(SourceResource {
                    resource_type: "Binary".into(),
                    id,
                    version_id: None,
                    last_updated: None,
                }),
                raw_path: Some(rel),
                ..of.clone()
            },
        ));
    }
    Ok(out)
}

/// A raw Binary's id, and the digest of the bytes it holds, if any; none when
/// it does not read.
fn binary_bytes(text: &str) -> Option<Option<(String, String)>> {
    let mut v: Value = serde_json::from_str(text).ok()?;
    let Some(Value::String(mut data)) = v.get_mut("data").map(Value::take) else {
        return Some(None);
    };
    data.retain(|c| !c.is_ascii_whitespace());
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .ok()?;
    let id = v.get("id").and_then(Value::as_str).unwrap_or_default();
    Some((!bytes.is_empty()).then(|| (id.to_string(), hash::sha256_hex(&bytes))))
}

/// Whether a paper has these bytes, as filed or as it was served since.
fn has_bytes(d: &Document, sha: &str) -> bool {
    d.sha256 == sha || d.renderings.iter().any(|r| r.sha256 == sha)
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
            // A sign-in's organization can be renamed between pulls (an
            // athenahealth-wide sign-in learns its practice's name), but a
            // practice's name is not given back for a connection's.
            let name = header.organization.name.trim();
            let knows_practice = s
                .organization_name
                .as_deref()
                .is_some_and(|n| !n.trim().is_empty() && !crate::plain::is_connection(n));
            if !name.is_empty() && !(crate::plain::is_connection(name) && knows_practice) {
                s.organization_name = Some(header.organization.name.clone());
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
    // One FHIR base can serve many practices (athenahealth's), so an
    // organization is told apart by its name.
    if !h
        .organizations
        .iter()
        .any(|o| o.name == header.organization.name)
    {
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
mod spool_tests {
    use super::*;

    #[test]
    fn a_spool_whose_process_is_gone_is_swept_and_a_live_one_kept() {
        let dir = pk_cli_drive::private_temp_dir("medrec-sweep-test").unwrap();
        // Names that say nothing of the owner: only `owner.pid` does.
        let mine = dir.path().join(format!("{SPOOL_PREFIX}-a"));
        let gone = dir.path().join(format!("{SPOOL_PREFIX}-b"));
        let other = dir.path().join("medrec-other-c");
        // A pid far above any macOS or Linux pid_max: no such process.
        let owners = [
            (&mine, std::process::id()),
            (&gone, 99_999_999),
            (&other, 99_999_999),
        ];
        for (d, pid) in owners {
            std::fs::create_dir(d).unwrap();
            std::fs::write(d.join("stream.ndjson"), b"{}").unwrap();
            std::fs::write(d.join(SPOOL_OWNER), pid.to_string()).unwrap();
        }
        // Just made, its owner file not yet written: kept.
        let making = dir.path().join(format!("{SPOOL_PREFIX}-d"));
        std::fs::create_dir(&making).unwrap();
        sweep_spools(dir.path());
        assert!(mine.is_dir(), "this process's spool is kept");
        assert!(!gone.exists(), "a spool whose process is gone is removed");
        assert!(other.is_dir(), "only spools are touched");
        assert!(making.is_dir(), "a spool being made is kept");
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
        merge(index, rec, Found::default(), filing, &mut Idless::default())
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

    /// A condition of one source, as its raw file has it.
    fn condition_of(org: &str, id: &str, status: &str, category: &str) -> Record {
        let v = serde_json::json!({"resourceType": "Condition", "id": id,
            "clinicalStatus": {"coding": [{"code": status}]},
            "category": [{"coding": [{"code": category}]}],
            "code": {"text": "Sample stiffness"}, "onsetDateTime": "2020-05-01"});
        let (_, mut p) = eob_from("", 0.0);
        p.source.organization_id = Some(org.into());
        p.source_resource = source_resource(&v);
        p.raw_path = Some(format!("Robin Example/Records/{org}/Condition/{id}.json"));
        normalize::record(&v, p).unwrap()
    }

    /// What each source says stays, whichever wrote the fields; one served
    /// again as it was writes nothing over another.
    #[test]
    fn each_source_keeps_its_word_and_writes_only_by_a_change() {
        let mut index = PersonIndex::new("p_test");
        let mut import = |org: &str, id: &str, status: &str, filing: Option<Filing>| {
            let rec = condition_of(org, id, status, "problem-list-item");
            let m = merge(
                &mut index,
                rec,
                Found::default(),
                filing,
                &mut Idless::default(),
            );
            let r = &index.records[0];
            let words: Vec<String> = r.words.iter().map(|w| w.status.clone().unwrap()).collect();
            (format!("{m:?}"), r.status.clone().unwrap(), words.join(" "))
        };
        let is = |m: &str, status: &str, words: &str| (m.into(), status.into(), words.into());
        let (again, newer) = (Some(Filing::Unchanged), Some(Filing::Replaces));
        assert_eq!(
            import("one", "a", "active", None),
            is("Added", "active", "active")
        );
        assert_eq!(
            import("two", "b", "resolved", None),
            is("Updated", "active", "active resolved")
        );
        for _ in 0..2 {
            for (org, id, status) in [("one", "a", "active"), ("two", "b", "resolved")] {
                for filing in [again, Some(Filing::Refreshed)] {
                    assert_eq!(
                        import(org, id, status, filing),
                        is("Unchanged", "active", "active resolved")
                    );
                }
            }
        }
        assert_eq!(
            import("two", "b", "inactive", newer),
            is("Updated", "inactive", "active inactive")
        );
        assert_eq!(
            import("one", "a", "active", again),
            is("Unchanged", "inactive", "active inactive")
        );
        // An import cut short after its raw files: the version the raw layer
        // has is not the one the record was read from.
        for filing in [again, Some(Filing::Refreshed)] {
            let status = if filing == again {
                "resolved"
            } else {
                "recurrence"
            };
            assert_eq!(
                import("one", "a", status, filing),
                is("Updated", status, &format!("{status} inactive"))
            );
        }
        assert_eq!(
            import("one", "a", "active", newer),
            is("Updated", "active", "active inactive")
        );
        assert_eq!(
            import("two", "b", "inactive", again),
            is("Unchanged", "active", "active inactive")
        );
        assert_eq!(
            import("two", "b", "inactive", newer),
            is("Updated", "inactive", "active inactive")
        );
        // An older version says nothing of it now.
        assert_eq!(
            import("one", "a", "resolved", Some(Filing::Older)),
            is("Older", "inactive", "active inactive")
        );
        // A word is of a raw file the record has.
        let r = &index.records[0];
        for w in &r.words {
            let of = |p: &Provenance| p.raw_path.as_ref() == Some(&w.raw_path);
            assert!(r.provenance.iter().any(of));
        }
    }

    /// The source whose fields a shared record has writes them again as a
    /// later medrec reads them, though its version is the one held; another
    /// writes none, and adds what it says of a provider they both print.
    #[test]
    fn a_shared_records_writer_writes_again_and_another_adds_providers() {
        let medicine = |org: &str, dosage: Option<&str>| {
            let mut v = serde_json::json!({"resourceType": "MedicationRequest", "id": org,
                "status": "active", "authoredOn": "2024-01-02",
                "requester": {"display": "Example Prescriber"},
                "medicationCodeableConcept": {"text": "Sample tablet"}});
            if let Some(d) = dosage {
                v["dosageInstruction"] = serde_json::json!([{"text": d}]);
            }
            let (_, mut p) = eob_from("", 0.0);
            p.source.organization_id = Some(org.into());
            p.source_resource = source_resource(&v);
            p.raw_path = Some(format!(
                "Robin Example/Records/{org}/MedicationRequest/{org}.json"
            ));
            normalize::record(&v, p).unwrap()
        };
        let found = |npi: &str| Found {
            refs: vec![ProviderRef {
                field: "prescriber".into(),
                npi: Some(npi.into()),
                state: None,
                city: None,
            }],
            ..Found::default()
        };
        let mut index = PersonIndex::new("p_test");
        let mut import = |rec: Record, found: Found, filing: Option<Filing>| {
            let m = merge(&mut index, rec, found, filing, &mut Idless::default());
            (format!("{m:?}"), index.records[0].clone())
        };
        let again = Some(Filing::Unchanged);
        import(medicine("one", None), Found::default(), None);
        let (m, r) = import(medicine("two", Some("Take one")), Found::default(), None);
        assert_eq!((m.as_str(), r.fields.get("dosage")), ("Updated", None));
        assert_eq!(r.fields_from, r.provenance[0].raw_path);

        // The other's fuller reading of the version held, and its provider.
        let (m, r) = import(
            medicine("two", Some("Take one")),
            found("0000000006"),
            again,
        );
        assert_eq!((m.as_str(), r.fields.get("dosage")), ("Updated", None));
        assert_eq!(r.providers, found("0000000006").refs);
        assert_eq!(r.fields_from, r.provenance[0].raw_path);
        let (m, _) = import(
            medicine("two", Some("Take one")),
            found("0000000006"),
            again,
        );
        assert_eq!(m, "Unchanged");

        // The writer's.
        let (m, r) = import(medicine("one", Some("Take two")), Found::default(), again);
        assert_eq!(m, "Updated");
        assert_eq!(r.fields["dosage"], "Take two");
        assert_eq!(r.providers, found("0000000006").refs);

        // An index an earlier medrec saved names no writer: the next to come
        // is, and then the rule holds.
        index.records[0].fields_from = None;
        let mut import = |rec: Record| {
            let found = Found::default();
            let m = merge(&mut index, rec, found, again, &mut Idless::default());
            (format!("{m:?}"), index.records[0].fields["dosage"].clone())
        };
        let two = || medicine("two", Some("Take one"));
        assert_eq!(import(two()), ("Updated".into(), "Take one".into()));
        let one = || medicine("one", Some("Take two"));
        assert_eq!(import(one()), ("Unchanged".into(), "Take one".into()));
        assert_eq!(import(two()), ("Unchanged".into(), "Take one".into()));

        // And where the writer's resource left the record.
        index.records[0].fields_from = Some("Robin Example/Records/gone/x.json".into());
        let one = medicine("one", Some("Take two"));
        merge(
            &mut index,
            one,
            Found::default(),
            again,
            &mut Idless::default(),
        );
        assert_eq!(index.records[0].fields["dosage"], "Take two");
        assert_eq!(
            index.records[0].fields_from,
            index.records[0].provenance[0].raw_path
        );
    }

    /// An allergy of one source, as import reads a version of it.
    fn allergy_of(org: &str, id: Option<&str>, criticality: &str) -> Record {
        let mut v = serde_json::json!({"resourceType": "AllergyIntolerance",
            "clinicalStatus": {"coding": [{"code": "active"}]},
            "criticality": criticality, "code": {"text": "Sample nut"}});
        if let Some(id) = id {
            v["id"] = id.into();
        }
        let raw = RawResource::from_value(&v).unwrap();
        let (_, mut p) = eob_from("", 0.0);
        p.source.organization_id = Some(org.into());
        p.source_resource = source_resource(&v);
        p.raw_path = Some(format!(
            "Robin Example/Records/{org}/{}.json",
            raw_sub(&raw)
        ));
        let mut r = normalize::record(&v, p).unwrap();
        r.words[0].version = Some(version::content_digest(&raw)[..16].into());
        r
    }

    /// An import cut short after its raw files is made good by the next,
    /// whose version the raw layer then has: by a change no word says, with
    /// a reindex between them, and of a resource with no id.
    #[test]
    fn an_import_cut_short_is_made_good_by_the_next() {
        for (id, filing, reindexed) in [
            (Some("b"), Filing::Refreshed, false),
            (Some("b"), Filing::Refreshed, true),
            (None, Filing::Unchanged, false),
        ] {
            let mut index = PersonIndex::new("p_test");
            let first = [
                allergy_of("one", Some("a"), "low"),
                allergy_of("two", id, "low"),
            ];
            for rec in first {
                let found = Found::default();
                merge(&mut index, rec, found, None, &mut Idless::default());
            }
            // Two's next version is in the raw layer and not here.
            let next = || allergy_of("two", id, "high");
            if reindexed {
                let mut word = next().words.remove(0);
                word.version = index.records[0].words[1].version.clone();
                index.records[0].say(&word);
            }
            let mut import = |rec: Record, filing: Filing| {
                let found = Found::default();
                let m = merge(&mut index, rec, found, Some(filing), &mut Idless::default());
                let r = &index.records[0];
                let of = |p: &Provenance| p.raw_path.clone().unwrap();
                let paths: Vec<String> = r.provenance.iter().map(of).collect();
                let said: Vec<&String> = r.words.iter().map(|w| &w.raw_path).collect();
                assert_eq!(said, paths.iter().collect::<Vec<_>>());
                format!("{m:?} {}", r.fields["criticality"].as_str().unwrap())
            };
            assert_eq!(import(next(), filing), "Updated high");
            assert_eq!(import(next(), Filing::Unchanged), "Unchanged high");
            // The other's, as it was, writes nothing over it.
            let one = allergy_of("one", Some("a"), "low");
            assert_eq!(import(one, Filing::Unchanged), "Unchanged high");
        }
    }

    /// A record whose writer is named as a source's later entry names none:
    /// the next that can write does.
    #[test]
    fn a_sources_later_entry_is_no_writer() {
        let mut index = PersonIndex::new("p_test");
        let path = |org: &str, id: &str| format!("Robin Example/Records/{org}/Condition/{id}.json");
        let mut import = |org: &str, id: &str, status: &str, filing: Option<Filing>| {
            let rec = condition_of(org, id, status, "problem-list-item");
            let found = Found::default();
            merge(&mut index, rec, found, filing, &mut Idless::default());
            let r = index.records[0].clone();
            (r.status.clone().unwrap(), r.fields_from.clone())
        };
        let again = Some(Filing::Unchanged);
        import("two", "a", "active", None);
        import("two", "b", "active", None);
        let (_, from) = import("one", "c", "active", None);
        assert_eq!(from, Some(path("two", "a")));
        let r = &mut index.records[0];
        assert_eq!([0, 1, 2].map(|i| can_write(r, i)), [true, false, true]);
        r.fields_from = Some(path("two", "b"));
        let mut import = |org: &str, id: &str, status: &str, filing: Option<Filing>| {
            let rec = condition_of(org, id, status, "problem-list-item");
            let found = Found::default();
            merge(&mut index, rec, found, filing, &mut Idless::default());
            let r = index.records[0].clone();
            (r.status.clone().unwrap(), r.fields_from.clone())
        };
        // The later entry's own change is its word alone.
        let (status, _) = import("two", "b", "resolved", Some(Filing::Replaces));
        assert_eq!(status, "active");
        assert_eq!(
            import("one", "c", "active", again),
            ("active".into(), Some(path("one", "c")))
        );
    }

    /// Two resources of one source that are one record are two words.
    #[test]
    fn one_source_can_have_two_words() {
        let mut index = PersonIndex::new("p_test");
        for (id, category) in [("a", "encounter-diagnosis"), ("b", "problem-list-item")] {
            merge_once(&mut index, condition_of("one", id, "active", category));
        }
        assert_eq!(index.records.len(), 1);
        let words: Vec<&[String]> = index.records[0]
            .words
            .iter()
            .map(|w| w.categories.as_slice())
            .collect();
        assert_eq!(words, [["encounter-diagnosis"], ["problem-list-item"]]);
    }

    /// A version with no id has a raw file of its own; the one whose place it
    /// takes says nothing any more.
    #[test]
    fn a_word_goes_with_its_raw_file() {
        let mut index = PersonIndex::new("p_test");
        let mut idless = |file: &str, status: &str| {
            let mut rec = condition_of("one", "", status, "problem-list-item");
            let path = format!("Robin Example/Records/one/Condition/{file}.json");
            rec.provenance[0].raw_path = Some(path.clone());
            rec.words[0].raw_path = path;
            merge(
                &mut index,
                rec,
                Found::default(),
                None,
                &mut Idless::default(),
            );
            let words: Vec<String> = index.records[0]
                .words
                .iter()
                .map(|w| w.status.clone().unwrap())
                .collect();
            (index.records.len(), words.join(" "))
        };
        assert_eq!(idless("noid-a", "active"), (1, "active".into()));
        assert_eq!(idless("noid-b", "resolved"), (1, "resolved".into()));
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
                .map(|v| merge(&mut index, filed(v), Found::default(), filing, &mut idless))
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
                left: HashSet::new(),
            };
            order
                .iter()
                .map(|&i| {
                    let (v, path) = served[i];
                    merge(
                        &mut index,
                        filed(v, path),
                        Found::default(),
                        filing,
                        &mut idless,
                    )
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
    fn a_sign_in_takes_its_organizations_name_on_every_pull_and_each_practice_is_listed() {
        let mut h: Household = serde_json::from_value(serde_json::json!({
            "schema": "medrec-household/v1", "format": 1, "folder_id": "f_1", "rev": 1,
            "created_at": "2026-01-01T00:00:00Z",
            "people": [{"id": "p_a", "name": "Alex Testperson", "folder": "Alex Testperson",
                        "created_at": "2026-01-01T00:00:00Z"}]
        }))
        .unwrap();
        // One FHIR base serving every practice, as athenahealth's does.
        let header = |org: &str, name: &str| -> Header {
            let line = crate::testdata::STREAM.lines().next().unwrap();
            let mut v: Value = serde_json::from_str(line).unwrap();
            v["organization"]["id"] = org.into();
            v["organization"]["name"] = name.into();
            v["organization"]["fhir_base"] = "https://athena.example.test/fhir/r4/".into();
            serde_json::from_value(v).unwrap()
        };
        record_source(
            &mut h,
            "p_a",
            "athena",
            &header("900001", "athenahealth (all practices)"),
        );
        record_source(
            &mut h,
            "p_a",
            "athena",
            &header("900001", "Example Pediatrics"),
        );
        record_source(
            &mut h,
            "p_a",
            "athena",
            &header("900002", "Example Family Medicine"),
        );
        // A later pull that names only the connection keeps the practice.
        record_source(
            &mut h,
            "p_a",
            "athena",
            &header("900001", "athenahealth (all practices)"),
        );
        // A renamed practice is renamed.
        record_source(
            &mut h,
            "p_a",
            "athena",
            &header("900002", "Example Family Health"),
        );
        let names: Vec<_> = h
            .sources
            .iter()
            .map(|s| (s.organization.as_deref(), s.organization_name.as_deref()))
            .collect();
        assert_eq!(
            names,
            [
                (Some("900001"), Some("Example Pediatrics")),
                (Some("900002"), Some("Example Family Health"))
            ]
        );
        // A record is labelled with its own sign-in's practice, not the
        // first organization at the shared base.
        let pv: crate::model::Provenance = serde_json::from_value(serde_json::json!({
            "source": {"kind": "fhir", "cli": "athena", "organization_id": "900002",
                       "fhir_base": "https://athena.example.test/fhir/r4"},
            "method": "fhir-api", "filed_at": "2026-11-20T10:00:00Z", "filed_by": "medrec 0.9.3"}))
        .unwrap();
        let idx = PersonIndex::new("p_a");
        let from = crate::sources::Sources::of(&h.sources, &idx);
        assert_eq!(from.label(&pv).as_deref(), Some("Example Family Health"));
        let orgs: Vec<&str> = h.organizations.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(
            orgs,
            [
                "athenahealth (all practices)",
                "Example Pediatrics",
                "Example Family Medicine",
                "Example Family Health"
            ]
        );
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
        assert_eq!(
            provider_words("kp", e.message()),
            "not signed in to this organization for this patient"
        );
        let e = provider_exit("kp", Some(3), "", None);
        assert_eq!(provider_words("kp", e.message()), "");
        let e = provider_exit("kp", Some(3), "error: no session (it ended)", None);
        assert_eq!(provider_words("kp", e.message()), "no session (it ended)");
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
