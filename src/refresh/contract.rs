//! The files `medrec refresh` writes and reads in the records folder
//! (docs/data-contract.md, `_status/` and `_requests/`).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const STATUS: &str = "_status/status.json";
pub const STATUS_DIR: &str = "_status";
pub const ATTEMPTS_DIR: &str = "_status/attempts";
pub const REQUESTS_DIR: &str = "_requests";
pub const DONE_DIR: &str = "_requests/done";

pub const STATUS_SCHEMA: &str = "medrec-status/v1";
pub const ATTEMPT_SCHEMA: &str = "medrec-attempt/v1";
pub const REQUEST_SCHEMA: &str = "medrec-request/v1";
pub const FORMAT: u32 = 1;

/// How long a pass goes without writing `status.json` at most.
pub const HEARTBEAT_SECS: u64 = 3600;
/// How long `done/` files are kept.
pub const DONE_KEEP_SECS: i64 = 90 * 86400;
/// The largest request medrec reads.
pub const REQUEST_MAX_BYTES: u64 = 4 * 1024;
/// The largest `done/` file medrec writes.
pub const DONE_MAX_BYTES: usize = 8 * 1024;
/// How long an error message in `_status/` may be, in characters.
pub const MESSAGE_MAX: usize = 300;
/// How long a shown string from a request may be, in characters.
pub const SHOWN_MAX: usize = 200;
/// The wait after a source's first failure; it doubles with each one after,
/// up to the source's minimum interval.
pub const FIRST_BACKOFF_SECS: u64 = 30 * 60;
/// When a sign-in that ends soon is `expiring`.
pub const EXPIRING_SECS: i64 = 72 * 3600;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Status {
    pub schema: String,
    pub format: u32,
    pub updated_at: String,
    pub refresher: Refresher,
    #[serde(default)]
    pub sources: Vec<SourceStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Refresher {
    pub machine: String,
    pub medrec_version: String,
    pub last_pass_at: Option<String>,
    pub next_pass_at: Option<String>,
    pub pass_every_secs: u64,
    pub heartbeat_every_secs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pause {
    /// `consent` or `user`.
    pub reason: String,
    #[serde(default)]
    pub note: Option<String>,
    pub since: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceStatus {
    pub source: String,
    pub person: String,
    pub cli: String,
    pub organization: Option<String>,
    pub auto: bool,
    pub paused: Option<Pause>,
    pub min_interval_secs: u64,
    /// `failed` attempts in a row since the last `ok`.
    #[serde(default)]
    pub failures: u32,
    pub next_run_at: Option<String>,
    pub sign_in: SignIn,
    pub last_attempt: Option<AttemptSummary>,
    pub last_success: Option<Success>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SignIn {
    /// `ok`, `expiring`, `expired`, `signed_out`, `not_needed` or `unknown`.
    pub state: String,
    pub expires_at: Option<String>,
    pub checked_at: Option<String>,
    pub needs_person: Option<NeedsPerson>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NeedsPerson {
    /// `sign_in` or `keychain`.
    pub reason: String,
    pub since: String,
    pub command: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ErrorInfo {
    pub code: String,
    pub message: String,
    pub remedy: Option<String>,
}

/// One line of `attempts/<YYYY-MM>.ndjson`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Attempt {
    pub schema: String,
    pub attempt: String,
    pub source: String,
    pub person: String,
    pub cli: String,
    pub organization: Option<String>,
    pub started_at: String,
    pub finished_at: Option<String>,
    /// `schedule`, `request` or `manual`.
    pub trigger: String,
    pub requests: Vec<String>,
    /// `running` (in status.json only), `ok`, `failed`, `needs_person`,
    /// `skipped_interval` or `skipped_paused`.
    pub outcome: String,
    pub exit_code: Option<i32>,
    pub error: Option<ErrorInfo>,
    pub import: Option<String>,
    pub counts: Option<BTreeMap<String, u64>>,
}

/// An attempt as `status.json` holds it: without the source's fields and
/// the counts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AttemptSummary {
    pub attempt: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub trigger: String,
    pub requests: Vec<String>,
    pub outcome: String,
    pub exit_code: Option<i32>,
    pub error: Option<ErrorInfo>,
    pub import: Option<String>,
}

impl Attempt {
    pub fn summary(&self) -> AttemptSummary {
        AttemptSummary {
            attempt: self.attempt.clone(),
            started_at: self.started_at.clone(),
            finished_at: self.finished_at.clone(),
            trigger: self.trigger.clone(),
            requests: self.requests.clone(),
            outcome: self.outcome.clone(),
            exit_code: self.exit_code,
            error: self.error.clone(),
            import: self.import.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Success {
    pub attempt: String,
    pub finished_at: String,
    pub import: String,
    pub counts: BTreeMap<String, u64>,
    #[serde(default)]
    pub withheld_types: Vec<String>,
}

/// `handled` in a `done/` file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Handled {
    pub picked_up_at: String,
    pub finished_at: String,
    /// `ok`, `partial`, `needs_person`, `failed`, `skipped` or `invalid`.
    pub outcome: String,
    pub sources: Vec<HandledSource>,
    pub message: String,
    pub by: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HandledSource {
    pub source: String,
    pub outcome: String,
    pub attempt: Option<String>,
    pub next_allowed_at: Option<String>,
}

/// A request's outcome from its sources' (data-contract.md, `handled`).
pub fn request_outcome(sources: &[HandledSource]) -> &'static str {
    let any = |o: &str| sources.iter().any(|s| s.outcome == o);
    let ok = any("ok");
    let failed = any("failed");
    let person = any("needs_person");
    match (ok, failed || person) {
        (true, false) => "ok",
        (true, true) => "partial",
        (false, _) if failed => "failed",
        (false, _) if person => "needs_person",
        _ => "skipped",
    }
}

/// How long a source waits after `failures` failed attempts in a row.
pub fn backoff(failures: u32, min_interval: u64) -> u64 {
    if failures == 0 {
        return 0;
    }
    let doubled = FIRST_BACKOFF_SECS.saturating_mul(1u64 << (failures - 1).min(20));
    doubled.min(min_interval)
}

/// `s` cut to `max` characters.
pub fn clip(s: &str, max: usize) -> String {
    let printable = crate::printable(s);
    match printable.char_indices().nth(max) {
        Some((i, _)) => printable[..i].to_string(),
        None => printable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(outcome: &str) -> HandledSource {
        HandledSource {
            source: "s_aaaaaaaaaaaaaaaaaaaa".into(),
            outcome: outcome.into(),
            attempt: None,
            next_allowed_at: None,
        }
    }

    #[test]
    fn a_requests_outcome_follows_its_sources() {
        let of = |o: &[&str]| request_outcome(&o.iter().map(|o| src(o)).collect::<Vec<_>>());
        assert_eq!(of(&["ok", "skipped_interval"]), "ok");
        assert_eq!(of(&["ok", "failed"]), "partial");
        assert_eq!(of(&["ok", "needs_person"]), "partial");
        assert_eq!(of(&["needs_person", "skipped_paused"]), "needs_person");
        assert_eq!(of(&["needs_person", "failed"]), "failed");
        assert_eq!(of(&["skipped_interval", "skipped_paused"]), "skipped");
        assert_eq!(of(&[]), "skipped");
    }

    #[test]
    fn a_failing_source_waits_longer_each_time_up_to_its_interval() {
        assert_eq!(backoff(0, 86400), 0);
        assert_eq!(backoff(1, 86400), 1800);
        assert_eq!(backoff(2, 86400), 3600);
        assert_eq!(backoff(5, 86400), 28800);
        assert_eq!(backoff(6, 86400), 57600);
        assert_eq!(backoff(7, 86400), 86400);
        assert_eq!(backoff(40, 86400), 86400);
        assert_eq!(backoff(9, 21600), 21600);
    }

    #[test]
    fn shown_strings_are_cut_to_their_limit() {
        assert_eq!(clip("abc", 2), "ab");
        assert_eq!(clip("ab", 5), "ab");
        assert_eq!(clip("ééé", 2).chars().count(), 2);
    }
}
