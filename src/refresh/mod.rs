//! `medrec refresh`: imports each source set up for it, on a schedule (a
//! LaunchAgent running `refresh --scheduled`) and on request (the terminal,
//! or a file in `_requests/`), and records what it did in `_status/`
//! (docs/data-contract.md).
//!
//! A scheduled pass never waits on a person: a provider's `auth status`
//! that hangs (a Keychain approval nobody gives) is stopped after
//! [`signin::SCHEDULED_WAIT`] and the source marked as needing a person,
//! with the command to run. It is not tried again until that has run.
//! medrec's own Drive sign-in is held to a stricter one: a scheduled pass
//! reads its key first, without ever raising a Keychain dialog
//! (`Vault::load_key_quietly`, no request), and one that a person has to
//! answer ends the pass with the error that names `medrec auth key`. The
//! pass shares one vault, so the folder's later reads in it use the key
//! already read. A slow network is an ordinary error.

pub mod contract;
pub mod launchd;
pub mod names;
pub mod policy;
pub mod requests;
pub mod signin;
pub mod state;
pub mod time;

use std::collections::{BTreeMap, BTreeSet};

use pk_cli_config::profile::ProfileName;
use pk_cli_core::CliError;
use serde::Serialize;

use crate::config::{Binding, Config};
use crate::folder::{self, Access, Folder};
use crate::import::{self, Provider};
use crate::model::{Household, Source};
use crate::store::{Location, Reads, Store};
use contract::*;
use requests::{Parsed, Scope};

/// How often the LaunchAgent runs a pass, unless `--install --pass-every` says.
pub const DEFAULT_PASS_SECS: u64 = 300;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// The LaunchAgent: nobody is at the terminal.
    Scheduled,
    /// A person ran `medrec refresh`.
    Manual,
}

/// What a terminal `medrec refresh` imports.
#[derive(Debug, Clone)]
pub enum Pick {
    All,
    Person(String),
    Source(String),
}

/// A pass, for its output and the LaunchAgent's log: ids, outcomes and
/// counts, never a name or record content.
#[derive(Debug, Default, Serialize)]
pub struct PassReport {
    /// `idle`, `busy`, `ran`, `waiting` (a take-over not yet settled) or
    /// `other_machine`.
    pub status: String,
    pub attempts: Vec<AttemptRow>,
    pub requests: Vec<RequestRow>,
    pub status_written: bool,
}

#[derive(Debug, Serialize)]
pub struct AttemptRow {
    pub source: String,
    pub cli: String,
    pub trigger: String,
    pub outcome: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub records_added: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub records_updated: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub documents_added: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RequestRow {
    pub request: String,
    /// `handled.outcome`; `retrying` or `waiting` for a provider request
    /// that stays pending.
    pub outcome: String,
    /// What kept a provider request from being written: which step, and the
    /// kind of error. Never its message, which can quote a record.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cause: Option<String>,
    /// medrec could not remove the pending file; its `done/` file stands.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub file_left: bool,
}

/// An attempt that found another writer holding the folder: not recorded.
const BUSY: &str = "busy";

/// What `medrec refresh sign-in` prints for a source.
pub fn sign_in_command(source: &str) -> String {
    format!("medrec refresh sign-in {source}")
}

/// The refresh settings of the selected profile.
fn settings(cfg: &Config) -> &crate::config::RefreshSettings {
    &cfg.active.refresh
}

/// The profile names this folder's files leave out: every provider
/// profile this profile's sources are bound to, and medrec's own.
fn names(cfg: &Config) -> names::Names {
    names::Names::new(
        settings(cfg)
            .sources
            .values()
            .filter_map(|b| b.provider_profile.as_deref()),
        cfg.profiles.names().map(|n| n.as_str()),
    )
}

fn pass_every(cfg: &Config) -> u64 {
    settings(cfg).pass_every_secs.unwrap_or(DEFAULT_PASS_SECS)
}

fn offline_after(s: &Status) -> i64 {
    (s.refresher.heartbeat_every_secs + s.refresher.pass_every_secs) as i64
}

/// Whether `s` names another machine that has refreshed recently enough
/// to be taken as online.
fn other_refresher(s: &Status, machine: &str, now: i64) -> bool {
    s.refresher.machine != machine
        && s.refresher
            .last_pass_at
            .as_deref()
            .and_then(time::parse)
            .is_some_and(|t| now - t <= offline_after(s))
}

fn not_the_refresher(s: &Status) -> CliError {
    CliError::Usage(format!(
        "another machine refreshes this folder (its last pass: {})",
        s.refresher.last_pass_at.as_deref().unwrap_or("unknown")
    ))
    .with_remedy("run `medrec refresh --install --take-over` on this machine to move refresh here")
}

/// A provider CLI name medrec runs: a plain command name looked up on
/// `PATH`, never a path.
pub fn plain_cli(name: &str) -> bool {
    let mut c = name.chars();
    c.next()
        .is_some_and(|f| f.is_ascii_lowercase() || f.is_ascii_digit())
        && c.all(|x| x.is_ascii_lowercase() || x.is_ascii_digit() || x == '-' || x == '_')
}

/// A `status.json` a later medrec wrote is not rewritten by this one.
fn check_format(s: &Status) -> Result<(), CliError> {
    if s.format > FORMAT {
        return Err(CliError::Other(format!(
            "_status/status.json is format {}, newer than this medrec (format {FORMAT}); run `medrec self-update`",
            s.format
        )));
    }
    Ok(())
}

/// The folder's `status.json` as this machine's starting point: its
/// per-source history is the folder's, kept across a take-over; a sign-in
/// is this machine's own, so another machine's is not taken as this one's.
fn adopt(mut s: Status, machine: &str) -> Status {
    if s.refresher.machine != machine {
        for e in &mut s.sources {
            e.sign_in = SignIn {
                state: "unknown".into(),
                ..SignIn::default()
            };
        }
    }
    s
}

/// The pieces of a pass that write `_status/`.
struct Writer<'a> {
    store: &'a Store,
    machine: String,
    pass_every: u64,
    wrote: bool,
    names: names::Names,
}

impl Writer<'_> {
    /// The contract's rule: every write to `status.json` or `attempts/`
    /// first reads `status.json` again, and stops when another machine has
    /// taken refresh over.
    fn still_refresher(&self) -> Result<(), CliError> {
        if let Some(s) = folder::read_json::<Status>(self.store, STATUS)? {
            check_format(&s)?;
            if other_refresher(&s, &self.machine, time::now()) {
                return Err(not_the_refresher(&s));
            }
        }
        Ok(())
    }

    fn status(
        &mut self,
        sources: &BTreeMap<String, SourceStatus>,
        local: &mut state::Local,
    ) -> Result<(), CliError> {
        self.still_refresher()?;
        let now = time::now();
        let status = Status {
            schema: STATUS_SCHEMA.into(),
            format: FORMAT,
            updated_at: time::fmt(now),
            refresher: Refresher {
                machine: self.machine.clone(),
                medrec_version: env!("CARGO_PKG_VERSION").into(),
                last_pass_at: Some(time::fmt(now)),
                next_pass_at: Some(time::fmt(now + self.pass_every as i64)),
                pass_every_secs: self.pass_every,
                heartbeat_every_secs: HEARTBEAT_SECS,
            },
            sources: self.sources(sources),
        };
        folder::write_json(self.store, STATUS, &status)?;
        local.status = Some(status);
        local.written_at = now;
        self.wrote = true;
        Ok(())
    }

    /// The sources as the folder gets them: no profile name in an error.
    fn sources(&self, sources: &BTreeMap<String, SourceStatus>) -> Vec<SourceStatus> {
        sources
            .values()
            .cloned()
            .map(|mut s| {
                if let Some(e) = s.last_attempt.as_mut().and_then(|a| a.error.as_mut()) {
                    self.names.error(e);
                }
                s
            })
            .collect()
    }

    /// Add one line to its month's file, never losing one already there.
    fn attempt(&self, a: &Attempt) -> Result<(), CliError> {
        self.still_refresher()?;
        let started = time::parse(&a.started_at).unwrap_or_else(time::now);
        let rel = format!("{ATTEMPTS_DIR}/{}.ndjson", time::month(started));
        let old = self.store.read_text(&rel)?.unwrap_or_default();
        // Lines an earlier medrec wrote with a profile name in an error
        // lose it here too.
        let mut lines: Vec<String> = old
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| self.names.line(l))
            .collect();
        let before = lines.len();
        let mut a = a.clone();
        if let Some(e) = a.error.as_mut() {
            self.names.error(e);
        }
        lines.push(
            serde_json::to_string(&a)
                .map_err(|e| CliError::Other(format!("encoding an attempt: {e}")))?,
        );
        if lines.len() != before + 1 {
            return Err(CliError::Other(format!(
                "{rel} would lose lines; not written"
            )));
        }
        let mut text = lines.join("\n");
        text.push('\n');
        self.store.write_text(&rel, &text)?;
        match self.store.read_text(&rel)? {
            Some(back) if back == text => Ok(()),
            _ => Err(CliError::Upstream(format!(
                "{} read back different from what was written",
                self.store.display_path(&rel)
            ))),
        }
    }
}

/// The latest `ImportRun` of `src` in its person's index: its id and when
/// it was filed.
fn latest_import(
    folder: &Folder,
    h: &Household,
    src: &Source,
) -> Result<Option<(String, i64)>, CliError> {
    let Some(person) = h.person(&src.person) else {
        return Ok(None);
    };
    let idx = folder.index(person)?;
    Ok(idx
        .imports
        .iter()
        .filter(|r| {
            Some(r.cli.as_str()) == src.cli.as_deref()
                && Some(r.organization.as_str()) == src.organization.as_deref()
                && Some(r.patient_id.as_str()) == src.patient_id.as_deref()
        })
        .filter_map(|r| time::parse(&r.imported_at).map(|t| (r.id.clone(), t)))
        .max_by_key(|(_, t)| *t))
}

/// `sign_in.renews` and `sign_in.lasts_secs` for a CLI: what its `auth
/// status` last declared, over medrec's table (`policy::renews`).
fn sign_in_life(
    cli: &str,
    declared: &BTreeMap<String, signin::Declared>,
) -> (Option<bool>, Option<u64>) {
    let d = declared.get(cli).copied().unwrap_or_default();
    (
        d.renews.or(policy::renews(cli)),
        d.lasts_secs.or(policy::lasts_secs(cli)),
    )
}

/// A source's entry, as kept or new.
fn entry(
    prev: Option<&SourceStatus>,
    id: &str,
    src: Option<&Source>,
    b: &Binding,
    declared: &BTreeMap<String, signin::Declared>,
) -> SourceStatus {
    let cli = b.cli.clone();
    let mut e = prev.cloned().unwrap_or(SourceStatus {
        source: id.into(),
        person: String::new(),
        cli: cli.clone(),
        organization: None,
        auto: true,
        paused: None,
        min_interval_secs: 0,
        failures: 0,
        next_run_at: None,
        sign_in: SignIn {
            state: "unknown".into(),
            ..SignIn::default()
        },
        last_attempt: None,
        last_success: None,
    });
    if let Some(s) = src {
        e.person = s.person.clone();
        e.organization = s.organization.clone();
    }
    e.cli = cli.clone();
    e.auto = b.auto;
    e.paused = b.paused.clone();
    e.min_interval_secs = policy::min_interval(&cli);
    (e.sign_in.renews, e.sign_in.lasts_secs) = sign_in_life(&cli, declared);
    e.next_run_at = next_run(&e, b);
    e
}

/// When the source is next imported on its schedule.
fn next_run(e: &SourceStatus, b: &Binding) -> Option<String> {
    if !b.auto || b.paused.is_some() || e.sign_in.needs_person.is_some() {
        return None;
    }
    let every = policy::every(&e.cli, b.every_secs) as i64;
    let after_success = e
        .last_success
        .as_ref()
        .and_then(|s| time::parse(&s.finished_at))
        .map(|t| t + every);
    let after_failure = e
        .last_attempt
        .as_ref()
        .filter(|a| a.outcome == "failed")
        .and_then(|a| time::parse(&a.started_at))
        .map(|t| t + backoff(e.failures, e.min_interval_secs) as i64);
    let at = match (after_success, after_failure) {
        (Some(a), Some(b)) => a.max(b),
        (a, b) => a.or(b).unwrap_or_else(time::now),
    };
    Some(time::fmt(at))
}

/// Whether the local record says the source is due: a guess that lets an
/// idle pass stop after one listing; the index decides.
fn due_by_local(local: &state::Local, id: &str, b: &Binding, now: i64) -> bool {
    if !b.auto || b.paused.is_some() {
        return false;
    }
    let Some(e) = local
        .status
        .as_ref()
        .and_then(|s| s.sources.iter().find(|e| e.source == id))
    else {
        return true;
    };
    if e.sign_in.needs_person.is_some() {
        return false;
    }
    e.next_run_at
        .as_deref()
        .and_then(time::parse)
        .is_none_or(|t| t <= now)
}

/// The top-level files of `_requests/` (one listing; `done/` is not read).
fn pending(store: &Store) -> Result<Vec<pk_cli_drive::FileEntry>, CliError> {
    Ok(store
        .list_here(REQUESTS_DIR)?
        .into_iter()
        .filter(|e| !e.rel.contains('/'))
        .collect())
}

struct Known<'a> {
    h: &'a Household,
    bound: &'a BTreeMap<String, Binding>,
}

impl requests::Known for Known<'_> {
    fn person(&self, id: &str) -> bool {
        self.h.person(id).is_some()
    }
    fn source(&self, id: &str) -> bool {
        self.h.sources.iter().any(|s| s.id == id)
    }
    fn set_up(&self, id: &str) -> bool {
        self.bound.contains_key(id)
    }
}

/// What one source answered in a pass, for the requests it served.
#[derive(Debug, Clone)]
struct Answer {
    outcome: String,
    attempt: Option<String>,
    next_allowed_at: Option<String>,
}

#[derive(Default)]
struct Want {
    schedule: bool,
    manual: bool,
    requests: Vec<String>,
    /// The earliest `requested_at` among them.
    requested_at: Option<i64>,
}

fn error_info(e: &CliError) -> ErrorInfo {
    ErrorInfo {
        code: if crate::proc::is_timeout(e) {
            "timeout".into()
        } else {
            e.code().into()
        },
        message: clip(e.message(), MESSAGE_MAX),
        remedy: e.remedy().map(|r| clip(r, MESSAGE_MAX)),
        provider_message: None,
    }
}

fn counts(r: &import::Report) -> BTreeMap<String, u64> {
    let v = serde_json::to_value(r).expect("an import report is plain data and serializes");
    let mut out = BTreeMap::new();
    for k in [
        "records_added",
        "records_updated",
        "records_unchanged",
        "records_older",
        "resources_written",
        "resources_unchanged",
        "documents_added",
        "documents_duplicate",
        "documents_unchanged",
        "documents_same_statement",
    ] {
        if let Some(n) = v.get(k).and_then(serde_json::Value::as_u64) {
            out.insert(k.to_string(), n);
        }
    }
    out
}

/// How long a pass waits for another writer before it leaves the
/// household's event decisions pending for the next pass (at most the
/// import's wait).
const EVENT_LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(15);

/// How long a provider request that cannot be written stays pending, from
/// the first pass that could not.
const PROVIDER_TRIES_SECS: i64 = 24 * 3600;

/// What a decision request came to.
struct Decided {
    outcome: &'static str,
    message: String,
    /// For the refreshing machine's own output ([`RequestRow::cause`]).
    cause: Option<String>,
}

impl Decided {
    fn new(outcome: &'static str, message: impl Into<String>) -> Decided {
        Decided {
            outcome,
            message: message.into(),
            cause: None,
        }
    }

    /// Failed at `doing`. Everyone's outcomes show together, so the message
    /// is the caller's own words.
    fn failed(message: &str, doing: &str, error: &CliError) -> Decided {
        Decided {
            cause: Some(format!("{doing}: {}", error.code())),
            ..Decided::new("failed", message)
        }
    }
}

/// Apply what a request says of a provider, as [`decision_request`] does.
fn provider_request(
    folder: &Folder,
    h: &crate::model::Household,
    v: &requests::Valid,
    person: &str,
    ask: &crate::corrections::Ask,
    at: i64,
) -> Decided {
    use crate::corrections::{decide, Done, Failed, Request};
    let Some(p) = h.person(person) else {
        return Decided::new("invalid", "its provider names no person in household.json");
    };
    let by = v.requested_by_name();
    let request = Request {
        id: &v.id,
        by: by.as_deref(),
        at,
    };
    match decide(folder, h, p, ask, Some(&request)) {
        Ok(Done::Recorded(c)) => Decided::new("ok", format!("recorded it as {}", c.id)),
        Ok(Done::Already(c)) => Decided::new("ok", format!("it was already recorded as {}", c.id)),
        Ok(Done::Undone) => Decided::new("ok", "undone"),
        Ok(Done::NothingToUndo) => Decided::new("ok", "there was nothing to undo"),
        Ok(Done::SaidLater) => Decided::new("skipped", "something said later stands"),
        Err(Failed::Refused(r)) => Decided::new("invalid", r.words()),
        Err(Failed::Folder { doing, error }) => Decided::failed(
            "the person's marks could not be read or written",
            doing,
            &error,
        ),
    }
}

/// Apply a request's decision (an event's, or one kept as a mark) through
/// the pass's one write `folder`, whose views follow once, as it closes:
/// `ok` once it is in the person's `overrides.json`, `invalid` when what it
/// names is not that person's (or, for `paid`, not a bill), `failed` when
/// the folder could not be written.
fn decision_request(
    folder: &Folder,
    h: &crate::model::Household,
    v: &requests::Valid,
) -> (&'static str, String) {
    if let Scope::Private {
        person,
        item,
        private,
    } = &v.scope
    {
        let Some(p) = h.person(person) else {
            return (
                "invalid",
                "its private names no person in household.json".into(),
            );
        };
        let by = v.requested_by_name();
        return match crate::private::mark(folder, h, p, item, *private, by.as_deref(), Some(&v.id))
        {
            Ok((Some(m), true)) => ("ok", format!("recorded the mark as {}", m.id)),
            Ok((Some(m), false)) => ("ok", format!("the mark was already recorded as {}", m.id)),
            Ok((None, _)) => ("ok", "the item follows the rules".into()),
            Err(CliError::NotFound(_)) => (
                "invalid",
                "the item it names is not one of the person's".into(),
            ),
            Err(CliError::Usage(m)) => ("invalid", m),
            Err(e) => ("failed", e.message().to_string()),
        };
    }
    if let Scope::Paid {
        person,
        document,
        on,
        amount,
        currency,
    } = &v.scope
    {
        let Some(p) = h.person(person) else {
            return (
                "invalid",
                "its paid names no person in household.json".into(),
            );
        };
        let by = v.requested_by_name();
        let amount = crate::model::Amount {
            amount: amount.clone(),
            currency: currency.clone(),
        };
        return match crate::bills::mark_paid(
            folder,
            p,
            document,
            on,
            &amount,
            by.as_deref(),
            Some(&v.id),
        ) {
            Ok((pm, true)) => ("ok", format!("recorded the payment as {}", pm.id)),
            Ok((pm, false)) => (
                "ok",
                format!("the payment was already recorded as {}", pm.id),
            ),
            Err(CliError::NotFound(_)) => (
                "invalid",
                "the document it names is not one of the person's".into(),
            ),
            Err(CliError::Usage(_)) => ("invalid", "the document it names is not a bill".into()),
            Err(e) => ("failed", e.message().to_string()),
        };
    }
    let Scope::Event {
        person,
        kind,
        members,
    } = &v.scope
    else {
        return ("invalid", "not a decision".into());
    };
    let Some(p) = h.person(person) else {
        return (
            "invalid",
            "its event names no person in household.json".into(),
        );
    };
    let by = v.requested_by_name();
    match crate::events::decide(
        folder,
        p,
        kind,
        &members[0],
        &members[1],
        by.as_deref(),
        Some(&v.id),
    ) {
        Ok((d, true)) => ("ok", format!("recorded {kind} as {}", d.id)),
        Ok((d, false)) => ("ok", format!("{kind} was already recorded as {}", d.id)),
        Err(CliError::NotFound(_)) => (
            "invalid",
            "a member it names is not one of the person's records or documents".into(),
        ),
        Err(e) => ("failed", e.message().to_string()),
    }
}

/// One pass: the scheduled one, or a person's `medrec refresh`.
pub fn pass(cfg: &Config, mode: Mode, pick: Option<&Pick>) -> Result<PassReport, CliError> {
    let location = cfg.folder_location()?;
    let Some(_lock) = state::try_lock(&location)? else {
        return match mode {
            Mode::Scheduled => Ok(PassReport {
                status: "busy".into(),
                ..PassReport::default()
            }),
            Mode::Manual => Err(CliError::Other(
                "a refresh of this folder is running on this machine; try again when it ends"
                    .into(),
            )),
        };
    };
    let bound = &settings(cfg).sources;
    let machine = state::machine()?;
    let mut local = state::load(&location)?;
    let started = time::now();
    if let (Mode::Scheduled, Location::Drive(_)) = (mode, &location) {
        // A key another process replaces while the pass runs is read again
        // within a bound.
        crate::store::bound_key_reads(signin::scheduled_wait());
        if let Some(failure) = crate::store::shared_vault()?.load_key_quietly()?.failure() {
            return Err(failure);
        }
    }
    let store = Store::open(&location, Reads::Fresh, cfg.store_budget())?;
    let every = pass_every(cfg);
    let mut report = PassReport {
        status: "ran".into(),
        ..PassReport::default()
    };

    let listing = pending(&store)?;
    let heartbeat_due = started - local.written_at >= HEARTBEAT_SECS as i64;
    if mode == Mode::Scheduled {
        let due = bound
            .iter()
            .any(|(id, b)| due_by_local(&local, id, b, started));
        // A request file answered already that medrec could not remove
        // costs nothing past the listing.
        let waiting = listing.iter().any(|e| !local.left.contains(&e.rel));
        if !waiting && !due && !heartbeat_due {
            report.status = "idle".into();
            return Ok(report);
        }
    }

    let in_folder: Option<Status> = folder::read_json(&store, STATUS)?;
    if let Some(s) = &local.status {
        check_format(s)?;
    }
    if let Some(s) = &in_folder {
        check_format(s)?;
        if other_refresher(s, &machine, started) {
            if mode == Mode::Scheduled {
                report.status = "other_machine".into();
                return Ok(report);
            }
            return Err(not_the_refresher(s));
        }
    }
    if local.status.is_none() {
        local.status = in_folder.map(|s| adopt(s, &machine));
    }
    let settling = local
        .took_over_at
        .is_some_and(|t| started - t < every as i64);
    if settling && mode == Mode::Manual {
        let from = local.took_over_at.unwrap_or(started) + every as i64;
        return Err(CliError::Other(format!(
            "this machine took refresh over and refreshes from {}, once the other machine has seen it",
            time::fmt(from)
        )));
    }

    let folder = Folder::open_at(&location, Access::Fresh, cfg.store_budget())?;
    let h = folder.household()?;
    let source_of = |id: &str| h.sources.iter().find(|s| s.id == id);

    let mut entries: BTreeMap<String, SourceStatus> = BTreeMap::new();
    for (id, b) in bound {
        let prev = local
            .status
            .as_ref()
            .and_then(|s| s.sources.iter().find(|e| e.source == *id));
        entries.insert(
            id.clone(),
            entry(prev, id, source_of(id), b, &local.declared),
        );
    }

    // Requests, untrusted.
    let known = Known { h: &h, bound };
    let mut parsed: Vec<Parsed> = Vec::new();
    if !settling {
        local.left.retain(|n| listing.iter().any(|e| &e.rel == n));
        local.failing.retain(|id, _| {
            listing
                .iter()
                .any(|e| requests::id_of_name(&e.rel).as_ref() == Some(id))
        });
        let answered: BTreeSet<String> = if listing.is_empty() {
            BTreeSet::new()
        } else {
            store
                .list_here(DONE_DIR)?
                .into_iter()
                .map(|e| e.rel)
                .collect()
        };
        for e in &listing {
            let rel = format!("{REQUESTS_DIR}/{}", e.rel);
            // Answered already, its file left because medrec could not
            // remove it (another Google account's): never served again.
            if answered.contains(&requests::done_name_of(&e.rel)) {
                if store.delete(&rel).is_ok() {
                    local.left.remove(&e.rel);
                } else {
                    local.left.insert(e.rel.clone());
                }
                continue;
            }
            let size = e.size.unwrap_or(0);
            if size > REQUEST_MAX_BYTES {
                parsed.push(requests::too_large(&e.rel, size));
                continue;
            }
            let body = match store.read_bytes(&rel) {
                Ok(b) => b,
                // Cancelled since the listing.
                Err(_) if !store.exists(&rel).unwrap_or(true) => continue,
                Err(e) => return Err(e),
            };
            parsed.push(requests::parse(&e.rel, &body, &known));
        }
    } else {
        report.status = "waiting".into();
    }

    // What to import, and why.
    let mut want: BTreeMap<String, Want> = BTreeMap::new();
    match (mode, pick) {
        (Mode::Scheduled, _) => {
            for (id, b) in bound {
                if due_by_local(&local, id, b, started) && !settling {
                    want.entry(id.clone()).or_default().schedule = true;
                }
            }
        }
        (Mode::Manual, pick) => {
            let ids: Vec<String> = match pick.unwrap_or(&Pick::All) {
                Pick::All => bound.keys().cloned().collect(),
                Pick::Person(q) => {
                    let p = crate::people::resolve(&h, q)?;
                    entries
                        .values()
                        .filter(|e| e.person == p.id)
                        .map(|e| e.source.clone())
                        .collect()
                }
                Pick::Source(s) => {
                    if !bound.contains_key(s) {
                        return Err(CliError::Usage(format!(
                            "source `{s}` is not set up for refresh"
                        ))
                        .with_remedy(format!(
                            "set it up with `medrec --profile <NAME> refresh add --source {s}`"
                        )));
                    }
                    vec![s.clone()]
                }
            };
            if ids.is_empty() {
                return Err(
                    CliError::Usage("no source is set up for refresh here".into()).with_remedy(
                        "set one up with `medrec --profile <NAME> refresh add --source <ID>`",
                    ),
                );
            }
            for id in ids {
                want.entry(id).or_default().manual = true;
            }
        }
    }
    for p in &parsed {
        let Parsed::Valid(v) = p else { continue };
        let at = v.requested_at();
        let ids: Vec<String> = match &v.scope {
            Scope::All => bound.keys().cloned().collect(),
            Scope::Person(pid) => entries
                .values()
                .filter(|e| e.person == *pid)
                .map(|e| e.source.clone())
                .collect(),
            Scope::Source(s) => vec![s.clone()],
            Scope::Event { .. }
            | Scope::Paid { .. }
            | Scope::Private { .. }
            | Scope::Provider { .. } => Vec::new(),
        };
        for id in ids {
            let w = want.entry(id).or_default();
            w.requests.push(v.id.clone());
            w.requested_at = match (w.requested_at, at) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
        }
    }

    let mut writer = Writer {
        store: &store,
        machine: machine.clone(),
        pass_every: every,
        wrote: false,
        names: names(cfg),
    };
    let mut answers: BTreeMap<String, Answer> = BTreeMap::new();
    let mut unfinished: Vec<String> = Vec::new();

    // An attempt left `running` by a pass that stopped: no refresh of this
    // folder runs here now (this pass holds the lock), so it failed.
    for e in entries.values_mut() {
        let Some(last) = e.last_attempt.clone().filter(|a| a.outcome == "running") else {
            continue;
        };
        let a = Attempt {
            schema: ATTEMPT_SCHEMA.into(),
            attempt: last.attempt,
            source: e.source.clone(),
            person: e.person.clone(),
            cli: e.cli.clone(),
            organization: e.organization.clone(),
            started_at: last.started_at,
            finished_at: None,
            trigger: last.trigger,
            requests: last.requests,
            outcome: "failed".into(),
            exit_code: None,
            error: Some(ErrorInfo {
                code: "interrupted".into(),
                message: format!(
                    "the pass running it stopped before it finished (found at {})",
                    time::fmt(started)
                ),
                remedy: None,
                ..ErrorInfo::default()
            }),
            import: None,
            counts: None,
        };
        writer.attempt(&a)?;
        e.failures = e.failures.saturating_add(1);
        e.last_attempt = Some(a.summary());
        if let Some(b) = bound.get(&e.source) {
            e.next_run_at = next_run(e, b);
        }
        report.attempts.push(AttemptRow::of(&a, e));
    }

    // Every source due or asked for signs in and exports first, and only
    // then are the exports filed: a short website session serves every
    // member before uploads to the folder use it up.
    let mut exported: Vec<Exported<'_>> = Vec::new();
    // A failure to write the folder stops the exports, but those already
    // taken are still filed below, so no sign-in is spent for nothing.
    let mut halted: Option<CliError> = None;
    let mut export_all = || -> Result<(), CliError> {
        for (id, w) in &want {
            let Some(b) = bound.get(id) else { continue };
            let t = Try {
                id,
                trigger: if !w.requests.is_empty() {
                    "request"
                } else if w.manual {
                    "manual"
                } else {
                    "schedule"
                },
                requests: &w.requests,
                asked: w.manual || !w.requests.is_empty(),
                now: time::now(),
            };
            let mut e = entries
                .get(id)
                .cloned()
                .unwrap_or_else(|| entry(None, id, source_of(id), b, &local.declared));

            let src = match source_of(id) {
                Some(s) if s.cli.as_deref() == Some(b.cli.as_str()) && plain_cli(&b.cli) => Some(s),
                Some(_) => {
                    let mut a = t.attempt(&e, "failed");
                    a.error = Some(ErrorInfo {
                    code: "provider".into(),
                    message: format!(
                        "household.json names another provider CLI for source {id} than the `{}` it was set up with; not run",
                        b.cli
                    ),
                    remedy: Some(format!(
                        "check the source, then set it up again with `medrec --profile <NAME> refresh add --source {id}`"
                    )),
                    ..ErrorInfo::default()
                });
                    t.record(&writer, &mut e, &a, None, true, &mut report, &mut answers)?;
                    entries.insert(id.clone(), e);
                    continue;
                }
                None => None,
            };
            let Some(src) = src else {
                let mut a = t.attempt(&e, "failed");
                a.error = Some(ErrorInfo {
                    code: "not_found".into(),
                    message: format!("source {id} is no longer in household.json"),
                    remedy: Some(format!(
                        "stop refreshing it with `medrec --profile <NAME> refresh remove {id}`"
                    )),
                    ..ErrorInfo::default()
                });
                t.record(
                    &writer,
                    &mut e,
                    &a,
                    None,
                    t.asked,
                    &mut report,
                    &mut answers,
                )?;
                entries.insert(id.clone(), e);
                continue;
            };
            if b.paused.is_some() {
                let a = t.attempt(&e, "skipped_paused");
                t.record(
                    &writer,
                    &mut e,
                    &a,
                    None,
                    t.asked,
                    &mut report,
                    &mut answers,
                )?;
                entries.insert(id.clone(), e);
                continue;
            }
            // Only a person at the terminal, naming the source, tries it again.
            if e.sign_in.needs_person.is_some() && !w.manual {
                let a = t.attempt(&e, "needs_person");
                t.record(
                    &writer,
                    &mut e,
                    &a,
                    None,
                    t.asked,
                    &mut report,
                    &mut answers,
                )?;
                entries.insert(id.clone(), e);
                continue;
            }

            let min = policy::min_interval(&e.cli) as i64;
            let every_src = policy::every(&e.cli, b.every_secs) as i64;
            let last = latest_import(&folder, &h, src)?;
            let after_failure = e
                .last_attempt
                .as_ref()
                .filter(|a| a.outcome == "failed")
                .and_then(|a| time::parse(&a.started_at))
                .map(|x| x + backoff(e.failures, min as u64) as i64);
            let next_allowed = last.as_ref().map(|(_, x)| x + min).max(after_failure);
            // The schedule's next import, counting one run by hand.
            let due_at = last.as_ref().map(|(_, x)| x + every_src).max(next_allowed);
            // A person's request waits only its own guard, from the latest
            // import, run of the provider or failed attempt, whatever asked.
            let guard = policy::REQUEST_GUARD as i64;
            let ran_at = local.ran_at.get(id.as_str()).copied();
            let failed_at = e
                .last_attempt
                .as_ref()
                .filter(|a| a.outcome == "failed" || a.outcome == "running")
                .and_then(|a| time::parse(&a.started_at));
            let guard_until = last
                .as_ref()
                .map(|(_, x)| *x)
                .max(ran_at)
                .max(failed_at)
                .map(|x| x + guard);

            // A request a cut-short pass left: answered by the import since.
            if !w.manual && !w.requests.is_empty() {
                if let (Some((import_id, imported)), Some(asked_at)) = (&last, w.requested_at) {
                    if *imported >= asked_at {
                        let attempt = e
                            .last_success
                            .as_ref()
                            .filter(|s| s.import == *import_id)
                            .map(|s| s.attempt.clone());
                        answers.insert(
                            id.clone(),
                            Answer {
                                outcome: "ok".into(),
                                attempt,
                                next_allowed_at: Some(time::fmt(imported + guard)),
                            },
                        );
                        entries.insert(id.clone(), e);
                        continue;
                    }
                }
            }
            if t.asked {
                if guard_until.is_some_and(|n| t.now < n) {
                    let a = t.attempt(&e, "skipped_interval");
                    t.record(
                        &writer,
                        &mut e,
                        &a,
                        guard_until,
                        true,
                        &mut report,
                        &mut answers,
                    )?;
                    if let Some(n) = e.next_run_at.as_deref().and_then(time::parse) {
                        e.next_run_at = due_at.max(Some(n)).map(time::fmt);
                    }
                    entries.insert(id.clone(), e);
                    continue;
                }
            } else {
                // On its schedule: not due after all when an import the local
                // record missed, or a failure's wait, says later.
                if due_at.is_some_and(|d| t.now < d) {
                    e.next_run_at = due_at.map(time::fmt);
                    entries.insert(id.clone(), e);
                    continue;
                }
            }

            let mut a = t.attempt(&e, "running");
            a.finished_at = None;
            let before = e.last_attempt.clone();
            e.last_attempt = Some(a.summary());
            entries.insert(id.clone(), e.clone());
            writer.status(&entries, &mut local)?;

            let provider = import::probe(&b.cli);
            let ready = match prepare(mode, &provider, src, b, &mut e, &mut a, &mut local.declared)
            {
                Ok(req) => match import::export(&folder, cfg, &provider, &req) {
                    Ok(spooled) => Ok((req, spooled)),
                    Err(err) => Err(import_failed(&err, &b.cli, &src.id, &mut e, &mut a)),
                },
                Err(outcome) => Err(outcome),
            };
            let exported_at = ready.is_ok().then(time::now);
            exported.push(Exported {
                id,
                t,
                b,
                e,
                a,
                before,
                min,
                exported_at,
                ready,
            });
        }
        Ok(())
    };
    if let Err(err) = export_all() {
        halted = Some(err);
    }

    // The views once, after every source is filed, not after each.
    let views = crate::viewfiles::hold(&location, cfg.store_budget());
    for x in exported {
        let Exported {
            id,
            t,
            b,
            mut e,
            mut a,
            before,
            min,
            exported_at,
            ready,
        } = x;
        let id = &id.to_string();
        let guard = policy::REQUEST_GUARD as i64;
        let outcome = match ready {
            Ok((req, spooled)) => file(cfg, &req, spooled, &mut e, &mut a),
            Err(outcome) => outcome,
        };
        // An export counts toward a request's guard, from when it ended,
        // once it is filed or failed; one a busy folder threw away does not.
        if let Some(at) = exported_at.filter(|_| outcome != BUSY) {
            local.ran_at.insert(id.clone(), at);
        }
        if outcome == BUSY {
            // Another writer held the folder: no attempt, and its requests
            // stay pending for the next pass.
            e.last_attempt = before;
            unfinished.push(id.clone());
            report.attempts.push(AttemptRow::of(&a, &e));
            entries.insert(id.clone(), e);
            if let Err(err) = writer.status(&entries, &mut local) {
                halted.get_or_insert(err);
            }
            continue;
        }
        a.finished_at = Some(time::fmt(time::now()));
        a.outcome = outcome.clone();
        match outcome.as_str() {
            "ok" => {
                e.failures = 0;
                e.sign_in.needs_person = None;
            }
            "failed" => e.failures = e.failures.saturating_add(1),
            _ => {}
        }
        let next = match outcome.as_str() {
            "ok" | "failed" if t.asked => Some(t.now + guard),
            "ok" => Some(time::now() + min),
            "failed" => Some(time::now() + backoff(e.failures, min as u64) as i64),
            _ => None,
        };
        // Each spool is filed whatever happens to the status writes; the
        // first failure ends the pass once all are.
        if let Err(err) = t.record(&writer, &mut e, &a, next, true, &mut report, &mut answers) {
            halted.get_or_insert(err);
        }
        e.next_run_at = next_run(&e, b);
        entries.insert(id.clone(), e);
        if let Err(err) = writer.status(&entries, &mut local) {
            halted.get_or_insert(err);
        }
    }
    drop(views);
    if let Some(err) = halted {
        // Keep this machine's record of what ran, so the guard holds for
        // the exports filed; the first failure is the one reported.
        let _ = state::save(&location, &local);
        return Err(err);
    }

    // Each request's outcome, then the pending file goes. The event
    // decisions share one write handle, opened at the first, so the views
    // follow them once; while another writer holds the folder they stay
    // pending for the next pass.
    let picked = time::fmt(started);
    let mut events_folder: Option<Result<Folder, CliError>> = None;
    // People with a provider request left pending: their later ones wait
    // behind it, so that they are said in the order they were asked.
    let mut behind: BTreeSet<&str> = BTreeSet::new();
    for p in &parsed {
        if let Parsed::Valid(v) = p {
            let waits = want
                .iter()
                .any(|(id, w)| w.requests.contains(&v.id) && unfinished.contains(id));
            if waits {
                continue;
            }
            if v.scope.is_decision() {
                let f = events_folder.get_or_insert_with(|| {
                    Folder::open_at_within(
                        &location,
                        Access::Write,
                        cfg.store_budget(),
                        EVENT_LOCK_WAIT.min(cfg.import_timeout()),
                    )
                });
                if f.as_ref().is_err_and(folder::is_lock_busy) {
                    continue;
                }
            }
        }
        let mut cause = None;
        let (sources, outcome, message): (Vec<HandledSource>, String, String) = match p {
            Parsed::Invalid(i) => (Vec::new(), "invalid".into(), clip(&i.why, MESSAGE_MAX)),
            Parsed::Valid(v) if v.scope.is_decision() => {
                let row = |outcome: &str, cause: Option<String>| RequestRow {
                    request: v.id.clone(),
                    outcome: outcome.into(),
                    cause,
                    file_left: false,
                };
                let d = match (&v.scope, events_folder.as_ref()) {
                    (Scope::Provider { person, .. }, _) if behind.contains(person.as_str()) => {
                        let why = "behind an earlier one of the person's";
                        report.requests.push(row("waiting", Some(why.into())));
                        continue;
                    }
                    (Scope::Provider { person, ask }, Some(Ok(f))) => {
                        // The store's time for its file, which is the same at
                        // each try and is not the asking device's clock. Never
                        // later than the first try.
                        let file = listing.iter().find(|e| e.rel == v.name);
                        let asked = file.and_then(|e| e.modified).or(v.requested_at());
                        let tried = local.failing.get(&v.id).copied();
                        let tried = tried.unwrap_or_else(time::now);
                        let at = asked.map_or(tried, |t| t.min(tried));
                        provider_request(f, &h, v, person, ask, at)
                    }
                    (Scope::Provider { .. }, Some(Err(e))) => Decided::failed(
                        "the folder could not be opened to write",
                        "opening the folder to write",
                        e,
                    ),
                    (_, Some(Ok(f))) => {
                        let (outcome, message) = decision_request(f, &h, v);
                        Decided::new(outcome, message)
                    }
                    (_, Some(Err(e))) => Decided::new("failed", e.message()),
                    (_, None) => Decided::new("failed", "the folder was not opened to write"),
                };
                // Its `done/` file would keep nothing of what it says, so one
                // that could not be written is tried again for a while.
                if let (Scope::Provider { person, .. }, "failed") = (&v.scope, d.outcome) {
                    let now = time::now();
                    let first = *local.failing.entry(v.id.clone()).or_insert(now);
                    if (0..PROVIDER_TRIES_SECS).contains(&(now - first)) {
                        behind.insert(person);
                        report.requests.push(row("retrying", d.cause));
                        continue;
                    }
                }
                local.failing.remove(&v.id);
                cause = d.cause;
                (Vec::new(), d.outcome.into(), clip(&d.message, MESSAGE_MAX))
            }
            Parsed::Valid(v) => {
                let ids: Vec<&String> = want
                    .iter()
                    .filter(|(_, w)| w.requests.contains(&v.id))
                    .map(|(id, _)| id)
                    .collect();
                let sources: Vec<HandledSource> = ids
                    .iter()
                    .map(|id| {
                        let a = answers.get(*id);
                        HandledSource {
                            source: (*id).clone(),
                            outcome: a
                                .map(|a| a.outcome.clone())
                                .unwrap_or_else(|| "failed".into()),
                            attempt: a.and_then(|a| a.attempt.clone()),
                            next_allowed_at: a.and_then(|a| a.next_allowed_at.clone()),
                        }
                    })
                    .collect();
                let outcome = request_outcome(&sources).to_string();
                let message = summarize(&sources);
                (sources, outcome, message)
            }
        };
        let mut handled = Handled {
            picked_up_at: picked.clone(),
            finished_at: time::fmt(time::now()),
            outcome: outcome.clone(),
            sources,
            message,
            by: crate::model::filed_by(),
        };
        let mut doc = requests::done(p, &handled);
        while serde_json::to_vec(&doc).map(|v| v.len()).unwrap_or(0) > DONE_MAX_BYTES - 512
            && !handled.sources.is_empty()
        {
            handled.sources.pop();
            handled.message = format!("{} (some sources left out of this file)", handled.message);
            doc = requests::done(p, &handled);
        }
        let done_name = match p {
            Parsed::Valid(v) => v.name.clone(),
            Parsed::Invalid(i) => i.done_name.clone(),
        };
        folder::write_json(&store, &format!("{DONE_DIR}/{done_name}"), &doc)?;
        // A file another Google account owns may not be removable by the
        // folder's: done/ already says it is handled, so it stays.
        let file_left = store
            .delete(&format!("{REQUESTS_DIR}/{}", p.name()))
            .is_err();
        if file_left {
            local.left.insert(p.name().to_string());
        }
        report.requests.push(RequestRow {
            file_left,
            request: match p {
                Parsed::Valid(v) => v.id.clone(),
                Parsed::Invalid(i) => i.id.clone().unwrap_or_else(|| i.done_name.clone()),
            },
            outcome,
            cause,
        });
    }

    // The event decisions' views, once.
    drop(events_folder);

    // Old outcomes go, once a day.
    if started - local.pruned_at >= 86400 {
        // An answer whose request file is still there is what keeps that
        // request from being served again: it stays as long as the file.
        let twins: BTreeSet<String> = listing
            .iter()
            .map(|e| requests::done_name_of(&e.rel))
            .collect();
        for e in store.list(DONE_DIR)? {
            if twins.contains(&e.rel) {
                continue;
            }
            if e.modified.is_some_and(|m| started - m > DONE_KEEP_SECS) {
                let _ = store.delete(&format!("{DONE_DIR}/{}", e.rel));
            }
        }
        local.pruned_at = started;
    }

    if !writer.wrote
        && (heartbeat_due
            || !parsed.is_empty()
            || !want.is_empty()
            || mode == Mode::Manual
            || local.status.is_none())
    {
        writer.status(&entries, &mut local)?;
    }
    if !settling {
        // This pass found its id still in place a full pass after taking
        // over (or never took over): it serves requests from now on.
        local.took_over_at = None;
    }
    report.status_written = writer.wrote;
    state::save(&location, &local)?;
    Ok(report)
}

/// One source's turn in a pass.
struct Try<'a> {
    id: &'a str,
    trigger: &'static str,
    requests: &'a [String],
    /// A person or a request asked for it (a skip is recorded only then).
    asked: bool,
    now: i64,
}

impl Try<'_> {
    fn attempt(&self, e: &SourceStatus, outcome: &str) -> Attempt {
        Attempt {
            schema: ATTEMPT_SCHEMA.into(),
            attempt: crate::ids::new("t"),
            source: self.id.to_string(),
            person: e.person.clone(),
            cli: e.cli.clone(),
            organization: e.organization.clone(),
            started_at: time::fmt(self.now),
            finished_at: Some(time::fmt(self.now)),
            trigger: self.trigger.into(),
            requests: self.requests.to_vec(),
            outcome: outcome.into(),
            exit_code: None,
            error: None,
            import: None,
            counts: None,
        }
    }

    /// `a` as the source's latest attempt, its line in `attempts/` when
    /// `logged`, its row in the pass's report, and its answer to requests.
    #[allow(clippy::too_many_arguments)]
    fn record(
        &self,
        writer: &Writer<'_>,
        e: &mut SourceStatus,
        a: &Attempt,
        next_allowed: Option<i64>,
        logged: bool,
        report: &mut PassReport,
        answers: &mut BTreeMap<String, Answer>,
    ) -> Result<(), CliError> {
        if logged {
            writer.attempt(a)?;
            e.last_attempt = Some(a.summary());
        }
        report.attempts.push(AttemptRow::of(a, e));
        answers.insert(
            self.id.to_string(),
            Answer {
                outcome: a.outcome.clone(),
                attempt: logged.then(|| a.attempt.clone()),
                next_allowed_at: next_allowed.map(time::fmt),
            },
        );
        Ok(())
    }
}

impl AttemptRow {
    fn of(a: &Attempt, e: &SourceStatus) -> AttemptRow {
        let count = |k: &str| a.counts.as_ref().and_then(|c| c.get(k).copied());
        AttemptRow {
            source: a.source.clone(),
            cli: a.cli.clone(),
            trigger: a.trigger.clone(),
            outcome: a.outcome.clone(),
            error: a.error.as_ref().map(|x| x.code.clone()),
            records_added: count("records_added"),
            records_updated: count("records_updated"),
            documents_added: count("documents_added"),
            command: e.sign_in.needs_person.as_ref().map(|n| n.command.clone()),
        }
    }
}

/// One line for a person about a request's sources, with no record content.
fn summarize(sources: &[HandledSource]) -> String {
    if sources.is_empty() {
        return "no source set up for refresh covers it".into();
    }
    let mut n: BTreeMap<&str, usize> = BTreeMap::new();
    for s in sources {
        *n.entry(s.outcome.as_str()).or_default() += 1;
    }
    let plural = |k: usize| if k == 1 { "source" } else { "sources" };
    let parts: Vec<String> = [
        ("ok", "imported"),
        ("failed", "failed"),
        ("needs_person", "need a person"),
        ("skipped_interval", "skipped by their interval"),
        ("skipped_paused", "paused"),
    ]
    .iter()
    .filter_map(|(k, what)| n.get(k).map(|c| format!("{c} {} {what}", plural(*c))))
    .collect();
    parts.join(", ")
}

/// A source a pass runs, between its export and its filing.
struct Exported<'a> {
    id: &'a str,
    t: Try<'a>,
    b: &'a Binding,
    e: SourceStatus,
    a: Attempt,
    before: Option<AttemptSummary>,
    min: i64,
    /// When its export ended, if it ran.
    exported_at: Option<i64>,
    /// Its request and spooled export, or the outcome that ended it
    /// before filing.
    ready: Result<(import::Request, import::Spooled), String>,
}

/// The provider's usability and sign-in, then the import's request; or the
/// attempt's outcome when it ends here, with `a` and `e` filled in.
#[allow(clippy::too_many_arguments)]
fn prepare(
    mode: Mode,
    provider: &Provider,
    src: &Source,
    b: &Binding,
    e: &mut SourceStatus,
    a: &mut Attempt,
    declared: &mut BTreeMap<String, signin::Declared>,
) -> Result<import::Request, String> {
    let command = sign_in_command(&src.id);
    let needs = |reason: &str, e: &mut SourceStatus| {
        e.sign_in.needs_person = Some(NeedsPerson {
            reason: reason.into(),
            since: e
                .sign_in
                .needs_person
                .as_ref()
                .filter(|n| n.reason == reason)
                .map(|n| n.since.clone())
                .unwrap_or_else(|| time::fmt(time::now())),
            command: command.clone(),
        });
    };
    if !provider.usable() {
        a.error = Some(ErrorInfo {
            code: "provider".into(),
            message: clip(
                &format!(
                    "`{}` cannot export records ({}{})",
                    provider.cli,
                    provider.status,
                    provider
                        .note
                        .as_deref()
                        .map(|n| format!(": {n}"))
                        .unwrap_or_default()
                ),
                MESSAGE_MAX,
            ),
            remedy: None,
            ..ErrorInfo::default()
        });
        return Err("failed".into());
    }
    if provider.login_profiles {
        let wait = match mode {
            Mode::Scheduled => signin::scheduled_wait(),
            Mode::Manual => signin::PERSON_WAIT,
        };
        e.sign_in.checked_at = Some(time::fmt(time::now()));
        match signin::read(provider, b.provider_profile.as_deref(), wait) {
            signin::Read::Read {
                state,
                expires_at,
                declared: said,
            } => {
                e.sign_in.state = state.into();
                e.sign_in.expires_at = expires_at;
                declared.insert(provider.cli.clone(), said);
                (e.sign_in.renews, e.sign_in.lasts_secs) = sign_in_life(&provider.cli, declared);
                if !signin::usable(state) {
                    needs("sign_in", e);
                    a.error = Some(ErrorInfo {
                        code: "needs_person".into(),
                        message: format!("{} needs a sign-in ({state})", provider.cli),
                        remedy: Some(format!("run `{command}` on this machine")),
                        ..ErrorInfo::default()
                    });
                    return Err("needs_person".into());
                }
                e.sign_in.needs_person = None;
            }
            signin::Read::TimedOut => {
                e.sign_in.state = "unknown".into();
                needs("keychain", e);
                a.error = Some(ErrorInfo {
                    code: "timeout".into(),
                    message: format!(
                        "`{} auth status` gave no answer within {} s: it is waiting on an approval (a Keychain prompt) nobody gave",
                        provider.cli,
                        wait.as_secs()
                    ),
                    remedy: Some(format!("run `{command}` on this machine and approve it")),
                    ..ErrorInfo::default()
                });
                return Err("needs_person".into());
            }
            signin::Read::Failed(msg) => {
                e.sign_in.state = "unknown".into();
                a.error = Some(ErrorInfo {
                    code: "auth_status".into(),
                    message: clip(&msg, MESSAGE_MAX),
                    remedy: None,
                    ..ErrorInfo::default()
                });
                return Err("failed".into());
            }
        }
    } else {
        e.sign_in.state = "not_needed".into();
        e.sign_in.expires_at = None;
        e.sign_in.needs_person = None;
    }
    let from_profile = match b.provider_profile.as_deref().map(ProfileName::parse) {
        Some(Ok(p)) => Some(p),
        Some(Err(err)) => {
            a.error = Some(ErrorInfo {
                code: "usage".into(),
                message: clip(
                    &format!("the provider profile in this source's binding: {err}"),
                    MESSAGE_MAX,
                ),
                remedy: None,
                ..ErrorInfo::default()
            });
            return Err("failed".into());
        }
        None => None,
    };
    let patient = b
        .patient
        .clone()
        .or_else(|| src.patient_id.clone())
        .or_else(|| src.patient_label.clone())
        .unwrap_or_default();
    let req = import::Request {
        from: provider.cli.clone(),
        from_profile,
        files: None,
        patient,
        person: Some(src.person.clone()),
        since: None,
        org: b.org.clone(),
        allow_birth_date_mismatch: false,
    };
    Ok(req)
}

/// An export or import that failed: `needs_person` when the provider
/// answered that its sign-in ended, with medrec's own instruction; else
/// `failed`.
fn import_failed(
    err: &CliError,
    cli: &str,
    source: &str,
    e: &mut SourceStatus,
    a: &mut Attempt,
) -> String {
    a.exit_code = Some(err.exit_code());
    if !err.is_auth() {
        a.error = Some(error_info(err));
        return "failed".into();
    }
    let command = sign_in_command(source);
    // The provider's remedy names its own sign-in command; the one to run is
    // medrec's, which also binds the source.
    a.error = Some(ErrorInfo {
        code: "needs_person".into(),
        message: format!("{cli} needs a sign-in: its export answered that the sign-in has ended"),
        remedy: Some(format!("run `{command}` on this machine")),
        // Present on every needs_person error, empty when the provider said
        // nothing: the key is what marks the error as medrec's own.
        provider_message: Some(clip(err.message(), MESSAGE_MAX)),
    });
    e.sign_in.needs_person = Some(NeedsPerson {
        reason: "sign_in".into(),
        since: e
            .sign_in
            .needs_person
            .as_ref()
            .filter(|n| n.reason == "sign_in")
            .map(|n| n.since.clone())
            .unwrap_or_else(|| time::fmt(time::now())),
        command,
    });
    "needs_person".into()
}

/// File a spooled export: the attempt's outcome, with `a` and `e` filled
/// in.
fn file(
    cfg: &Config,
    req: &import::Request,
    spooled: import::Spooled,
    e: &mut SourceStatus,
    a: &mut Attempt,
) -> String {
    let folder = match Folder::open(cfg, Access::Fresh) {
        Ok(f) => f,
        Err(err) => {
            a.error = Some(error_info(&err));
            return "failed".into();
        }
    };
    match import::file_spooled(&folder, cfg, req, spooled) {
        Err(err) if folder::is_lock_busy(&err) => {
            a.outcome = BUSY.into();
            BUSY.into()
        }
        Ok(r) => {
            a.exit_code = Some(0);
            a.import = Some(r.import.clone());
            let c = counts(&r);
            a.counts = Some(c.clone());
            e.last_success = Some(Success {
                attempt: a.attempt.clone(),
                finished_at: time::fmt(time::now()),
                import: r.import.clone(),
                counts: c,
                withheld_types: r.withheld_types.clone(),
            });
            "ok".into()
        }
        Err(err) => {
            let source = a.source.clone();
            import_failed(&err, &req.from, &source, e, a)
        }
    }
}

// ---- setup ----------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct Installed {
    pub profile: String,
    pub agent: String,
    pub pass_every_secs: u64,
    pub log: String,
    pub machine: String,
    pub took_over: bool,
}

/// Create `_status/`, `_requests/` and `_requests/done/`, write
/// `status.json` as this machine's, and load the LaunchAgent.
pub fn install(
    cfg: &mut Config,
    every: Option<u64>,
    take_over: bool,
) -> Result<Installed, CliError> {
    let profile = cfg
        .profile
        .clone()
        .ok_or_else(|| CliError::Other("no profile selected".into()))?;
    let location = cfg.folder_location()?;
    let store = Store::open(&location, Reads::Fresh, cfg.store_budget())?;
    // The folder must be a records folder.
    Folder::open_at(&location, Access::Fresh, cfg.store_budget())?;
    let machine = state::machine()?;
    let now = time::now();
    let in_folder: Option<Status> = folder::read_json(&store, STATUS)?;
    let mut took_over = false;
    if let Some(s) = &in_folder {
        check_format(s)?;
        if s.refresher.machine != machine {
            if other_refresher(s, &machine, now) && !take_over {
                return Err(not_the_refresher(s));
            }
            took_over = true;
        }
    }
    let every = every.unwrap_or_else(|| pass_every(cfg)).max(60);
    cfg.active.refresh.pass_every_secs = Some(every);
    for dir in [STATUS_DIR, ATTEMPTS_DIR, REQUESTS_DIR, DONE_DIR] {
        store.mkdir(dir)?;
    }
    let mut local = state::load(&location)?;
    if local.status.is_none() {
        local.status = in_folder.clone().map(|s| adopt(s, &machine));
    }
    let entries = current_entries(
        cfg,
        &local,
        &Folder::open_at(&location, Access::Fresh, cfg.store_budget())?.household()?,
    );
    let mut writer = Writer {
        store: &store,
        machine: machine.clone(),
        pass_every: every,
        wrote: false,
        names: names(cfg),
    };
    // A take-over writes over the other machine's id on purpose.
    let status = Status {
        schema: STATUS_SCHEMA.into(),
        format: FORMAT,
        updated_at: time::fmt(now),
        refresher: Refresher {
            machine: machine.clone(),
            medrec_version: env!("CARGO_PKG_VERSION").into(),
            last_pass_at: Some(time::fmt(now)),
            next_pass_at: Some(time::fmt(now + every as i64)),
            pass_every_secs: every,
            heartbeat_every_secs: HEARTBEAT_SECS,
        },
        sources: writer.sources(&entries),
    };
    if took_over {
        folder::write_json(&store, STATUS, &status)?;
        local.status = Some(status);
        local.written_at = now;
        local.took_over_at = Some(now);
    } else {
        writer.status(&entries, &mut local)?;
    }
    state::save(&location, &local)?;
    let agent = launchd::install(profile.as_str(), every)?;
    Ok(Installed {
        profile: profile.to_string(),
        agent: agent.to_string_lossy().into_owned(),
        pass_every_secs: every,
        log: launchd::log_path(profile.as_str())?
            .to_string_lossy()
            .into_owned(),
        machine,
        took_over,
    })
}

#[derive(Debug, Serialize)]
pub struct Uninstalled {
    pub profile: String,
    pub removed: bool,
}

pub fn uninstall(cfg: &Config) -> Result<Uninstalled, CliError> {
    let profile = cfg
        .profile
        .clone()
        .ok_or_else(|| CliError::Other("no profile selected".into()))?;
    Ok(Uninstalled {
        removed: launchd::uninstall(profile.as_str())?,
        profile: profile.to_string(),
    })
}

/// Every bound source's entry, from what this machine last wrote.
fn current_entries(
    cfg: &Config,
    local: &state::Local,
    h: &Household,
) -> BTreeMap<String, SourceStatus> {
    settings(cfg)
        .sources
        .iter()
        .map(|(id, b)| {
            let prev = local
                .status
                .as_ref()
                .and_then(|s| s.sources.iter().find(|e| e.source == *id));
            (
                id.clone(),
                entry(
                    prev,
                    id,
                    h.sources.iter().find(|s| s.id == *id),
                    b,
                    &local.declared,
                ),
            )
        })
        .collect()
}

/// After a binding changed: `status.json` again, when this machine is the
/// refresher (refresh is installed here). Nothing is imported.
pub fn publish(cfg: &Config) -> Result<bool, CliError> {
    let location = cfg.folder_location()?;
    let mut local = state::load(&location)?;
    if local.status.is_none() {
        return Ok(false);
    }
    let Some(_lock) = state::try_lock(&location)? else {
        return Ok(false);
    };
    let store = Store::open(&location, Reads::Fresh, cfg.store_budget())?;
    let h = Folder::open_at(&location, Access::Fresh, cfg.store_budget())?.household()?;
    let entries = current_entries(cfg, &local, &h);
    let mut writer = Writer {
        store: &store,
        machine: state::machine()?,
        pass_every: pass_every(cfg),
        wrote: false,
        names: names(cfg),
    };
    writer.status(&entries, &mut local)?;
    state::save(&location, &local)?;
    Ok(true)
}

#[derive(Debug, Serialize)]
pub struct Bound {
    pub source: String,
    pub person: String,
    pub cli: String,
    pub provider_profile: Option<String>,
    pub auto: bool,
    pub paused: Option<String>,
    pub every_secs: u64,
    pub min_interval_secs: u64,
}

/// `refresh add`: the explicit binding of a source to a provider login.
/// `wanted.paused` is ignored: a binding made again keeps its pause.
pub fn add(
    cfg: &mut Config,
    h: &Household,
    source: &str,
    wanted: Binding,
) -> Result<Bound, CliError> {
    let provider_profile = wanted.provider_profile.as_deref();
    let src = h
        .sources
        .iter()
        .find(|s| s.id == source)
        .ok_or_else(|| CliError::NotFound(format!("no source `{source}` in household.json")))?;
    let cli = source_cli(src)?;
    let provider = import::probe(&cli);
    if !provider.usable() {
        return Err(CliError::Usage(format!(
            "`{cli}` cannot export records ({}{})",
            provider.status,
            provider
                .note
                .as_deref()
                .map(|n| format!(": {n}"))
                .unwrap_or_default()
        )));
    }
    match (provider.login_profiles, provider_profile) {
        (true, None) => {
            return Err(CliError::Usage(format!(
                "`{cli}` signs in, so the binding names its login: --provider-profile <NAME>"
            )))
        }
        (true, Some(p)) => {
            ProfileName::parse(p)
                .map_err(|e| CliError::Usage(format!("--provider-profile: {e}")))?;
        }
        (false, Some(_)) => {
            return Err(CliError::Usage(format!(
                "`{cli}` needs no login, so it takes no --provider-profile"
            )))
        }
        (false, None) => {}
    }
    let min = policy::min_interval(&cli);
    if let Some(e) = wanted.every_secs {
        if e < min {
            return Err(CliError::Usage(format!(
                "`{cli}` may be imported at most every {} h; --every is shorter",
                min / 3600
            )));
        }
    }
    let b = Binding {
        cli: cli.clone(),
        paused: cfg
            .active
            .refresh
            .sources
            .get(source)
            .and_then(|b| b.paused.clone()),
        ..wanted
    };
    cfg.active
        .refresh
        .sources
        .insert(source.to_string(), b.clone());
    Ok(bound_row(source, src.person.clone(), &cli, &b))
}

/// The provider CLI a source imports from, as a plain command name.
fn source_cli(src: &Source) -> Result<String, CliError> {
    let cli = src.cli.clone().ok_or_else(|| {
        CliError::Usage(format!(
            "source `{}` has no provider CLI (it is a {} source); only an import refreshes",
            src.id, src.kind
        ))
    })?;
    if !plain_cli(&cli) {
        return Err(CliError::Usage(format!(
            "source `{}` names its provider CLI as `{cli}`, which is not a plain command name",
            src.id
        )));
    }
    Ok(cli)
}

fn bound_row(source: &str, person: String, cli: &str, b: &Binding) -> Bound {
    Bound {
        source: source.into(),
        person,
        cli: cli.into(),
        provider_profile: b.provider_profile.clone(),
        auto: b.auto,
        paused: b.paused.as_ref().map(|p| p.reason.clone()),
        every_secs: policy::every(cli, b.every_secs),
        min_interval_secs: policy::min_interval(cli),
    }
}

/// The bindings, from this machine's config alone (no network).
pub fn list(cfg: &Config) -> Result<Vec<serde_json::Value>, CliError> {
    let location = cfg.folder_location()?;
    let local = state::load(&location)?;
    Ok(settings(cfg)
        .sources
        .iter()
        .map(|(id, b)| {
            let e = local
                .status
                .as_ref()
                .and_then(|s| s.sources.iter().find(|e| e.source == *id));
            let cli = b.cli.clone();
            serde_json::json!({
                "source": id,
                "cli": cli,
                "provider_profile": b.provider_profile,
                "auto": b.auto,
                "paused": b.paused.as_ref().map(|p| p.reason.clone()),
                "every_secs": policy::every(&cli, b.every_secs),
                "last_outcome": e.and_then(|e| e.last_attempt.as_ref().map(|a| a.outcome.clone())),
                "next_run_at": e.and_then(|e| e.next_run_at.clone()),
                "needs_person": e.and_then(|e| e.sign_in.needs_person.as_ref().map(|n| n.command.clone())),
            })
        })
        .collect())
}

/// Which profile binds `source`, for `refresh sign-in` (the coordinator's
/// ruling): exactly one must, and a given `--profile` must be it.
pub fn binding_profile(
    cfg: &Config,
    source: &str,
    given: Option<&str>,
) -> Result<ProfileName, CliError> {
    let mut hits: Vec<ProfileName> = Vec::new();
    for name in cfg.profiles.names() {
        let s: crate::config::FolderSettings = cfg.profiles.settings(name)?;
        if s.refresh.sources.contains_key(source) {
            hits.push(name.clone());
        }
    }
    let list = |v: &[ProfileName]| {
        v.iter()
            .map(|p| p.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    };
    match (hits.as_slice(), given) {
        ([one], None) => Ok(one.clone()),
        ([one], Some(g)) if one.as_str() == g => Ok(one.clone()),
        ([one], Some(g)) => Err(CliError::Usage(format!(
            "source `{source}` is set up for refresh in profile `{one}`, not `{g}`"
        ))),
        ([], _) => Err(CliError::Usage(format!(
            "no profile on this machine sets up source `{source}` for refresh"
        ))
        .with_remedy(format!(
            "set it up with `medrec --profile <NAME> refresh add --source {source} --provider-profile <LOGIN>`"
        ))),
        (many, _) => Err(CliError::Usage(format!(
            "source `{source}` is set up for refresh in more than one profile: {}; name one with --profile",
            list(many)
        ))),
    }
}

#[derive(Debug, Serialize)]
pub struct SignedIn {
    pub source: String,
    pub profile: String,
    pub provider_profile: Option<String>,
    pub state: String,
    pub needs_person: bool,
    pub logged_in: bool,
}

/// `refresh sign-in`: read the provider's sign-in with a person at the
/// terminal; run the provider's own sign-in command (its `login_hint`, for
/// this login) when it is not usable or the provider refused the last
/// export; clear `needs_person` once it is usable and, after a refusal,
/// the sign-in succeeded.
pub fn sign_in(cfg: &Config, source: &str) -> Result<SignedIn, CliError> {
    let profile = cfg
        .profile
        .clone()
        .ok_or_else(|| CliError::Other("no profile selected".into()))?;
    let b =
        settings(cfg).sources.get(source).cloned().ok_or_else(|| {
            CliError::Usage(format!("source `{source}` is not set up for refresh"))
        })?;
    let location = cfg.folder_location()?;
    let Some(_lock) = state::try_lock(&location)? else {
        return Err(CliError::Other(
            "a refresh of this folder is running on this machine; try again when it ends".into(),
        ));
    };
    let h = Folder::open_at(&location, Access::Fresh, cfg.store_budget())?.household()?;
    let src = h
        .sources
        .iter()
        .find(|s| s.id == source)
        .ok_or_else(|| CliError::NotFound(format!("no source `{source}` in household.json")))?;
    let cli = source_cli(src)?;
    if cli != b.cli {
        return Err(CliError::Usage(format!(
            "household.json names `{cli}` for source `{source}`, set up for refresh with `{}`",
            b.cli
        ))
        .with_remedy(format!(
            "check the source, then set it up again with `medrec --profile <NAME> refresh add --source {source}`"
        )));
    }
    let provider = import::probe(&cli);
    if !provider.usable() {
        return Err(CliError::Usage(format!(
            "`{cli}` cannot export records ({})",
            provider.status
        )));
    }
    let pp = b.provider_profile.as_deref();
    let mut local = state::load(&location)?;
    let store = Store::open(&location, Reads::Fresh, cfg.store_budget())?;
    let mut entries = current_entries(cfg, &local, &h);
    // The provider refused this login's last export, whatever its status
    // reads: a member site's session can read as valid on this Mac and be
    // dead on the site. Only a new sign-in settles it.
    let refused = entries
        .get(source)
        .and_then(|e| e.sign_in.needs_person.as_ref())
        .is_some_and(|n| n.reason == "sign_in");
    let mut logged_in = false;
    let state = if provider.login_profiles {
        let mut read = signin::read(&provider, pp, signin::PERSON_WAIT);
        let ok = matches!(&read, signin::Read::Read { state, .. } if signin::usable(state));
        if !ok || refused {
            let fill = signin::Fill {
                profile: pp,
                org: b.org.as_deref().or(src.organization.as_deref()),
                label: src.patient_label.as_deref(),
            };
            let mut args = signin::login_args(&cli, provider.login_hint.as_deref(), fill)
                .map_err(|m| {
                    CliError::Usage(m).with_remedy(format!(
                        "sign in with `{cli}`'s own `auth login`, then run `medrec refresh sign-in {source}` again"
                    ))
                })?;
            // The person asked for a new sign-in: it replaces the stored one.
            if !args.iter().any(|a| a == "--overwrite") && signin::login_overwrites(&provider) {
                args.push("--overwrite".into());
            }
            let code = signin::login(&provider, &args)?;
            logged_in = code == 0;
            read = signin::read(&provider, pp, signin::PERSON_WAIT);
        }
        read
    } else {
        signin::Read::Read {
            state: "not_needed",
            expires_at: None,
            declared: signin::Declared::default(),
        }
    };
    let e = entries.get_mut(source).ok_or_else(|| {
        CliError::NotFound(format!("source `{source}` is not set up for refresh"))
    })?;
    e.sign_in.checked_at = Some(time::fmt(time::now()));
    let state_text = match &state {
        signin::Read::Read {
            state,
            expires_at,
            declared: said,
        } => {
            e.sign_in.state = (*state).into();
            e.sign_in.expires_at = expires_at.clone();
            if *said != signin::Declared::default() {
                local.declared.insert(e.cli.clone(), *said);
            }
            (e.sign_in.renews, e.sign_in.lasts_secs) =
                sign_in_life(&e.cli.clone(), &local.declared);
            if signin::usable(state) && (!refused || logged_in) {
                e.sign_in.needs_person = None;
            }
            (*state).to_string()
        }
        signin::Read::TimedOut | signin::Read::Failed(_) => {
            e.sign_in.state = "unknown".into();
            "unknown".into()
        }
    };
    let needs_person = e.sign_in.needs_person.is_some();
    e.next_run_at = next_run(e, &b);
    if local.status.is_some() {
        let mut writer = Writer {
            store: &store,
            machine: state::machine()?,
            pass_every: pass_every(cfg),
            wrote: false,
            names: names(cfg),
        };
        writer.status(&entries, &mut local)?;
        state::save(&location, &local)?;
    }
    Ok(SignedIn {
        source: source.into(),
        profile: profile.to_string(),
        provider_profile: b.provider_profile.clone(),
        state: state_text,
        needs_person,
        logged_in,
    })
}

/// `_status/status.json` as the folder holds it now.
pub fn read_status(cfg: &Config) -> Result<Option<Status>, CliError> {
    let store = Store::open(&cfg.folder_location()?, Reads::Fresh, cfg.store_budget())?;
    let s: Option<Status> = folder::read_json(&store, STATUS)?;
    if let Some(s) = &s {
        check_format(s)?;
    }
    Ok(s)
}

/// The name a pending request is filed under, for tests and the synthetic
/// app: `<YYYYMMDDTHHMMSSZ>_<id>.json`.
pub fn request_name(at: i64, id: &str) -> String {
    format!("{}_{id}.json", time::compact(at))
}
