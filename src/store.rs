//! The one seam between the records folder and where its bytes live.
//!
//! Every operation goes through [`Store`], which moves files under
//! folder-relative paths. It is backed by `pk-cli-drive`: a folder in Google
//! Drive reached through medrec's own sign-in (canonical, behind the read
//! cache; `medrec drive`), or a local directory (a test folder, a disk).
//! medrec runs no rclone.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use pk_cli_auth::vault::{KeyStore, Vault};
use pk_cli_secrets::{AskedRead, CredentialStore, QuietRead, Secret};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use md5::{Digest, Md5};
use serde::Serialize;

use pk_cli_core::CliError;
use pk_cli_drive::google::{self as gdrive, Drive, DriveCli, DriveSpec, Scope};
use pk_cli_drive::{
    cache, Backend, CachePolicy, CachedRemote, FileEntry, LocalCopy, RemoteRead, RemoteStore,
};

pub const BIN: &str = "medrec";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// How medrec mounts the Drive commands, and the flag that names a Drive
/// profile on them (`--profile` is a records profile), for remedies.
pub const DRIVE_COMMAND: &str = "medrec drive";
pub const DRIVE_PROFILE_FLAG: &str = "--drive-profile";

/// How a command reaches the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Location {
    /// `gdrive:<drive profile>/<folder id>[/<path>]`: a folder in one Google
    /// account, through medrec's own sign-in.
    Drive(DriveSpec),
    /// A local directory.
    Dir(PathBuf),
}

impl Location {
    /// `gdrive:<profile>/<folder id>[/<path>]` is a Drive folder; anything
    /// else (absolute, `./x`, `~/x`) is a directory. Any other `name:path`
    /// is an rclone remote, which medrec no longer reads: exit 2 naming the
    /// move. A single-letter name before a colon is never taken as a
    /// remote, so a Windows-style `C:\x` is not misread.
    pub fn parse(s: &str) -> Result<Location, CliError> {
        let s = s.trim();
        if DriveSpec::is_spec(s) {
            return DriveSpec::parse(s)
                .map(Location::Drive)
                .map_err(|e| e.with_remedy(MOVE_OFF_RCLONE));
        }
        if let Some((name, _)) = s.split_once(':') {
            if name.len() > 1 && !name.contains('/') && !s.starts_with('.') && !s.starts_with('~') {
                return Err(CliError::Usage(format!(
                    "`{s}` is an rclone remote, and medrec no longer uses rclone"
                ))
                .with_remedy(MOVE_OFF_RCLONE));
            }
        }
        Ok(Location::Dir(expand_home(s)))
    }

    pub fn display(&self) -> String {
        match self {
            Location::Drive(spec) => spec.to_string(),
            Location::Dir(d) => d.display().to_string(),
        }
    }

    /// The location of `rel` inside this one (the inbox of a records folder).
    pub fn join(&self, rel: &str) -> Result<Location, CliError> {
        match self {
            Location::Drive(spec) => spec.join(rel).map(Location::Drive),
            Location::Dir(d) => Ok(Location::Dir(d.join(rel))),
        }
    }
}

/// The remedy for a folder named the rclone way.
pub const MOVE_OFF_RCLONE: &str = "sign a Drive profile in with `medrec drive login \
     --drive-profile <NAME>`, then name the folder `gdrive:<NAME>/<folder id>` (README, \
     \"Google Drive\")";

/// medrec's Drive sign-ins (`medrec drive …`): its own OAuth client and
/// tokens in its keychain item, `drive.file` unless a login asks for more.
pub fn drive_cli() -> Result<DriveCli, CliError> {
    #[cfg(test)]
    if let Some(cli) = testing::cli() {
        return Ok(cli);
    }
    Ok(DriveCli::new(BIN, VERSION, DRIVE_COMMAND)?
        .with_vault(shared_vault()?)
        .profile_flag(DRIVE_PROFILE_FLAG)
        .default_scopes(&[Scope::File]))
}

/// One vault for the process. A vault reads its key from the Keychain once
/// and keeps it, so a run that opens its folder more than once (a refresh
/// pass, the MCP server) asks macOS at most once, whether the person chose
/// Allow or Always Allow. `Vault::load_key` on it bounds that read
/// ([`crate::refresh::signin::read_drive`]), and `Vault::load_key_quietly`
/// makes it without a dialog (a scheduled pass).
pub fn shared_vault() -> Result<Arc<Vault>, CliError> {
    static VAULT: Mutex<Option<Arc<Vault>>> = Mutex::new(None);
    let mut held = VAULT.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(v) = held.as_ref() {
        return Ok(Arc::clone(v));
    }
    let mut v = Vault::new(
        Keys(pk_cli_auth::vault::keychain_for(BIN)),
        pk_cli_auth::vault::default_dir(BIN)?,
    );
    if let Some(wait) = KEY_READ_BOUND.get() {
        v = v.bound_key_reads(*wait);
    }
    let v = Arc::new(v);
    *held = Some(Arc::clone(&v));
    Ok(v)
}

static KEY_READ_BOUND: std::sync::OnceLock<Duration> = std::sync::OnceLock::new();

/// Hold every Keychain read of the vault's key in this process to `wait`
/// (`Vault::bound_key_reads`): for a process nobody may be watching, the
/// MCP server and a scheduled pass. Call it before the first Drive read;
/// a later call changes nothing. A read past it is exit 3 naming the
/// dialog, and the vault's other calls fail at once until it is answered,
/// rather than wait behind it. A command at the terminal waits for the
/// person's answer.
pub fn bound_key_reads(wait: Duration) {
    let _ = KEY_READ_BOUND.set(wait);
}

/// A test hook's value: the variable `name` in a debug build, which is what
/// `cargo test` runs; never in a release build, so a stray variable cannot
/// reach a shipped binary's Keychain or timing.
pub(crate) fn test_hook(name: &str) -> Option<std::ffi::OsString> {
    if cfg!(debug_assertions) {
        std::env::var_os(name).filter(|v| !v.is_empty())
    } else {
        None
    }
}

/// medrec's Keychain item, with two hooks for its tests, read in debug
/// builds only ([`test_hook`]). `MEDREC_TEST_KEYCHAIN_LOG` makes it a
/// keychain holding no key, which writes a line to that file for each read
/// (`read` for one that may ask, `quiet` for one that may not) and never
/// reaches the real Keychain. `MEDREC_TEST_KEYCHAIN_DELAY_SECS` makes it one
/// that asks: a quiet read is answered that a person is needed, and the
/// process's first other read is held that long, as a dialog holds it until
/// someone answers.
struct Keys(CredentialStore);

impl Keys {
    fn test_log() -> Option<std::ffi::OsString> {
        test_hook("MEDREC_TEST_KEYCHAIN_LOG")
    }

    fn test_read(log: std::ffi::OsString, line: &str) {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log)
        {
            let _ = writeln!(f, "{line}");
        }
    }

    fn test_delay() -> Option<u64> {
        test_hook("MEDREC_TEST_KEYCHAIN_DELAY_SECS").and_then(|s| s.to_str()?.parse().ok())
    }
}

impl KeyStore for Keys {
    fn get(&self, account: &str) -> Result<Option<Secret>, CliError> {
        let Some(log) = Self::test_log() else {
            return self.0.get(account);
        };
        static DELAYED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        Self::test_read(log, "read");
        if let Some(secs) = Self::test_delay() {
            if !DELAYED.swap(true, std::sync::atomic::Ordering::SeqCst) {
                std::thread::sleep(Duration::from_secs(secs));
            }
        }
        Ok(None)
    }

    fn set(&self, account: &str, value: &Secret) -> Result<(), CliError> {
        if Self::test_log().is_some() {
            return Err(CliError::Keychain("a test's keychain takes no key".into()));
        }
        self.0.set(account, value)
    }

    fn delete(&self, account: &str) -> Result<bool, CliError> {
        if Self::test_log().is_some() {
            return Ok(false);
        }
        self.0.delete(account)
    }

    fn get_quiet(&self, account: &str) -> Result<QuietRead, CliError> {
        let Some(log) = Self::test_log() else {
            return self.0.get_quiet(account);
        };
        Self::test_read(log, "quiet");
        Ok(match Self::test_delay() {
            Some(_) => QuietRead::NeedsPerson,
            None => QuietRead::Absent,
        })
    }

    fn get_asking(&self, account: &str) -> Result<AskedRead, CliError> {
        if Self::test_log().is_none() {
            return self.0.get_asking(account);
        }
        Ok(match self.get(account)? {
            Some(item) => AskedRead::Found(item),
            None => AskedRead::Absent,
        })
    }

    fn keychain_service(&self) -> Option<&str> {
        Some(self.0.service())
    }
}

/// The Drive store at `spec`, every request bounded by `budget`.
fn drive(spec: &DriveSpec, budget: Duration) -> Result<Drive, CliError> {
    Ok(drive_cli()?
        .drive(spec)?
        .temp_prefix(BIN)
        .with_budget(budget))
}

/// The read cache's directory for `spec`; none in a unit test.
fn cache_dir(spec: &DriveSpec) -> Option<PathBuf> {
    #[cfg(test)]
    if testing::cli().is_some() {
        return testing::cache_dir();
    }
    cache::cache_dir_for(BIN, &spec.to_string())
}

/// Whether `rel` is a relative path of plain names, with no root, `.` or `..`.
/// A path an index gives is one: anyone who can write the folder can edit an
/// index, so no such path may lead outside the place it is read in.
pub fn plain_rel(rel: &str) -> bool {
    !rel.is_empty()
        && !rel.contains('\\')
        && Path::new(rel)
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
}

/// Every path a [`Store`] is given stays in the folder: it has no root and no
/// `..`. A backslash, or any other character a file's name can hold, is the
/// name's own, so a file another program named is read, answered and removed
/// as it was before. The stricter [`plain_rel`] is for the paths an index
/// gives. The error names no path: it is one an edited file gave.
fn inside(rel: &str) -> Result<(), CliError> {
    use std::path::Component;
    let leaves = |c: Component| {
        matches!(
            c,
            Component::RootDir | Component::Prefix(_) | Component::ParentDir
        )
    };
    if !Path::new(rel).components().any(leaves) {
        Ok(())
    } else {
        Err(CliError::Other(
            "a path in the records folder's files leads outside it: an index or household.json may have been edited".into(),
        ))
    }
}

/// `rel` under `root`, where it is a [`plain_rel`]; `None` where it is not.
pub fn within(root: &Path, rel: &str) -> Option<PathBuf> {
    plain_rel(rel).then(|| root.join(rel))
}

#[cfg(test)]
mod rel_tests {
    use super::*;

    /// A name that holds a backslash is a file's own, in the store; a root
    /// or `..` is refused before anything is read.
    #[test]
    fn a_store_path_is_refused_only_where_it_leaves_the_folder() {
        let root = crate::testutil::temp_dir("store-inside");
        let dir = root.join("records");
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(
            &Location::Dir(dir.clone()),
            Reads::Fresh,
            Duration::from_secs(5),
        )
        .unwrap();
        store.write_text("inbox/a\\b.pdf", "x").unwrap();
        assert_eq!(
            store.read_text("inbox/a\\b.pdf").unwrap().as_deref(),
            Some("x")
        );
        assert!(store.delete("inbox/a\\b.pdf").unwrap());
        for rel in ["/etc/hosts", "../outside", "inbox/../../outside"] {
            let err = store.read_text(rel).unwrap_err();
            assert!(!err.to_string().contains(rel), "{err}");
            assert!(store.write_text(rel, "x").is_err(), "{rel}");
        }
        assert!(!root.join("outside").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn only_a_relative_path_of_plain_names_is_within() {
        let root = Path::new("/tmp/copy");
        for ok in ["a/b.json", "Robin Testperson/Records/x/Claim", "a//b"] {
            assert!(plain_rel(ok), "{ok}");
            assert!(
                within(root, ok).is_some_and(|p| p.starts_with(root)),
                "{ok}"
            );
        }
        for not in ["", "/a/b.json", "../a", "a/../b", "./a", "a\\b", "/"] {
            assert!(!plain_rel(not), "{not}");
            assert!(within(root, not).is_none(), "{not}");
        }
    }
}

pub fn expand_home(s: &str) -> PathBuf {
    if let Some(rest) = s.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(s)
}

/// Whether reads may come from the per-machine cache. A command that will
/// rewrite what it reads asks for `Fresh`, so it never derives a write from
/// a copy another machine has since replaced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reads {
    Cached,
    Fresh,
}

/// Whether every read is of Drive itself (`--fresh`).
static FRESH: AtomicBool = AtomicBool::new(false);

/// Read every records folder from Drive from now on, and not from this
/// machine's copies.
pub fn read_fresh() {
    FRESH.store(true, Ordering::Relaxed);
}

fn fresh() -> bool {
    #[cfg(test)]
    if testing::fresh() {
        return true;
    }
    FRESH.load(Ordering::Relaxed)
}

/// Where what a command read of a records folder on Drive came from.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ReadFrom {
    /// `drive`, `cache`, or `both`.
    pub from: &'static str,
    /// Reads that Drive answered.
    pub drive: usize,
    /// Reads of this machine's copies.
    pub cache: usize,
    /// Of those, past the time a copy is taken as it is, or of no known age.
    pub stale: usize,
    /// Of those, the ones made because Drive could not be reached.
    pub unreachable: usize,
    /// The age of the oldest copy; `None` where none was read or one's is not
    /// known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oldest_seconds: Option<u64>,
}

thread_local! {
    static READ: RefCell<Option<ReadFrom>> = const { RefCell::new(None) };
}

/// Where what this thread read came from, of the folders on Drive it opened to
/// read. `None` where it opened none.
pub fn read_from() -> Option<ReadFrom> {
    READ.with(|r| r.borrow().clone())
}

pub struct Store {
    backend: Backend,
    location: Location,
    budget: Duration,
    /// What is counted of it already, of a folder on Drive opened to read.
    counted: Option<Mutex<pk_cli_drive::Reads>>,
}

/// The store budget for one command: every Drive request it makes is
/// bounded by the time left of this much from the command's start.
pub const DEFAULT_BUDGET: Duration = Duration::from_secs(1800);

impl Store {
    /// The store at `location`. A Drive folder whose profile is not signed
    /// in on this machine is exit 2 naming `medrec drive login`; nothing is
    /// read from Drive until the first operation.
    pub fn open(location: &Location, reads: Reads, budget: Duration) -> Result<Store, CliError> {
        let backend = match location {
            Location::Dir(d) => Backend::Mount(d.clone()),
            Location::Drive(spec) => {
                let remote = drive(spec, budget)?;
                match cache_dir(spec) {
                    None => Backend::Drive(remote),
                    Some(dir) => {
                        let mut policy = CachePolicy::from_env(BIN);
                        policy.bypass_reads |= reads == Reads::Fresh || fresh();
                        let mut cached = CachedRemote::new(remote, dir, policy);
                        if let Some(r) = cache::background_revalidator(
                            &policy,
                            &["cache", "sync"],
                            &spec.to_string(),
                        ) {
                            cached = cached.with_revalidator(r);
                        }
                        Backend::CachedDrive(cached)
                    }
                }
            }
        };
        let counts = matches!(location, Location::Drive(_)) && reads == Reads::Cached;
        if counts {
            READ.with(|r| {
                r.borrow_mut().get_or_insert_with(|| ReadFrom {
                    from: "drive",
                    ..ReadFrom::default()
                });
            });
        }
        Ok(Store {
            backend,
            location: location.clone(),
            budget,
            counted: counts.then(Mutex::default),
        })
    }

    /// Count a read of this store, and give what it gave. `past` is one that
    /// the read cache has no part in.
    fn read<T>(&self, past: bool, gave: Result<T, CliError>) -> Result<T, CliError> {
        let Some(counted) = &self.counted else {
            return gave;
        };
        let mut had = counted.lock().unwrap_or_else(|p| p.into_inner());
        let now = self.backend.reads();
        let direct = gave.is_ok() && (past || now.is_none());
        let now = now.unwrap_or_default();
        READ.with(|r| {
            let mut r = r.borrow_mut();
            let r = r.get_or_insert_with(ReadFrom::default);
            let known = r.cache == 0 || r.oldest_seconds.is_some();
            r.drive += usize::from(direct) + now.remote - had.remote;
            r.cache += now.cache - had.cache;
            r.stale += now.stale - had.stale;
            r.unreachable += now.unreachable - had.unreachable;
            let its = now.oldest.map(|age| age.as_secs());
            let all = known && (now.cache == 0 || its.is_some());
            r.oldest_seconds = r.oldest_seconds.max(its).filter(|_| all);
            r.from = match (r.cache, r.drive) {
                (0, _) => "drive",
                (_, 0) => "cache",
                _ => "both",
            };
        });
        *had = now;
        gave
    }

    /// The Drive store under a Drive location, uncached: listings with
    /// hashes, tree copies and space.
    fn remote(&self) -> &Drive {
        match &self.backend {
            Backend::Drive(d) => d,
            Backend::CachedDrive(c) => c.remote(),
            _ => unreachable!("a Drive location opens a Drive backend"),
        }
    }

    pub fn location(&self) -> &Location {
        &self.location
    }

    pub fn display_path(&self, rel: &str) -> String {
        self.backend.display_path(rel)
    }

    pub fn read_text(&self, rel: &str) -> Result<Option<String>, CliError> {
        inside(rel)?;
        self.read(false, self.backend.read_file(rel))
    }

    /// The same, never from the read cache.
    pub fn read_text_fresh(&self, rel: &str) -> Result<Option<String>, CliError> {
        inside(rel)?;
        match &self.location {
            Location::Dir(_) => self.read_text(rel),
            Location::Drive(_) => self.read(true, self.remote().cat(rel)),
        }
    }

    pub fn write_text(&self, rel: &str, content: &str) -> Result<(), CliError> {
        inside(rel)?;
        if let Location::Dir(root) = &self.location {
            // Atomic and owner-only on a local directory; pk-cli-drive's
            // mount write is a plain overwrite.
            let path = root.join(rel);
            create_parent(&path)?;
            return pk_cli_drive::private_file::write(
                &path,
                content.as_bytes(),
                pk_cli_drive::private_file::Replace::Always,
            )
            .map_err(|e| CliError::Other(format!("writing {}: {e}", path.display())));
        }
        self.backend.write_file(rel, content)
    }

    /// Write bytes to `rel` whole, replacing any file there: atomic and
    /// owner-only on a local directory, one upload on Drive.
    pub fn write_bytes(&self, rel: &str, content: &[u8]) -> Result<(), CliError> {
        inside(rel)?;
        if let Location::Dir(root) = &self.location {
            let path = root.join(rel);
            create_parent(&path)?;
            return pk_cli_drive::private_file::write(
                &path,
                content,
                pk_cli_drive::private_file::Replace::Always,
            )
            .map_err(|e| CliError::Other(format!("writing {}: {e}", path.display())));
        }
        if let Ok(text) = std::str::from_utf8(content) {
            return self.backend.write_file(rel, text);
        }
        let dir = pk_cli_drive::private_temp_dir("medrec-upload")?;
        let local = dir.path().join("file");
        std::fs::write(&local, content)
            .map_err(|e| CliError::Other(format!("staging {}: {e}", self.display_path(rel))))?;
        self.backend.put_file(&local, rel)
    }

    /// Write several files (binary-safe), each replacing any file at its
    /// `rel`, stopping at the first failure. On a local directory one after
    /// another, owner-only; on Drive a few at a time within pk-cli-drive's
    /// limit, skipping a file whose MD5 already matches. How many were sent.
    pub fn write_many(&self, files: &[(&str, &[u8])]) -> Result<usize, CliError> {
        for (rel, _) in files {
            inside(rel)?;
        }
        if let Location::Dir(_) = &self.location {
            for (rel, bytes) in files {
                self.write_bytes(rel, bytes)?;
            }
            return Ok(files.len());
        }
        self.backend.write_many(files)
    }

    /// Upload a local file (binary-safe), replacing any file at `rel`. On a
    /// local directory the file is owner-only (0600) in owner-only folders.
    pub fn put_file(&self, local: &Path, rel: &str) -> Result<(), CliError> {
        inside(rel)?;
        if let Location::Dir(root) = &self.location {
            return private_copy(local, &root.join(rel), Existing::Replace);
        }
        self.backend.put_file(local, rel)
    }

    /// The bytes of `rel` on local disk (the file itself on a directory; a
    /// download into an owner-only temp dir otherwise).
    pub fn fetch(&self, rel: &str) -> Result<LocalCopy, CliError> {
        inside(rel)?;
        self.read(false, self.backend.local_copy(rel))
    }

    pub fn read_bytes(&self, rel: &str) -> Result<Vec<u8>, CliError> {
        let copy = self.fetch(rel)?;
        std::fs::read(&copy.path)
            .map_err(|e| CliError::Other(format!("reading {}: {e}", self.display_path(rel))))
    }

    /// Whether it is a folder of this machine.
    pub fn is_local(&self) -> bool {
        matches!(self.location, Location::Dir(_))
    }

    /// The same, never from the read cache; `None` where the file is not there.
    pub fn read_bytes_fresh(&self, rel: &str) -> Result<Option<Vec<u8>>, CliError> {
        inside(rel)?;
        let read = match &self.location {
            Location::Dir(root) => std::fs::read(root.join(rel)).map_err(|e| {
                let says = format!("reading {}: {e}", self.display_path(rel));
                match e.kind() {
                    std::io::ErrorKind::NotFound => CliError::NotFound(says),
                    _ => CliError::Other(says),
                }
            }),
            // A download, which the cache has no part in.
            Location::Drive(_) => self.read_bytes(rel),
        };
        match read {
            Ok(b) => Ok(Some(b)),
            Err(e) if matches!(e.base(), CliError::NotFound(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub fn exists(&self, rel: &str) -> Result<bool, CliError> {
        inside(rel)?;
        self.read(false, self.backend.exists(rel))
    }

    /// Every file under `rel`, recursively, with sizes.
    pub fn list(&self, rel: &str) -> Result<Vec<FileEntry>, CliError> {
        inside(rel)?;
        self.read(false, self.backend.list_entries(rel))
    }

    /// Delete one file. `Ok(false)` when it was not there. pk-cli-drive
    /// refuses an empty path, a folder, an absolute path and `..`.
    pub fn delete(&self, rel: &str) -> Result<bool, CliError> {
        inside(rel)?;
        self.backend.delete_file(rel)
    }
}

/// A file in the store with Drive's MD5 (computed locally for a directory).
#[derive(Debug, Clone, Serialize)]
pub struct Hashed {
    pub rel: String,
    pub size: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub md5: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct Space {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub free: Option<u64>,
}

impl Store {
    /// Every file under the folder `rel` (recursively) with its MD5, in one
    /// listing. Use [`Store::md5`] for a single file.
    pub fn hashes(&self, rel: &str) -> Result<Vec<Hashed>, CliError> {
        let rel = rel.trim_matches('/');
        inside(rel)?;
        match &self.location {
            Location::Dir(root) => {
                let base = if rel.is_empty() {
                    root.clone()
                } else {
                    root.join(rel)
                };
                let mut out = Vec::new();
                if base.is_dir() {
                    walk(&base, &mut |p| {
                        let sub = p.strip_prefix(root).unwrap_or(p).to_string_lossy();
                        // A backslash is a separator on Windows alone; on this
                        // Mac it is a character of the name.
                        #[cfg(windows)]
                        let sub = sub.replace('\\', "/");
                        let sub = sub.to_string();
                        let size = p.metadata().map(|m| m.len()).unwrap_or(0);
                        out.push(Hashed {
                            rel: sub,
                            size,
                            md5: md5_file(p).ok(),
                        });
                    })?;
                } else if base.is_file() {
                    out.push(Hashed {
                        rel: rel.to_string(),
                        size: base.metadata().map(|m| m.len()).unwrap_or(0),
                        md5: md5_file(&base).ok(),
                    });
                }
                out.sort_by(|a, b| a.rel.cmp(&b.rel));
                Ok(out)
            }
            Location::Drive(_) => Ok(self
                .read(true, self.remote().hashes(rel))?
                .into_iter()
                .map(|h| Hashed {
                    rel: h.rel,
                    size: h.size,
                    md5: h.md5,
                })
                .collect()),
        }
    }

    /// The files directly in the folder `rel`, not below it, in one listing
    /// that never comes from the cache: what a cheap, frequent check of a
    /// folder another program writes into needs. Empty when the folder is
    /// not there.
    pub fn list_here(&self, rel: &str) -> Result<Vec<FileEntry>, CliError> {
        let rel = rel.trim_matches('/');
        inside(rel)?;
        match &self.location {
            Location::Dir(root) => {
                let dir = root.join(rel);
                let rd = match std::fs::read_dir(&dir) {
                    Ok(rd) => rd,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
                    Err(e) => {
                        return Err(CliError::Other(format!("listing {}: {e}", dir.display())))
                    }
                };
                let mut out = Vec::new();
                let failed =
                    |e: std::io::Error| CliError::Other(format!("listing {}: {e}", dir.display()));
                for entry in rd {
                    let entry = entry.map_err(failed)?;
                    let meta = match entry.metadata() {
                        Ok(m) => m,
                        // Gone since the listing: a cancelled request.
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                        Err(e) => return Err(failed(e)),
                    };
                    if !meta.is_file() {
                        continue;
                    }
                    let modified = meta
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs() as i64);
                    out.push(FileEntry {
                        rel: entry.file_name().to_string_lossy().into_owned(),
                        size: Some(meta.len()),
                        modified,
                        md5: None,
                    });
                }
                out.sort_by(|a, b| a.rel.cmp(&b.rel));
                Ok(out)
            }
            Location::Drive(_) => self.read(true, self.remote().list_here(rel)),
        }
    }

    /// Make the folder `rel` (and its parents); owner-only on a directory.
    /// On Drive, the one call that also makes a missing folder the
    /// location's own path names (an inbox inside the records folder).
    pub fn mkdir(&self, rel: &str) -> Result<(), CliError> {
        let rel = rel.trim_matches('/');
        inside(rel)?;
        match &self.location {
            Location::Dir(root) => private_dirs(&root.join(rel)),
            Location::Drive(_) => match &self.backend {
                Backend::CachedDrive(c) => c.mkdir(rel),
                _ => self.remote().mkdir(rel),
            },
        }
    }

    /// The MD5 the store holds for one file; `None` when it is not there.
    pub fn md5(&self, rel: &str) -> Result<Option<String>, CliError> {
        inside(rel)?;
        match &self.location {
            Location::Dir(root) => {
                let p = root.join(rel);
                if !p.is_file() {
                    return Ok(None);
                }
                md5_file(&p).map(Some)
            }
            Location::Drive(_) => self.read(true, self.remote().md5(rel)),
        }
    }

    /// Copy a local directory's contents under `rel`. Never replaces a
    /// file: one already there with the same bytes is left as it is, and one
    /// with other bytes refuses the whole copy before anything is uploaded,
    /// so a filed file cannot be overwritten by a later upload.
    pub fn put_tree(&self, local: &Path, rel: &str) -> Result<(), CliError> {
        inside(rel)?;
        match &self.location {
            Location::Dir(root) => copy_tree(local, &root.join(rel), Existing::Keep),
            Location::Drive(_) => self
                .remote()
                .put_tree(local, rel, gdrive::Existing::Keep)
                .map(|_| ()),
        }
    }

    /// Copy a local directory's contents under `rel`, replacing a file there
    /// whose bytes differ (in place on Drive, keeping its id): a backup's.
    pub fn put_tree_replacing(&self, local: &Path, rel: &str) -> Result<(), CliError> {
        inside(rel)?;
        match &self.location {
            Location::Dir(root) => copy_tree(local, &root.join(rel), Existing::Replace),
            Location::Drive(_) => self
                .remote()
                .put_tree(local, rel, gdrive::Existing::Replace)
                .map(|_| ()),
        }
    }

    /// Copy everything under `rel` into a local directory (owner-only).
    pub fn fetch_tree(&self, rel: &str, local: &Path) -> Result<(), CliError> {
        inside(rel)?;
        match &self.location {
            Location::Dir(root) => copy_tree(&root.join(rel), local, Existing::Replace),
            Location::Drive(_) => self.read(true, self.remote().fetch_tree(rel, local).map(|_| ())),
        }
    }

    /// Move a file within the store; refuses to replace one.
    pub fn move_no_clobber(&self, from: &str, to: &str) -> Result<(), CliError> {
        inside(from)?;
        inside(to)?;
        self.backend.move_no_clobber(from, to)
    }

    /// The total and free space Google Drive (or the file system) reports.
    pub fn space(&self) -> Result<Space, CliError> {
        match &self.location {
            Location::Dir(root) => Ok(statvfs_space(root)),
            Location::Drive(_) => {
                let about = self.remote().about()?;
                Ok(Space {
                    total: about.limit,
                    free: about.free(),
                })
            }
        }
    }

    /// The Google account a Drive folder is in, as Drive names it; `None`
    /// for a directory.
    pub fn account(&self) -> Result<Option<String>, CliError> {
        match &self.location {
            Location::Dir(_) => Ok(None),
            Location::Drive(_) => self.remote().about().map(|a| Some(a.email)),
        }
    }

    pub fn budget(&self) -> Duration {
        self.budget
    }
}

/// The background refresh a stale cached read spawns (`medrec cache
/// sync --revalidate-*`), built as [`Store::open`] built the cache.
pub fn revalidate(
    spec: &str,
    listing: bool,
    rel: &str,
    budget: Duration,
) -> Result<String, CliError> {
    let Location::Drive(spec) = Location::parse(spec)? else {
        return Err(CliError::Usage(format!(
            "`{spec}` is not a Google Drive folder, so it has no read cache"
        )));
    };
    let dir = cache_dir(&spec).ok_or_else(|| {
        CliError::Other("cannot resolve a cache directory (is $HOME set?)".into())
    })?;
    let kind = if listing {
        cache::Kind::Listing
    } else {
        cache::Kind::File
    };
    let outcome = cache::run_revalidation(
        drive(&spec, budget)?,
        dir,
        CachePolicy::from_env(BIN),
        kind,
        rel,
    )?;
    Ok(outcome.as_str().to_string())
}

pub fn md5_file(path: &Path) -> Result<String, CliError> {
    let mut f = std::fs::File::open(path)
        .map_err(|e| CliError::Other(format!("reading {}: {e}", path.display())))?;
    let mut h = Md5::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = f
            .read(&mut buf)
            .map_err(|e| CliError::Other(format!("reading {}: {e}", path.display())))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(crate::hash::hex(&h.finalize()))
}

fn walk(dir: &Path, f: &mut dyn FnMut(&Path)) -> Result<(), CliError> {
    let rd = std::fs::read_dir(dir)
        .map_err(|e| CliError::Other(format!("listing {}: {e}", dir.display())))?;
    let mut entries: Vec<_> = rd.filter_map(Result::ok).collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let p = e.path();
        let ft = e.file_type().map_err(|e| CliError::Other(e.to_string()))?;
        if ft.is_dir() {
            walk(&p, f)?;
        } else if ft.is_file() {
            f(&p);
        }
    }
    Ok(())
}

/// What a copy does with a file already at its destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Existing {
    /// Replace it, atomically.
    Replace,
    /// Keep it: the same bytes are fine, other bytes are an error.
    Keep,
}

pub fn copy_tree(from: &Path, to: &Path, existing: Existing) -> Result<(), CliError> {
    if from.is_file() {
        return private_copy(from, to, existing);
    }
    let mut files = Vec::new();
    walk(from, &mut |p| files.push(p.to_path_buf()))?;
    private_dirs(to)?;
    for p in files {
        let rel = p.strip_prefix(from).unwrap_or(&p);
        private_copy(&p, &to.join(rel), existing)?;
    }
    Ok(())
}

/// `dir` and every folder under it 0700, every file 0600: for a copy made
/// by something that sets its own modes from the umask. Symlinks are left
/// alone.
pub fn restrict_tree(dir: &Path) -> Result<(), CliError> {
    use std::os::unix::fs::PermissionsExt;
    let set = |p: &Path, mode: u32| {
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode))
            .map_err(|e| CliError::Other(format!("restricting {}: {e}", p.display())))
    };
    set(dir, 0o700)?;
    for e in std::fs::read_dir(dir)
        .map_err(|e| CliError::Other(format!("reading {}: {e}", dir.display())))?
        .flatten()
    {
        let ft = e.file_type().map_err(|e| CliError::Other(e.to_string()))?;
        if ft.is_dir() {
            restrict_tree(&e.path())?;
        } else if ft.is_file() {
            set(&e.path(), 0o600)?;
        }
    }
    Ok(())
}

/// Create `dir` and any missing parents owner-only (0700). Directories that
/// already exist keep their mode.
pub fn private_dirs(dir: &Path) -> Result<(), CliError> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|e| CliError::Other(format!("creating {}: {e}", dir.display())))
}

/// Copy `from` to `to` as an owner-only file (0600) in owner-only folders.
/// Records are the most sensitive data a household has; a copy never takes
/// the mode of the file it came from.
pub fn private_copy(from: &Path, to: &Path, existing: Existing) -> Result<(), CliError> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let fail = |e: std::io::Error| {
        CliError::Other(format!(
            "copying {} to {}: {e}",
            from.display(),
            to.display()
        ))
    };
    if let Some(parent) = to.parent() {
        private_dirs(parent)?;
    }
    if existing == Existing::Keep && to.symlink_metadata().is_ok() {
        if to.is_file() && md5_file(to)? == md5_file(from)? {
            return Ok(());
        }
        return Err(CliError::Upstream(format!(
            "refusing to overwrite {}: a different file is already there; nothing was replaced",
            to.display()
        )));
    }
    let mut src = std::fs::File::open(from).map_err(fail)?;
    // Keep: create the destination itself, so a file that appeared since
    // the check above is never replaced. Replace: a temporary file beside
    // it, renamed over it once complete.
    let write_to = match existing {
        Existing::Keep => to.to_path_buf(),
        Existing::Replace => {
            let name = to
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            to.with_file_name(format!(".{name}.medrec-{}", std::process::id()))
        }
    };
    let mut dst = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&write_to)
        .map_err(fail)?;
    let copied = std::io::copy(&mut src, &mut dst)
        .and_then(|_| dst.flush())
        .and_then(|_| dst.sync_all());
    if let Err(e) = copied {
        let _ = std::fs::remove_file(&write_to);
        return Err(fail(e));
    }
    if existing == Existing::Replace {
        if let Err(e) = std::fs::rename(&write_to, to) {
            let _ = std::fs::remove_file(&write_to);
            return Err(fail(e));
        }
    }
    Ok(())
}

pub fn statvfs_space(path: &Path) -> Space {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return Space {
            total: None,
            free: None,
        };
    };
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: c is a valid NUL-terminated path; s is a zeroed statvfs.
    if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 {
        return Space {
            total: None,
            free: None,
        };
    }
    let frsize = s.f_frsize as u64;
    Space {
        total: Some(s.f_blocks as u64 * frsize),
        free: Some(s.f_bavail as u64 * frsize),
    }
}

fn create_parent(path: &Path) -> Result<(), CliError> {
    match path.parent() {
        Some(parent) => private_dirs(parent),
        None => Ok(()),
    }
}

/// A unit test's Drive: its own sign-ins (a vault in memory) against a fake
/// Drive server, set for the test's thread. No keychain, no network.
#[cfg(test)]
pub mod testing {
    use std::cell::{Cell, RefCell};
    use std::path::PathBuf;
    use std::rc::Rc;

    use pk_cli_drive::google::DriveCli;

    type Make = Rc<dyn Fn() -> DriveCli>;

    thread_local! {
        static DRIVE: RefCell<Option<(Make, Option<PathBuf>)>> = const { RefCell::new(None) };
    }

    /// Every store this thread opens on a Drive location uses `make`'s
    /// sign-ins, with the read cache in `cache` (none when `None`), until
    /// the guard drops.
    pub fn use_drive(make: impl Fn() -> DriveCli + 'static, cache: Option<PathBuf>) -> Guard {
        DRIVE.with(|d| *d.borrow_mut() = Some((Rc::new(make), cache)));
        Guard
    }

    pub struct Guard;

    impl Drop for Guard {
        fn drop(&mut self) {
            DRIVE.with(|d| *d.borrow_mut() = None);
        }
    }

    thread_local! {
        static FRESH: Cell<bool> = const { Cell::new(false) };
    }

    /// Whether this thread's stores read from Drive itself, as `--fresh` has
    /// every thread's do. What this thread read so far is not counted.
    pub fn read_fresh(fresh: bool) {
        FRESH.with(|f| f.set(fresh));
        super::READ.with(|r| *r.borrow_mut() = None);
    }

    pub(super) fn fresh() -> bool {
        FRESH.with(Cell::get)
    }

    pub(super) fn cli() -> Option<DriveCli> {
        DRIVE.with(|d| d.borrow().as_ref().map(|(make, _)| make()))
    }

    pub(super) fn cache_dir() -> Option<PathBuf> {
        DRIVE.with(|d| d.borrow().as_ref().and_then(|(_, c)| c.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locations_parse_drive_folders_and_directories_and_refuse_rclone_remotes() {
        let Location::Drive(spec) = Location::parse("gdrive:adult1/1AbC_d-9/inbox/").unwrap()
        else {
            panic!("a Drive folder");
        };
        assert_eq!(spec.to_string(), "gdrive:adult1/1AbC_d-9/inbox");
        assert_eq!(
            Location::parse("gdrive:adult1/1AbC_d-9")
                .unwrap()
                .join("inbox")
                .unwrap(),
            Location::Drive(spec)
        );
        for retired in [
            "records:Medical Records",
            "gdrive:Medical Records",
            "gdrive:",
        ] {
            let err = Location::parse(retired).unwrap_err();
            assert_eq!(err.exit_code(), 2, "{retired}");
            assert!(
                err.remedy()
                    .is_some_and(|r| r.contains("medrec drive login")),
                "{retired}"
            );
        }
        assert!(matches!(
            Location::parse("/tmp/records"),
            Ok(Location::Dir(_))
        ));
        assert!(matches!(Location::parse("./a:b"), Ok(Location::Dir(_))));
        assert!(matches!(Location::parse("C:\\x"), Ok(Location::Dir(_))));
    }

    fn mode(p: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn local_copies_are_owner_only_and_keep_never_replaces() {
        use std::os::unix::fs::PermissionsExt;
        let root = crate::testutil::temp_dir("store-private");
        let one = root.join("one.bin");
        let two = root.join("two.bin");
        std::fs::write(&one, b"one").unwrap();
        std::fs::write(&two, b"two").unwrap();
        std::fs::set_permissions(&one, std::fs::Permissions::from_mode(0o644)).unwrap();

        let dst = root.join("out/a/b.bin");
        private_copy(&one, &dst, Existing::Keep).unwrap();
        assert_eq!(
            (
                mode(&dst),
                mode(&root.join("out")),
                mode(&root.join("out/a"))
            ),
            (0o600, 0o700, 0o700)
        );
        // The same bytes again are fine; other bytes are refused and the
        // filed copy stays as it was.
        private_copy(&one, &dst, Existing::Keep).unwrap();
        assert!(matches!(
            private_copy(&two, &dst, Existing::Keep),
            Err(CliError::Upstream(_))
        ));
        assert_eq!(std::fs::read(&dst).unwrap(), b"one");

        private_copy(&two, &dst, Existing::Replace).unwrap();
        assert_eq!(
            (std::fs::read(&dst).unwrap(), mode(&dst)),
            (b"two".to_vec(), 0o600)
        );
        assert_eq!(
            std::fs::read_dir(root.join("out/a")).unwrap().count(),
            1,
            "no temporary left"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_directory_store_writes_reads_lists_and_deletes() {
        let root = crate::testutil::temp_dir("store");
        let store =
            Store::open(&Location::Dir(root.clone()), Reads::Fresh, DEFAULT_BUDGET).unwrap();
        store.write_text("a/b.txt", "hello").unwrap();
        assert_eq!(
            store.read_text("a/b.txt").unwrap().as_deref(),
            Some("hello")
        );
        assert_eq!(store.list("a").unwrap().len(), 1);
        assert!(store.delete("a/b.txt").unwrap());
        assert!(!store.delete("a/b.txt").unwrap());
        assert!(store.delete("a").is_err());
        assert!(store.delete("").is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_directory_store_hashes_copies_trees_and_moves_without_clobbering() {
        let root = crate::testutil::temp_dir("store-hash");
        let store =
            Store::open(&Location::Dir(root.clone()), Reads::Fresh, DEFAULT_BUDGET).unwrap();
        store.write_text("p/a.txt", "abc").unwrap();
        assert_eq!(
            store.md5("p/a.txt").unwrap().as_deref(),
            Some("900150983cd24fb0d6963f7d28e17f72")
        );
        assert_eq!(store.md5("p/missing").unwrap(), None);
        let h = store.hashes("p").unwrap();
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].rel, "p/a.txt");
        let local = crate::testutil::temp_dir("store-tree");
        std::fs::create_dir_all(local.join("s")).unwrap();
        std::fs::write(local.join("s/x.dcm"), b"x").unwrap();
        store.put_tree(&local, "q").unwrap();
        assert!(store.exists("q/s/x.dcm").unwrap());
        assert_eq!(mode(&root.join("q/s/x.dcm")), 0o600);
        // An upload never replaces a filed file.
        store.put_tree(&local, "q").unwrap();
        std::fs::write(local.join("s/x.dcm"), b"y").unwrap();
        assert!(store.put_tree(&local, "q").is_err());
        assert_eq!(store.read_text("q/s/x.dcm").unwrap().as_deref(), Some("x"));
        store.write_text("p/b.txt", "b").unwrap();
        assert!(store.move_no_clobber("p/a.txt", "p/b.txt").is_err());
        assert!(store.space().unwrap().free.is_some());
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(local).unwrap();
    }
}
