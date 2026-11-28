//! Published code tables, kept on this machine: the plain names of
//! diagnosis codes, and which codes are vaccines.
//!
//! medrec ships none of them. `medrec tables fetch` downloads each
//! publisher's whole file from the address below, keeps it only when its
//! SHA-256 is the one recorded here, and nothing is ever looked up by code
//! over the network. Without a table, or with a file whose hash is not the
//! recorded one, a code has no name and the source's own text stands.
//! CDC's vaccine tables have no version: one that is not the recorded file
//! is kept only when the person says so (`--accept`), and is then used as
//! the recorded one is.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use pk_cli_core::{private_file, CliError};
use serde::{Deserialize, Serialize};

use crate::hash::sha256_hex;
use crate::nppes;

/// One published file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table {
    pub id: &'static str,
    pub title: &'static str,
    pub publisher: &'static str,
    pub version: &'static str,
    /// The release date, or the publisher's own date of the file.
    pub date: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
    pub bytes: u64,
    pub form: Form,
    /// How many codes it has.
    pub rows: usize,
}

/// How a table's file is kept and read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Form {
    /// One file of a zip, of exactly `bytes` bytes.
    Zip { member: &'static str, bytes: u64 },
    /// The publisher's text file itself, with no version: rows of `columns`
    /// values between `|`, each column of `numbers` a code number, and in
    /// `changed` the day the row last changed (`YYYY/MM/DD`).
    Text {
        columns: usize,
        numbers: &'static [usize],
        changed: usize,
    },
    /// The publisher's zip of XML files, kept whole and read where it is
    /// used: CDC's CDSi supporting data, whose series `rows` counts. Its
    /// version is its folder's name, and its date its release notes'.
    Archive,
}

pub const ICD10CM: Table = Table {
    id: "icd10cm",
    title: "ICD-10-CM code titles",
    publisher: "Centers for Medicare & Medicaid Services",
    version: "FY2027",
    date: "2026-06-01",
    url: "https://www.cms.gov/files/zip/2027-code-descriptions-tabular-order.zip",
    sha256: "91c6c9d1117764ce72375a0f3a5493b1725dafbc1ab283b55799076c9e194965",
    bytes: 2_208_022,
    form: Form::Zip {
        member: "Code Descriptions/icd10cm_order_2027.txt",
        bytes: 14_724_229,
    },
    rows: 98_403,
};

/// CDC's CVX codes, and which of them are no vaccine (`nonvaccine`). Its
/// version is the newest day a row changed.
pub const CVX: Table = Table {
    id: "cvx",
    title: "CVX vaccine codes",
    publisher: "Centers for Disease Control and Prevention",
    version: "2026-09-10",
    date: "2026-09-10",
    url: "https://www2a.cdc.gov/vaccines/iis/iisstandards/downloads/cvx.txt",
    sha256: "efd2a6770f0f648ce76e10ac2889602b2ea8fa8ca4b63deb33ca883e7d0d7413",
    bytes: 55_005,
    form: Form::Text {
        columns: 7,
        numbers: &[0],
        changed: 6,
    },
    rows: 290,
};

/// CDC's CPT codes of vaccines, each with its CVX code. Of a row only the two
/// code numbers are read; its descriptions never are.
pub const CPT_CVX: Table = Table {
    id: "cpt-cvx",
    title: "CPT codes of vaccines, with their CVX codes",
    publisher: "Centers for Disease Control and Prevention",
    version: "2026-07-07",
    date: "2026-07-07",
    url: "https://www2a.cdc.gov/vaccines/iis/iisstandards/downloads/cpt.txt",
    sha256: "878759ca30defe5ded69258ec9e25aa4f76efde134f0bb22edce1b94e6ef5e64",
    bytes: 33_097,
    form: Form::Text {
        columns: 8,
        numbers: &[0, 4],
        changed: 6,
    },
    rows: 163,
};

/// CDC's CDSi supporting data: the schedules of the Advisory Committee on
/// Immunization Practices, as series of doses by age, and which vaccines each
/// CVX code counts for. Its date is its release notes'.
pub const CDSI: Table = Table {
    id: "cdsi",
    title: "CDSi supporting data: immunization schedules",
    publisher: "Centers for Disease Control and Prevention",
    version: "4.66",
    date: "2026-10-09",
    url: "https://www.cdc.gov/iis/downloads/supporting-data-4.66-508.zip",
    sha256: "64797032e1420f193766665271f5fecbb0e635d16a6d021ef47125c8c36c8739",
    bytes: 2_596_260,
    form: Form::Archive,
    rows: 150,
};

pub const TABLES: &[&Table] = &[&ICD10CM, &CVX, &CPT_CVX, &CDSI];

/// The most a text table may be: it is read whole into memory.
const TEXT_MAX: u64 = 4 << 20;

/// The most an archive table may be.
const ARCHIVE_MAX: u64 = 16 << 20;

/// What the person accepted of a table that is not the recorded file
/// (`medrec-table-accepted/v1`), kept beside it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Accepted {
    pub schema: String,
    pub sha256: String,
    pub bytes: u64,
    pub rows: u64,
    /// Of an archive, the version it says; a text table's is its `date`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// The newest day one of its rows changed, or an archive's own date.
    pub date: String,
    /// The day it was kept.
    pub accepted: String,
}

const ACCEPTED: &str = "medrec-table-accepted/v1";

/// How long one download may take.
const TIMEOUT: Duration = Duration::from_secs(180);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// On this machine, with the recorded hash.
    Ready,
    /// Not downloaded.
    Absent,
    /// A file is there, and it is not the recorded one or does not read: it
    /// is not used.
    Changed,
    /// The provider directory, as another version of medrec keeps it: it is
    /// not used, and `tables fetch nppes` replaces it.
    OtherVersion,
}

#[derive(Debug, Serialize)]
pub struct Row {
    pub id: &'static str,
    pub title: &'static str,
    pub publisher: &'static str,
    /// Null, as the four below are, for the provider directory when it was
    /// never fetched or does not read: each month's file is another.
    pub version: Option<String>,
    pub date: Option<String>,
    pub state: State,
    pub bytes: Option<u64>,
    pub rows: Option<u64>,
    pub sha256: Option<String>,
    pub url: String,
    /// The records folder the provider directory was read for, and how many
    /// of its rows are kept.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kept: Option<u64>,
    /// The file used is one the person accepted (`--accept`), and `version`,
    /// `date`, `bytes`, `rows` and `sha256` are its own.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub accepted: bool,
}

#[derive(Debug, Serialize)]
pub struct Fetched {
    pub id: &'static str,
    pub version: String,
    /// `unchanged`, `downloaded`, `copied`, or `refused`: a text table's file
    /// that is not the recorded one, which was not kept.
    pub status: &'static str,
    pub bytes: u64,
    /// It is not the recorded file, and was kept on the person's word.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub accepted: bool,
    /// Of a refused file, its SHA-256, and the command that keeps it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keep_with: Option<String>,
    /// Of a refused file, what to tell the person.
    #[serde(skip)]
    pub refusal: Option<String>,
}

impl Fetched {
    pub fn refused(&self) -> bool {
        self.status == "refused"
    }
}

impl Accepted {
    /// The version it says: an archive's own, or a text table's newest day.
    fn version(&self) -> String {
        self.version.clone().unwrap_or_else(|| self.date.clone())
    }
}

/// A table on this machine: whether it may be used, its bytes when it may,
/// and what the person accepted when it is not the recorded file.
struct Kept {
    state: State,
    bytes: Option<Vec<u8>>,
    accepted: Option<Accepted>,
}

/// Where the tables are: under the cache, since they can be fetched again.
pub fn dir() -> Option<PathBuf> {
    Some(pk_cli_drive::cache::cache_base("medrec")?.join("tables"))
}

pub fn by_id(id: &str) -> Result<&'static Table, CliError> {
    TABLES.iter().find(|t| t.id == id).copied().ok_or_else(|| {
        let ids: Vec<&str> = TABLES.iter().map(|t| t.id).chain([nppes::ID]).collect();
        CliError::Usage(format!("no table `{id}`: {}", ids.join(", ")))
    })
}

impl Table {
    fn path(&self, dir: &Path) -> PathBuf {
        match self.form {
            Form::Zip { .. } => dir.join(format!("{}-{}.zip", self.id, self.version)),
            Form::Text { .. } => dir.join(format!("{}.txt", self.id)),
            Form::Archive => dir.join(format!("{}.zip", self.id)),
        }
    }

    fn accepted_path(&self, dir: &Path) -> PathBuf {
        dir.join(format!("{}.accepted.json", self.id))
    }

    /// Whether its publisher can change the file at its address, so that one
    /// named is downloaded again, and another file is kept on the person's
    /// word (`--accept`).
    fn accepts(&self) -> bool {
        !matches!(self.form, Form::Zip { .. })
    }

    /// What the person accepted of this table, if it may be another file.
    fn accepted(&self, dir: &Path) -> Option<Accepted> {
        if !self.accepts() {
            return None;
        }
        let a: Accepted =
            serde_json::from_slice(&std::fs::read(self.accepted_path(dir)).ok()?).ok()?;
        (a.schema == ACCEPTED).then_some(a)
    }

    /// The file's bytes when its hash is the recorded one, or one the person
    /// accepted.
    fn kept(&self, dir: &Path) -> Kept {
        let changed = Kept {
            state: State::Changed,
            bytes: None,
            accepted: None,
        };
        let path = self.path(dir);
        let accepted = self.accepted(dir);
        let size = |n: u64| n == self.bytes || accepted.as_ref().is_some_and(|a| a.bytes == n);
        match std::fs::metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Kept {
                    state: State::Absent,
                    ..changed
                }
            }
            Ok(m) if size(m.len()) => {}
            _ => return changed,
        }
        let Ok(b) = std::fs::read(&path) else {
            return changed;
        };
        let sha = sha256_hex(&b);
        if b.len() as u64 == self.bytes && sha == self.sha256 {
            return Kept {
                state: State::Ready,
                bytes: Some(b),
                accepted: None,
            };
        }
        match accepted.filter(|a| a.sha256 == sha && a.bytes == b.len() as u64) {
            Some(a) => Kept {
                state: State::Ready,
                bytes: Some(b),
                accepted: Some(a),
            },
            None => changed,
        }
    }

    /// The file's bytes when it may be used.
    fn verified(&self, dir: &Path) -> (State, Option<Vec<u8>>) {
        let k = self.kept(dir);
        (k.state, k.bytes)
    }

    /// `<id> <SHA-256>` of the file used, when one may be: what a view made
    /// with it records.
    pub fn identity(&self, dir: &Path) -> Option<String> {
        let b = self.verified(dir).1?;
        Some(format!("{} {}", self.id, sha256_hex(&b)))
    }

    /// The table's file, decompressed, when it is on this machine and whole.
    fn read(&self, dir: &Path) -> Option<Vec<u8>> {
        match self.form {
            Form::Zip { member: m, bytes } => member(&self.verified(dir).1?, m, bytes),
            Form::Text { .. } | Form::Archive => None,
        }
    }

    /// An archive table's zip, when it is on this machine and may be used.
    pub fn archive(&self, dir: &Path) -> Option<Vec<u8>> {
        matches!(self.form, Form::Archive)
            .then(|| self.verified(dir).1)
            .flatten()
    }

    /// Of each row of a text table on this machine, the values of the columns
    /// `wanted`; none when it is absent, changed, or not a text table.
    pub fn columns(&self, dir: &Path, wanted: &[usize]) -> Option<Vec<Vec<String>>> {
        let file = self.verified(dir).1?;
        let rows = text_rows(&file, self.form)?;
        let of = |r: &Vec<&str>| {
            wanted
                .iter()
                .map(|&i| r.get(i).map(|v| v.to_string()))
                .collect()
        };
        rows.iter().map(of).collect()
    }
}

/// The values of each row of a text table, trimmed, when every row has the
/// form's columns, numbers and day; `None` for a zip's form or a file that
/// does not.
fn text_rows(file: &[u8], form: Form) -> Option<Vec<Vec<&str>>> {
    let Form::Text {
        columns,
        numbers,
        changed,
    } = form
    else {
        return None;
    };
    let file = std::str::from_utf8(file.strip_prefix(b"\xef\xbb\xbf").unwrap_or(file)).ok()?;
    let digits = |v: &str| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit());
    let day = |v: &str| {
        let b = v.as_bytes();
        b.len() == 10
            && b[4] == b'/'
            && b[7] == b'/'
            && digits(&v[..4])
            && digits(&v[5..7])
            && digits(&v[8..])
    };
    file.split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let row: Vec<&str> = l.split('|').map(str::trim).collect();
            let whole = row.len() == columns
                && numbers.iter().all(|&i| digits(row[i]))
                && day(row[changed]);
            whole.then_some(row)
        })
        .collect()
}

/// The newest day a row of a text table changed, as `YYYY-MM-DD`.
fn newest(rows: &[Vec<&str>], form: Form) -> String {
    let Form::Text { changed, .. } = form else {
        return String::new();
    };
    let max = rows.iter().map(|r| r[changed]).max().unwrap_or_default();
    max.replace('/', "-")
}

/// Every table, and the provider directory once for each records folder it
/// was read for.
pub fn list() -> Vec<Row> {
    let dir = dir();
    let mut rows: Vec<Row> = TABLES
        .iter()
        .map(|t| {
            let k = dir.as_deref().map(|d| t.kept(d));
            let state = k.as_ref().map_or(State::Absent, |k| k.state);
            let a = k.and_then(|k| k.accepted);
            Row {
                id: t.id,
                title: t.title,
                publisher: t.publisher,
                version: Some(a.as_ref().map_or(t.version.into(), Accepted::version)),
                date: Some(a.as_ref().map_or(t.date.into(), |a| a.date.clone())),
                state,
                bytes: Some(a.as_ref().map_or(t.bytes, |a| a.bytes)),
                rows: Some(a.as_ref().map_or(t.rows as u64, |a| a.rows)),
                sha256: Some(a.as_ref().map_or(t.sha256.into(), |a| a.sha256.clone())),
                url: t.url.into(),
                folder: None,
                kept: None,
                accepted: a.is_some(),
            }
        })
        .collect();
    let kept = nppes::dir().map(|d| nppes::list(&d)).unwrap_or_default();
    if kept.is_empty() {
        rows.push(directory_row(None, None, State::Absent));
    }
    for (folder, k) in kept {
        let state = match k {
            Ok(_) => State::Ready,
            Err(nppes::Unused::OtherVersion) => State::OtherVersion,
            Err(_) => State::Changed,
        };
        rows.push(directory_row(Some(folder), k.ok(), state));
    }
    rows
}

fn directory_row(folder: Option<String>, k: Option<nppes::Kept>, state: State) -> Row {
    let k = k.as_ref();
    Row {
        id: nppes::ID,
        title: nppes::TITLE,
        publisher: nppes::PUBLISHER,
        version: k.map(|k| k.month.clone()),
        date: k.map(|k| k.date.clone()),
        state,
        bytes: k.map(|k| k.bytes),
        rows: k.map(|k| k.rows),
        sha256: k.map(|k| k.sha256.clone()),
        url: k.map_or_else(|| nppes::CMS.base.into(), |k| k.url.clone()),
        folder,
        kept: k.map(|k| k.kept),
        accepted: false,
    }
}

/// Keep `t` on this machine: from its publisher, or from a file someone
/// downloaded. Nothing is kept unless it is the recorded file, or, of a text
/// table, the file whose SHA-256 is `accept`: the person's word, given after
/// a run that refused it said its hash. A text table already kept is
/// downloaded `again` when it is named, since its publisher changes it in
/// place; it is `unchanged` when the download is the file kept.
pub fn fetch(
    t: &Table,
    dir: &Path,
    from: Option<&Path>,
    accept: Option<&str>,
    again: bool,
) -> Result<Fetched, CliError> {
    if accept.is_some() && !t.accepts() {
        let text: Vec<&str> = TABLES
            .iter()
            .filter(|t| t.accepts())
            .map(|t| t.id)
            .collect();
        return Err(CliError::Usage(format!(
            "--accept is for a table whose publisher can change its file in place ({}), not {}",
            text.join(", "),
            t.id
        )));
    }
    let k = t.kept(dir);
    let unchanged = || {
        let a = k.accepted.as_ref();
        Fetched {
            id: t.id,
            version: a.map_or(t.version.into(), Accepted::version),
            status: "unchanged",
            bytes: a.map_or(t.bytes, |a| a.bytes),
            accepted: a.is_some(),
            sha256: None,
            keep_with: None,
            refusal: None,
        }
    };
    let ready = k.state == State::Ready;
    if from.is_none() && accept.is_none() && ready && !(again && t.accepts()) {
        return Ok(unchanged());
    }
    let (bytes, status) = match from {
        Some(p) => {
            let f = std::fs::File::open(p)
                .map_err(|e| CliError::Usage(format!("{}: {e}", p.display())))?;
            (whole(t, f).map_err(CliError::Usage)?, "copied")
        }
        None => (download(t)?, "downloaded"),
    };
    let sha = sha256_hex(&bytes);
    // The file kept, downloaded or given again: nothing changes.
    let same = k.bytes.as_deref().map(sha256_hex);
    if ready && same.as_deref() == Some(sha.as_str()) {
        return Ok(unchanged());
    }
    let recorded = bytes.len() as u64 == t.bytes && sha == t.sha256;
    let unread = || {
        CliError::Other(format!(
            "{} {} does not have the rows medrec expects, so it was not kept",
            t.id, t.version
        ))
    };
    // A file whose rows do not read is never offered to be kept.
    let mut schedules = None;
    let rows = match t.form {
        Form::Zip { .. } => None,
        Form::Text { .. } => {
            let rows = text_rows(&bytes, t.form).filter(|r| !r.is_empty());
            let rows = rows.ok_or_else(unread)?;
            if recorded && rows.len() != t.rows {
                return Err(unread());
            }
            Some(rows)
        }
        Form::Archive => {
            let s = crate::schedule::Schedules::read(&bytes).ok_or_else(unread)?;
            if recorded && s.series != t.rows {
                return Err(unread());
            }
            schedules = Some(s);
            None
        }
    };
    // What the file says it is: a text table's newest day, an archive's own.
    let said = || match (&rows, &schedules) {
        (Some(r), _) => (newest(r, t.form), newest(r, t.form), r.len() as u64),
        (_, Some(s)) => (s.version.clone(), s.date.clone(), s.series as u64),
        _ => (String::new(), String::new(), 0),
    };
    if !recorded && !accept.is_some_and(|a| a.trim().eq_ignore_ascii_case(&sha)) {
        let mut keep_with = format!("medrec tables fetch {} --accept {sha}", t.id);
        if let Some(p) = from {
            keep_with.push_str(&format!(" --file {}", shell_word(&p.display().to_string())));
        }
        let refusal = format!(
            "{} is now another file than the one medrec was written against ({} bytes, \
             SHA-256 {sha}; medrec's has {} bytes, SHA-256 {}), so it was not kept. {}{} \
             To keep it: {keep_with}",
            t.id,
            bytes.len(),
            t.bytes,
            t.sha256,
            match t.form {
                Form::Archive =>
                    "Of its files medrec reads the series, their ages and codes alone.",
                _ => "Of its rows medrec reads code numbers alone.",
            },
            if ready {
                " The file kept before is still used."
            } else {
                ""
            },
        );
        return Ok(Fetched {
            id: t.id,
            version: said().0,
            status: "refused",
            bytes: bytes.len() as u64,
            accepted: false,
            sha256: Some(sha),
            keep_with: Some(keep_with),
            refusal: Some(refusal),
        });
    }
    let accepted = match t.form {
        Form::Zip {
            member: m,
            bytes: size,
        } => {
            let file = member(&bytes, m, size).ok_or_else(|| {
                CliError::Other(format!("{} {} cannot be unpacked", t.id, t.version))
            })?;
            if icd_rows(&file).count() != t.rows {
                return Err(unread());
            }
            None
        }
        Form::Text { .. } | Form::Archive => {
            let (version, date, rows) = said();
            (!recorded).then(|| Accepted {
                schema: ACCEPTED.into(),
                sha256: sha.clone(),
                bytes: bytes.len() as u64,
                rows,
                version: matches!(t.form, Form::Archive).then_some(version),
                date,
                accepted: crate::model::today(),
            })
        }
    };
    let keep = |e: std::io::Error| CliError::Other(format!("keeping {}: {e}", t.id));
    private_file::create_dir_all(dir).map_err(keep)?;
    // The word goes first, so a file is never there without the word that
    // lets it be used, nor the recorded file with an old one.
    let word = t.accepted_path(dir);
    match &accepted {
        Some(a) => {
            let json = serde_json::to_vec(a).map_err(|e| CliError::Other(e.to_string()))?;
            private_file::write(&word, &json, private_file::Replace::Always).map_err(keep)?;
        }
        None => match std::fs::remove_file(&word) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(keep(e)),
            _ => {}
        },
    }
    private_file::write(&t.path(dir), &bytes, private_file::Replace::Always).map_err(keep)?;
    Ok(Fetched {
        id: t.id,
        version: accepted
            .as_ref()
            .map_or(t.version.into(), Accepted::version),
        status,
        bytes: bytes.len() as u64,
        accepted: accepted.is_some(),
        sha256: None,
        keep_with: None,
        refusal: None,
    })
}

/// `s` as one word of a shell command.
fn shell_word(s: &str) -> String {
    let plain = |c: char| c.is_ascii_alphanumeric() || "_./-+:,@%=".contains(c);
    if !s.is_empty() && s.chars().all(plain) {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn download(t: &Table) -> Result<Vec<u8>, CliError> {
    pk_cli_core::transport::require_secure_url(t.url)?;
    let up = |e: reqwest::Error| {
        CliError::Upstream(format!("{} {}: {}", t.id, t.version, e.without_url()))
    };
    let r = reqwest::blocking::Client::builder()
        .user_agent(concat!("medrec/", env!("CARGO_PKG_VERSION")))
        .timeout(TIMEOUT)
        .redirect(pk_cli_core::transport::same_host_redirects())
        .build()
        .map_err(up)?
        .get(t.url)
        .send()
        .map_err(up)?;
    if !r.status().is_success() {
        return Err(CliError::Upstream(format!(
            "{} answered {} for {} {}",
            t.publisher,
            r.status().as_u16(),
            t.id,
            t.version
        )));
    }
    whole(t, r).map_err(CliError::Upstream)
}

/// Everything `r` has: of a zip, only the recorded file; of a text table, any
/// file of at most [`TEXT_MAX`] bytes, which [`fetch`] checks.
fn whole(t: &Table, r: impl Read) -> Result<Vec<u8>, String> {
    let most = match t.form {
        Form::Text { .. } => TEXT_MAX,
        Form::Archive => ARCHIVE_MAX,
        Form::Zip { .. } => t.bytes,
    };
    let mut bytes = Vec::with_capacity(t.bytes as usize);
    r.take(most + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("reading {}: {e}", t.id))?;
    if bytes.len() as u64 > most {
        return Err(format!("{} is over {most} bytes, so it was not kept", t.id));
    }
    if !t.accepts() && (bytes.len() as u64 != t.bytes || sha256_hex(&bytes) != t.sha256) {
        return Err(format!(
            "this is not {} {} as medrec records it (another size or SHA-256), so it was not kept",
            t.id, t.version
        ));
    }
    Ok(bytes)
}

fn u16_at(b: &[u8], at: usize) -> Option<usize> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?) as usize)
}

fn u32_at(b: &[u8], at: usize) -> Option<usize> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?) as usize)
}

/// One file of a zip, as its central directory gives it.
pub(crate) struct Entry {
    pub name: String,
    method: usize,
    packed: usize,
    pub size: u64,
    local: usize,
    /// The day its time stamp says, `YYYY-MM-DD`.
    pub day: Option<String>,
}

/// Every file of a zip, through its central directory.
pub(crate) fn entries(zip: &[u8]) -> Option<Vec<Entry>> {
    let end = (0..=zip.len().checked_sub(22)?)
        .rev()
        .take(66_000)
        .find(|at| zip[*at..].starts_with(b"PK\x05\x06"))?;
    let mut at = u32_at(zip, end + 16)?;
    let mut out = Vec::new();
    for _ in 0..u16_at(zip, end + 10)? {
        if !zip.get(at..)?.starts_with(b"PK\x01\x02") {
            return None;
        }
        let (n, x, c) = (
            u16_at(zip, at + 28)?,
            u16_at(zip, at + 30)?,
            u16_at(zip, at + 32)?,
        );
        let d = u16_at(zip, at + 14)?;
        let (y, m, day) = (1980 + (d >> 9), (d >> 5) & 15, d & 31);
        out.push(Entry {
            name: String::from_utf8_lossy(zip.get(at + 46..at + 46 + n)?).into_owned(),
            method: u16_at(zip, at + 10)?,
            packed: u32_at(zip, at + 20)?,
            size: u32_at(zip, at + 24)? as u64,
            local: u32_at(zip, at + 42)?,
            day: ((1..=12).contains(&m) && (1..=31).contains(&day))
                .then(|| format!("{y:04}-{m:02}-{day:02}")),
        });
        at += 46 + n + x + c;
    }
    Some(out)
}

/// The bytes of `e`, stored or deflated, when they are its whole size.
pub(crate) fn unpack(zip: &[u8], e: &Entry) -> Option<Vec<u8>> {
    let local = e.local;
    if !zip.get(local..)?.starts_with(b"PK\x03\x04") {
        return None;
    }
    let start = local + 30 + u16_at(zip, local + 26)? + u16_at(zip, local + 28)?;
    let data = zip.get(start..start.checked_add(e.packed)?)?;
    let mut out = Vec::new();
    match e.method {
        0 => out.extend_from_slice(data),
        8 => {
            flate2::read::DeflateDecoder::new(data)
                .take(e.size + 1)
                .read_to_end(&mut out)
                .ok()?;
        }
        _ => return None,
    }
    (out.len() as u64 == e.size).then_some(out)
}

/// One file of a zip, of exactly `size` bytes: stored or deflated, found
/// through the central directory.
fn member(zip: &[u8], name: &str, size: u64) -> Option<Vec<u8>> {
    let e = entries(zip)?.into_iter().find(|e| e.name == name)?;
    (e.size == size).then(|| unpack(zip, &e)).flatten()
}

/// The code and long title of each row of CMS's "order" file: fixed
/// columns, headings included.
fn icd_rows(file: &[u8]) -> impl Iterator<Item = (&str, &str)> {
    file.split(|b| *b == b'\n').filter_map(|l| {
        let l = std::str::from_utf8(l).ok()?;
        let (code, title) = (l.get(6..13)?.trim(), l.get(77..)?.trim());
        (!code.is_empty() && !title.is_empty()).then_some((code, title))
    })
}

/// A diagnosis code as the tables write it: upper case, no dot.
pub fn bare(code: &str) -> String {
    code.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

/// The titles of every code and heading, from the table on this machine.
pub struct Titles {
    file: Vec<u8>,
}

impl Titles {
    /// None when the table is absent or not the recorded file.
    pub fn load(dir: &Path) -> Option<Titles> {
        ICD10CM.read(dir).map(|file| Titles { file })
    }

    /// What identifies the table the titles came from.
    pub fn identity() -> String {
        format!("{} {}", ICD10CM.id, ICD10CM.version)
    }

    /// The titles of `codes`, each as [`bare`] writes it.
    pub fn of(&self, codes: &BTreeSet<String>) -> BTreeMap<String, String> {
        icd_rows(&self.file)
            .filter(|(c, _)| codes.contains(*c))
            .map(|(c, t)| (c.to_string(), t.to_string()))
            .collect()
    }

    /// Invented titles, in the published layout.
    #[cfg(test)]
    pub fn invented(rows: &[(&str, &str)]) -> Titles {
        let rows: Vec<_> = rows.iter().map(|(c, t)| (*c, "1", *t)).collect();
        Titles {
            file: tests::order(&rows),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    /// Invented rows in the publishers' layouts.
    pub(super) fn order(rows: &[(&str, &str, &str)]) -> Vec<u8> {
        rows.iter()
            .enumerate()
            .flat_map(|(i, (code, flag, title))| {
                let short: String = title.chars().take(60).collect();
                format!("{:05} {code:<7} {flag} {short:<60} {title}\r\n", i + 1).into_bytes()
            })
            .collect()
    }

    /// A zip of one deflated file, its sizes only in the central directory.
    fn zip(name: &str, data: &[u8]) -> Vec<u8> {
        let mut enc = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::best());
        enc.write_all(data).unwrap();
        let packed = enc.finish().unwrap();
        let n = name.len() as u16;
        let mut z = b"PK\x03\x04\x14\0\x08\0\x08\0\0\0\0\0\0\0\0\0".to_vec();
        z.extend([0u8; 8]);
        z.extend(n.to_le_bytes());
        z.extend([0u8; 2]);
        z.extend(name.as_bytes());
        z.extend(&packed);
        let at = z.len() as u32;
        z.extend(b"PK\x01\x02\x14\0\x14\0\x08\0\x08\0\0\0\0\0\0\0\0\0");
        z.extend((packed.len() as u32).to_le_bytes());
        z.extend((data.len() as u32).to_le_bytes());
        z.extend(n.to_le_bytes());
        z.extend([0u8; 12]);
        z.extend(0u32.to_le_bytes());
        z.extend(name.as_bytes());
        let len = z.len() as u32 - at;
        z.extend(b"PK\x05\x06\0\0\0\0\x01\0\x01\0");
        z.extend(len.to_le_bytes());
        z.extend(at.to_le_bytes());
        z.extend([0u8; 2]);
        z
    }

    #[test]
    fn a_zips_member_is_read_whole_or_not_at_all() {
        let data = order(&[("Z98A1", "1", "Sample sneeze, left")]);
        let z = zip("folder/order.txt", &data);
        assert_eq!(
            member(&z, "folder/order.txt", data.len() as u64).as_deref(),
            Some(&data[..])
        );
        assert_eq!(member(&z, "folder/order.txt", 5), None);
        assert_eq!(member(&z, "order.txt", data.len() as u64), None);
        for cut in 0..z.len() {
            assert_eq!(
                member(&z[..cut], "folder/order.txt", data.len() as u64),
                None
            );
        }
    }

    #[test]
    fn titles_come_from_their_fixed_columns_with_headings() {
        let f = order(&[
            ("Z98A", "0", "Sample signs"),
            ("Z98A1", "1", "Sample sneeze, left"),
        ]);
        assert_eq!(
            icd_rows(&f).collect::<Vec<_>>(),
            [("Z98A", "Sample signs"), ("Z98A1", "Sample sneeze, left")]
        );
    }

    #[test]
    fn codes_are_looked_up_without_their_dot() {
        assert_eq!(bare("z98.a1"), "Z98A1");
        assert_eq!(bare(" Z98A1 "), "Z98A1");
    }

    #[test]
    fn a_table_is_kept_only_when_it_is_the_recorded_file() {
        let dir = std::env::temp_dir().join(format!("medrec-tables-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let data = order(&[("Z98A1", "1", "Sample sneeze, left")]);
        let z = zip("o.txt", &data);
        let sha: &'static str = Box::leak(sha256_hex(&z).into_boxed_str());
        let t = Table {
            sha256: sha,
            bytes: z.len() as u64,
            form: Form::Zip {
                member: "o.txt",
                bytes: data.len() as u64,
            },
            rows: 1,
            ..ICD10CM
        };
        let local = dir.join("tables");
        assert_eq!(t.verified(&local).0, State::Absent);

        let src = dir.join("mine.zip");
        let mut other = z.clone();
        other[40] ^= 1;
        std::fs::write(&src, &other).unwrap();
        assert!(matches!(
            fetch(&t, &local, Some(&src), None, false),
            Err(CliError::Usage(_))
        ));
        assert!(!local.exists());
        // A zip has a version: no word keeps another file.
        let sha_other = sha256_hex(&other);
        assert!(matches!(
            fetch(&t, &local, Some(&src), Some(&sha_other), false),
            Err(CliError::Usage(_))
        ));

        std::fs::write(&src, &z).unwrap();
        assert_eq!(
            fetch(&t, &local, Some(&src), None, false).unwrap().status,
            "copied"
        );
        assert_eq!(t.verified(&local).0, State::Ready);
        // A zip has a version: named again, it is not downloaded again.
        for named in [false, true] {
            let f = fetch(&t, &local, None, None, named).unwrap();
            assert_eq!(f.status, "unchanged");
        }
        assert_eq!(std::fs::read_dir(&local).unwrap().count(), 1);

        std::fs::write(t.path(&local), &other).unwrap();
        assert_eq!(t.verified(&local), (State::Changed, None));
        assert_eq!(t.read(&local), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_download_must_be_whole_and_the_recorded_file() {
        let data = order(&[("Z98A1", "1", "Sample sneeze, left")]);
        let z = zip("o.txt", &data);
        let sha: &'static str = Box::leak(sha256_hex(&z).into_boxed_str());
        let t = |url| Table {
            url,
            sha256: sha,
            bytes: z.len() as u64,
            ..ICD10CM
        };
        assert_eq!(download(&t(serve("200 OK", z.clone()))).unwrap(), z);
        let mut longer = z.clone();
        longer.push(0);
        for (status, body) in [
            ("200 OK", longer),
            ("200 OK", z[..z.len() - 1].to_vec()),
            ("403 Forbidden", z.clone()),
        ] {
            assert!(matches!(
                download(&t(serve(status, body))),
                Err(CliError::Upstream(_))
            ));
        }
    }

    /// Invented rows in CDC's layouts: a CVX code, three texts, a status, the
    /// no-vaccine flag and the day; a CPT code, its text, an empty column,
    /// the CVX text and code, a comment, the day and an id.
    pub(crate) fn cdc(rows: &[&[&str]]) -> Vec<u8> {
        let mut out = b"\xef\xbb\xbf".to_vec();
        for r in rows {
            out.extend(r.join("|").into_bytes());
            out.extend(b"\r\n");
        }
        out
    }

    /// The address of a server on this machine that answers one request
    /// with `status` and `body`.
    fn serve(status: &'static str, body: Vec<u8>) -> &'static str {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/t", l.local_addr().unwrap());
        std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut buf = [0u8; 4096];
            let _ = s.read(&mut buf);
            let _ = s.write_all(
                format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .as_bytes(),
            );
            let _ = s.write_all(&body);
        });
        Box::leak(url.into_boxed_str())
    }

    fn text_table(file: &[u8], form: Form) -> Table {
        Table {
            sha256: Box::leak(sha256_hex(file).into_boxed_str()),
            bytes: file.len() as u64,
            form,
            rows: text_rows(file, form).unwrap().len(),
            ..CVX
        }
    }

    #[test]
    fn a_text_tables_rows_are_whole_or_none_are_read() {
        let form = CVX.form;
        let good = cdc(&[
            &[
                "900",
                "Sample",
                "Sample vaccine",
                "",
                "Active",
                "False",
                "2024/01/02",
            ],
            &[
                "901",
                "Sample",
                "Sample globulin",
                "",
                "Active",
                "True",
                "2024/03/04",
            ],
        ]);
        let rows = text_rows(&good, form).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!((rows[0][0], rows[1][5]), ("900", "True"));
        assert_eq!(newest(&rows, form), "2024-03-04");
        for bad in [
            cdc(&[&["900", "Sample", "Sample vaccine", "", "Active", "False"]]),
            cdc(&[&[
                "9O0",
                "Sample",
                "Sample vaccine",
                "",
                "Active",
                "False",
                "2024/01/02",
            ]]),
            cdc(&[&[
                "900",
                "Sample",
                "Sample vaccine",
                "",
                "Active",
                "False",
                "2024-01-02",
            ]]),
            vec![0xff, b'|'],
        ] {
            assert_eq!(text_rows(&bad, form), None);
        }
        assert_eq!(text_rows(&good, ICD10CM.form), None);
    }

    fn cvx_row(code: &str, name: &str, day: &str) -> [String; 7] {
        let row = [code, "Sample", name, "", "Active", "False", day];
        row.map(str::to_string)
    }

    fn cvx_file(rows: &[[String; 7]]) -> Vec<u8> {
        let rows: Vec<Vec<&str>> = rows
            .iter()
            .map(|r| r.iter().map(String::as_str).collect())
            .collect();
        let rows: Vec<&[&str]> = rows.iter().map(Vec::as_slice).collect();
        cdc(&rows)
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("medrec-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_text_table_that_changed_is_kept_only_on_the_persons_word() {
        let dir = scratch("text-tables");
        let first = cvx_file(&[cvx_row("900", "Sample vaccine", "2024/01/02")]);
        let later = cvx_file(&[
            cvx_row("900", "Sample vaccine", "2024/01/02"),
            cvx_row("902", "Sample vaccine B", "2024/05/06"),
        ]);
        let t = text_table(&first, CVX.form);
        let local = dir.join("tables");
        let src = dir.join("cvx.txt");
        std::fs::write(&src, &later).unwrap();
        let sha = sha256_hex(&later);
        // Refused, with the command that keeps it.
        let f = fetch(&t, &local, Some(&src), None, true).unwrap();
        assert!(f.refused());
        let keep = format!(
            "medrec tables fetch cvx --accept {sha} --file {}",
            src.display()
        );
        assert_eq!(f.keep_with.as_deref(), Some(keep.as_str()));
        assert_eq!(
            (f.sha256.as_deref(), f.version.as_str()),
            (Some(sha.as_str()), "2024-05-06")
        );
        assert!(f.refusal.unwrap().ends_with(&keep));
        let other = fetch(&t, &local, Some(&src), Some(&sha_of("another")), true).unwrap();
        assert!(other.refused());
        assert_eq!(t.verified(&local).0, State::Absent);

        let kept = fetch(&t, &local, Some(&src), Some(&sha.to_uppercase()), true).unwrap();
        assert!(kept.accepted && !kept.refused());
        assert_eq!(
            (kept.version.as_str(), kept.bytes),
            ("2024-05-06", later.len() as u64)
        );
        assert_eq!(t.verified(&local).0, State::Ready);
        let a = t.accepted(&local).unwrap();
        assert_eq!((a.sha256.as_str(), a.rows), (sha.as_str(), 2));
        let codes = t.columns(&local, &[0, 5]).unwrap();
        assert_eq!(codes, [["900", "False"], ["902", "False"]]);
        assert_eq!(t.identity(&local), Some(format!("cvx {sha}")));
        // Not named, it is not downloaded again; given again, it is unchanged.
        let again = fetch(&t, &local, None, None, false).unwrap();
        assert_eq!((again.status, again.accepted), ("unchanged", true));
        let given = fetch(&t, &local, Some(&src), None, true).unwrap();
        assert_eq!((given.status, given.accepted), ("unchanged", true));

        // Named, it is: the file kept is unchanged, another is refused and the
        // one kept still used, and the recorded file replaces it with no word.
        let at = |body: &[u8]| Table {
            url: serve("200 OK", body.to_vec()),
            ..t
        };
        let same = fetch(&at(&later), &local, None, None, true).unwrap();
        assert_eq!((same.status, same.accepted), ("unchanged", true));
        let newer = cvx_file(&[cvx_row("903", "Sample vaccine C", "2024/07/08")]);
        let f = fetch(&at(&newer), &local, None, None, true).unwrap();
        assert!(f.refused());
        let keep = format!("medrec tables fetch cvx --accept {}", sha256_hex(&newer));
        assert_eq!(f.keep_with.as_deref(), Some(keep.as_str()));
        assert!(f
            .refusal
            .unwrap()
            .contains("The file kept before is still used."));
        assert_eq!(t.accepted(&local).unwrap().sha256, sha);
        assert_eq!(t.verified(&local).0, State::Ready);
        let back = fetch(&at(&first), &local, None, None, true).unwrap();
        assert_eq!((back.status, back.accepted), ("downloaded", false));
        assert!(t.accepted(&local).is_none());
        assert_eq!(t.identity(&local), Some(format!("cvx {}", t.sha256)));

        // A file that is neither is not used.
        std::fs::write(t.path(&local), &first[..first.len() - 2]).unwrap();
        assert_eq!(t.verified(&local), (State::Changed, None));
        assert_eq!(t.columns(&local, &[0]), None);
        assert_eq!(t.identity(&local), None);

        // The recorded file needs no word, and leaves none behind.
        std::fs::write(&src, &first).unwrap();
        let back = fetch(&t, &local, Some(&src), None, true).unwrap();
        assert!(!back.accepted);
        assert_eq!(back.version, t.version);
        assert!(t.accepted(&local).is_none());
        assert_eq!(std::fs::read_dir(&local).unwrap().count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A file too big, empty, or of no table's rows is never kept nor offered
    /// to be, whatever hash is given.
    #[test]
    fn a_text_table_that_is_no_table_is_never_kept() {
        let dir = scratch("text-tables-none");
        let first = cvx_file(&[cvx_row("900", "Sample vaccine", "2024/01/02")]);
        let t = text_table(&first, CVX.form);
        let big = std::io::Read::take(std::io::repeat(b'9'), TEXT_MAX + 1);
        assert!(whole(&t, big).unwrap_err().contains("is over"));
        let at_most = std::io::Read::take(std::io::repeat(b'9'), TEXT_MAX);
        assert_eq!(whole(&t, at_most).unwrap().len() as u64, TEXT_MAX);
        let local = dir.join("tables");
        let src = dir.join("cvx.txt");
        let page = b"<html><body>Sample page</body></html>\n".to_vec();
        for body in [Vec::new(), b"\n\n".to_vec(), page] {
            std::fs::write(&src, &body).unwrap();
            for accept in [None, Some(sha256_hex(&body))] {
                match fetch(&t, &local, Some(&src), accept.as_deref(), true) {
                    Err(CliError::Other(m)) => assert!(m.contains("rows medrec expects"), "{m}"),
                    other => panic!("{other:?}"),
                }
            }
        }
        assert!(!local.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn sha_of(s: &str) -> String {
        sha256_hex(s.as_bytes())
    }

    #[test]
    fn the_recorded_tables_are_whole_files_over_https() {
        for t in TABLES {
            assert!(t.url.starts_with("https://"), "{}", t.id);
            assert!(t.sha256.len() == 64 && t.sha256.bytes().all(|b| b.is_ascii_hexdigit()));
            assert!(t.bytes > 0 && t.rows > 0 && !t.url.contains('?'));
        }
    }
}
