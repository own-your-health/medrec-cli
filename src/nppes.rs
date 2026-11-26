//! The provider directory: CMS's monthly NPPES file, read as it downloads,
//! and what of it one records folder needs, kept on this machine.
//!
//! `medrec tables fetch nppes` states the size, asks, and reads the whole
//! published zip in one pass; nothing is looked up by name or number over
//! the network, and neither the zip nor its files are kept. CMS replaces the
//! file every month, so no hash is recorded here: the columns are checked by
//! name, every NPI by its check digit, the whole by its size, its checksum
//! and a least number of rows, and the link, month, size, SHA-256 and row
//! count are kept beside what was read. A file that fails leaves the last
//! one in use.

use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Read};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use pk_cli_core::{private_file, CliError};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::hash::sha256_hex;
use crate::model::PersonIndex;
use crate::providers::npi_holds;

pub const ID: &str = "nppes";
pub const TITLE: &str = "Provider directory (NPPES)";
pub const PUBLISHER: &str = "Centers for Medicare & Medicaid Services";
/// v1 is 0.9.6's, which says neither what it was asked for nor some of
/// what is kept of a provider.
const SCHEMA: &str = "medrec-nppes/v2";
const FORMAT: &str = "medrec-nppes/";

/// Where the file comes from, and the fewest rows it can have.
#[derive(Debug, Clone, Copy)]
pub struct Source<'a> {
    pub base: &'a str,
    pub min_rows: u64,
}

pub const CMS: Source<'static> = Source {
    base: "https://download.cms.gov/nppes/",
    min_rows: 9_000_000,
};

/// The page that lists the files.
const PAGE: &str = "NPI_Files.html";

/// The columns read, by position and by the name the first row must give
/// them.
const NPI: usize = 0;
const ENTITY: usize = 1;
const ORGANIZATION: usize = 4;
const LAST: usize = 5;
const FIRST: usize = 6;
const MIDDLE: usize = 7;
const CREDENTIAL: usize = 10;
const OTHER_ORGANIZATION: usize = 11;
const OTHER_TYPE: usize = 12;
const OTHER_LAST: usize = 13;
const OTHER_FIRST: usize = 14;
const CITY: usize = 30;
const STATE: usize = 31;
const PHONE: usize = 34;
const TAXONOMY: usize = 47;
const PRIMARY: usize = 50;
/// A provider's taxonomy codes and their primary switches, four columns
/// apart.
const TAXONOMIES: usize = 15;

const COLUMNS: &[(usize, &str)] = &[
    (NPI, "NPI"),
    (ENTITY, "Entity Type Code"),
    (
        ORGANIZATION,
        "Provider Organization Name (Legal Business Name)",
    ),
    (LAST, "Provider Last Name (Legal Name)"),
    (FIRST, "Provider First Name"),
    (MIDDLE, "Provider Middle Name"),
    (CREDENTIAL, "Provider Credential Text"),
    (OTHER_ORGANIZATION, "Provider Other Organization Name"),
    (OTHER_TYPE, "Provider Other Organization Name Type Code"),
    (OTHER_LAST, "Provider Other Last Name"),
    (OTHER_FIRST, "Provider Other First Name"),
    (
        CITY,
        "Provider Business Practice Location Address City Name",
    ),
    (
        STATE,
        "Provider Business Practice Location Address State Name",
    ),
    (
        PHONE,
        "Provider Business Practice Location Address Telephone Number",
    ),
    (TAXONOMY, "Healthcare Provider Taxonomy Code_1"),
    (PRIMARY, "Healthcare Provider Primary Taxonomy Switch_1"),
    (
        TAXONOMY + 4 * (TAXONOMIES - 1),
        "Healthcare Provider Taxonomy Code_15",
    ),
    (
        PRIMARY + 4 * (TAXONOMIES - 1),
        "Healthcare Provider Primary Taxonomy Switch_15",
    ),
];

/// The type code of the name an organization does business as.
const BUSINESS_NAME: &[u8] = b"3";

/// How long one read of the server may take, and the whole download.
const READ: Duration = Duration::from_secs(60);
const WHOLE: Duration = Duration::from_secs(2 * 60 * 60);
/// The longest row.
const MAX_ROW: usize = 64 * 1024;
/// The most the zip's files may hold together.
const MAX_INSIDE: u64 = 64 << 30;

/// One monthly file, as the page lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Offer {
    pub url: String,
    /// `YYYY-MM`.
    pub month: String,
    pub bytes: u64,
}

/// What one folder's records need of the file.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Wanted {
    pub npis: BTreeSet<String>,
    pub states: BTreeSet<String>,
    /// The surname and first initial of each person printed with no NPI.
    pub people: BTreeSet<(String, char)>,
}

/// One provider kept from the file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provider {
    pub npi: String,
    /// An organization's name; empty for a person.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub organization: String,
    /// The name it does business as.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub business_name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub last: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub first: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub middle: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub other_last: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub other_first: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub credential: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub city: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub state: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub phone: String,
    /// The primary taxonomy code, else the first.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub taxonomy: String,
}

/// The first line of a kept file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Kept {
    pub schema: String,
    pub url: String,
    pub month: String,
    /// The last day the file covers, as its name says.
    pub date: String,
    pub bytes: u64,
    pub sha256: String,
    pub rows: u64,
    pub fetched_at: String,
    pub wanted_sha256: String,
    /// Those it was asked for, whether CMS's file has them or not.
    pub npis: BTreeSet<String>,
    /// The states every organization was kept of, and with the people below
    /// every person of those names.
    pub states: BTreeSet<String>,
    pub people: BTreeSet<(String, char)>,
    pub kept: u64,
    /// Of the lines below this one.
    pub kept_sha256: String,
}

/// The providers one folder's records need.
#[derive(Debug, Clone)]
pub struct Directory {
    pub about: Kept,
    pub providers: Vec<Provider>,
}

#[derive(Debug, Serialize)]
pub struct Fetched {
    pub id: &'static str,
    pub version: String,
    /// `unchanged` or `downloaded`.
    pub status: &'static str,
    pub bytes: u64,
    pub rows: u64,
    pub kept: u64,
}

impl Wanted {
    /// What every person's records need: the NPIs and states their sources
    /// give, and the people they print without one.
    pub fn of<'a>(indexes: impl IntoIterator<Item = &'a PersonIndex>) -> Wanted {
        let mut w = Wanted::default();
        for idx in indexes {
            for r in &idx.records {
                for p in &r.providers {
                    w.npis.extend(p.npi.clone());
                    w.states.extend(p.state.clone());
                }
                for f in crate::providers::FIELDS {
                    let known = r.providers.iter().any(|p| p.field == *f && p.npi.is_some());
                    if let Some(printed) = r.fields.get(*f).and_then(Value::as_str) {
                        if !known {
                            w.people.extend(people(printed));
                        }
                    }
                }
            }
            for d in &idx.documents {
                w.people
                    .extend(d.organization.iter().flat_map(|o| people(o)));
            }
        }
        w
    }

    /// What identifies these: one line each, of its kind and value, in
    /// order.
    fn sha256(&self) -> String {
        let npis = self.npis.iter().map(|n| format!("npi {n}\n"));
        let states = self.states.iter().map(|s| format!("state {s}\n"));
        let people = self.people.iter().map(|(l, i)| format!("person {i} {l}\n"));
        sha256_hex(
            npis.chain(states)
                .chain(people)
                .collect::<String>()
                .as_bytes(),
        )
    }

    fn wants(&self, row: &Row) -> bool {
        let state = || self.states.contains(&row.text(STATE).to_ascii_uppercase());
        match row.raw(ENTITY) {
            b"1" => {
                self.npis.contains(&*row.text(NPI))
                    || (!self.people.is_empty()
                        && state()
                        && [(LAST, FIRST), (OTHER_LAST, OTHER_FIRST)]
                            .iter()
                            .any(|(l, f)| {
                                person(&row.text(*l), &row.text(*f))
                                    .is_some_and(|p| self.people.contains(&p))
                            }))
            }
            b"2" => self.npis.contains(&*row.text(NPI)) || state(),
            // Deactivated: the row has no name.
            _ => false,
        }
    }
}

/// Letters and digits in capitals, with the accents of Latin letters and
/// everything else left out.
pub fn fold(s: &str) -> String {
    s.chars()
        .filter_map(|c| match c {
            'à'..='å' | 'À'..='Å' => Some('A'),
            'ç' | 'Ç' => Some('C'),
            'è'..='ë' | 'È'..='Ë' => Some('E'),
            'ì'..='ï' | 'Ì'..='Ï' => Some('I'),
            'ñ' | 'Ñ' => Some('N'),
            'ò'..='ö' | 'Ò'..='Ö' => Some('O'),
            'ù'..='ü' | 'Ù'..='Ü' => Some('U'),
            'ý' | 'ÿ' | 'Ý' => Some('Y'),
            c if c.is_ascii_alphanumeric() => Some(c.to_ascii_uppercase()),
            _ => None,
        })
        .collect()
}

/// A person's surname and first initial, as [`people`] gives them.
pub fn person(last: &str, first: &str) -> Option<(String, char)> {
    let last = fold(last);
    let initial = fold(first).chars().next()?;
    (last.len() > 1).then_some((last, initial))
}

/// What a printed name carries besides the name.
const AROUND_A_NAME: &[&str] = &[
    "APRN", "ARNP", "AUD", "BC", "BS", "BSN", "CNM", "CNP", "CNS", "CRNA", "DC", "DDS", "DMD",
    "DNP", "DO", "DPM", "DPT", "DR", "FAAP", "FACC", "FACOG", "FACP", "FACS", "FNP", "FNPBC",
    "FNPC", "II", "III", "IV", "JR", "LCSW", "LMFT", "LMHC", "LPC", "MA", "MBBS", "MD", "MED",
    "MPH", "MS", "MSN", "MSW", "NP", "NPC", "OD", "OTR", "PA", "PAC", "PHARMD", "PHD", "PNP",
    "PSYD", "PT", "RD", "RN", "RPH", "SLP", "SR", "WHNP",
];

/// Whether a word, in capitals, is what a printed name carries besides the name.
pub fn around_a_name(word: &str) -> bool {
    AROUND_A_NAME.contains(&word)
}

/// The words of a name in capitals, without what is around a name.
pub fn name_words(printed: &str) -> Vec<String> {
    printed
        .split(|c: char| c.is_whitespace() || c == ',')
        .map(fold)
        .filter(|t| !t.is_empty() && !around_a_name(t))
        .collect()
}

/// The surname and first initial a printed name can be of: `Jordan A.
/// Sample, MD`, `SAMPLE, JORDAN A` and `SAMPLE JORDAN A`. More than one when
/// the name can be read in more than one way.
pub fn people(printed: &str) -> BTreeSet<(String, char)> {
    readings(printed)
        .iter()
        .filter_map(|(last, given)| person(last, &given[0]))
        .collect()
}

/// The ways a printed name can be read: the surname, and the words of the
/// given names, at least one.
pub fn readings(printed: &str) -> Vec<(String, Vec<String>)> {
    let pieces: Vec<Vec<String>> = printed
        .split(',')
        .map(name_words)
        .filter(|p| !p.is_empty())
        .collect();
    let mut out = Vec::new();
    let mut add = |last: &[String], given: &[String]| out.push((last.concat(), given.to_vec()));
    match pieces.as_slice() {
        [] => {}
        [one] => {
            let n = one.len();
            if (2..=5).contains(&n) {
                add(&one[n - 1..], &one[..n - 1]);
                add(&one[..1], &one[1..]);
                if n >= 3 {
                    add(&one[n - 2..], &one[..n - 2]);
                    add(&one[..2], &one[2..]);
                }
            }
        }
        [last, given, ..] => {
            if last.len() <= 3 && given.len() <= 3 {
                add(last, given);
                // `Jordan Sample, Example Group`.
                if let [before @ .., l] = last.as_slice() {
                    if !before.is_empty() {
                        add(std::slice::from_ref(l), before);
                    }
                }
            }
        }
    }
    out
}

/// The monthly file the page lists, with the size the server gives it.
pub fn offer(source: &Source) -> Result<Offer, CliError> {
    let http = client()?;
    let page = get(&http, &format!("{}{PAGE}", source.base))?
        .text()
        .map_err(upstream)?;
    let (month, name) = monthly(&page).ok_or_else(|| {
        CliError::Upstream("CMS's page lists no monthly NPPES file medrec knows".into())
    })?;
    let url = format!("{}{name}", source.base);
    pk_cli_core::transport::require_secure_url(&url)?;
    let head = http.head(&url).send().map_err(upstream)?;
    if !head.status().is_success() {
        return Err(CliError::Upstream(format!(
            "CMS answered {} for its NPPES file",
            head.status().as_u16()
        )));
    }
    let bytes = head
        .headers()
        .get(reqwest::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok()?.parse::<u64>().ok())
        .filter(|n| *n > 0)
        .ok_or_else(|| CliError::Upstream("CMS gave no size for its NPPES file".into()))?;
    Ok(Offer { url, month, bytes })
}

const MONTHS: &[&str] = &[
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// The newest `NPPES_Data_Dissemination_<Month>_<Year>.zip` a page links to,
/// with or without `_V2`, and its month.
fn monthly(page: &str) -> Option<(String, String)> {
    page.split(['"', '\''])
        .filter_map(|s| {
            let name = s.strip_prefix("./").unwrap_or(s);
            let rest = name
                .strip_prefix("NPPES_Data_Dissemination_")?
                .strip_suffix(".zip")?;
            let mut parts = rest.split('_');
            let (month, year, rest) = (parts.next()?, parts.next()?, parts.next());
            let m = MONTHS.iter().position(|m| *m == month)? + 1;
            let dated = year.len() == 4 && year.bytes().all(|b| b.is_ascii_digit());
            let version = rest.is_none_or(|v| {
                v.len() > 1 && v.starts_with('V') && v[1..].bytes().all(|b| b.is_ascii_digit())
            });
            (dated && version && parts.next().is_none())
                .then(|| (format!("{year}-{m:02}"), name.to_string()))
        })
        .max()
}

/// `timeout` is of each read of a body, not of the whole.
fn client() -> Result<reqwest::blocking::Client, CliError> {
    reqwest::blocking::Client::builder()
        .user_agent(concat!("medrec/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(30))
        .timeout(READ)
        .redirect(pk_cli_core::transport::same_host_redirects())
        .build()
        .map_err(upstream)
}

fn upstream(e: reqwest::Error) -> CliError {
    CliError::Upstream(format!("{ID}: {}", e.without_url()))
}

fn get(
    http: &reqwest::blocking::Client,
    url: &str,
) -> Result<reqwest::blocking::Response, CliError> {
    pk_cli_core::transport::require_secure_url(url)?;
    let r = http.get(url).send().map_err(upstream)?;
    if !r.status().is_success() {
        return Err(CliError::Upstream(format!(
            "CMS answered {} for {ID}",
            r.status().as_u16()
        )));
    }
    Ok(r)
}

/// Keep what `wanted` needs of the file `offer` names. `progress` is told
/// how many bytes have arrived. Nothing already kept is replaced until the
/// whole file has passed.
pub fn fetch(
    source: &Source,
    offer: &Offer,
    wanted: &Wanted,
    path: &Path,
    progress: &mut dyn FnMut(u64),
) -> Result<Fetched, CliError> {
    let r = get(&client()?, &offer.url)?;
    let until = Instant::now() + WHOLE;
    let (about, lines) = read_zip(r, offer.bytes, source.min_rows, wanted, until, progress)
        .map_err(|e| CliError::Upstream(format!("CMS's NPPES file was not kept: {e}")))?;
    let about = Kept {
        schema: SCHEMA.into(),
        url: offer.url.clone(),
        month: offer.month.clone(),
        fetched_at: crate::model::now_rfc3339(),
        wanted_sha256: wanted.sha256(),
        npis: wanted.npis.clone(),
        states: wanted.states.clone(),
        people: wanted.people.clone(),
        ..about
    };
    let mut file =
        serde_json::to_vec(&about).map_err(|e| CliError::Other(format!("writing {ID}: {e}")))?;
    file.push(b'\n');
    file.extend(lines);
    path.parent()
        .map_or(Ok(()), private_file::create_dir_all)
        .and_then(|()| private_file::write(path, &file, private_file::Replace::Always))
        .map_err(|e| CliError::Other(format!("keeping {ID}: {e}")))?;
    Ok(about.fetched("downloaded"))
}

impl Kept {
    /// Whether this is what `offer` and `wanted` would be read as again.
    pub fn is_current(&self, offer: &Offer, wanted: &Wanted) -> bool {
        self.url == offer.url && self.bytes == offer.bytes && self.wanted_sha256 == wanted.sha256()
    }

    pub fn fetched(&self, status: &'static str) -> Fetched {
        Fetched {
            id: ID,
            version: self.month.clone(),
            status,
            bytes: self.bytes,
            rows: self.rows,
            kept: self.kept,
        }
    }
}

/// Where the kept providers are: in the cache and, unlike a code table,
/// removed by `cache clear`, since the records chose them.
pub fn dir() -> Option<PathBuf> {
    Some(pk_cli_drive::cache::cache_base("medrec")?.join("directory"))
}

/// The providers kept for the folder `folder_id`.
pub fn path(dir: &Path, folder_id: &str) -> PathBuf {
    dir.join(format!("{ID}-{folder_id}.jsonl"))
}

/// Every kept file in `dir`: the folder it is for, and its first line, or
/// `None` when it does not read.
pub fn list(dir: &Path) -> Vec<(String, Result<Kept, Unused>)> {
    let mut found: Vec<(String, Result<Kept, Unused>)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|e| {
            let path = e.ok()?.path();
            let name = path.file_name()?.to_str()?;
            let folder = name
                .strip_prefix(ID)?
                .strip_prefix('-')?
                .strip_suffix(".jsonl")?
                .to_string();
            Some((folder, Directory::load(&path).map(|d| d.about)))
        })
        .collect();
    found.sort_by(|a, b| a.0.cmp(&b.0));
    found
}

/// Why there is no directory to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unused {
    Absent,
    /// Another medrec's: whole, perhaps, and not in this one's format.
    OtherVersion,
    /// Not as it was written.
    Changed,
}

/// A kept file as it was written, its rows not read yet.
pub struct Stored {
    pub about: Kept,
    file: Vec<u8>,
    at: usize,
}

impl Stored {
    pub fn open(path: &Path) -> Result<Stored, Unused> {
        let file = std::fs::read(path).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => Unused::Absent,
            _ => Unused::Changed,
        })?;
        let at = file
            .iter()
            .position(|b| *b == b'\n')
            .ok_or(Unused::Changed)?
            + 1;
        let first: Value = serde_json::from_slice(&file[..at]).map_err(|_| Unused::Changed)?;
        match first["schema"].as_str() {
            Some(SCHEMA) => {}
            Some(s) if s.starts_with(FORMAT) => return Err(Unused::OtherVersion),
            _ => return Err(Unused::Changed),
        }
        let about: Kept = serde_json::from_value(first).map_err(|_| Unused::Changed)?;
        if sha256_hex(&file[at..]) != about.kept_sha256 {
            return Err(Unused::Changed);
        }
        Ok(Stored { about, file, at })
    }

    /// Where each row is, with its NPI and whether it is a person's: as
    /// [`Provider`] is written, the NPI first and an organization's name
    /// after it.
    pub fn lines(&self) -> impl Iterator<Item = (Range<usize>, String, bool)> + '_ {
        const BEFORE: &[u8] = b"{\"npi\":\"";
        let mut at = self.at;
        self.file[self.at..]
            .split(|b| *b == b'\n')
            .filter_map(move |line| {
                let here = at..at + line.len();
                at = here.end + 1;
                let rest = line.strip_prefix(BEFORE)?;
                let npi = std::str::from_utf8(rest.get(..10)?).ok()?.to_string();
                let person = !rest[10..].starts_with(b"\",\"organization\":");
                Some((here, npi, person))
            })
    }

    pub fn provider(&self, at: Range<usize>) -> Option<Provider> {
        serde_json::from_slice(self.file.get(at)?).ok()
    }
}

impl Directory {
    pub fn load(path: &Path) -> Result<Directory, Unused> {
        let s = Stored::open(path)?;
        let providers: Vec<Provider> = s
            .lines()
            .map(|(at, _, _)| s.provider(at))
            .collect::<Option<_>>()
            .ok_or(Unused::Changed)?;
        if providers.len() as u64 != s.about.kept {
            return Err(Unused::Changed);
        }
        Ok(Directory {
            about: s.about,
            providers,
        })
    }
}

/// Counts and hashes what passes.
struct Tap<'a, R> {
    r: R,
    sha: Sha256,
    n: u64,
    until: Instant,
    progress: &'a mut dyn FnMut(u64),
}

impl<R: Read> Read for Tap<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if Instant::now() > self.until {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "the download took too long",
            ));
        }
        let n = self.r.read(buf)?;
        self.sha.update(&buf[..n]);
        self.n += n as u64;
        (self.progress)(self.n);
        Ok(n)
    }
}

/// Checksums what passes.
struct Crc<R> {
    r: R,
    crc: flate2::Crc,
}

impl<R: Read> Read for Crc<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.r.read(buf)?;
        self.crc.update(&buf[..n]);
        Ok(n)
    }
}

const LOCAL: [u8; 4] = *b"PK\x03\x04";
const CENTRAL: [u8; 4] = *b"PK\x01\x02";
const DESCRIPTOR: [u8; 4] = *b"PK\x07\x08";
const END: [u8; 4] = *b"PK\x05\x06";

fn bytes<const N: usize>(r: &mut impl Read) -> Result<[u8; N], String> {
    let mut b = [0u8; N];
    r.read_exact(&mut b)
        .map_err(|e| format!("it ends early ({e})"))?;
    Ok(b)
}

fn u16_of(b: &[u8]) -> u64 {
    u64::from(u16::from_le_bytes([b[0], b[1]]))
}

fn u32_of(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

/// Read a zip of `size` bytes as it arrives: the provider file's rows, and
/// every other file only for its checksum. What the first line of a kept
/// file says, and the lines below it.
fn read_zip(
    r: impl Read,
    size: u64,
    min_rows: u64,
    wanted: &Wanted,
    until: Instant,
    progress: &mut dyn FnMut(u64),
) -> Result<(Kept, Vec<u8>), String> {
    let mut r = BufReader::with_capacity(
        1 << 20,
        Tap {
            r: r.take(size + 1),
            sha: Sha256::new(),
            n: 0,
            until,
            progress,
        },
    );
    let mut found: Option<(String, u64, u64, Vec<u8>)> = None;
    let mut sig = bytes::<4>(&mut r)?;
    while sig == LOCAL {
        let h = bytes::<26>(&mut r)?;
        let (flags, method) = (u16_of(&h[2..]), u16_of(&h[4..]));
        let mut name = vec![0u8; u16_of(&h[22..]) as usize];
        r.read_exact(&mut name)
            .map_err(|e| format!("it ends early ({e})"))?;
        std::io::copy(&mut (&mut r).take(u16_of(&h[24..])), &mut std::io::sink())
            .map_err(|e| e.to_string())?;
        if method != 8 || flags & 8 == 0 {
            return Err("its files are not stored as medrec expects".into());
        }
        let name = String::from_utf8_lossy(&name).into_owned();
        let mut inside = Crc {
            r: flate2::bufread::DeflateDecoder::new(&mut r),
            crc: flate2::Crc::new(),
        };
        if let Some(date) = data_file(&name) {
            if found.is_some() {
                return Err("it has two provider files".into());
            }
            let (rows, kept, lines) = read_rows((&mut inside).take(MAX_INSIDE), wanted)?;
            found = Some((date, rows, kept, lines));
        }
        std::io::copy(&mut (&mut inside).take(MAX_INSIDE), &mut std::io::sink())
            .map_err(|e| format!("{name}: {e}"))?;
        let (crc, packed, unpacked) = (inside.crc.sum(), inside.r.total_in(), inside.r.total_out());
        sig = descriptor(&mut r, crc, packed, unpacked).map_err(|e| format!("{name}: {e}"))?;
    }
    if sig != CENTRAL {
        return Err("it is not a zip medrec can read".into());
    }
    let mut rest = Vec::new();
    r.read_to_end(&mut rest).map_err(|e| e.to_string())?;
    let end = rest.windows(END.len()).rposition(|w| w == END);
    if end.is_none_or(|at| rest.len() < at + 22) {
        return Err("it ends early".into());
    }
    let tap = r.into_inner();
    if tap.n != size {
        return Err(format!("{} bytes came, and {size} were offered", tap.n));
    }
    let (date, rows, kept, lines) = found.ok_or("it has no provider file")?;
    if rows < min_rows {
        return Err(format!(
            "it has {rows} rows, and a whole one has at least {min_rows}"
        ));
    }
    let about = Kept {
        schema: String::new(),
        url: String::new(),
        month: String::new(),
        date,
        bytes: size,
        sha256: crate::hash::hex(&tap.sha.finalize()),
        rows,
        fetched_at: String::new(),
        wanted_sha256: String::new(),
        npis: BTreeSet::new(),
        states: BTreeSet::new(),
        people: BTreeSet::new(),
        kept,
        kept_sha256: sha256_hex(&lines),
    };
    Ok((about, lines))
}

/// The end date of `npidata_pfile_<from>-<to>.csv`, as `YYYY-MM-DD`.
fn data_file(name: &str) -> Option<String> {
    let to = name
        .strip_prefix("npidata_pfile_")?
        .strip_suffix(".csv")?
        .split_once('-')?
        .1;
    (to.len() == 8 && to.bytes().all(|b| b.is_ascii_digit()))
        .then(|| format!("{}-{}-{}", &to[..4], &to[4..6], &to[6..]))
}

/// The checksum and sizes after a file, which must be those of what was
/// read, and the signature that follows. Sizes are four bytes or eight.
fn descriptor(r: &mut impl Read, crc: u32, packed: u64, unpacked: u64) -> Result<[u8; 4], String> {
    let wrong = || Err("its checksum or size is wrong".to_string());
    let mut first = bytes::<4>(r)?;
    if first == DESCRIPTOR {
        first = bytes::<4>(r)?;
    }
    if u32_of(&first) != crc {
        return wrong();
    }
    let sizes = bytes::<8>(r)?;
    let next = bytes::<4>(r)?;
    if next == LOCAL || next == CENTRAL {
        return if u32_of(&sizes) == packed as u32 && u32_of(&sizes[4..]) == unpacked as u32 {
            Ok(next)
        } else {
            wrong()
        };
    }
    let rest = bytes::<4>(r)?;
    let wide = u64::from(u32_of(&next)) | u64::from(u32_of(&rest)) << 32;
    if u64::from_le_bytes(sizes) != packed || wide != unpacked {
        return wrong();
    }
    bytes::<4>(r)
}

/// One row of the file, its columns by position.
#[derive(Default)]
struct Row {
    buf: Vec<u8>,
    at: Vec<(usize, usize)>,
}

impl Row {
    /// The next row, or false at the end of the file.
    fn next(&mut self, r: &mut impl BufRead) -> Result<bool, String> {
        self.buf.clear();
        loop {
            let before = self.buf.len();
            let room = (MAX_ROW - before) as u64;
            Read::take(&mut *r, room)
                .read_until(b'\n', &mut self.buf)
                .map_err(|e| e.to_string())?;
            if self.buf.len() == before {
                return if before == 0 {
                    Ok(false)
                } else {
                    Err("a row is cut short".into())
                };
            }
            if self.split() {
                return Ok(true);
            }
            if self.buf.len() >= MAX_ROW {
                return Err("a row is longer than any can be".into());
            }
        }
    }

    /// The columns of `buf`; false when it ends inside a quoted one, which
    /// holds the line break.
    fn split(&mut self) -> bool {
        self.at.clear();
        let mut end = self.buf.len();
        while end > 0 && matches!(self.buf[end - 1], b'\n' | b'\r') {
            end -= 1;
        }
        let b = &self.buf[..end];
        let mut i = 0;
        loop {
            if b.get(i) == Some(&b'"') {
                let start = i + 1;
                let mut j = start;
                loop {
                    match b[j..].iter().position(|c| *c == b'"') {
                        None => return false,
                        Some(n) => {
                            j += n;
                            if b.get(j + 1) == Some(&b'"') {
                                j += 2;
                            } else {
                                break;
                            }
                        }
                    }
                }
                self.at.push((start, j));
                i = j + 1;
            } else {
                let n = b[i..].iter().position(|c| *c == b',').unwrap_or(end - i);
                self.at.push((i, i + n));
                i += n;
            }
            match b.get(i) {
                Some(b',') => i += 1,
                _ => return true,
            }
        }
    }

    fn raw(&self, column: usize) -> &[u8] {
        self.at.get(column).map_or(&[], |(a, b)| &self.buf[*a..*b])
    }

    fn text(&self, column: usize) -> String {
        String::from_utf8_lossy(self.raw(column))
            .replace("\"\"", "\"")
            .trim()
            .to_string()
    }

    fn provider(&self) -> Provider {
        let organization = self.raw(ENTITY) == b"2";
        let of = |kind: bool, c: usize| if kind { self.text(c) } else { String::new() };
        Provider {
            npi: self.text(NPI),
            organization: of(organization, ORGANIZATION),
            business_name: of(
                organization && self.raw(OTHER_TYPE) == BUSINESS_NAME,
                OTHER_ORGANIZATION,
            ),
            last: of(!organization, LAST),
            first: of(!organization, FIRST),
            middle: of(!organization, MIDDLE),
            other_last: of(!organization, OTHER_LAST),
            other_first: of(!organization, OTHER_FIRST),
            credential: of(!organization, CREDENTIAL),
            city: self.text(CITY),
            state: self.text(STATE).to_ascii_uppercase(),
            phone: self.text(PHONE),
            taxonomy: (0..TAXONOMIES)
                .find(|k| self.raw(PRIMARY + 4 * k) == b"Y")
                .map(|k| self.text(TAXONOMY + 4 * k))
                .unwrap_or_else(|| self.text(TAXONOMY)),
        }
    }
}

/// How many rows the provider file has, and those `wanted` needs, one JSON
/// object a line.
fn read_rows(r: impl Read, wanted: &Wanted) -> Result<(u64, u64, Vec<u8>), String> {
    let mut r = BufReader::with_capacity(1 << 20, r);
    let mut row = Row::default();
    if !row.next(&mut r)? {
        return Err("its provider file is empty".into());
    }
    if COLUMNS.iter().any(|(c, name)| row.text(*c) != *name) {
        return Err("its columns are not the ones medrec reads".into());
    }
    let columns = row.at.len();
    let (mut rows, mut kept, mut lines) = (0u64, 0u64, Vec::new());
    while row.next(&mut r)? {
        rows += 1;
        if row.at.len() != columns {
            return Err(format!("row {rows} has another number of columns"));
        }
        if !std::str::from_utf8(row.raw(NPI)).is_ok_and(npi_holds) {
            return Err(format!("row {rows} has an NPI whose check digit is wrong"));
        }
        if wanted.wants(&row) {
            serde_json::to_writer(&mut lines, &row.provider()).map_err(|e| e.to_string())?;
            lines.push(b'\n');
            kept += 1;
        }
    }
    Ok((rows, kept, lines))
}

/// A kept file of these rows, as [`fetch`] writes one.
#[cfg(test)]
pub(crate) fn keep_rows(path: &Path, date: &str, wanted: &Wanted, rows: &str) {
    let about = Kept {
        schema: SCHEMA.into(),
        url: "https://download.cms.gov/nppes/NPPES_Data_Dissemination_March_2026_V2.zip".into(),
        month: date[..7].into(),
        date: date.into(),
        bytes: 1,
        sha256: sha256_hex(b""),
        rows: 1,
        fetched_at: format!("{date}T12:00:00Z"),
        wanted_sha256: wanted.sha256(),
        npis: wanted.npis.clone(),
        states: wanted.states.clone(),
        people: wanted.people.clone(),
        kept: rows.lines().count() as u64,
        kept_sha256: sha256_hex(rows.as_bytes()),
    };
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let first = serde_json::to_string(&about).unwrap();
    std::fs::write(path, format!("{first}\n{rows}")).unwrap();
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::net::TcpListener;

    use super::*;

    /// CMS's columns and invented providers.
    const ROWS: &str = include_str!("../tests/fixtures/nppes/npidata.csv");
    const KEPT_V1: &str = include_str!("../tests/fixtures/nppes/kept-v1.jsonl");
    const PAGE_HTML: &str = include_str!("../tests/fixtures/nppes/page.html");

    /// How a zip's descriptors give their sizes.
    #[derive(Clone, Copy)]
    enum Sizes {
        Four,
        Eight,
    }

    /// A zip as CMS writes it: every file deflated, its checksum and sizes
    /// after it.
    fn zip(files: &[(&str, &[u8])], sizes: Sizes) -> Vec<u8> {
        let mut z = Vec::new();
        for (name, data) in files {
            let mut enc =
                flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::fast());
            enc.write_all(data).unwrap();
            let packed = enc.finish().unwrap();
            let mut crc = flate2::Crc::new();
            crc.update(data);
            z.extend(b"PK\x03\x04\x14\0\x08\x08\x08\0\0\0\0\0");
            z.extend([0u8; 12]);
            z.extend((name.len() as u16).to_le_bytes());
            z.extend(0u16.to_le_bytes());
            z.extend(name.as_bytes());
            z.extend(&packed);
            z.extend(DESCRIPTOR);
            z.extend(crc.sum().to_le_bytes());
            match sizes {
                Sizes::Four => {
                    z.extend((packed.len() as u32).to_le_bytes());
                    z.extend((data.len() as u32).to_le_bytes());
                }
                Sizes::Eight => {
                    z.extend((packed.len() as u64).to_le_bytes());
                    z.extend((data.len() as u64).to_le_bytes());
                }
            }
        }
        z.extend(CENTRAL);
        z.extend([0u8; 42]);
        z.extend(b"PK\x05\x06");
        z.extend([0u8; 18]);
        z
    }

    const DATA: &str = "npidata_pfile_20050523-20260302.csv";

    fn whole(rows: &str) -> Vec<u8> {
        zip(
            &[
                ("endpoint_pfile_20050523-20260302.csv", b"a,b\n1,2\n"),
                (DATA, rows.as_bytes()),
                ("npidata_pfile_20050523-20260302_fileheader.csv", b"NPI\n"),
            ],
            Sizes::Four,
        )
    }

    fn wanted(npis: &[&str], states: &[&str], printed: &[&str]) -> Wanted {
        Wanted {
            npis: npis.iter().map(|s| s.to_string()).collect(),
            states: states.iter().map(|s| s.to_string()).collect(),
            people: printed.iter().flat_map(|p| people(p)).collect(),
        }
    }

    fn read(z: &[u8], min: u64, w: &Wanted) -> Result<(Kept, Vec<Provider>), String> {
        let (about, lines) = read_zip(z, z.len() as u64, min, w, soon(), &mut |_| {})?;
        let providers = lines
            .split(|b| *b == b'\n')
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_slice(l).unwrap())
            .collect();
        Ok((about, providers))
    }

    fn soon() -> Instant {
        Instant::now() + Duration::from_secs(60)
    }

    fn kept(w: &Wanted) -> Vec<String> {
        let (about, providers) = read(&whole(ROWS), 10, w).unwrap();
        assert_eq!(about.rows, 10);
        assert_eq!(about.kept, providers.len() as u64);
        providers.into_iter().map(|p| p.npi).collect()
    }

    #[test]
    fn only_what_the_records_need_is_kept() {
        // By NPI, whatever the state.
        assert_eq!(
            kept(&wanted(&["0000000014", "0000000048"], &[], &[])),
            ["0000000014", "0000000048"]
        );
        // The organizations of a state, and no person.
        assert_eq!(
            kept(&wanted(&[], &["FL"], &[])),
            ["0000000022", "0000000097"]
        );
        // The people of a state a printed name can be.
        assert_eq!(
            kept(&wanted(&[], &["FL"], &["Jordan Sample, MD"])),
            ["0000000006", "0000000022", "0000000097"]
        );
        // A former name counts, and so do an apostrophe and a hyphen.
        assert_eq!(
            kept(&wanted(
                &[],
                &["FL"],
                &["SAMPLE, RILEY", "Riley O'Example-Sample"]
            )),
            ["0000000022", "0000000063", "0000000071", "0000000097"]
        );
        // A printed name in no state keeps no one, and a deactivated number
        // has nothing to keep.
        assert!(kept(&wanted(&["0000000089"], &[], &["Jordan Sample"])).is_empty());
    }

    #[test]
    fn a_provider_is_kept_with_its_name_place_and_primary_code() {
        let w = wanted(&["0000000014", "0000000030", "0000000097"], &[], &[]);
        let (about, p) = read(&whole(ROWS), 1, &w).unwrap();
        assert_eq!(about.date, "2026-03-02");
        assert_eq!(about.sha256, sha256_hex(&whole(ROWS)));
        assert_eq!(
            p[0],
            Provider {
                npi: "0000000014".into(),
                last: "EXAMPLE".into(),
                first: "CASEY".into(),
                credential: "ARNP".into(),
                city: "EXAMPLETON".into(),
                state: "TX".into(),
                phone: "5550100102".into(),
                taxonomy: "363LF0000X".into(),
                ..Provider::default()
            }
        );
        assert_eq!(p[1].organization, "SAMPLE \"SUNRISE\" CLINIC, INC.");
        assert_eq!(p[2].organization, "EXAMPLE\nTWO LINES CLINIC");
    }

    #[test]
    fn a_file_that_is_not_whole_or_not_the_layout_is_refused() {
        let w = wanted(&["0000000006"], &["FL"], &[]);
        let refused = |z: &[u8], min: u64, why: &str| {
            let e = read(z, min, &w).unwrap_err();
            assert!(e.contains(why), "{e}");
        };
        let z = whole(ROWS);
        assert!(read(&z, 10, &w).is_ok());
        refused(&z, 11, "at least 11");
        refused(
            &whole(&ROWS.replace("0000000048", "0000000049")),
            1,
            "check digit",
        );
        refused(
            &whole(&ROWS.replace("\"Entity Type Code\"", "\"Entity\"")),
            1,
            "columns",
        );
        refused(
            &whole(&ROWS.replace("\"PLACEHOLDER LAB\"", "\"PLACEHOLDER\",\"LAB\"")),
            1,
            "number of columns",
        );
        refused(&whole(""), 1, "empty");
        refused(&whole(ROWS.trim_end_matches("\"\n")), 1, "cut short");
        refused(
            &zip(&[("other.csv", b"x")], Sizes::Four),
            1,
            "no provider file",
        );
        let two = zip(
            &[(DATA, ROWS.as_bytes()), (DATA, ROWS.as_bytes())],
            Sizes::Four,
        );
        refused(&two, 1, "two provider files");
        refused(b"not a zip at all", 1, "not a zip");

        // Every way of ending early, and bytes after the size offered.
        for cut in (0..z.len()).step_by(97) {
            assert!(read(&z[..cut], 1, &w).is_err(), "{cut}");
        }
        let mut longer = z.clone();
        longer.push(0);
        let size = z.len() as u64;
        assert!(read_zip(&longer[..], size, 1, &w, soon(), &mut |_| {}).is_err());
        assert!(read_zip(&z[..], size + 1, 1, &w, soon(), &mut |_| {}).is_err());

        // A download past its time.
        let past = Instant::now() - Duration::from_secs(1);
        let e = read_zip(&z[..], size, 1, &w, past, &mut |_| {}).unwrap_err();
        assert!(e.contains("too long"), "{e}");

        // A checksum that is not its file's.
        let at = z
            .windows(4)
            .enumerate()
            .filter(|(_, b)| *b == DESCRIPTOR)
            .nth(1)
            .unwrap()
            .0;
        let mut bad = z.clone();
        bad[at + 4] ^= 1;
        refused(&bad, 1, "checksum");
        let mut bad = z.clone();
        bad[at + 12] ^= 1;
        refused(&bad, 1, "checksum or size");
    }

    #[test]
    fn a_descriptor_gives_its_sizes_in_four_bytes_or_eight() {
        let w = wanted(&["0000000006"], &[], &[]);
        let z = zip(
            &[("a.csv", b"x"), (DATA, ROWS.as_bytes()), ("b.csv", b"y")],
            Sizes::Eight,
        );
        assert_eq!(read(&z, 10, &w).unwrap().1.len(), 1);
    }

    #[test]
    fn the_newest_monthly_file_is_the_one_offered() {
        assert_eq!(
            monthly(PAGE_HTML),
            Some((
                "2026-03".into(),
                "NPPES_Data_Dissemination_March_2026_V2.zip".into()
            ))
        );
        assert_eq!(
            monthly("<a href='./NPPES_Deactivated_NPI_Report_030926_V2.zip'>"),
            None
        );
        assert_eq!(monthly(""), None);
    }

    #[test]
    fn a_printed_name_gives_the_surnames_it_can_be_of() {
        let of = |s: &str| -> Vec<String> {
            people(s)
                .into_iter()
                .map(|(l, i)| format!("{l} {i}"))
                .collect()
        };
        assert!(of("Jordan A. Sample, MD").contains(&"SAMPLE J".to_string()));
        assert!(of("SAMPLE, JORDAN A").contains(&"SAMPLE J".to_string()));
        assert!(of("SAMPLE JORDAN A").contains(&"SAMPLE J".to_string()));
        assert!(of("Dr. Riley O'Example-Sample PA-C").contains(&"OEXAMPLESAMPLE R".to_string()));
        assert!(of("Casey de la Example, ARNP").contains(&"EXAMPLE C".to_string()));
        assert!(of("José Muñoz").contains(&"MUNOZ J".to_string()));
        assert!(of("Jordan Sample, Example Primary Care").contains(&"SAMPLE J".to_string()));
        assert!(of("").is_empty());
        assert!(of("MD").is_empty());
        assert!(of("Sample").is_empty());
        assert!(of("The Example Center for Invented Care of Samples").is_empty());
    }

    /// What the records need: an NPI names a provider, and a name printed
    /// with none is looked for.
    #[test]
    fn what_the_records_need_is_read_from_every_person() {
        let idx: PersonIndex = serde_json::from_value(serde_json::json!({
            "schema": "medrec-index/v1", "person": "p_aaaaaaaaaaaaaaaaaaaa", "rev": 1,
            "updated_at": "2026-03-05T12:00:00Z",
            "documents": [], "studies": [], "review": [],
            "records": [
                {
                    "id": "r_aaaaaaaaaaaaaaaaaaaa", "kind": "encounter", "key": "k",
                    "display": "Office visit", "provenance": [],
                    "fields": {"practitioner": "Jordan Sample", "location": "Example Annex"},
                    "providers": [
                        {"field": "practitioner", "npi": "0000000006", "state": "FL"},
                        {"field": "location", "state": "TX"}
                    ]
                },
                {
                    "id": "r_bbbbbbbbbbbbbbbbbbbb", "kind": "procedure", "key": "k2",
                    "display": "Skin check", "provenance": [],
                    "fields": {"performer": "EXAMPLE, CASEY"}
                }
            ]
        }))
        .unwrap();
        let w = Wanted::of([&idx]);
        assert_eq!(w.npis, BTreeSet::from(["0000000006".to_string()]));
        assert_eq!(
            w.states,
            BTreeSet::from(["FL".to_string(), "TX".to_string()])
        );
        assert!(w.people.contains(&("EXAMPLE".into(), 'C')));
        assert!(!w.people.contains(&("SAMPLE".into(), 'J')));
    }

    /// Compared from one run to the next to tell whether the records need
    /// other rows, so it is the same in every build.
    #[test]
    fn what_is_wanted_has_one_hash() {
        let w = Wanted {
            npis: BTreeSet::from(["0000000006".into(), "0000000014".into()]),
            states: BTreeSet::from(["FL".into()]),
            people: BTreeSet::from([("SAMPLE".into(), 'J')]),
        };
        assert_eq!(
            w.sha256(),
            sha256_hex(b"npi 0000000006\nnpi 0000000014\nstate FL\nperson J SAMPLE\n")
        );
        assert_eq!(Wanted::default().sha256(), sha256_hex(b""));
    }

    /// CMS's server: the page, and the file's size and bytes.
    fn serve(page: &'static str, file: Vec<u8>) -> String {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}/nppes/", l.local_addr().unwrap());
        std::thread::spawn(move || {
            for s in l.incoming() {
                let mut s = s.unwrap();
                let mut line = String::new();
                let mut r = BufReader::new(s.try_clone().unwrap());
                r.read_line(&mut line).unwrap();
                let mut rest = String::new();
                while r.read_line(&mut rest).unwrap() > 2 {
                    rest.clear();
                }
                let path = line.split_whitespace().nth(1).unwrap_or("").to_string();
                let (status, body): (&str, &[u8]) = if path.ends_with(PAGE) {
                    ("200 OK", page.as_bytes())
                } else if path.ends_with("_March_2026_V2.zip") {
                    ("200 OK", &file)
                } else {
                    ("404 Not Found", b"")
                };
                let _ = s.write_all(
                    format!(
                        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .as_bytes(),
                );
                if !line.starts_with("HEAD") {
                    let _ = s.write_all(body);
                }
            }
        });
        base
    }

    #[test]
    fn a_file_is_kept_for_its_folder_and_one_that_fails_leaves_the_last() {
        let dir = std::env::temp_dir().join(format!("medrec-nppes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = path(&dir, "f_aaaaaaaaaaaaaaaaaaaa");
        let w = wanted(&["0000000006"], &["FL"], &[]);
        assert_eq!(Directory::load(&path).unwrap_err(), Unused::Absent);
        assert!(list(&dir).is_empty());

        let file = whole(ROWS);
        let base = serve(PAGE_HTML, file.clone());
        let source = Source {
            base: &base,
            min_rows: 10,
        };
        let offer_ = offer(&source).unwrap();
        assert_eq!(
            (offer_.month.as_str(), offer_.bytes),
            ("2026-03", file.len() as u64)
        );
        let mut seen = 0;
        let f = fetch(&source, &offer_, &w, &path, &mut |n| seen = n).unwrap();
        assert_eq!((f.status, f.rows, f.kept), ("downloaded", 10, 3));
        assert_eq!(seen, file.len() as u64);
        let d = Directory::load(&path).unwrap();
        assert_eq!(d.providers.len(), 3);
        assert_eq!(d.about.sha256, sha256_hex(&file));
        assert!(d.about.is_current(&offer_, &w));
        assert!(!d.about.is_current(&offer_, &Wanted::default()));
        let folder = "f_aaaaaaaaaaaaaaaaaaaa".to_string();
        assert_eq!(list(&dir), [(folder.clone(), Ok(d.about.clone()))]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        // Too few rows, a page that lists nothing, a size that is not the
        // file's: nothing changes.
        let few = Source {
            base: &base,
            min_rows: 11,
        };
        assert!(matches!(
            fetch(&few, &offer_, &w, &path, &mut |_| {}),
            Err(CliError::Upstream(_))
        ));
        let larger = Offer {
            bytes: offer_.bytes + 1,
            ..offer_.clone()
        };
        assert!(fetch(&source, &larger, &w, &path, &mut |_| {}).is_err());
        let empty = serve("<html></html>", Vec::new());
        assert!(matches!(
            offer(&Source {
                base: &empty,
                min_rows: 1
            }),
            Err(CliError::Upstream(_))
        ));
        assert_eq!(Directory::load(&path).unwrap().about, d.about);

        // A kept file that was changed is not used.
        let mut changed = std::fs::read(&path).unwrap();
        let last = changed.len() - 3;
        changed[last] ^= 1;
        std::fs::write(&path, changed).unwrap();
        assert_eq!(Directory::load(&path).unwrap_err(), Unused::Changed);
        assert_eq!(list(&dir), [(folder.clone(), Err(Unused::Changed))]);

        // One 0.9.6 kept is not one that was changed, and a fetch replaces it.
        std::fs::write(&path, KEPT_V1).unwrap();
        assert_eq!(Directory::load(&path).unwrap_err(), Unused::OtherVersion);
        assert_eq!(list(&dir), [(folder.clone(), Err(Unused::OtherVersion))]);
        fetch(&source, &offer_, &w, &path, &mut |_| {}).unwrap();
        assert_eq!(list(&dir)[0].1.as_ref().unwrap().kept, 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cms_is_asked_over_https_and_a_whole_file_is_expected() {
        assert_eq!(CMS.base, "https://download.cms.gov/nppes/");
        assert_eq!(CMS.min_rows, 9_000_000);
        for (i, (c, _)) in COLUMNS.iter().enumerate() {
            assert!(COLUMNS[..i].iter().all(|(d, _)| d != c));
        }
    }

    /// The published file, read from a copy on this machine: `MEDREC_NPPES`
    /// is its path.
    #[test]
    #[ignore = "needs the published file"]
    fn the_published_file_reads() {
        let p = std::env::var("MEDREC_NPPES").unwrap();
        let f = std::fs::File::open(&p).unwrap();
        let size = f.metadata().unwrap().len();
        let started = Instant::now();
        let (about, _) = read_zip(
            f,
            size,
            CMS.min_rows,
            &wanted(&[], &["WY"], &[]),
            soon(),
            &mut |_| {},
        )
        .unwrap();
        eprintln!(
            "{} rows, {} kept, {} bytes, through {}, sha256 {}, {:?}",
            about.rows,
            about.kept,
            about.bytes,
            about.date,
            about.sha256,
            started.elapsed()
        );
    }
}
