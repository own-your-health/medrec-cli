//! Published code tables, kept on this machine: the plain names of
//! diagnosis codes.
//!
//! medrec ships none of them. `medrec tables fetch` downloads each
//! publisher's whole file from the address below, keeps it only when its
//! SHA-256 is the one recorded here, and nothing is ever looked up by code
//! over the network. Without a table, or with a file whose hash is not the
//! recorded one, a code has no name and the source's own text stands.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use pk_cli_core::{private_file, CliError};
use serde::Serialize;

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
    /// The file inside the zip, and how many codes it has.
    pub member: &'static str,
    pub member_bytes: u64,
    pub rows: usize,
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
    member: "Code Descriptions/icd10cm_order_2027.txt",
    member_bytes: 14_724_229,
    rows: 98_403,
};

pub const TABLES: &[&Table] = &[&ICD10CM];

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
}

#[derive(Debug, Serialize)]
pub struct Row {
    pub id: &'static str,
    pub title: &'static str,
    pub publisher: &'static str,
    pub version: String,
    pub date: String,
    pub state: State,
    pub bytes: u64,
    pub rows: u64,
    pub sha256: String,
    pub url: String,
    /// The rows kept for a records folder, of a table read for one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kept: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct Fetched {
    pub id: &'static str,
    pub version: &'static str,
    /// `unchanged`, `downloaded` or `copied`.
    pub status: &'static str,
    pub bytes: u64,
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
        dir.join(format!("{}-{}.zip", self.id, self.version))
    }

    /// The file's bytes when its hash is the recorded one.
    fn verified(&self, dir: &Path) -> (State, Option<Vec<u8>>) {
        let path = self.path(dir);
        match std::fs::metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (State::Absent, None),
            Ok(m) if m.len() == self.bytes => {}
            _ => return (State::Changed, None),
        }
        match std::fs::read(&path) {
            Ok(b) if sha256_hex(&b) == self.sha256 => (State::Ready, Some(b)),
            _ => (State::Changed, None),
        }
    }

    /// The table's file, decompressed, when it is on this machine and whole.
    fn read(&self, dir: &Path) -> Option<Vec<u8>> {
        member(&self.verified(dir).1?, self.member, self.member_bytes)
    }
}

/// Every table, and the provider directory once for each records folder it
/// was read for.
pub fn list() -> Vec<Row> {
    let dir = dir();
    let mut rows: Vec<Row> = TABLES
        .iter()
        .map(|t| Row {
            id: t.id,
            title: t.title,
            publisher: t.publisher,
            version: t.version.into(),
            date: t.date.into(),
            state: dir.as_deref().map_or(State::Absent, |d| t.verified(d).0),
            bytes: t.bytes,
            rows: t.rows as u64,
            sha256: t.sha256.into(),
            url: t.url.into(),
            kept: None,
        })
        .collect();
    let mut kept = dir.as_deref().map(nppes::list).unwrap_or_default();
    if kept.is_empty() {
        rows.push(directory_row(None, State::Absent));
    }
    for k in kept.drain(..) {
        let state = if k.is_some() {
            State::Ready
        } else {
            State::Changed
        };
        rows.push(directory_row(k, state));
    }
    rows
}

fn directory_row(k: Option<nppes::Kept>, state: State) -> Row {
    Row {
        id: nppes::ID,
        title: nppes::TITLE,
        publisher: nppes::PUBLISHER,
        version: k.as_ref().map(|k| k.month.clone()).unwrap_or_default(),
        date: k.as_ref().map(|k| k.date.clone()).unwrap_or_default(),
        state,
        bytes: k.as_ref().map_or(0, |k| k.bytes),
        rows: k.as_ref().map_or(0, |k| k.rows),
        sha256: k.as_ref().map(|k| k.sha256.clone()).unwrap_or_default(),
        kept: k.as_ref().map(|k| k.kept),
        url: k.map_or_else(|| nppes::CMS.base.into(), |k| k.url),
    }
}

/// Keep `t` on this machine: from its publisher, or from a file someone
/// downloaded. Nothing is kept unless it is the recorded file.
pub fn fetch(t: &Table, dir: &Path, from: Option<&Path>) -> Result<Fetched, CliError> {
    if from.is_none() && t.verified(dir).0 == State::Ready {
        return Ok(Fetched {
            id: t.id,
            version: t.version,
            status: "unchanged",
            bytes: t.bytes,
        });
    }
    let (bytes, status) = match from {
        Some(p) => {
            let f = std::fs::File::open(p)
                .map_err(|e| CliError::Usage(format!("{}: {e}", p.display())))?;
            (whole(t, f).map_err(CliError::Usage)?, "copied")
        }
        None => (download(t)?, "downloaded"),
    };
    let file = member(&bytes, t.member, t.member_bytes)
        .ok_or_else(|| CliError::Other(format!("{} {} cannot be unpacked", t.id, t.version)))?;
    if icd_rows(&file).count() != t.rows {
        return Err(CliError::Other(format!(
            "{} {} does not have the rows medrec expects",
            t.id, t.version
        )));
    }
    private_file::create_dir_all(dir)
        .and_then(|()| private_file::write(&t.path(dir), &bytes, private_file::Replace::Always))
        .map_err(|e| CliError::Other(format!("keeping {}: {e}", t.id)))?;
    Ok(Fetched {
        id: t.id,
        version: t.version,
        status,
        bytes: t.bytes,
    })
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

/// Everything `r` has, when it is the recorded file.
fn whole(t: &Table, r: impl Read) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::with_capacity(t.bytes as usize);
    r.take(t.bytes + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("reading {}: {e}", t.id))?;
    if bytes.len() as u64 != t.bytes || sha256_hex(&bytes) != t.sha256 {
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

/// One file of a zip, of exactly `size` bytes: stored or deflated, found
/// through the central directory.
fn member(zip: &[u8], name: &str, size: u64) -> Option<Vec<u8>> {
    let end = (0..=zip.len().checked_sub(22)?)
        .rev()
        .take(66_000)
        .find(|at| zip[*at..].starts_with(b"PK\x05\x06"))?;
    let mut at = u32_at(zip, end + 16)?;
    for _ in 0..u16_at(zip, end + 10)? {
        if !zip.get(at..)?.starts_with(b"PK\x01\x02") {
            return None;
        }
        let (method, packed) = (u16_at(zip, at + 10)?, u32_at(zip, at + 20)?);
        let (n, x, c) = (
            u16_at(zip, at + 28)?,
            u16_at(zip, at + 30)?,
            u16_at(zip, at + 32)?,
        );
        if zip.get(at + 46..at + 46 + n)? == name.as_bytes() {
            let local = u32_at(zip, at + 42)?;
            if !zip.get(local..)?.starts_with(b"PK\x03\x04") {
                return None;
            }
            let start = local + 30 + u16_at(zip, local + 26)? + u16_at(zip, local + 28)?;
            let data = zip.get(start..start.checked_add(packed)?)?;
            let mut out = Vec::new();
            match method {
                0 => out.extend_from_slice(data),
                8 => {
                    flate2::read::DeflateDecoder::new(data)
                        .take(size + 1)
                        .read_to_end(&mut out)
                        .ok()?;
                }
                _ => return None,
            }
            return (out.len() as u64 == size).then_some(out);
        }
        at += 46 + n + x + c;
    }
    None
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
            member: "o.txt",
            member_bytes: data.len() as u64,
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
            fetch(&t, &local, Some(&src)),
            Err(CliError::Usage(_))
        ));
        assert!(!local.exists());

        std::fs::write(&src, &z).unwrap();
        assert_eq!(fetch(&t, &local, Some(&src)).unwrap().status, "copied");
        assert_eq!(t.verified(&local).0, State::Ready);
        assert_eq!(fetch(&t, &local, None).unwrap().status, "unchanged");
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
        let serve = |status: &'static str, body: Vec<u8>| -> &'static str {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}/t.zip", l.local_addr().unwrap());
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
        };
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

    #[test]
    fn the_recorded_tables_are_whole_files_over_https() {
        for t in TABLES {
            assert!(t.url.starts_with("https://"), "{}", t.id);
            assert!(t.sha256.len() == 64 && t.sha256.bytes().all(|b| b.is_ascii_hexdigit()));
            assert!(t.bytes > 0 && t.rows > 0 && !t.url.contains('?'));
        }
    }
}
