//! Non-secret, per-machine settings (`~/.config/medrec/config.json`).
//!
//! A records folder is a login (the Drive profile its spec names), so its
//! settings belong to a profile (cli-common `pk_cli_config::profile`): `--profile <NAME>`,
//! or `MEDREC_PROFILE`, names the one a command uses, with no default. The
//! rest are this machine's, whatever the profile.

use pk_cli_config::profile::{self, ProfileName, Profiles};
use pk_cli_config::ConfigStore;
use pk_cli_core::CliError;
use serde::{Deserialize, Serialize};

use crate::store::{Location, BIN};

/// The environment's fallback for `--profile`.
pub const PROFILE_ENV: &str = "MEDREC_PROFILE";

/// A profile's settings, set with `--profile <NAME> config set`.
pub const PROFILE_KEYS: &[&str] = &["folder", "mount", "inbox", "me"];

pub const KEYS: &str = "folder, mount, inbox, me (a profile's); viewer, ocr_languages, \
                        ocr_max_pages, max_file_mb, providers, import_timeout_secs, import_max_mb, \
                        store_budget_secs (this machine's)";

/// What a profile holds: one records folder and how this machine reaches it.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct FolderSettings {
    /// The records folder: `gdrive:<drive profile>/<folder id>` (canonical)
    /// or a local directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    /// Where the records folder is mounted locally (Google Drive for
    /// desktop), if it is. Optional: medrec reads and writes through the
    /// remote, and `doctor` only checks that the mount shows the same folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mount: Option<String>,
    /// The inbox `--inbox` drains: `gdrive:<drive profile>/<folder id>[/<path>]`
    /// or a local directory.
    /// Default: `inbox/` in the records folder. Files there are moved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inbox: Option<String>,
    /// The person who uses medrec on this machine (a person id), named as
    /// the author of notes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub me: Option<String>,
    /// What `medrec refresh` imports, and how: set by `refresh add`,
    /// `remove`, `pause`, `resume` and `--install`, never by `config set`.
    /// The provider login profiles named here stay on this machine.
    #[serde(default, skip_serializing_if = "RefreshSettings::is_empty")]
    pub refresh: RefreshSettings,
}

/// A profile's refresh settings (`refresh` in its config).
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct RefreshSettings {
    /// Each source set up for refresh, by its id in household.json.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub sources: std::collections::BTreeMap<String, Binding>,
    /// How often the LaunchAgent runs a pass (`--install --pass-every`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pass_every_secs: Option<u64>,
}

impl RefreshSettings {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// One source's refresh: the binding `refresh add` made on purpose.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Binding {
    /// The provider CLI the binding was made for. A pass runs this name,
    /// never one read from the records folder, and refuses a source whose
    /// `household.json` names another.
    #[serde(default)]
    pub cli: String,
    /// The provider's login profile its export runs with; `None` for a
    /// provider that signs in to nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_profile: Option<String>,
    /// What the export's `--patient` takes; the source's patient id when
    /// not given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patient: Option<String>,
    /// The export's `--org`, when the import that set it up named one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub org: Option<String>,
    /// Imported on its schedule.
    #[serde(default = "yes")]
    pub auto: bool,
    /// How often, when on its schedule; never below its minimum interval.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub every_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused: Option<crate::refresh::contract::Pause>,
}

fn yes() -> bool {
    true
}

impl FolderSettings {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Config {
    /// The settings this machine kept before profiles, top-level in the
    /// file. Nothing reads them but `profiles migrate`, which moves them
    /// into a profile.
    #[serde(flatten)]
    pub unprofiled: FolderSettings,
    /// One per records folder, named by the household.
    #[serde(default, skip_serializing_if = "Profiles::is_empty")]
    pub profiles: Profiles,
    /// The profile this command runs as ([`Config::select`]), never saved.
    #[serde(skip)]
    pub profile: Option<ProfileName>,
    /// Its settings, read by everything that reaches the records folder.
    #[serde(skip)]
    pub active: FolderSettings,
    /// The DICOM viewer `images open` launches: an `.app` bundle or an
    /// executable. Default: Weasis in /Applications.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewer: Option<String>,
    /// tesseract languages, `eng` by default (`eng+spa` for two).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ocr_languages: Option<String>,
    /// Pages OCR'd per document (default 20).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ocr_max_pages: Option<u32>,
    /// Largest file intake accepts, MiB (default 200).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_file_mb: Option<u64>,
    /// Provider CLIs to consider besides the known ones (comma-separated).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub providers: Option<String>,
    /// How long one provider export may run (default 1800 s).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub import_timeout_secs: Option<u64>,
    /// Largest records stream one import reads, MiB (default 4096). An
    /// import holds the stream until its trailer, so this bounds memory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub import_max_mb: Option<u64>,
    /// How long all of one command's store requests may take (default
    /// 1800 s). Read from `rclone_budget_secs` too, its name before medrec
    /// reached Drive itself.
    #[serde(
        default,
        alias = "rclone_budget_secs",
        skip_serializing_if = "Option::is_none"
    )]
    pub store_budget_secs: Option<u64>,
}

impl Config {
    pub fn load(store: &ConfigStore) -> Result<Self, CliError> {
        store.load()
    }

    /// Run as the profile `given` (`--profile`), else `MEDREC_PROFILE`'s:
    /// it must be configured, and a missing or unknown one is exit 2 listing
    /// those that are. Its settings become [`Config::active`].
    pub fn select(&mut self, given: Option<&str>) -> Result<&ProfileName, CliError> {
        let from_env = std::env::var(PROFILE_ENV).ok().filter(|v| !v.is_empty());
        self.select_from(given, from_env.as_deref())
    }

    /// [`Config::select`] with the environment's value passed in.
    pub fn select_from(
        &mut self,
        given: Option<&str>,
        from_env: Option<&str>,
    ) -> Result<&ProfileName, CliError> {
        let name = profile::resolve_from(given, from_env, PROFILE_ENV, &self.profiles, BIN)
            .map_err(|e| {
                if self.profiles.is_empty() && !self.unprofiled.is_empty() {
                    e.with_remedy(format!(
                        "move this machine's records folder into a profile with `{BIN} profiles \
                         migrate <NAME>`"
                    ))
                } else {
                    e
                }
            })?;
        self.active = self.profiles.settings(&name)?;
        Ok(self.profile.insert(name))
    }

    /// Write [`Config::active`] back into its profile, for a save.
    pub fn keep_active(&mut self) -> Result<(), CliError> {
        let Some(name) = self.profile.clone() else {
            return Err(CliError::Other("no profile selected".into()));
        };
        let active = self.active.clone();
        self.profiles.set_settings(&name, &active)
    }

    /// Move the settings kept before profiles into `name`, adding it when it
    /// is new. `false` when there were none to move. A profile that already
    /// names another records folder is refused, so nothing is overwritten.
    pub fn migrate(&mut self, name: &ProfileName) -> Result<bool, CliError> {
        if self.unprofiled.is_empty() {
            return Ok(false);
        }
        self.profiles.add(name.clone());
        let mut kept: FolderSettings = self.profiles.settings(name)?;
        if kept.folder.is_some() && kept.folder != self.unprofiled.folder {
            return Err(CliError::Usage(format!(
                "profile `{name}` already has a records folder; migrate into a new profile"
            )));
        }
        let old = std::mem::take(&mut self.unprofiled);
        kept.folder = kept.folder.or(old.folder);
        kept.mount = kept.mount.or(old.mount);
        kept.inbox = kept.inbox.or(old.inbox);
        kept.me = kept.me.or(old.me);
        self.profiles.set_settings(name, &kept)?;
        Ok(true)
    }

    pub fn folder_location(&self) -> Result<Location, CliError> {
        self.active
            .folder
            .as_deref()
            .map(Location::parse)
            .transpose()?
            .ok_or_else(|| {
                let name = self
                    .profile
                    .as_ref()
                    .map(ProfileName::as_str)
                    .unwrap_or("<NAME>");
                CliError::Usage(format!("profile `{name}` has no records folder")).with_remedy(
                    format!(
                        "run `{BIN} --profile {name} init <gdrive:PROFILE/FOLDER-ID|dir>` or \
                     `{BIN} --profile {name} config set folder <gdrive:PROFILE/FOLDER-ID|dir>`"
                    ),
                )
            })
    }

    /// The inbox: configured, else `inbox/` in the records folder.
    pub fn inbox_location(&self) -> Result<Location, CliError> {
        if let Some(i) = &self.active.inbox {
            return Location::parse(i);
        }
        self.folder_location()?.join(crate::folder::INBOX)
    }

    pub fn ocr_languages(&self) -> String {
        self.ocr_languages.clone().unwrap_or_else(|| "eng".into())
    }

    pub fn ocr_max_pages(&self) -> u32 {
        self.ocr_max_pages.unwrap_or(20).clamp(1, 500)
    }

    pub fn max_file_bytes(&self) -> u64 {
        self.max_file_mb.unwrap_or(200).clamp(1, 4096) * 1024 * 1024
    }

    pub fn import_max_bytes(&self) -> u64 {
        self.import_max_mb.unwrap_or(4096).clamp(16, 65_536) * 1024 * 1024
    }

    pub fn import_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.import_timeout_secs.unwrap_or(1800).clamp(10, 86_400))
    }

    pub fn store_budget(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.store_budget_secs.unwrap_or(1800).clamp(10, 86_400))
    }

    pub fn extra_providers(&self) -> Vec<String> {
        self.providers
            .as_deref()
            .unwrap_or("")
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect()
    }

    /// Whether `key` is a profile's setting rather than this machine's.
    pub fn is_profile_key(key: &str) -> bool {
        PROFILE_KEYS.contains(&key)
    }

    /// Set `key`; a profile's setting goes to [`Config::active`], so select
    /// the profile first and [`Config::keep_active`] after.
    pub fn set(&mut self, key: &str, value: &str) -> Result<(), CliError> {
        let num = |v: &str| {
            v.parse::<u64>()
                .map_err(|_| CliError::Usage(format!("`{key}` takes a whole number, not `{v}`")))
        };
        match key {
            // A folder named the rclone way is refused here, not on first use.
            "folder" => {
                Location::parse(value)?;
                self.active.folder = Some(value.into())
            }
            "mount" => self.active.mount = Some(value.into()),
            "inbox" => {
                Location::parse(value)?;
                self.active.inbox = Some(value.into())
            }
            "me" => {
                if !crate::ids::is_valid("p", value) {
                    return Err(CliError::Usage(format!(
                        "`{value}` is not a person id (see `medrec people list`)"
                    )));
                }
                self.active.me = Some(value.into())
            }
            "viewer" => self.viewer = Some(value.into()),
            "ocr_languages" => {
                if value.is_empty()
                    || !value
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '_')
                {
                    return Err(CliError::Usage(
                        "ocr_languages takes tesseract codes like `eng` or `eng+spa`".into(),
                    ));
                }
                self.ocr_languages = Some(value.into())
            }
            "ocr_max_pages" => self.ocr_max_pages = Some(num(value)? as u32),
            "max_file_mb" => self.max_file_mb = Some(num(value)?),
            "providers" => self.providers = Some(value.into()),
            "import_timeout_secs" => self.import_timeout_secs = Some(num(value)?),
            "import_max_mb" => self.import_max_mb = Some(num(value)?),
            "store_budget_secs" | "rclone_budget_secs" => {
                self.store_budget_secs = Some(num(value)?)
            }
            other => {
                return Err(CliError::Usage(format!(
                    "unknown config key `{other}` (known: {KEYS})"
                )))
            }
        }
        Ok(())
    }

    pub fn unset(&mut self, key: &str) -> Result<(), CliError> {
        match key {
            "folder" => self.active.folder = None,
            "mount" => self.active.mount = None,
            "inbox" => self.active.inbox = None,
            "me" => self.active.me = None,
            "viewer" => self.viewer = None,
            "ocr_languages" => self.ocr_languages = None,
            "ocr_max_pages" => self.ocr_max_pages = None,
            "max_file_mb" => self.max_file_mb = None,
            "providers" => self.providers = None,
            "import_timeout_secs" => self.import_timeout_secs = None,
            "import_max_mb" => self.import_max_mb = None,
            "store_budget_secs" | "rclone_budget_secs" => self.store_budget_secs = None,
            other => {
                return Err(CliError::Usage(format!(
                    "unknown config key `{other}` (known: {KEYS})"
                )))
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with(names: &[&str]) -> Config {
        let mut c = Config::default();
        for n in names {
            c.profiles.add(ProfileName::parse(n).unwrap());
        }
        c
    }

    #[test]
    fn the_flag_wins_over_the_environment_and_neither_is_guessed() {
        let mut c = with(&["home", "away"]);
        assert_eq!(
            c.select_from(Some("home"), Some("away")).unwrap().as_str(),
            "home"
        );
        assert_eq!(c.select_from(None, Some("away")).unwrap().as_str(), "away");
        let mut lone = with(&["home"]);
        let e = lone.select_from(None, None).unwrap_err();
        assert!(e.to_string().contains("configured: home"), "{e}");
        assert!(lone.select_from(Some("nope"), None).is_err());
    }

    #[test]
    fn a_folder_from_before_profiles_names_migrate_and_moves_once() {
        let mut c = Config::default();
        c.unprofiled.folder = Some("gdrive:home-drive/1Records_synthetic".into());
        c.unprofiled.me = Some("p_aaaaaaaaaaaaaaaaaaaa".into());
        let e = c.select_from(None, None).unwrap_err();
        assert!(e.remedy().unwrap_or("").contains("profiles migrate"), "{e}");
        let home = ProfileName::parse("home").unwrap();
        assert!(c.migrate(&home).unwrap());
        assert!(c.unprofiled.is_empty());
        assert!(!c.migrate(&home).unwrap());
        c.select_from(Some("home"), None).unwrap();
        assert_eq!(
            c.active.folder.as_deref(),
            Some("gdrive:home-drive/1Records_synthetic")
        );
        assert_eq!(c.active.me.as_deref(), Some("p_aaaaaaaaaaaaaaaaaaaa"));
        // A profile that has a different records folder keeps it.
        c.unprofiled.folder = Some("gdrive:home-drive/1Other_synthetic".into());
        assert!(c.migrate(&home).is_err());
        assert_eq!(
            c.unprofiled.folder.as_deref(),
            Some("gdrive:home-drive/1Other_synthetic")
        );
    }

    #[test]
    fn a_profile_setting_is_saved_in_its_profile_only() {
        let mut c = with(&["home"]);
        c.select_from(Some("home"), None).unwrap();
        c.set("folder", "gdrive:home-drive/1Records_synthetic")
            .unwrap();
        c.set("viewer", "none").unwrap();
        c.keep_active().unwrap();
        let v = serde_json::to_value(&c).unwrap();
        assert_eq!(
            v["profiles"]["home"]["folder"],
            "gdrive:home-drive/1Records_synthetic"
        );
        assert_eq!(v["viewer"], "none");
        assert!(v.get("folder").is_none(), "{v}");
        assert!(
            v.get("active").is_none() && v.get("profile").is_none(),
            "{v}"
        );
    }
}
