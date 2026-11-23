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
    wait_or(SCHEDULED_WAIT)
}

/// `default`, or `MEDREC_TEST_AUTH_WAIT_SECS` (tests only).
pub fn wait_or(default: Duration) -> Duration {
    std::env::var("MEDREC_TEST_AUTH_WAIT_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .map(Duration::from_secs)
        .unwrap_or(default)
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

/// The medrec binary running: its path, size and modification time, which
/// change when it is rebuilt or updated, as its Keychain approval does.
/// `MEDREC_TEST_DRIVE_PROBE` (tests only) stands in for it.
pub fn exe_id() -> Option<String> {
    let exe = drive_probe_exe()?;
    let meta = std::fs::metadata(&exe).ok()?;
    let mtime = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos();
    Some(format!("{}:{}:{mtime}", exe.display(), meta.len()))
}

fn drive_probe_exe() -> Option<std::path::PathBuf> {
    match std::env::var_os("MEDREC_TEST_DRIVE_PROBE") {
        Some(p) if !p.is_empty() => Some(p.into()),
        _ => std::env::current_exe().ok(),
    }
}

/// medrec's own read of the Drive sign-in from the Keychain for `profile`
/// (`medrec drive-keychain-probe`), as a bounded subprocess: an in-process
/// Keychain read cannot be stopped, a child can. It sends no request, so a
/// slow network never reads as an approval nobody gave: `TimedOut` is the
/// Keychain alone. `ok` when the sign-in is there, `signed_out` when not.
pub fn read_drive(profile: &str, wait: Duration) -> Read {
    let Some(exe) = drive_probe_exe() else {
        return Read::Failed("cannot find the medrec binary to check its Drive sign-in".into());
    };
    let ran = match Bounded::new(&exe)
        .args(["drive-keychain-probe", "--json", "--drive-profile", profile])
        .timeout(wait)
        .max_stdout(1024 * 1024)
        .run()
    {
        Ok(r) => r,
        Err(e) if crate::proc::is_timeout(&e) => return Read::TimedOut,
        Err(e) => return Read::Failed(e.message().to_string()),
    };
    match ran.code {
        Some(0) => Read::Read {
            state: "ok",
            expires_at: None,
        },
        Some(3) => Read::Read {
            state: "signed_out",
            expires_at: None,
        },
        _ => Read::Failed(format!(
            "`medrec drive-keychain-probe` exited {}: {}",
            ran.code
                .map(|c| c.to_string())
                .unwrap_or_else(|| "on a signal".into()),
            ran.stderr_tail.lines().last().unwrap_or("no answer")
        )),
    }
}

/// What fills a provider's `info.auth.login_hint` for one source.
#[derive(Debug, Default, Clone, Copy)]
pub struct Fill<'a> {
    /// `<NAME>`: the provider profile the source is bound to.
    pub profile: Option<&'a str>,
    /// `<ORG>`: the source's organization, as the provider names it.
    pub org: Option<&'a str>,
    /// `<LABEL>`, `<WHO>`: the patient's label at that sign-in.
    pub label: Option<&'a str>,
}

/// The provider's own sign-in command for one login, from its
/// `info.auth.login_hint` (`uhc auth login --browser --profile <NAME>`,
/// `athena --profile <NAME> auth login --org <ORG> [--label <LABEL>]`): the
/// arguments after the program. Of a hint of several commands (`mychart
/// profiles add <NAME>; mychart auth login ...`) the `auth login` one is
/// used. A `[...]` part goes in only when it can be filled; a placeholder
/// elsewhere that cannot be is an error naming the hint. Without a hint it
/// is `auth login --profile <NAME>`.
pub fn login_args(cli: &str, hint: Option<&str>, fill: Fill) -> Result<Vec<String>, String> {
    let Some(hint) = hint else {
        let mut v = vec!["auth".to_string(), "login".to_string()];
        if let Some(p) = fill.profile {
            v.extend(["--profile".to_string(), p.to_string()]);
        }
        return Ok(v);
    };
    let command = hint
        .split(';')
        .map(str::trim)
        .find(|c| {
            let t: Vec<&str> = c.split_whitespace().collect();
            t.windows(2).any(|w| w == ["auth", "login"])
        })
        .ok_or_else(|| format!("`{cli}`'s login hint `{hint}` has no `auth login`"))?;
    let mut tokens = command.split_whitespace();
    if tokens.next() != Some(cli) {
        return Err(format!(
            "`{cli}`'s login hint `{hint}` runs another program"
        ));
    }
    let value = |name: &str| match name {
        "NAME" | "PROFILE" => fill.profile,
        "ORG" | "ORGANIZATION" | "PRACTICE" => fill.org,
        "LABEL" | "WHO" => fill.label,
        _ => None,
    };
    // Each `<X>` in a token, filled; None when one cannot be.
    let filled = |t: &str| -> Option<String> {
        let mut out = String::new();
        let mut rest = t;
        while let Some(open) = rest.find('<') {
            let close = rest[open..].find('>')? + open;
            out.push_str(&rest[..open]);
            out.push_str(value(&rest[open + 1..close])?);
            rest = &rest[close + 1..];
        }
        out.push_str(rest);
        Some(out)
    };
    let mut args = Vec::new();
    let mut group: Option<Vec<&str>> = None;
    for t in tokens {
        if let Some(g) = &mut group {
            let end = t.strip_suffix(']');
            g.push(end.unwrap_or(t));
            if end.is_some() {
                let g = group.take().expect("in a group");
                let parts: Option<Vec<String>> = g.iter().map(|t| filled(t)).collect();
                args.extend(parts.unwrap_or_default());
            }
            continue;
        }
        if let Some(t) = t.strip_prefix('[') {
            match t.strip_suffix(']') {
                Some(one) => args.extend(filled(one)),
                None => group = Some(vec![t]),
            }
            continue;
        }
        args.push(filled(t).ok_or_else(|| {
            format!("cannot fill `{t}` in `{cli}`'s login hint `{hint}` for this source")
        })?);
    }
    if group.is_some() {
        return Err(format!("`{cli}`'s login hint `{hint}` leaves a `[` open"));
    }
    if let Some(p) = fill.profile {
        if !args.windows(2).any(|w| w[0] == "--profile" && w[1] == p) {
            args.extend(["--profile".to_string(), p.to_string()]);
        }
    }
    Ok(args)
}

/// Whether the provider's `auth login` takes `--overwrite` (the family's
/// flag for replacing a stored sign-in), from its `auth login --help`.
pub fn login_overwrites(provider: &Provider) -> bool {
    Bounded::new(&provider.path)
        .args(["auth", "login", "--help"])
        .timeout(Duration::from_secs(15))
        .max_stdout(256 * 1024)
        .run()
        .is_ok_and(|r| lists_overwrite(&String::from_utf8_lossy(&r.stdout)))
}

/// Whether a `--help` text lists `--overwrite` as a flag: on an option line
/// (`      --overwrite`, `  -o, --overwrite`), not in prose about it.
fn lists_overwrite(help: &str) -> bool {
    help.lines().any(|l| {
        let l = l.trim_start();
        l.starts_with('-')
            && l.split(|c: char| c.is_whitespace() || c == ',')
                .take_while(|t| t.is_empty() || t.starts_with('-'))
                .any(|t| t == "--overwrite")
    })
}

/// `<cli> <args>` in the person's terminal: its prompts, browser and codes
/// are theirs. Its exit code.
pub fn login(provider: &Provider, args: &[String]) -> Result<i32, pk_cli_core::CliError> {
    let mut cmd = std::process::Command::new(&provider.path);
    cmd.args(args);
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
    fn a_login_hint_becomes_the_providers_own_sign_in_for_one_login() {
        let fill = Fill {
            profile: Some("p1"),
            org: Some("org-1"),
            label: Some("kid one"),
        };
        let args = |cli: &str, hint: &str, fill: Fill| login_args(cli, Some(hint), fill);
        assert_eq!(
            args("uhc", "uhc auth login --browser --profile <NAME>", fill).unwrap(),
            ["auth", "login", "--browser", "--profile", "p1"]
        );
        assert_eq!(
            args(
                "athena",
                "athena --profile <NAME> auth login --org <ORG> [--label <LABEL>]",
                fill
            )
            .unwrap(),
            [
                "--profile",
                "p1",
                "auth",
                "login",
                "--org",
                "org-1",
                "--label",
                "kid one"
            ]
        );
        // The optional part goes when nothing fills it.
        let no_label = Fill {
            label: None,
            ..fill
        };
        assert_eq!(
            args(
                "athena",
                "athena --profile <NAME> auth login --org <ORG> [--label <LABEL>]",
                no_label
            )
            .unwrap(),
            ["--profile", "p1", "auth", "login", "--org", "org-1"]
        );
        assert_eq!(
            args(
                "mychart",
                "mychart profiles add <NAME>; mychart auth login --profile <NAME> --org <ORG> --label <WHO>",
                fill
            )
            .unwrap(),
            ["auth", "login", "--profile", "p1", "--org", "org-1", "--label", "kid one"]
        );
        // A hint without the profile still signs in that login.
        assert_eq!(
            args("x", "x auth login --browser", fill).unwrap(),
            ["auth", "login", "--browser", "--profile", "p1"]
        );
        let no_org = Fill { org: None, ..fill };
        assert!(args("athena", "athena auth login --org <ORG>", no_org).is_err());
        assert!(args("uhc", "uhc auth login --code <CODE>", fill).is_err());
        assert!(args("uhc", "sh -c auth login", fill).is_err());
        assert!(args("uhc", "uhc auth status", fill).is_err());
        assert_eq!(
            login_args("fakeprov", None, fill).unwrap(),
            ["auth", "login", "--profile", "p1"]
        );
        // A group goes whole: one placeholder it cannot fill drops all of it.
        assert_eq!(
            args(
                "x",
                "x auth login [--org <ORG> --label <LABEL>] --browser",
                no_org
            )
            .unwrap(),
            ["auth", "login", "--browser", "--profile", "p1"]
        );
        assert_eq!(
            args("x", "x auth login [--org <ORG> --label <LABEL>]", fill).unwrap(),
            [
                "auth",
                "login",
                "--org",
                "org-1",
                "--label",
                "kid one",
                "--profile",
                "p1"
            ]
        );
        assert!(args("x", "x auth login [--label <LABEL>", fill)
            .unwrap_err()
            .contains("leaves a `[` open"));
    }

    #[test]
    fn overwrite_is_known_from_its_option_line_whatever_the_layout() {
        assert!(lists_overwrite(
            "Options:\n      --overwrite\n          Replace it\n"
        ));
        assert!(lists_overwrite(
            "  -o, --overwrite    Replace a stored sign-in\n"
        ));
        assert!(lists_overwrite("  --overwrite, -o\n"));
        assert!(!lists_overwrite("      --overwrite-all\n"));
        assert!(!lists_overwrite(
            "A stored session is kept; pass --overwrite to replace it.\n"
        ));
        assert!(!lists_overwrite("  --stdin   read it; see --overwrite\n"));
    }

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
