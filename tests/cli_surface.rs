//! Black-box tests of the binary's surface: offline, no Keychain, no
//! network, no records folder. Each test gets its own config, cache and
//! home directory.

use assert_cmd::Command;
use serde_json::Value;
use std::sync::atomic::{AtomicU32, Ordering};

fn medrec() -> Command {
    static SEQ: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "medrec-surface-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let mut c = Command::cargo_bin("medrec").unwrap();
    c.env("XDG_CONFIG_HOME", dir.join("config"))
        .env("TZ", "America/New_York")
        .env("XDG_CACHE_HOME", dir.join("cache"))
        .env("HOME", &dir)
        .env("MEDREC_KEYCHAIN_SERVICE", "piekstra.medrec.hermetic-tests")
        .env_remove("MEDREC_DRIVE_PROFILE")
        .env_remove("MEDREC_PROFILE")
        .env_remove("NO_COLOR");
    c
}

/// A run with its own config holding one profile, `home`, and no records
/// folder.
fn with_profile() -> impl Fn(&[&str]) -> std::process::Output {
    static SEQ: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "medrec-surface-p-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let run = move |args: &[&str]| {
        let mut c = Command::cargo_bin("medrec").unwrap();
        c.env("XDG_CONFIG_HOME", dir.join("config"))
            .env("TZ", "America/New_York")
            .env("XDG_CACHE_HOME", dir.join("cache"))
            .env("HOME", &dir)
            .env("MEDREC_KEYCHAIN_SERVICE", "piekstra.medrec.hermetic-tests")
            .env_remove("MEDREC_DRIVE_PROFILE")
            .env_remove("MEDREC_PROFILE")
            .env_remove("NO_COLOR");
        c.args(args).output().unwrap()
    };
    assert!(run(&["profiles", "add", "home"]).status.success());
    run
}

fn json_of(out: &std::process::Output) -> Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout is not one JSON document ({e}): {}",
            String::from_utf8_lossy(&out.stdout)
        )
    })
}

#[test]
fn every_command_renders_its_help() {
    let commands: &[&[&str]] = &[
        &[],
        &["profiles"],
        &["profiles", "list"],
        &["profiles", "add"],
        &["profiles", "remove"],
        &["profiles", "migrate"],
        &["init"],
        &["people"],
        &["people", "add"],
        &["people", "list"],
        &["people", "show"],
        &["people", "edit"],
        &["intake"],
        &["review"],
        &["review", "list"],
        &["review", "show"],
        &["review", "file"],
        &["review", "drop"],
        &["import"],
        &["providers", "list"],
        &["tables"],
        &["tables", "list"],
        &["tables", "fetch"],
        &["refresh"],
        &["refresh", "add"],
        &["refresh", "remove"],
        &["refresh", "list"],
        &["refresh", "status"],
        &["refresh", "pause"],
        &["refresh", "resume"],
        &["refresh", "sign-in"],
        &["timeline"],
        &["search"],
        &["results"],
        &["meds"],
        &["conditions"],
        &["allergies"],
        &["immunizations"],
        &["records", "show"],
        &["records", "raw"],
        &["records", "fields"],
        &["records", "about-another"],
        &["documents", "list"],
        &["documents", "open"],
        &["note", "add"],
        &["note", "list"],
        &["images", "intake"],
        &["images", "list"],
        &["images", "show"],
        &["images", "open"],
        &["images", "export"],
        &["doctor"],
        &["verify"],
        &["backup"],
        &["drive"],
        &["drive", "login"],
        &["drive", "status"],
        &["drive", "logout"],
        &["drive", "about"],
        &["drive", "client", "import"],
        &["drive", "folder", "create"],
        &["drive", "folder", "list"],
        &["drive", "profiles", "list"],
        &["drive", "profiles", "remove"],
        &["cache"],
        &["cache", "clear"],
        &["config", "path"],
        &["config", "show"],
        &["config", "set"],
        &["config", "unset"],
        &["auth", "status"],
        &["auth", "key"],
        &["self-update"],
        &["completions"],
        &["info"],
    ];
    for c in commands {
        let mut args: Vec<&str> = c.to_vec();
        args.push("--help");
        medrec().args(&args).assert().success();
    }
}

#[test]
fn info_names_the_drive_sign_in() {
    let out = medrec().args(["info", "--json"]).output().unwrap();
    assert!(out.status.success());
    let v = json_of(&out);
    assert_eq!(v["schema"], "cli-info/v1");
    assert_eq!(v["name"], "medrec");
    assert_eq!(v["auth"]["required"], false);
    assert_eq!(v["auth"]["method"], "browser-session");
    assert_eq!(
        v["auth"]["login_hint"],
        "medrec drive login --drive-profile <NAME>"
    );
    // Its records-folder commands take --profile.
    assert!(v["capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c == "login-profiles"));
    let out = medrec()
        .args(["auth", "status", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(json_of(&out)["schema"], "auth-status/v1");
}

/// `info` and `--version` say what the binary was built from; `-V` is the
/// version alone.
#[test]
fn a_binary_says_what_it_was_built_from() {
    let version = env!("CARGO_PKG_VERSION");
    let v = json_of(&medrec().args(["info", "--json"]).output().unwrap());
    assert_eq!(v["version"], version);
    let build = &v["build"];
    assert!(build["built_at"].is_string(), "{v}");
    let text = |flag: &str| -> String {
        let out = medrec().arg(flag).output().unwrap();
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    };
    assert_eq!(text("-V"), format!("medrec {version}"));
    let long = text("--version");
    assert!(
        long.starts_with(&format!("medrec {version} (")) && long.contains(", built "),
        "{long}"
    );
    let git = std::process::Command::new("git")
        .args(["-C", env!("CARGO_MANIFEST_DIR"), "rev-parse", "HEAD"])
        .output();
    if git.is_ok_and(|o| o.status.success()) {
        let commit = build["commit"].as_str().unwrap_or_default();
        assert_eq!(commit.len(), 40, "{v}");
        assert!(long.contains(&commit[..12]), "{long}: {commit}");
    }
}

#[test]
fn no_color_and_completions_work() {
    medrec()
        .env("NO_COLOR", "1")
        .args(["auth", "status"])
        .assert()
        .success();
    for shell in ["bash", "zsh", "fish"] {
        medrec().args(["completions", shell]).assert().success();
    }
}

/// Exit 2 from argument checking, with a message that says `says` and
/// does not mention the records folder.
fn refused(args: &[&str], says: &str) {
    let out = medrec().args(args).output().unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.status.code(), Some(2), "{args:?}: {text}");
    assert!(text.contains(says), "{args:?}: {text}");
    assert!(!text.contains("records folder"), "{args:?}: {text}");
}

#[test]
fn bad_arguments_exit_2_before_anything_is_read() {
    medrec().arg("no-such-command").assert().code(2);
    medrec().arg("intake").assert().code(2);
    medrec()
        .args([
            "import",
            "--from",
            "x",
            "--patient",
            "p",
            "--since",
            "03/04/2026",
        ])
        .assert()
        .code(2);
    medrec()
        .args(["timeline", "--since", "yesterday"])
        .assert()
        .code(2);
    // A typo in a kind is an error, not an empty timeline; a limit is 1 or
    // more; --force means "do not ask" only, and the birth-date checks have
    // their own flag. Each message names the argument, so none of these can
    // pass on the missing records folder instead.
    refused(
        &["timeline", "--kind", "results"],
        "invalid value 'results'",
    );
    refused(
        &["search", "x", "--limit", "0"],
        "invalid value '0' for '--limit <N>'",
    );
    refused(
        &["documents", "list", "--limit", "0"],
        "invalid value '0' for '--limit <N>'",
    );
    refused(
        &["people", "list", "--limit=-1"],
        "invalid value '-1' for '--limit <N>'",
    );
    refused(
        &["import", "--from", "x", "--patient", "p", "--force"],
        "unexpected argument '--force'",
    );
    refused(
        &["images", "intake", "x", "--force"],
        "unexpected argument '--force'",
    );
    // Checked before the records folder is looked for: the message names
    // the argument, not the missing folder.
    let out = medrec()
        .args([
            "--json",
            "intake",
            "x.pdf",
            "--classified-by",
            "organize-scans",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stdout).contains("give at least one"));
    let out = medrec()
        .args(["--json", "intake", "x.pdf", "--date", "03/04/2026"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(!String::from_utf8_lossy(&out.stdout).contains("records folder"));
    medrec()
        .args(["config", "set", "no_such_key", "1"])
        .assert()
        .code(2);
    medrec()
        .args(["config", "set", "ocr_max_pages", "many"])
        .assert()
        .code(2);
}

#[test]
fn without_a_profile_commands_say_how_to_add_one() {
    let out = medrec()
        .args(["--json", "people", "list"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let text = json_of(&out).to_string();
    assert!(text.contains("--profile <NAME>"), "{text}");
    assert!(text.contains("medrec profiles add <NAME>"), "{text}");
}

#[test]
fn a_profile_is_named_by_flag_or_environment_and_never_picked() {
    let run = with_profile();
    // One profile configured, and still not picked for the caller.
    let out = run(&["--json", "people", "list"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(json_of(&out).to_string().contains("configured: home"));
    let out = run(&["--json", "--profile", "away", "people", "list"]);
    assert_eq!(out.status.code(), Some(2));
    let text = json_of(&out).to_string();
    assert!(
        text.contains("no profile `away`") && text.contains("configured: home"),
        "{text}"
    );
    // A profile without a records folder says how to give it one.
    let out = run(&["--json", "--profile", "home", "people", "list"]);
    assert_eq!(out.status.code(), Some(2));
    let text = json_of(&out).to_string();
    assert!(text.contains("medrec --profile home init"), "{text}");
    let out = medrec()
        .args(["--json", "--profile", "Home", "people", "list"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    medrec().args(["profiles", "add", "-x"]).assert().code(2);
    medrec().args(["profiles", "add", "a b"]).assert().code(2);
}

#[test]
fn profiles_are_listed_added_and_removed() {
    let run = with_profile();
    let v = json_of(&run(&["--json", "profiles", "list"]));
    assert_eq!(v["schema"], "profile-list/v1");
    assert_eq!(v["profiles"][0]["name"], "home");
    assert!(v["profiles"][0].get("folder").is_none());
    let v = json_of(&run(&["--json", "profiles", "add", "home"]));
    assert_eq!(v["status"], "exists");
    let v = json_of(&run(&["--json", "profiles", "add", "away"]));
    assert_eq!(
        (&v["schema"], &v["status"]),
        (&"profile/v1".into(), &"added".into())
    );
    let v = json_of(&run(&["--json", "profiles", "list"]));
    let names: Vec<&str> = v["profiles"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["away", "home"]);
    // Removing asks; without a terminal it needs --force.
    assert_eq!(
        run(&["--json", "profiles", "remove", "away"]).status.code(),
        Some(6)
    );
    let v = json_of(&run(&["--json", "profiles", "remove", "away", "--force"]));
    assert_eq!(v["status"], "removed");
    assert_eq!(
        run(&["--json", "profiles", "remove", "away", "--force"])
            .status
            .code(),
        Some(2)
    );
    let empty = json_of(
        &medrec()
            .args(["--json", "profiles", "list"])
            .output()
            .unwrap(),
    );
    assert_eq!(empty["profiles"].as_array().unwrap().len(), 0);
}

#[test]
fn a_mutation_needs_force_without_a_terminal() {
    let run = with_profile();
    let out = run(&[
        "--json",
        "--profile",
        "home",
        "review",
        "drop",
        "q_aaaaaaaaaaaaaaaaaaaa",
    ]);
    assert_eq!(out.status.code(), Some(6));
    json_of(&out);
}

#[test]
fn cache_clear_works_with_nothing_cached() {
    let out = medrec()
        .args(["--json", "cache", "clear"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(json_of(&out)["files"], 0);
}

#[test]
fn config_round_trips() {
    let dir = std::env::temp_dir().join(format!("medrec-config-{}", std::process::id()));
    let run = |args: &[&str]| {
        let mut c = Command::cargo_bin("medrec").unwrap();
        c.env("XDG_CONFIG_HOME", dir.join("config"))
            .env("TZ", "America/New_York")
            .env("XDG_CACHE_HOME", dir.join("cache"))
            .env("MEDREC_KEYCHAIN_SERVICE", "piekstra.medrec.hermetic-tests")
            .env_remove("MEDREC_PROFILE");
        c.args(args).output().unwrap()
    };
    let _ = std::fs::remove_dir_all(&dir);
    assert!(run(&["profiles", "add", "home"]).status.success());
    assert!(run(&["config", "set", "viewer", "none"]).status.success());
    // A records folder is a profile's.
    assert_eq!(
        run(&[
            "config",
            "set",
            "folder",
            "gdrive:home-drive/1AbCdEf_synthetic"
        ])
        .status
        .code(),
        Some(2)
    );
    let set = json_of(&run(&[
        "--json",
        "--profile",
        "home",
        "config",
        "set",
        "folder",
        "gdrive:home-drive/1AbCdEf_synthetic",
    ]));
    assert_eq!(set["profile"], "home");
    assert_eq!(set["folder"], "gdrive:home-drive/1AbCdEf_synthetic");
    let v = json_of(&run(&["--json", "config", "show"]));
    assert_eq!(v["viewer"], "none");
    assert!(v.get("folder").is_none(), "{v}");
    assert_eq!(
        v["profiles"]["home"]["folder"],
        "gdrive:home-drive/1AbCdEf_synthetic"
    );
    let v = json_of(&run(&["--json", "profiles", "list"]));
    assert_eq!(
        v["profiles"][0]["folder"],
        "gdrive:home-drive/1AbCdEf_synthetic"
    );
    assert!(run(&["config", "unset", "viewer"]).status.success());
    let v = json_of(&run(&["--json", "config", "show"]));
    assert!(v.get("viewer").is_none());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn intake_help_lists_every_document_type() {
    let out = medrec().args(["intake", "--help"]).output().unwrap();
    let help = String::from_utf8_lossy(&out.stdout);
    let line = help
        .lines()
        .find(|l| l.starts_with("Document types: "))
        .expect("a Document types line");
    let listed: Vec<&str> = line["Document types: ".len()..]
        .trim_end_matches('.')
        .split(", ")
        .collect();
    let types: Vec<&str> = medrec::intake::classify::DOC_TYPES
        .iter()
        .map(|t| t.0)
        .collect();
    assert_eq!(listed, types);
}

#[test]
fn a_folder_named_the_rclone_way_is_refused_with_the_move() {
    let run = with_profile();
    for args in [
        &[
            "--profile",
            "home",
            "config",
            "set",
            "folder",
            "records:Medical Records",
        ][..],
        &["--profile", "home", "init", "records:Medical Records"][..],
        &[
            "--profile",
            "home",
            "config",
            "set",
            "inbox",
            "records:Inbox",
        ][..],
    ] {
        let out = run(args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("medrec no longer uses rclone"), "{err}");
        assert!(err.contains("medrec drive login --drive-profile"), "{err}");
    }
}

#[test]
fn drive_profiles_are_medrecs_own_and_none_is_a_default() {
    let run = with_profile();
    // No Drive profile yet: the list is empty and reads no keychain.
    let v = json_of(&run(&["--json", "drive", "profiles", "list"]));
    assert_eq!(v["profiles"].as_array().map(Vec::len), Some(0), "{v}");
    // A Drive command with none named, and none in the folder, is exit 2.
    let out = run(&["drive", "status", "--local"]);
    assert_eq!(out.status.code(), Some(2));
    // With no records folder named, no sign-in is needed yet.
    let v = json_of(&run(&["--json", "auth", "status"]));
    assert_eq!(v["required"], false, "{v}");
    // A folder on Drive whose profile is not signed in here: the command is
    // refused before anything is read, naming the sign-in.
    assert!(run(&[
        "--profile",
        "home",
        "config",
        "set",
        "folder",
        "gdrive:home-drive/1AbCdEf_synthetic"
    ])
    .status
    .success());
    let out = run(&["--profile", "home", "people", "list"]);
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("medrec drive login --drive-profile home-drive"),
        "{err}"
    );
    // The records folder names the Drive profile its commands default to.
    let out = run(&["--profile", "home", "--json", "drive", "status", "--local"]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("home-drive"));
}

#[test]
fn the_code_tables_need_no_profile_and_start_absent() {
    let out = medrec()
        .args(["--json", "tables", "list"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let v = json_of(&out);
    let rows = v["items"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["id"], "icd10cm");
    assert_eq!(rows[0]["state"], "absent");
    assert_eq!(rows[1]["id"], "nppes");
    assert_eq!(rows[1]["state"], "absent");
    for k in ["version", "date", "bytes", "rows", "sha256"] {
        assert!(rows[1][k].is_null(), "{k}");
    }
    assert!(rows[1].get("folder").is_none());
    assert_eq!(rows[0]["sha256"].as_str().unwrap().len(), 64);
    for (args, why) in [
        (&["tables", "fetch", "nosuch"][..], "no table `nosuch`"),
        (
            &["tables", "fetch", "--file", "/nonexistent.zip"][..],
            "required",
        ),
        (
            &["tables", "fetch", "icd10cm", "--file", "/nonexistent.zip"][..],
            "nonexistent.zip",
        ),
        (&["tables", "fetch", "icd10cm", "--force"][..], "--force"),
        (&["tables", "fetch", "--force"][..], "required"),
    ] {
        let out = medrec().args(args).output().unwrap();
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains(why),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// The provider directory is one folder's, and over a gigabyte: it needs a
/// profile and a yes, and is refused before anything is read or fetched.
#[test]
fn the_provider_directory_needs_a_profile_and_a_yes() {
    let run = with_profile();
    let out = run(&["tables", "fetch", "nppes"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("home"));
    let out = run(&[
        "--profile",
        "home",
        "tables",
        "fetch",
        "nppes",
        "--file",
        "/nonexistent.zip",
    ]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("--file"));
    for args in [
        &["--profile", "home", "tables", "fetch", "nppes"][..],
        &[
            "--json",
            "--profile",
            "home",
            "tables",
            "fetch",
            "icd10cm",
            "nppes",
        ][..],
    ] {
        let out = run(args);
        assert_eq!(out.status.code(), Some(6), "{args:?}");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("over a gigabyte"),
            "{args:?}"
        );
    }
}

#[test]
fn cache_clear_keeps_the_code_tables() {
    let dir = std::env::temp_dir().join(format!("medrec-surface-tables-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let base = dir.join("cache/medrec");
    std::fs::create_dir_all(base.join("tables")).unwrap();
    std::fs::create_dir_all(base.join("copies")).unwrap();
    std::fs::write(base.join("tables/icd10cm-FY2027.zip"), b"x").unwrap();
    std::fs::create_dir_all(base.join("directory")).unwrap();
    std::fs::write(base.join("directory/nppes-f_x.jsonl"), b"x").unwrap();
    std::fs::write(base.join("copies/a.pdf"), b"x").unwrap();
    let run = |args: &[&str]| {
        medrec()
            .env("XDG_CACHE_HOME", dir.join("cache"))
            .args(args)
            .output()
            .unwrap()
    };
    // One that does not read is there for its folder, and never used.
    let v = json_of(&run(&["--json", "tables", "list"]));
    assert_eq!(v["items"][1]["state"], "changed");
    assert_eq!(v["items"][1]["folder"], "f_x");
    let out = run(&["--json", "cache", "clear"]);
    assert!(out.status.success());
    assert_eq!(json_of(&out)["files"], 2);
    assert!(!base.join("directory").exists());
    assert!(base.join("tables/icd10cm-FY2027.zip").exists());
    assert!(!base.join("copies").exists());
    // A file that is not the recorded one is there and never read.
    let v = json_of(&run(&["--json", "tables", "list"]));
    assert_eq!(v["items"][0]["state"], "changed");
    assert_eq!(v["items"][1]["state"], "absent");
    let _ = std::fs::remove_dir_all(&dir);
}
