//! A provider's sign-in, read from its `auth status --json`
//! (`auth-status/v1`) and mapped to `status.json`'s `sign_in.state`.

use std::time::Duration;

use serde_json::Value;

use super::contract::EXPIRING_SECS;
use super::time;
use crate::import::Provider;
use crate::proc::Bounded;

/// How long a scheduled pass waits on `auth status`: a Keychain approval
/// nobody gives would otherwise hold the pass.
pub const SCHEDULED_WAIT: Duration = Duration::from_secs(30);
/// [`SCHEDULED_WAIT`], or `MEDREC_TEST_AUTH_WAIT_SECS` (tests only, so a
/// hang is seen in a second).
pub fn scheduled_wait() -> Duration {
    std::env::var("MEDREC_TEST_AUTH_WAIT_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .map(Duration::from_secs)
        .unwrap_or(SCHEDULED_WAIT)
}

/// How long a person at the terminal has to approve one.
pub const PERSON_WAIT: Duration = Duration::from_secs(600);

#[derive(Debug, Clone, PartialEq)]
pub enum Read {
    /// `state` and `expires_at`, as `status.json` holds them.
    Read {
        state: &'static str,
        expires_at: Option<String>,
    },
    /// The call outlived its wait.
    TimedOut,
    /// It exited non-zero or printed no `auth-status/v1`.
    Failed(String),
}

/// medrec's reading of an `auth-status/v1` document at `now`
/// (data-contract.md, `sign_in.state`).
pub fn state_of(v: &Value, now: i64) -> (&'static str, Option<String>) {
    let expires_at = v
        .get("expires_at")
        .and_then(Value::as_str)
        .filter(|t| time::parse(t).is_some())
        .map(str::to_string);
    if v.get("required").and_then(Value::as_bool) == Some(false) {
        return ("not_needed", expires_at);
    }
    if v.get("authenticated").and_then(Value::as_bool) != Some(true) {
        return ("signed_out", expires_at);
    }
    let ends = expires_at.as_deref().and_then(time::parse);
    if v.get("session_valid").and_then(Value::as_bool) == Some(false)
        || ends.is_some_and(|e| e <= now)
    {
        return ("expired", expires_at);
    }
    if ends.is_some_and(|e| e - now <= EXPIRING_SECS) {
        return ("expiring", expires_at);
    }
    ("ok", expires_at)
}

/// Whether a refresh can go on with this sign-in.
pub fn usable(state: &str) -> bool {
    matches!(state, "ok" | "expiring" | "not_needed")
}

/// `<cli> auth status --json [--profile <p>]`, waiting at most `wait`.
pub fn read(provider: &Provider, profile: Option<&str>, wait: Duration) -> Read {
    let mut cmd = Bounded::new(&provider.path)
        .args(["auth", "status", "--json"])
        .timeout(wait)
        .max_stdout(1024 * 1024);
    if let Some(p) = profile {
        cmd = cmd.args(["--profile", p]);
    }
    let ran = match cmd.run() {
        Ok(r) => r,
        Err(e) if crate::proc::is_timeout(&e) => return Read::TimedOut,
        Err(e) => return Read::Failed(e.message().to_string()),
    };
    let v: Option<Value> = serde_json::from_slice(&ran.stdout).ok();
    match v {
        Some(v) if v.get("schema").and_then(Value::as_str) == Some("auth-status/v1") => {
            let (state, expires_at) = state_of(&v, time::now());
            Read::Read { state, expires_at }
        }
        _ if ran.code != Some(0) => Read::Failed(format!(
            "`{} auth status --json` exited {}",
            provider.cli,
            ran.code
                .map(|c| c.to_string())
                .unwrap_or_else(|| "on a signal".into())
        )),
        _ => Read::Failed(format!(
            "`{} auth status --json` printed no auth-status/v1",
            provider.cli
        )),
    }
}

/// `<cli> auth login [--profile <p>]` in the person's terminal: its prompts,
/// browser and codes are theirs. Its exit code.
pub fn login(provider: &Provider, profile: Option<&str>) -> Result<i32, pk_cli_core::CliError> {
    let mut cmd = std::process::Command::new(&provider.path);
    cmd.args(["auth", "login"]);
    if let Some(p) = profile {
        cmd.args(["--profile", p]);
    }
    for (k, _) in std::env::vars_os() {
        if k.to_string_lossy().starts_with("MEDREC_") {
            cmd.env_remove(&k);
        }
    }
    // Its prompts go to the terminal through stderr, so medrec's stdout
    // holds only medrec's own result.
    cmd.stdout(std::process::Stdio::from(std::io::stderr()));
    let status = cmd.status().map_err(|e| {
        pk_cli_core::CliError::Other(format!("running `{} auth login`: {e}", provider.cli))
    })?;
    Ok(status.code().unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn auth_status_reads_as_medrecs_sign_in_state() {
        let now = time::parse("2026-11-22T20:00:00Z").unwrap();
        let of = |v: Value| state_of(&v, now).0;
        assert_eq!(
            of(json!({"required": false, "authenticated": true})),
            "not_needed"
        );
        assert_eq!(
            of(json!({"required": true, "authenticated": false})),
            "signed_out"
        );
        assert_eq!(
            of(json!({"authenticated": true, "session_valid": false})),
            "expired"
        );
        assert_eq!(
            of(json!({"authenticated": true, "expires_at": "2026-11-22T19:00:00Z"})),
            "expired"
        );
        assert_eq!(
            of(json!({"authenticated": true, "expires_at": "2026-11-24T20:00:00Z"})),
            "expiring"
        );
        assert_eq!(
            of(json!({"authenticated": true, "expires_at": "2026-12-22T20:00:00Z"})),
            "ok"
        );
        assert_eq!(of(json!({"authenticated": true})), "ok");
        assert_eq!(
            of(json!({"authenticated": true, "expires_at": "soon"})),
            "ok"
        );
        assert!(
            usable("expiring") && usable("not_needed") && !usable("expired") && !usable("unknown")
        );
    }
}
