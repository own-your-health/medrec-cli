//! `medrec refresh`, through the binary, on a local records folder with
//! synthetic data and a fake provider (`medrec::testdata`). Offline; no
//! Keychain; no launchd (a fake `launchctl` records its arguments).

use std::path::{Path, PathBuf};
use std::process::Output;

use assert_cmd::Command;
use serde_json::{json, Value};

struct Env {
    root: PathBuf,
    path: String,
}

impl Env {
    fn new(tag: &str) -> Env {
        let root =
            std::env::temp_dir().join(format!("medrec-refresh-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let dir =
            medrec::testdata::write_provider(&root.join("provider"), medrec::testdata::STREAM);
        let launchctl = root.join("launchctl");
        std::fs::write(
            &launchctl,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n",
                root.join("launchctl-calls").display()
            ),
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&launchctl, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let path = format!(
            "{}:{}",
            dir.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let e = Env { root, path };
        e.ok(&["profiles", "add", "home"]);
        e
    }

    fn run_env(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let mut c = Command::cargo_bin("medrec").unwrap();
        c.env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_CACHE_HOME", self.root.join("cache"))
            .env("HOME", &self.root)
            .env("PATH", &self.path)
            .env("MEDREC_PROFILE", "home")
            .env("MEDREC_KEYCHAIN_SERVICE", "piekstra.medrec.hermetic-tests")
            .env("MEDREC_LAUNCH_AGENTS_DIR", self.root.join("LaunchAgents"))
            .env("MEDREC_LAUNCHCTL", self.root.join("launchctl"))
            .env("MEDREC_TEST_AUTH_WAIT_SECS", "1")
            .env_remove("NO_COLOR")
            .env_remove("FAKEPROV_AUTH");
        for (k, v) in env {
            c.env(k, v);
        }
        c.args(args).output().unwrap()
    }

    fn run(&self, args: &[&str]) -> Output {
        self.run_env(args, &[])
    }

    /// A `--json` run: its exit code and document.
    fn doc(&self, args: &[&str], env: &[(&str, &str)]) -> (Option<i32>, Value) {
        let mut a = vec!["--json"];
        a.extend_from_slice(args);
        let out = self.run_env(&a, env);
        let v = serde_json::from_slice(&out.stdout).unwrap_or_else(|_| {
            panic!(
                "medrec {args:?} printed no JSON document: {}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            )
        });
        (out.status.code(), v)
    }

    fn ok(&self, args: &[&str]) -> Value {
        self.ok_env(args, &[])
    }

    fn ok_env(&self, args: &[&str], env: &[(&str, &str)]) -> Value {
        let (code, v) = self.doc(args, env);
        assert_eq!(code, Some(0), "medrec {args:?}: {v}");
        v
    }

    fn code(&self, args: &[&str]) -> (Option<i32>, String) {
        let out = self.run(args);
        (
            out.status.code(),
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        )
    }

    fn records(&self) -> PathBuf {
        self.root.join("records")
    }

    fn json_at(&self, rel: &str) -> Value {
        serde_json::from_slice(&std::fs::read(self.records().join(rel)).unwrap()).unwrap()
    }

    /// Make the fake provider's last import two days old, so the interval
    /// allows another.
    fn age_imports(&self) {
        let p = self.records().join("Robin Testperson/index.json");
        let mut v: Value = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
        for r in v["imports"].as_array_mut().unwrap() {
            r["imported_at"] = "2026-01-01T00:00:00Z".into();
        }
        std::fs::write(&p, serde_json::to_vec_pretty(&v).unwrap()).unwrap();
    }

    fn request(&self, id: &str, scope: Value) {
        let dir = self.records().join("_requests");
        std::fs::create_dir_all(&dir).unwrap();
        let name = format!("20261122T200312Z_{id}.json");
        std::fs::write(
            dir.join(name),
            serde_json::to_vec(&json!({
                "schema": "medrec-request/v1",
                "id": id,
                "requested_at": "2026-11-22T20:03:12Z",
                "requested_by": {"email": "alex@example.com", "name": "Alex Testperson"},
                "scope": scope,
                "app": {"name": "own-your-health-app", "version": "0.1.0"},
            }))
            .unwrap(),
        )
        .unwrap();
    }

    fn auth_calls(&self) -> usize {
        std::fs::read_to_string(self.root.join("provider/auth-calls"))
            .map(|t| t.lines().filter(|l| l.starts_with("auth status")).count())
            .unwrap_or(0)
    }
}

fn walk(dir: &Path, f: &mut dyn FnMut(&Path)) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let p = entry.unwrap().path();
        if p.is_dir() {
            walk(&p, f);
        } else {
            f(&p);
        }
    }
}

fn attempt_lines(e: &Env) -> Vec<Value> {
    let dir = e.records().join("_status/attempts");
    let mut out = Vec::new();
    if dir.is_dir() {
        walk(&dir, &mut |p| {
            for l in std::fs::read_to_string(p).unwrap().lines() {
                out.push(serde_json::from_str(l).unwrap());
            }
        });
    }
    out
}

/// A records folder, Robin, and one import from fakeprov: its source id.
fn setup(e: &Env) -> String {
    e.ok(&["init", e.records().to_str().unwrap()]);
    e.ok(&[
        "people",
        "add",
        "Alex Testperson",
        "--birth-date",
        "1990-01-01",
    ]);
    e.ok(&[
        "people",
        "add",
        "Robin Testperson",
        "--birth-date",
        "2020-02-02",
        "--guardian",
        "Alex",
    ]);
    e.ok(&[
        "import",
        "--from",
        "fakeprov",
        "--from-profile",
        "adult1",
        "--patient",
        "pat-0001",
        "--person",
        "Robin",
    ]);
    let h = e.json_at("household.json");
    h["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["cli"] == "fakeprov")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string()
}

#[test]
fn refresh_imports_on_request_and_never_waits_on_a_person() {
    let e = Env::new("flow");
    let src = setup(&e);

    // A binding names both profiles, on the command line.
    let (code, text) = e.code(&[
        "refresh",
        "add",
        "--source",
        &src,
        "--provider-profile",
        "adult1",
    ]);
    assert_eq!(code, Some(2), "{text}");
    assert!(text.contains("--profile"), "{text}");
    let (code, text) = e.code(&["--profile", "home", "refresh", "add", "--source", &src]);
    assert_eq!(code, Some(2), "{text}");
    assert!(text.contains("--provider-profile"), "{text}");
    let (code, text) = e.code(&[
        "--profile",
        "home",
        "refresh",
        "add",
        "--source",
        &src,
        "--provider-profile",
        "adult1",
        "--every",
        "1h",
    ]);
    assert_eq!(
        code,
        Some(2),
        "a member site is imported at most daily: {text}"
    );
    let b = e.ok(&[
        "--profile",
        "home",
        "refresh",
        "add",
        "--source",
        &src,
        "--provider-profile",
        "adult1",
    ]);
    assert_eq!(
        (b["auto"].clone(), b["every_secs"].clone()),
        (json!(true), json!(86400))
    );
    assert_eq!(
        e.ok(&["refresh", "list"])["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    // Imported a moment ago: the interval holds, even when asked.
    let (code, v) = e.doc(&["refresh"], &[]);
    assert_eq!(code, Some(0), "{v}");
    assert_eq!(v["attempts"][0]["outcome"], "skipped_interval", "{v}");

    // Two days on, it imports.
    e.age_imports();
    let v = e.ok(&["refresh", "--source", &src]);
    assert_eq!(v["attempts"][0]["outcome"], "ok", "{v}");
    let st = e.json_at("_status/status.json");
    assert_eq!(st["schema"], "medrec-status/v1");
    assert_eq!(st["format"], 1);
    assert!(st["refresher"]["machine"]
        .as_str()
        .unwrap()
        .starts_with("h_"));
    let s0 = &st["sources"][0];
    assert_eq!(s0["source"], src);
    assert_eq!(s0["sign_in"]["state"], "ok");
    assert_eq!(s0["failures"], 0);
    assert!(
        s0["last_success"]["import"]
            .as_str()
            .unwrap()
            .starts_with("i_"),
        "{s0}"
    );
    let lines = attempt_lines(&e);
    assert_eq!(
        lines
            .iter()
            .map(|l| l["outcome"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["skipped_interval", "ok"]
    );
    assert!(lines
        .iter()
        .all(|l| l["schema"] == "medrec-attempt/v1"
            && l["attempt"].as_str().unwrap().starts_with("t_")));

    // Nothing due and nothing asked: a scheduled pass does nothing more
    // than list _requests/.
    let v = e.ok(&["refresh", "--scheduled"]);
    assert_eq!(v["status"], "idle", "{v}");

    // A request is served by the next scheduled pass and moved to done/;
    // a file that is not a request is answered as invalid.
    e.age_imports();
    e.request("x_aaaaaaaaaaaaaaaaaaaa", json!({ "source": src }));
    std::fs::write(
        e.records().join("_requests/notes.json"),
        b"{\"please\": true}",
    )
    .unwrap();
    let v = e.ok(&["refresh", "--scheduled"]);
    assert_eq!(v["attempts"][0]["trigger"], "request", "{v}");
    assert_eq!(v["attempts"][0]["outcome"], "ok", "{v}");
    let pending: Vec<_> = std::fs::read_dir(e.records().join("_requests"))
        .unwrap()
        .filter_map(|d| d.ok())
        .filter(|d| d.path().is_file())
        .collect();
    assert!(pending.is_empty(), "requests left pending");
    let done = e.json_at("_requests/done/20261122T200312Z_x_aaaaaaaaaaaaaaaaaaaa.json");
    assert_eq!(done["handled"]["outcome"], "ok", "{done}");
    assert_eq!(done["handled"]["sources"][0]["source"], src);
    assert!(done["handled"]["sources"][0]["attempt"]
        .as_str()
        .unwrap()
        .starts_with("t_"));
    let mut invalid = 0;
    walk(&e.records().join("_requests/done"), &mut |p| {
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        if name.starts_with("invalid_") {
            let v: Value = serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap();
            assert_eq!(v["handled"]["outcome"], "invalid");
            assert_eq!(v["received"]["bytes"], 16);
            assert!(v.get("please").is_none());
            invalid += 1;
        }
    });
    assert_eq!(invalid, 1);

    // A sign-in that waits on an approval nobody gives is stopped, and the
    // source needs a person, with the command to run.
    e.age_imports();
    e.request("x_bbbbbbbbbbbbbbbbbbbb", json!({"all": true}));
    let hang = [("FAKEPROV_AUTH", "hang")];
    let started = std::time::Instant::now();
    let v = e.ok_env(&["refresh", "--scheduled"], &hang);
    assert!(started.elapsed().as_secs() < 20, "it waited on the sign-in");
    assert_eq!(v["attempts"][0]["outcome"], "needs_person", "{v}");
    assert_eq!(v["attempts"][0]["error"], "timeout", "{v}");
    let command = format!("medrec refresh sign-in {src}");
    assert_eq!(v["attempts"][0]["command"], command.as_str());
    let st = e.json_at("_status/status.json");
    assert_eq!(
        st["sources"][0]["sign_in"]["needs_person"]["reason"],
        "keychain"
    );
    assert_eq!(
        st["sources"][0]["sign_in"]["needs_person"]["command"],
        command.as_str()
    );
    assert_eq!(st["sources"][0]["next_run_at"], Value::Null);
    let done = e.json_at("_requests/done/20261122T200312Z_x_bbbbbbbbbbbbbbbbbbbb.json");
    assert_eq!(done["handled"]["outcome"], "needs_person", "{done}");

    // Not tried again in the background until a person has signed in.
    let calls = e.auth_calls();
    e.request("x_cccccccccccccccccccc", json!({"all": true}));
    let v = e.ok_env(&["refresh", "--scheduled"], &hang);
    assert_eq!(v["attempts"][0]["outcome"], "needs_person", "{v}");
    assert_eq!(e.auth_calls(), calls, "the sign-in was read again");

    // `sign-in` takes the source id: the profile is the one bound to it,
    // printed before anything signs in. Another profile is refused.
    e.ok(&["profiles", "add", "other"]);
    let (code, text) = e.code(&["--profile", "other", "refresh", "sign-in", &src]);
    assert_eq!(code, Some(2), "{text}");
    assert!(text.contains("profile `home`"), "{text}");
    let out = e.run_env(
        &["--json", "refresh", "sign-in", &src],
        &[("FAKEPROV_AUTH", "signed-out")],
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("medrec profile `home`, provider profile `adult1`"),
        "{stderr}"
    );
    assert!(stderr.contains("fakeprov: signed in"), "{stderr}");
    let v: Value = serde_json::from_slice(&out.stdout).expect("one JSON document");
    assert_eq!(out.status.code(), Some(0), "{v}");
    assert_eq!(
        (v["state"].clone(), v["needs_person"].clone()),
        (json!("ok"), json!(false))
    );
    let st = e.json_at("_status/status.json");
    assert_eq!(st["sources"][0]["sign_in"]["needs_person"], Value::Null);

    // A paused source answers without being tried.
    e.ok(&[
        "refresh",
        "pause",
        &src,
        "--reason",
        "consent",
        "--note",
        "billing consent",
    ]);
    let calls = e.auth_calls();
    e.request("x_dddddddddddddddddddd", json!({ "source": src }));
    let v = e.ok(&["refresh", "--scheduled"]);
    assert_eq!(v["attempts"][0]["outcome"], "skipped_paused", "{v}");
    assert_eq!(e.auth_calls(), calls);
    let st = e.json_at("_status/status.json");
    assert_eq!(st["sources"][0]["paused"]["reason"], "consent");
    e.ok(&["refresh", "resume", &src]);

    // The provider's profile name never reaches the records folder.
    walk(&e.records(), &mut |p| {
        if let Ok(t) = std::fs::read_to_string(p) {
            assert!(!t.contains("adult1"), "{}", p.display());
        }
    });
    // And verify takes _status/ and _requests/ as the folder's own.
    let v = e.ok(&["verify"]);
    assert_eq!(v["missing"].as_array().unwrap().len(), 0, "{v}");
    assert_eq!(v["unindexed"].as_array().unwrap().len(), 0, "{v}");

    let _ = std::fs::remove_dir_all(&e.root);
}

#[test]
fn one_machine_refreshes_a_folder_until_another_takes_over() {
    let e = Env::new("install");
    let src = setup(&e);
    e.ok(&[
        "--profile",
        "home",
        "refresh",
        "add",
        "--source",
        &src,
        "--provider-profile",
        "adult1",
    ]);

    let v = e.ok(&["refresh", "--install", "--pass-every", "10m"]);
    assert_eq!(v["pass_every_secs"], 600);
    assert_eq!(v["took_over"], false);
    let plist = e
        .root
        .join("LaunchAgents/io.github.own-your-health.medrec.refresh.home.plist");
    let text = std::fs::read_to_string(&plist).unwrap();
    assert!(text.contains("<string>--scheduled</string>"), "{text}");
    assert!(text.contains("<integer>600</integer>"), "{text}");
    let calls = std::fs::read_to_string(e.root.join("launchctl-calls")).unwrap();
    assert!(calls.contains("bootstrap gui/"), "{calls}");
    for dir in ["_status/attempts", "_requests/done"] {
        assert!(e.records().join(dir).is_dir(), "{dir}");
    }

    // Another machine's fresh heartbeat: this one stands down.
    let path = e.records().join("_status/status.json");
    let mut st: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    st["refresher"]["machine"] = "h_bbbbbbbbbbbbbbbbbbbb".into();
    st["refresher"]["last_pass_at"] = medrec::model::now_rfc3339().into();
    std::fs::write(&path, serde_json::to_vec(&st).unwrap()).unwrap();
    let (code, text) = e.code(&["refresh"]);
    assert_eq!(code, Some(2), "{text}");
    assert!(text.contains("--take-over"), "{text}");
    e.request("x_aaaaaaaaaaaaaaaaaaaa", json!({"all": true}));
    let v = e.ok(&["refresh", "--scheduled"]);
    assert_eq!(v["status"], "other_machine", "{v}");
    assert!(e
        .records()
        .join("_requests/20261122T200312Z_x_aaaaaaaaaaaaaaaaaaaa.json")
        .exists());
    assert_eq!(e.code(&["refresh", "--install"]).0, Some(2));

    // Taking over waits one pass before serving requests.
    let v = e.ok(&["refresh", "--install", "--take-over"]);
    assert_eq!(v["took_over"], true);
    let st = e.json_at("_status/status.json");
    assert_ne!(st["refresher"]["machine"], "h_bbbbbbbbbbbbbbbbbbbb");
    let v = e.ok(&["refresh", "--scheduled"]);
    assert_eq!(v["status"], "waiting", "{v}");
    assert!(v["requests"].as_array().unwrap().is_empty(), "{v}");
    let (code, text) = e.code(&["refresh"]);
    assert_ne!(code, Some(0), "{text}");
    assert!(text.contains("took refresh over"), "{text}");

    let v = e.ok(&["refresh", "--uninstall"]);
    assert_eq!(v["removed"], true);
    assert!(!plist.exists());
    assert_eq!(e.ok(&["refresh", "--uninstall"])["removed"], false);
    let _ = std::fs::remove_dir_all(&e.root);
}

#[test]
fn an_attempt_a_stopped_pass_left_running_counts_as_a_failure() {
    let e = Env::new("interrupted");
    let src = setup(&e);
    e.ok(&[
        "--profile",
        "home",
        "refresh",
        "add",
        "--source",
        &src,
        "--provider-profile",
        "adult1",
    ]);
    e.age_imports();
    e.ok(&["refresh"]);
    // What a pass killed mid-import leaves behind on this machine.
    let mut state = None;
    walk(&e.root.join("cache/medrec/refresh"), &mut |p| {
        if p.extension().is_some_and(|x| x == "json") {
            state = Some(p.to_path_buf());
        }
    });
    let state = state.expect("refresh keeps its state in the cache");
    let mut v: Value = serde_json::from_slice(&std::fs::read(&state).unwrap()).unwrap();
    let la = &mut v["status"]["sources"][0]["last_attempt"];
    la["outcome"] = "running".into();
    la["finished_at"] = Value::Null;
    la["started_at"] = medrec::model::now_rfc3339().into();
    std::fs::write(&state, serde_json::to_vec(&v).unwrap()).unwrap();

    e.age_imports();
    let (code, v) = e.doc(&["refresh"], &[]);
    assert_eq!(code, Some(5), "a failure found exits 5: {v}");
    let outcomes: Vec<&str> = v["attempts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["outcome"].as_str().unwrap())
        .collect();
    // The interrupted one is recorded as failed, and its 30-minute wait
    // holds the source back.
    assert_eq!(outcomes, ["failed", "skipped_interval"], "{v}");
    assert_eq!(v["attempts"][0]["error"], "interrupted");
    let st = e.json_at("_status/status.json");
    assert_eq!(st["sources"][0]["failures"], 1);
    let _ = std::fs::remove_dir_all(&e.root);
}

#[test]
fn a_signed_out_login_needs_a_person_and_is_not_read_again_in_the_background() {
    let e = Env::new("signed-out");
    let src = setup(&e);
    e.ok(&[
        "--profile",
        "home",
        "refresh",
        "add",
        "--source",
        &src,
        "--from-profile",
        "adult1",
    ]);
    e.age_imports();
    e.request("x_aaaaaaaaaaaaaaaaaaaa", json!({"all": true}));
    let out = [("FAKEPROV_AUTH", "signed-out")];
    let v = e.ok_env(&["refresh", "--scheduled"], &out);
    assert_eq!(v["attempts"][0]["outcome"], "needs_person", "{v}");
    assert_eq!(v["attempts"][0]["error"], "needs_person", "{v}");
    let st = e.json_at("_status/status.json");
    let s0 = &st["sources"][0];
    assert_eq!(s0["sign_in"]["state"], "signed_out");
    assert_eq!(s0["sign_in"]["needs_person"]["reason"], "sign_in");
    assert_eq!(
        s0["sign_in"]["needs_person"]["command"],
        format!("medrec refresh sign-in {src}").as_str()
    );
    assert_eq!(s0["failures"], 0, "a sign-in is not a failure to back off");
    let calls = e.auth_calls();
    e.request("x_bbbbbbbbbbbbbbbbbbbb", json!({"all": true}));
    e.ok_env(&["refresh", "--scheduled"], &[("FAKEPROV_AUTH", "expired")]);
    assert_eq!(e.auth_calls(), calls);

    // `cache clear` keeps this machine the refresher, and its history.
    e.ok(&["cache", "clear"]);
    e.request("x_cccccccccccccccccccc", json!({"all": true}));
    let v = e.ok(&["refresh", "--scheduled"]);
    assert_eq!(v["status"], "ran", "{v}");
    assert_eq!(v["requests"][0]["outcome"], "needs_person", "{v}");
    let st = e.json_at("_status/status.json");
    assert!(
        st["sources"][0]["sign_in"]["needs_person"].is_object(),
        "{st}"
    );
    let _ = std::fs::remove_dir_all(&e.root);
}

#[test]
fn a_pass_runs_only_the_provider_the_binding_names() {
    let e = Env::new("tampered");
    let src = setup(&e);
    e.ok(&[
        "--profile",
        "home",
        "refresh",
        "add",
        "--source",
        &src,
        "--provider-profile",
        "adult1",
    ]);
    let b = e.ok(&["refresh", "list"]);
    assert_eq!(b["items"][0]["cli"], "fakeprov", "{b}");
    e.age_imports();
    let path = e.records().join("household.json");
    let mut h: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    for s in h["sources"].as_array_mut().unwrap() {
        if s["id"] == src.as_str() {
            s["cli"] = e
                .root
                .join("provider/fakeprov")
                .to_string_lossy()
                .into_owned()
                .into();
        }
    }
    std::fs::write(&path, serde_json::to_vec_pretty(&h).unwrap()).unwrap();
    let calls = e.auth_calls();
    let (code, v) = e.doc(&["refresh"], &[]);
    assert_eq!(code, Some(5), "{v}");
    assert_eq!(v["attempts"][0]["outcome"], "failed", "{v}");
    assert_eq!(v["attempts"][0]["error"], "provider", "{v}");
    assert_eq!(e.auth_calls(), calls, "the provider ran");
    let _ = std::fs::remove_file(e.root.join("provider/export-args"));
    let (code, _) = e.doc(&["refresh"], &[]);
    assert_eq!(code, Some(5));
    assert!(
        !e.root.join("provider/export-args").exists(),
        "the provider exported"
    );
    let _ = std::fs::remove_dir_all(&e.root);
}

#[test]
fn a_newer_status_is_left_alone_and_a_take_over_keeps_the_history() {
    let e = Env::new("format");
    let src = setup(&e);
    e.ok(&[
        "--profile",
        "home",
        "refresh",
        "add",
        "--source",
        &src,
        "--provider-profile",
        "adult1",
    ]);
    e.age_imports();
    e.ok(&["refresh"]);
    let path = e.records().join("_status/status.json");
    let before: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert!(before["sources"][0]["last_success"].is_object());

    // Another machine, its heartbeat gone stale: a take-over keeps each
    // source's history, and not that machine's sign-in.
    let mut other = before.clone();
    other["refresher"]["machine"] = "h_bbbbbbbbbbbbbbbbbbbb".into();
    other["refresher"]["last_pass_at"] = "2026-01-01T00:00:00Z".into();
    std::fs::write(&path, serde_json::to_vec(&other).unwrap()).unwrap();
    let _ = std::fs::remove_dir_all(e.root.join("cache/medrec/refresh"));
    e.ok(&["refresh", "--install", "--take-over"]);
    let st = e.json_at("_status/status.json");
    assert_eq!(
        st["sources"][0]["last_success"], before["sources"][0]["last_success"],
        "{st}"
    );
    assert_eq!(st["sources"][0]["sign_in"]["state"], "unknown");

    // A status.json a later medrec wrote is not rewritten.
    let mut newer = st.clone();
    newer["format"] = 2.into();
    std::fs::write(&path, serde_json::to_vec(&newer).unwrap()).unwrap();
    let written = std::fs::read(&path).unwrap();
    let (code, text) = e.code(&["refresh"]);
    assert_ne!(code, Some(0), "{text}");
    assert!(text.contains("self-update"), "{text}");
    assert_eq!(e.code(&["refresh", "status"]).0, Some(1));
    assert_eq!(std::fs::read(&path).unwrap(), written);
    let _ = std::fs::remove_dir_all(&e.root);
}

#[test]
fn a_folder_another_writer_holds_records_nothing_and_keeps_the_request() {
    let e = Env::new("busy");
    let src = setup(&e);
    e.ok(&[
        "--profile",
        "home",
        "refresh",
        "add",
        "--source",
        &src,
        "--provider-profile",
        "adult1",
    ]);
    e.ok(&["config", "set", "import_timeout_secs", "1"]);
    e.age_imports();
    e.ok(&["refresh"]);
    let lines = attempt_lines(&e).len();
    e.age_imports();

    // The folder's lock, held here as another medrec writing it would.
    let mut lock = None;
    walk(&e.root.join("cache/medrec/locks"), &mut |p| {
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        if name.ends_with(".lock") && !name.starts_with("refresh-") {
            lock = Some(p.to_path_buf());
        }
    });
    let lock = lock.expect("the import left the folder's lock file");
    let held = std::fs::OpenOptions::new().write(true).open(&lock).unwrap();
    held.lock().unwrap();

    e.request("x_aaaaaaaaaaaaaaaaaaaa", json!({"all": true}));
    let v = e.ok(&["refresh", "--scheduled"]);
    drop(held);
    assert_eq!(v["attempts"][0]["outcome"], "busy", "{v}");
    assert!(v["requests"].as_array().unwrap().is_empty(), "{v}");
    assert_eq!(attempt_lines(&e).len(), lines, "an attempt was recorded");
    let st = e.json_at("_status/status.json");
    assert_eq!(st["sources"][0]["failures"], 0);
    assert_eq!(st["sources"][0]["last_attempt"]["outcome"], "ok", "{st}");
    assert!(e
        .records()
        .join("_requests/20261122T200312Z_x_aaaaaaaaaaaaaaaaaaaa.json")
        .exists());

    // The next pass, with the folder free, serves it.
    let v = e.ok(&["refresh", "--scheduled"]);
    assert_eq!(v["requests"][0]["outcome"], "ok", "{v}");
    let _ = std::fs::remove_dir_all(&e.root);
}

/// Gives a directory its owner's write permission back, whatever a test
/// asserted before.
struct Writable(std::path::PathBuf);

impl Drop for Writable {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o700));
    }
}

#[test]
fn a_request_file_medrec_cannot_remove_is_handled_once() {
    use std::os::unix::fs::PermissionsExt;
    // SAFETY: geteuid reads the process's effective user id; no memory.
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("skipped: as root, a directory without write permission still allows unlinking");
        return;
    }
    let e = Env::new("left");
    let src = setup(&e);
    e.ok(&[
        "--profile",
        "home",
        "refresh",
        "add",
        "--source",
        &src,
        "--provider-profile",
        "adult1",
    ]);
    e.age_imports();
    std::fs::create_dir_all(e.records().join("_requests/done")).unwrap();
    e.request("x_aaaaaaaaaaaaaaaaaaaa", json!({"all": true}));
    let requests = e.records().join("_requests");
    let name = "20261122T200312Z_x_aaaaaaaaaaaaaaaaaaaa.json";
    let pending = requests.join(name);
    let answer = requests.join("done").join(name);
    // As a file another account owns: the folder's owner cannot remove it.
    let writable = Writable(requests.clone());
    std::fs::set_permissions(&requests, std::fs::Permissions::from_mode(0o500)).unwrap();

    let v = e.ok(&["refresh", "--scheduled"]);
    assert_eq!(v["requests"][0]["outcome"], "ok", "{v}");
    assert_eq!(v["requests"][0]["file_left"], true, "{v}");
    assert!(pending.exists() && answer.exists());

    // A scheduled pass passes over it without opening the folder.
    let v = e.ok(&["refresh", "--scheduled"]);
    assert_eq!(v["status"], "idle", "{v}");

    // Past the answers' 90 days, at the daily prune, its answer stays.
    std::fs::File::options()
        .write(true)
        .open(&answer)
        .unwrap()
        .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000))
        .unwrap();
    let mut state = None;
    walk(&e.root.join("cache/medrec/refresh"), &mut |p| {
        if p.extension().is_some_and(|x| x == "json") {
            state = Some(p.to_path_buf());
        }
    });
    let state = state.expect("refresh keeps its state in the cache");
    let mut s: Value = serde_json::from_slice(&std::fs::read(&state).unwrap()).unwrap();
    assert_eq!(s["left"], json!([name]), "{s}");
    s["pruned_at"] = 0.into();
    std::fs::write(&state, serde_json::to_vec(&s).unwrap()).unwrap();
    let v = e.ok(&["refresh"]);
    assert!(v["requests"].as_array().unwrap().is_empty(), "{v}");
    let pruned: Value = serde_json::from_slice(&std::fs::read(&state).unwrap()).unwrap();
    assert!(
        pruned["pruned_at"].as_i64().unwrap() > 0,
        "the prune did not run"
    );
    assert!(
        answer.exists(),
        "the prune removed the answer of a request still pending"
    );
    assert!(pending.exists());

    // Once it can, a later pass removes it.
    drop(writable);
    e.ok(&["refresh"]);
    assert!(!pending.exists());
    let s: Value = serde_json::from_slice(&std::fs::read(&state).unwrap()).unwrap();
    assert!(s.get("left").is_none(), "{s}");
    let _ = std::fs::remove_dir_all(&e.root);
}

#[test]
fn a_refused_export_signs_in_again_with_the_providers_own_command_and_names_no_profile() {
    let e = Env::new("refused");
    let src = setup(&e);
    e.ok(&[
        "--profile",
        "home",
        "refresh",
        "add",
        "--source",
        &src,
        "--provider-profile",
        "adult1",
    ]);
    e.age_imports();
    // The site refuses a session `auth status` reads as valid, and says so
    // naming the login, as a member site's CLI does.
    let refused = [
        ("FAKEPROV_EXIT", "3"),
        (
            "FAKEPROV_STDERR",
            "error: the session for profile adult1 has expired; sign in again: `fakeprov auth login --browser --profile adult1 --overwrite`",
        ),
    ];
    let v = e.ok_env(&["refresh", "--scheduled"], &refused);
    assert_eq!(v["attempts"][0]["outcome"], "needs_person", "{v}");
    let st = e.json_at("_status/status.json");
    let s0 = &st["sources"][0];
    assert_eq!(s0["sign_in"]["state"], "ok", "{st}");
    assert_eq!(s0["sign_in"]["needs_person"]["reason"], "sign_in");
    let message = s0["last_attempt"]["error"]["message"].as_str().unwrap();
    assert!(message.contains("<provider profile>"), "{message}");
    // No file in the folder names a profile, medrec's or the provider's.
    let mut named = Vec::new();
    walk(&e.records(), &mut |p| {
        let text = String::from_utf8_lossy(&std::fs::read(p).unwrap()).into_owned();
        if text.contains("adult1") || text.contains("\"home\"") || text.contains(" home ") {
            named.push(p.to_path_buf());
        }
    });
    assert!(named.is_empty(), "{named:?}");

    // A sign-in the person does not finish leaves the source waiting on
    // one, though the status still reads as valid.
    let (_, v) = e.doc(
        &["refresh", "sign-in", &src],
        &[("FAKEPROV_LOGIN_FAIL", "1")],
    );
    assert_eq!(v["logged_in"], false, "{v}");
    assert_eq!(v["needs_person"], true, "{v}");

    // `sign-in` runs the provider's own sign-in for this login although its
    // status reads as valid, replacing the stored one.
    let v = e.ok(&["refresh", "sign-in", &src]);
    assert_eq!(v["logged_in"], true, "{v}");
    assert_eq!(v["needs_person"], false, "{v}");
    let calls = std::fs::read_to_string(e.root.join("provider/auth-calls")).unwrap();
    let login: Vec<&str> = calls
        .lines()
        .filter(|l| l.starts_with("auth login"))
        .collect();
    let own = "auth login --browser --profile adult1 --org example-health --label Robin Testperson --overwrite";
    assert_eq!(login, [own, own], "{calls}");
    let st = e.json_at("_status/status.json");
    assert_eq!(
        st["sources"][0]["sign_in"]["needs_person"],
        Value::Null,
        "{st}"
    );

    // A line an earlier medrec wrote with the name loses it at the next
    // attempt, and none is lost.
    let month = std::fs::read_dir(e.records().join("_status/attempts"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let mut text = std::fs::read_to_string(&month).unwrap();
    text.push_str(r#"{"schema":"medrec-attempt/v1","attempt":"t_aaaaaaaaaaaaaaaaaaaa","outcome":"needs_person","error":{"code":"auth","message":"profile adult1 expired","remedy":"`fakeprov auth login --profile adult1`"}}"#);
    text.push('\n');
    std::fs::write(&month, &text).unwrap();
    let lines = text.lines().count();
    e.age_imports();
    let v = e.ok(&["refresh", "--scheduled"]);
    assert_eq!(v["attempts"][0]["outcome"], "ok", "{v}");
    let after = std::fs::read_to_string(&month).unwrap();
    assert_eq!(after.lines().count(), lines + 1);
    assert!(!after.contains("adult1"), "{after}");
    assert!(after.contains("t_aaaaaaaaaaaaaaaaaaaa"));
    let _ = std::fs::remove_dir_all(&e.root);
}

/// A stand-in for `medrec drive-keychain-probe` that counts its runs, then
/// hangs (a Keychain approval nobody gives) or answers signed in.
fn drive_probe(e: &Env, name: &str, hang: bool) -> PathBuf {
    let path = e.root.join(name);
    let count = e.root.join(format!("{name}-runs"));
    let body = if hang {
        "sleep 30".to_string()
    } else {
        r#"printf '%s' '{"schema":"auth-status/v1","required":true,"authenticated":true,"session_valid":true}'"#.to_string()
    };
    std::fs::write(
        &path,
        format!("#!/bin/sh\necho run >> '{}'\n{body}\n", count.display()),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn runs(e: &Env, name: &str) -> usize {
    std::fs::read_to_string(e.root.join(format!("{name}-runs")))
        .map(|s| s.lines().count())
        .unwrap_or(0)
}

#[test]
fn a_scheduled_pass_never_waits_on_the_drive_sign_ins_keychain_approval() {
    let e = Env::new("drive-gate");
    e.ok(&[
        "config",
        "set",
        "folder",
        "gdrive:home-drive/1Synthetic_folder",
    ]);
    let hang = drive_probe(&e, "hang", true);
    let hang = hang.to_str().unwrap();
    let env = [
        ("MEDREC_TEST_DRIVE_PROBE", hang),
        ("MEDREC_TEST_AUTH_WAIT_SECS", "1"),
    ];
    let out = e.run_env(&["--json", "refresh", "--scheduled"], &env);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["status"], "needs_person", "{v}");
    assert_eq!(v["command"], "medrec --profile home refresh");
    // The same binary is not checked again: no second approval is asked.
    let out = e.run_env(&["--json", "refresh", "--scheduled"], &env);
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["status"], "needs_person");
    assert_eq!(runs(&e, "hang"), 1);
    // Another binary (an update) is checked; one that answers lets the pass
    // go on to the folder, which here has no sign-in on this machine.
    let ok = drive_probe(&e, "ok", false);
    let env = [
        ("MEDREC_TEST_DRIVE_PROBE", ok.to_str().unwrap()),
        ("MEDREC_TEST_AUTH_WAIT_SECS", "1"),
    ];
    let out = e.run_env(&["--json", "refresh", "--scheduled"], &env);
    assert_eq!(
        out.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(String::from_utf8_lossy(&out.stderr)
        .contains("medrec drive login --drive-profile home-drive"));
    assert_eq!(runs(&e, "ok"), 1);
    // Known now to read without an approval: not checked again.
    e.run_env(&["--json", "refresh", "--scheduled"], &env);
    assert_eq!(runs(&e, "ok"), 1);
}
