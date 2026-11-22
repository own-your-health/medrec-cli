//! The LaunchAgent that runs `medrec refresh --scheduled`: one per profile,
//! in the person's own session, never as root.

use std::path::{Path, PathBuf};

use pk_cli_core::CliError;

/// Where LaunchAgents live; `MEDREC_LAUNCH_AGENTS_DIR` replaces it in
/// tests only.
fn agents_dir() -> Result<PathBuf, CliError> {
    if let Some(d) = std::env::var_os("MEDREC_LAUNCH_AGENTS_DIR") {
        return Ok(PathBuf::from(d));
    }
    let home =
        std::env::var_os("HOME").ok_or_else(|| CliError::Other("$HOME is not set".into()))?;
    Ok(PathBuf::from(home).join("Library/LaunchAgents"))
}

/// `launchctl`; `MEDREC_LAUNCHCTL` replaces it in tests only.
fn launchctl() -> PathBuf {
    std::env::var_os("MEDREC_LAUNCHCTL")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/bin/launchctl"))
}

pub fn label(profile: &str) -> String {
    format!("io.github.own-your-health.medrec.refresh.{profile}")
}

pub fn plist_path(profile: &str) -> Result<PathBuf, CliError> {
    Ok(agents_dir()?.join(format!("{}.plist", label(profile))))
}

pub fn log_path(profile: &str) -> Result<PathBuf, CliError> {
    let home =
        std::env::var_os("HOME").ok_or_else(|| CliError::Other("$HOME is not set".into()))?;
    Ok(PathBuf::from(home).join(format!("Library/Logs/medrec/refresh-{profile}.log")))
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The agent: `<exe> --profile <profile> refresh --scheduled` every
/// `every_secs`, with this shell's `PATH` so it finds rclone and the
/// provider CLIs, and its output in `log`.
pub fn plist(profile: &str, exe: &Path, every_secs: u64, path_env: &str, log: &Path) -> String {
    let args = [
        exe.to_string_lossy().into_owned(),
        "--profile".into(),
        profile.into(),
        "refresh".into(),
        "--scheduled".into(),
    ];
    let args: String = args
        .iter()
        .map(|a| format!("    <string>{}</string>\n", xml(a)))
        .collect();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{label}</string>
  <key>ProgramArguments</key>
  <array>
{args}  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key>
    <string>{path}</string>
  </dict>
  <key>StartInterval</key>
  <integer>{every_secs}</integer>
  <key>RunAtLoad</key>
  <true/>
  <key>ProcessType</key>
  <string>Background</string>
  <key>LowPriorityIO</key>
  <true/>
  <key>StandardOutPath</key>
  <string>{log}</string>
  <key>StandardErrorPath</key>
  <string>{log}</string>
</dict>
</plist>
"#,
        label = xml(&label(profile)),
        path = xml(path_env),
        log = xml(&log.to_string_lossy()),
    )
}

fn uid() -> u32 {
    // SAFETY: getuid has no preconditions and cannot fail.
    unsafe { libc::getuid() }
}

fn run_launchctl(args: &[&str]) -> Result<i32, CliError> {
    let out = std::process::Command::new(launchctl())
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| CliError::Other(format!("running launchctl: {e}")))?;
    Ok(out.status.code().unwrap_or(1))
}

/// Write the agent and load it, replacing one already loaded.
pub fn install(profile: &str, every_secs: u64) -> Result<PathBuf, CliError> {
    let exe = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|e| CliError::Other(format!("finding medrec's own path: {e}")))?;
    let path_env = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into());
    let log = log_path(profile)?;
    if let Some(dir) = log.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| CliError::Other(format!("creating {}: {e}", dir.display())))?;
    }
    let plist_file = plist_path(profile)?;
    if let Some(dir) = plist_file.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| CliError::Other(format!("creating {}: {e}", dir.display())))?;
    }
    let text = plist(profile, &exe, every_secs, &path_env, &log);
    std::fs::write(&plist_file, text)
        .map_err(|e| CliError::Other(format!("writing {}: {e}", plist_file.display())))?;
    let domain = format!("gui/{}", uid());
    let service = format!("{domain}/{}", label(profile));
    let _ = run_launchctl(&["bootout", &service]);
    let code = run_launchctl(&["bootstrap", &domain, &plist_file.to_string_lossy()])?;
    if code != 0 {
        return Err(CliError::Other(format!(
            "`launchctl bootstrap {domain} {}` exited {code}",
            plist_file.display()
        )));
    }
    Ok(plist_file)
}

/// Unload the agent and remove it. `false` when there was none.
pub fn uninstall(profile: &str) -> Result<bool, CliError> {
    let plist_file = plist_path(profile)?;
    let service = format!("gui/{}/{}", uid(), label(profile));
    let _ = run_launchctl(&["bootout", &service]);
    match std::fs::remove_file(&plist_file) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(CliError::Other(format!(
            "removing {}: {e}",
            plist_file.display()
        ))),
    }
}

pub fn installed(profile: &str) -> bool {
    plist_path(profile).is_ok_and(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_agent_runs_a_scheduled_pass_as_the_profile() {
        let p = plist(
            "home",
            Path::new("/Users/x/.cargo/bin/medrec"),
            300,
            "/usr/bin:/bin:/a&b",
            Path::new("/Users/x/Library/Logs/medrec/refresh-home.log"),
        );
        assert!(p.contains("<string>io.github.own-your-health.medrec.refresh.home</string>"));
        assert!(p.contains(
            "    <string>/Users/x/.cargo/bin/medrec</string>\n    <string>--profile</string>\n    <string>home</string>\n    <string>refresh</string>\n    <string>--scheduled</string>\n"
        ));
        assert!(p.contains("<integer>300</integer>"));
        assert!(p.contains("/usr/bin:/bin:/a&amp;b"));
        assert!(!p.contains("UserName"), "never as another user");
    }
}
