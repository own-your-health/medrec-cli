//! What `medrec refresh` keeps on this machine, under the cache: its
//! machine id, a lock so one refresh runs at a time, and the `status.json`
//! it last wrote, so an idle pass costs one listing of `_requests/`.

use std::fs::File;
use std::path::PathBuf;

use pk_cli_core::CliError;
use serde::{Deserialize, Serialize};

use super::contract::Status;
use crate::store::Location;

fn base() -> Result<PathBuf, CliError> {
    pk_cli_drive::cache::cache_base("medrec")
        .ok_or_else(|| CliError::Other("cannot resolve a cache directory ($HOME unset)".into()))
}

fn private_dir(dir: &PathBuf) -> Result<(), CliError> {
    let mut b = std::fs::DirBuilder::new();
    b.recursive(true);
    {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(0o700);
    }
    b.create(dir)
        .map_err(|e| CliError::Other(format!("creating {}: {e}", dir.display())))
}

fn folder_key(location: &Location) -> String {
    crate::hash::sha256_hex(location.display().as_bytes())[..16].to_string()
}

/// This machine's refresher id (`h_…`), made the first time it is needed.
pub fn machine() -> Result<String, CliError> {
    let dir = base()?.join("refresh");
    private_dir(&dir)?;
    let path = dir.join("machine");
    if let Ok(s) = std::fs::read_to_string(&path) {
        let s = s.trim();
        if crate::ids::is_valid("h", s) {
            return Ok(s.to_string());
        }
    }
    let id = crate::ids::new("h");
    crate::write_private(&path, format!("{id}\n").as_bytes())?;
    Ok(id)
}

/// The lock one refresh of a folder holds on this machine.
pub struct RunLock {
    _file: File,
}

/// `None` when another refresh of this folder is running here.
pub fn try_lock(location: &Location) -> Result<Option<RunLock>, CliError> {
    let dir = base()?.join("locks");
    private_dir(&dir)?;
    let path = dir.join(format!("refresh-{}.lock", folder_key(location)));
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|e| CliError::Other(format!("opening {}: {e}", path.display())))?;
    match file.try_lock() {
        Ok(()) => Ok(Some(RunLock { _file: file })),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(e)) => {
            Err(CliError::Other(format!("locking {}: {e}", path.display())))
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Local {
    /// The `status.json` this machine last wrote.
    #[serde(default)]
    pub status: Option<Status>,
    /// When it was written (Unix seconds).
    #[serde(default)]
    pub written_at: i64,
    /// When `done/` was last pruned.
    #[serde(default)]
    pub pruned_at: i64,
    /// When `--take-over` wrote this machine's id; requests wait one pass
    /// after it.
    #[serde(default)]
    pub took_over_at: Option<i64>,
    /// Pending request files already answered in `done/` that medrec could
    /// not remove (another Google account's): passed over until they go.
    #[serde(default, skip_serializing_if = "std::collections::BTreeSet::is_empty")]
    pub left: std::collections::BTreeSet<String>,
    /// The medrec binary ([`super::signin::exe_id`]) whose read of the Drive
    /// sign-in hung on an approval nobody gave. No scheduled pass by it
    /// reads the sign-in again until a person runs a pass.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drive_blocked: Option<String>,
}

fn local_path(location: &Location) -> Result<PathBuf, CliError> {
    let dir = base()?.join("refresh");
    private_dir(&dir)?;
    Ok(dir.join(format!("{}.json", folder_key(location))))
}

pub fn load(location: &Location) -> Result<Local, CliError> {
    let path = local_path(location)?;
    match std::fs::read(&path) {
        Ok(b) => Ok(serde_json::from_slice(&b).unwrap_or_default()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Local::default()),
        Err(e) => Err(CliError::Other(format!("reading {}: {e}", path.display()))),
    }
}

pub fn save(location: &Location, local: &Local) -> Result<(), CliError> {
    let path = local_path(location)?;
    let text = serde_json::to_vec_pretty(local)
        .map_err(|e| CliError::Other(format!("encoding refresh state: {e}")))?;
    pk_cli_drive::private_file::write(&path, &text, pk_cli_drive::private_file::Replace::Always)
        .map_err(|e| CliError::Other(format!("writing {}: {e}", path.display())))
}
