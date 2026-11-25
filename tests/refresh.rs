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
            .env("TZ", "America/New_York")
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
        self.loaded_at("2026-01-01T00:00:00Z");
    }

    /// Every import, and this machine's record of when each source last
    /// ran its provider, put at `at`.
    fn loaded_at(&self, at: &str) {
        for person in std::fs::read_dir(self.records()).unwrap() {
            let p = person.unwrap().path().join("index.json");
            if !p.is_file() {
                continue;
            }
            let mut v: Value = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
            for r in v["imports"].as_array_mut().unwrap() {
                r["imported_at"] = at.into();
            }
            std::fs::write(&p, serde_json::to_vec_pretty(&v).unwrap()).unwrap();
        }
        let secs = medrec::refresh::time::parse(at).unwrap();
        self.local(&mut |local| {
            if let Some(m) = local.get_mut("ran_at").and_then(Value::as_object_mut) {
                for t in m.values_mut() {
                    *t = json!(secs);
                }
            }
        });
    }

    /// This machine's record of its passes, changed by `f`.
    fn local(&self, f: &mut dyn FnMut(&mut Value)) {
        let cache = self.root.join("cache");
        if !cache.is_dir() {
            return;
        }
        walk(&cache, &mut |p| {
            let in_refresh = p.parent().and_then(|d| d.file_name()) == Some("refresh".as_ref());
            if !in_refresh || p.extension() != Some("json".as_ref()) {
                return;
            }
            let mut local: Value = serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap();
            f(&mut local);
            std::fs::write(p, serde_json::to_vec(&local).unwrap()).unwrap();
        });
    }

    fn request(&self, id: &str, scope: Value) {
        self.request_at(id, scope, "2026-11-22T20:03:12Z");
    }

    fn request_at(&self, id: &str, scope: Value, at: &str) {
        let dir = self.records().join("_requests");
        std::fs::create_dir_all(&dir).unwrap();
        let stamp: String = at.chars().filter(|c| *c != '-' && *c != ':').collect();
        let name = format!("{stamp}_{id}.json");
        std::fs::write(
            dir.join(name),
            serde_json::to_vec(&json!({
                "schema": "medrec-request/v1",
                "id": id,
                "requested_at": at,
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
    // And a decision on Robin's events, which waits for the folder too.
    let h = e.json_at("household.json");
    let robin = h["people"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "Robin Testperson")
        .unwrap()
        .clone();
    let idx = e.json_at(&format!("{}/index.json", robin["folder"].as_str().unwrap()));
    let ids: Vec<&str> = idx["records"]
        .as_array()
        .unwrap()
        .iter()
        .take(2)
        .map(|r| r["id"].as_str().unwrap())
        .collect();
    event_request(
        &e,
        "x_eventaaaaaaaaaaaaaaa",
        json!({"person": robin["id"], "apart": ids}),
    );
    let pending = e
        .records()
        .join("_requests/20261124T120000Z_x_eventaaaaaaaaaaaaaaa.json");
    let v = e.ok(&["refresh", "--scheduled"]);
    drop(held);
    assert!(pending.exists(), "the decision stays pending");
    assert!(!e
        .records()
        .join("_requests/done/20261124T120000Z_x_eventaaaaaaaaaaaaaaa.json")
        .exists());
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

    // The next pass, with the folder free, serves both.
    let v = e.ok(&["refresh", "--scheduled"]);
    let outcomes: Vec<&str> = v["requests"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["outcome"].as_str().unwrap())
        .collect();
    assert_eq!(outcomes, ["ok", "ok"], "{v}");
    assert!(!pending.exists());
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
    // The one instruction is medrec's; the provider's own words, with its
    // own sign-in command, are kept apart.
    let error = &s0["last_attempt"]["error"];
    assert_eq!(error["code"], "needs_person", "{error}");
    assert!(
        error["remedy"]
            .as_str()
            .unwrap()
            .contains("medrec refresh sign-in"),
        "{error}"
    );
    let message = error["message"].as_str().unwrap();
    assert!(!message.contains("auth login"), "{message}");
    let provider = error["provider_message"].as_str().unwrap();
    assert!(provider.contains("<provider profile>"), "{provider}");
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

#[test]
fn a_persons_request_waits_its_own_guard_not_the_schedules_floor() {
    let e = Env::new("guard");
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
    let now = medrec::refresh::time::now();
    let at = |secs: i64| medrec::refresh::time::fmt(secs);

    // Loaded 16 minutes ago: the schedule's day has not passed, the
    // request's guard has.
    e.loaded_at(&at(now - 16 * 60));
    e.request_at("x_aaaaaaaaaaaaaaaaaaaa", json!({ "source": src }), &at(now));
    let v = e.ok(&["refresh", "--scheduled"]);
    assert_eq!(v["attempts"][0]["trigger"], "request", "{v}");
    assert_eq!(v["attempts"][0]["outcome"], "ok", "{v}");
    let st = e.json_at("_status/status.json");
    let s0 = &st["sources"][0];
    assert!(
        s0["sign_in"].get("renews").is_none(),
        "fakeprov is in no table: {s0}"
    );

    // Asked again at once: skipped, saying when it may load.
    // A second after the load, so that load does not answer it.
    let asked = medrec::refresh::time::now() + 1;
    e.request_at(
        "x_bbbbbbbbbbbbbbbbbbbb",
        json!({ "source": src }),
        &at(asked),
    );
    let v = e.ok(&["refresh", "--scheduled"]);
    assert_eq!(v["attempts"][0]["outcome"], "skipped_interval", "{v}");
    let mut done = None;
    walk(&e.records().join("_requests/done"), &mut |p| {
        if p.to_string_lossy().contains("x_bbbbbbbbbbbbbbbbbbbb") {
            done = Some(serde_json::from_slice::<Value>(&std::fs::read(p).unwrap()).unwrap());
        }
    });
    let done = done.expect("the second request's outcome");
    let next = done["handled"]["sources"][0]["next_allowed_at"]
        .as_str()
        .unwrap();
    let next = medrec::refresh::time::parse(next).unwrap();
    assert!(
        (asked + 14 * 60..=asked + 15 * 60 + 5).contains(&next),
        "15 minutes from the load, not a day: {done}"
    );

    // A terminal refresh is a person asking too. Its skip leaves the
    // schedule's next run a day from the load, not now.
    let v = e.ok(&["refresh", "--source", &src]);
    assert_eq!(v["attempts"][0]["outcome"], "skipped_interval", "{v}");
    let st = e.json_at("_status/status.json");
    let next = st["sources"][0]["next_run_at"].as_str().unwrap();
    let next = medrec::refresh::time::parse(next).unwrap();
    assert!(next > asked + 23 * 3600, "{st}");
    e.loaded_at(&at(now - 16 * 60));
    let v = e.ok(&["refresh", "--source", &src]);
    assert_eq!(v["attempts"][0]["outcome"], "ok", "{v}");

    // The schedule keeps its floor.
    e.loaded_at(&at(now - 16 * 60));
    let v = e.ok(&["refresh", "--scheduled"]);
    assert!(v["attempts"].as_array().unwrap().is_empty(), "{v}");
}

#[test]
fn a_providers_declared_sign_in_life_wins_over_the_table() {
    let e = Env::new("declared");
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
    let v = e.ok_env(
        &["refresh", "--source", &src],
        &[("FAKEPROV_AUTH", "short")],
    );
    assert_eq!(v["attempts"][0]["outcome"], "ok", "{v}");
    let st = e.json_at("_status/status.json");
    let s0 = &st["sources"][0]["sign_in"];
    assert_eq!(
        (s0["renews"].clone(), s0["lasts_secs"].clone()),
        (json!(false), json!(1800)),
        "{s0}"
    );
    // Kept between reads.
    e.ok(&["refresh", "--scheduled"]);
    let st = e.json_at("_status/status.json");
    assert_eq!(st["sources"][0]["sign_in"]["renews"], false, "{st}");
}

#[test]
fn a_pass_exports_every_source_before_it_files_any() {
    let e = Env::new("spool");
    let first = setup(&e);
    // A second patient on the same site, as a household's two members are.
    let other = medrec::testdata::STREAM.replace("pat-0001", "pat-0002");
    std::fs::write(e.root.join("provider/synthetic-pat-0002.ndjson"), other).unwrap();
    e.ok(&[
        "import",
        "--from",
        "fakeprov",
        "--from-profile",
        "adult2",
        "--patient",
        "pat-0002",
        "--person",
        "Robin",
    ]);
    let h = e.json_at("household.json");
    let second = h["sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap().to_string())
        .find(|id| *id != first)
        .unwrap();
    for (src, login) in [(&first, "adult1"), (&second, "adult2")] {
        e.ok(&[
            "--profile",
            "home",
            "refresh",
            "add",
            "--source",
            src,
            "--provider-profile",
            login,
        ]);
    }
    e.age_imports();
    let records = e.records();
    let v = e.ok_env(
        &["refresh"],
        &[("FAKEPROV_WATCH", records.to_str().unwrap())],
    );
    let outcomes: Vec<_> = v["attempts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["outcome"].as_str().unwrap())
        .collect();
    assert_eq!(outcomes, ["ok", "ok"], "{v}");
    // What each export saw of the people's indexes: the same, so neither
    // was filed before the other was exported.
    let log = std::fs::read_to_string(e.root.join("provider/export-log")).unwrap();
    let seen: Vec<_> = log.lines().collect();
    assert_eq!(seen.len(), 2, "{log}");
    assert_eq!(seen[0], seen[1], "an import was filed between the exports");
    let idx = e.json_at("Robin Testperson/index.json");
    assert_eq!(idx["imports"].as_array().unwrap().len(), 4);
    // The views were brought up to date once, after both filings: they
    // record the files as the pass left them.
    let md5 = |rel: &str| medrec::hash::md5_hex(&std::fs::read(records.join(rel)).unwrap());
    let views = e.json_at("views/views.json");
    assert_eq!(views["household"], md5("household.json"));
    let robin = e.json_at("household.json")["people"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["folder"] == "Robin Testperson")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        views["people"][&robin]["index"],
        md5("Robin Testperson/index.json")
    );
}

#[test]
fn an_import_of_several_members_exports_them_all_before_filing() {
    let e = Env::new("batch");
    setup(&e);
    let provider = e.root.join("provider");
    let stream = medrec::testdata::STREAM;
    std::fs::write(
        provider.join("synthetic-pat-0002.ndjson"),
        stream.replace("pat-0001", "pat-0002"),
    )
    .unwrap();
    // Someone else's records: a birth date that is not Robin's.
    std::fs::write(
        provider.join("synthetic-pat-0003.ndjson"),
        stream
            .replace("pat-0001", "pat-0003")
            .replace("2020-02-02", "1950-05-05"),
    )
    .unwrap();
    let import_env = |patients: &[&str], people: &[&str], env: &[(&str, &str)]| {
        let mut args = vec!["import", "--from", "fakeprov", "--from-profile", "adult1"];
        for p in patients {
            args.extend(["--patient", p]);
        }
        for p in people {
            args.extend(["--person", p]);
        }
        let records = e.records();
        let mut env = env.to_vec();
        env.push(("FAKEPROV_WATCH", records.to_str().unwrap()));
        e.doc(&args, &env)
    };
    let import = |patients: &[&str], people: &[&str]| import_env(patients, people, &[]);

    let (code, v) = import(&["pat-0001", "pat-0002"], &["Robin"]);
    assert_eq!(code, Some(2), "{v}");
    assert!(
        !provider.join("export-log").exists(),
        "nothing was exported"
    );

    let (code, v) = import(&["pat-0001", "pat-0002"], &["Robin", "Robin"]);
    assert_eq!(code, Some(0), "{v}");
    assert_eq!(v["schema"], "import-batch/v1", "{v}");
    assert_eq!(v["ok"], true);
    let items = v["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert!(items
        .iter()
        .all(|i| i["import"]["import"].as_str().unwrap().starts_with("i_")));
    let log = std::fs::read_to_string(provider.join("export-log")).unwrap();
    let seen: Vec<_> = log.lines().collect();
    assert_eq!(seen.len(), 2, "{log}");
    assert_eq!(
        seen[0], seen[1],
        "a member was filed before the next export"
    );

    // One member's records refused: the others are filed, and the exit is
    // that member's.
    let (code, v) = import(&["pat-0003", "pat-0002"], &["Robin", "Robin"]);
    assert_eq!(code, Some(2), "{v}");
    assert_eq!(v["ok"], false);
    assert_eq!(v["items"][0]["error"]["code"], "usage", "{v}");
    assert!(v["items"][1]["import"].is_object(), "{v}");

    // One member's export refused for its sign-in: the next member is
    // still exported and filed, and the exit is the refusal's.
    let before = e.json_at("Robin Testperson/index.json")["imports"]
        .as_array()
        .unwrap()
        .len();
    let (code, v) = import_env(
        &["pat-0001", "pat-0002"],
        &["Robin", "Robin"],
        &[("FAKEPROV_EXIT_FOR", "pat-0001")],
    );
    assert_eq!(code, Some(3), "{v}");
    assert_eq!(v["ok"], false);
    assert_eq!(v["items"][0]["patient"], "pat-0001");
    assert!(v["items"][0]["error"]["message"].is_string(), "{v}");
    assert!(v["items"][1]["import"].is_object(), "{v}");
    let after = e.json_at("Robin Testperson/index.json")["imports"]
        .as_array()
        .unwrap()
        .len();
    assert_eq!(after, before + 1, "the second member was filed");

    // Downloaded files are one member's.
    let files = e.root.join("files");
    std::fs::create_dir_all(&files).unwrap();
    let (code, v) = e.doc(
        &[
            "import",
            "--from",
            "fakeprov",
            "--from-files",
            files.to_str().unwrap(),
            "--patient",
            "pat-0001",
            "--patient",
            "pat-0002",
        ],
        &[("FAKEPROV_INFO", "files")],
    );
    assert_eq!(code, Some(2), "{v}");
    assert!(
        v["error"]["message"]
            .as_str()
            .unwrap()
            .contains("--from-files"),
        "{v}"
    );
    let _ = std::fs::remove_dir_all(&e.root);
}

#[test]
fn a_skipped_request_counts_an_import_run_by_hand_in_the_next_run() {
    let e = Env::new("next-run");
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
    // Imported by hand a moment ago; refresh itself has never run it.
    let v = e.ok(&["refresh", "--source", &src]);
    assert_eq!(v["attempts"][0]["outcome"], "skipped_interval", "{v}");
    let st = e.json_at("_status/status.json");
    let next = st["sources"][0]["next_run_at"].as_str().unwrap();
    let next = medrec::refresh::time::parse(next).unwrap();
    let now = medrec::refresh::time::now();
    assert!(
        next > now + 23 * 3600,
        "a day from the import, not now: {st}"
    );
}

/// A sealed entry in medrec's vault, so a read of the sign-in needs the
/// vault's key: what is in it does not matter, as the test keychain holds
/// no key to open it.
fn vault_entry(e: &Env) {
    let dir = e.root.join(".config/medrec/vault");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("another-entry.pkv"), b"sealed").unwrap();
}

/// The reads medrec made of its Keychain item (`MEDREC_TEST_KEYCHAIN_LOG`):
/// `read` for one that may ask a person, `quiet` for one that may not.
fn keychain_reads(log: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(log)
        .map(|s| s.lines().map(str::to_string).collect())
        .unwrap_or_default()
}

#[test]
fn a_scheduled_pass_raises_no_keychain_dialog() {
    let e = Env::new("drive-gate");
    e.ok(&[
        "config",
        "set",
        "folder",
        "gdrive:home-drive/1Synthetic_folder",
    ]);
    let log = e.root.join("keychain-reads");
    let log_s = log.to_str().unwrap();
    let asks = [
        ("MEDREC_TEST_KEYCHAIN_DELAY_SECS", "30"),
        ("MEDREC_TEST_KEYCHAIN_LOG", log_s),
        ("MEDREC_TEST_AUTH_WAIT_SECS", "1"),
    ];
    let answers = [("MEDREC_TEST_KEYCHAIN_LOG", log_s)];
    let scheduled = ["--json", "refresh", "--scheduled"];
    let error = |out: &std::process::Output| -> Value {
        let v: Value = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|x| panic!("{x}: {}", String::from_utf8_lossy(&out.stderr)));
        v["error"].clone()
    };

    // A key that a person has to allow: every pass stops at once, saying
    // what to run, and asks nobody. Each looks again, so a key allowed
    // since is read.
    for passes in [1, 2] {
        let started = std::time::Instant::now();
        let out = e.run_env(&scheduled, &asks);
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
        let x = error(&out);
        assert_eq!(out.status.code(), Some(3), "{x}");
        assert_eq!(x["reason"], "credential_unreadable", "{x}");
        assert!(x["remedy"].as_str().unwrap().contains("auth key"), "{x}");
        assert_eq!(keychain_reads(&log), vec!["quiet"; passes]);
    }

    // A vault whose key is gone is exit 1, which no one can allow.
    vault_entry(&e);
    let out = e.run_env(&scheduled, &answers);
    assert_eq!(out.status.code(), Some(1), "{}", error(&out));

    // With nothing sealed, the pass goes on to the folder, which here has no
    // sign-in on this machine.
    std::fs::remove_dir_all(e.root.join(".config/medrec/vault")).unwrap();
    let out = e.run_env(&scheduled, &answers);
    let x = error(&out);
    assert_eq!(out.status.code(), Some(2), "{x}");
    assert!(
        x.to_string()
            .contains("medrec drive login --drive-profile home-drive"),
        "{x}"
    );
    assert!(
        keychain_reads(&log).iter().all(|r| r == "quiet"),
        "{:?}",
        keychain_reads(&log)
    );
    let _ = std::fs::remove_dir_all(&e.root);
}

/// `auth key` reads the key and nothing else, and says what the Keychain did.
#[test]
fn auth_key_says_what_the_keychain_did() {
    let e = Env::new("auth-key");
    let log = e.root.join("keychain-reads");
    let log_s = log.to_str().unwrap();
    let asks = [
        ("MEDREC_TEST_KEYCHAIN_DELAY_SECS", "30"),
        ("MEDREC_TEST_KEYCHAIN_LOG", log_s),
    ];
    let key = |args: &[&str], env: &[(&str, &str)]| -> (Option<i32>, Value) {
        let args: Vec<&str> = ["--json", "auth", "key"]
            .iter()
            .chain(args)
            .copied()
            .collect();
        let out = e.run_env(&args, env);
        let v = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|x| panic!("{x}: {}", String::from_utf8_lossy(&out.stderr)));
        (out.status.code(), v)
    };
    // Nothing stored, and nothing that needs a key.
    let (code, v) = key(&[], &asks[1..]);
    assert_eq!(
        (code, &v["schema"], &v["outcome"], &v["read"]),
        (
            Some(0),
            &json!("auth-key/v1"),
            &json!("absent"),
            &json!(false)
        ),
        "{v}"
    );
    // A dialog that would be needed is said, and none is raised.
    let (code, v) = key(&["--no-dialog"], &asks);
    assert_eq!(
        (code, &v["outcome"], &v["dialog_needed"]),
        (Some(3), &json!("needs_dialog"), &json!(true)),
        "{v}"
    );
    assert_eq!(v["error"]["reason"], "credential_unreadable", "{v}");
    assert_eq!(keychain_reads(&log), ["quiet", "quiet"]);
    // One that nobody answers, raised once and waited for as long as said.
    let (code, v) = key(&["--wait", "1"], &asks);
    assert_eq!((code, &v["outcome"]), (Some(3), &json!("no_answer")), "{v}");
    assert_eq!(keychain_reads(&log)[2..], ["quiet", "read"]);
    // A key that sealed files need is gone.
    vault_entry(&e);
    let (code, v) = key(&[], &asks[1..]);
    assert_eq!((code, &v["outcome"]), (Some(1), &json!("absent")), "{v}");
    let _ = std::fs::remove_dir_all(&e.root);
}

/// A pending request file carrying an event decision.
fn event_request(e: &Env, id: &str, event: Value) {
    decision_request(e, id, "event", event)
}

/// A household decision the app writes: `event` or `paid`.
fn decision_request(e: &Env, id: &str, key: &str, body: Value) {
    decision_request_at(e, id, key, body, "2026-11-24T12:00:00Z");
}

fn decision_request_at(e: &Env, id: &str, key: &str, body: Value, at: &str) {
    decision_request_by(e, id, key, body, at, at);
}

/// One whose device's clock says `clock` when the store's says `at`.
fn decision_request_by(e: &Env, id: &str, key: &str, body: Value, at: &str, clock: &str) {
    let dir = e.records().join("_requests");
    std::fs::create_dir_all(&dir).unwrap();
    let stamp: String = clock.chars().filter(|c| *c != '-' && *c != ':').collect();
    let path = dir.join(format!("{stamp}_{id}.json"));
    std::fs::write(
        &path,
        serde_json::to_vec(&json!({
            "schema": "medrec-request/v1",
            "id": id,
            "requested_at": clock,
            "requested_by": {"email": "alex@example.com", "name": "Alex Testperson"},
            key: body,
            "app": {"name": "own-your-health-app", "version": "0.1.0"},
        }))
        .unwrap(),
    )
    .unwrap();
    let secs = medrec::refresh::time::parse(at).unwrap() as u64;
    let modified = std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs);
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(modified)
        .unwrap();
}

/// A person's events view, by the name views.json gives it.
fn events_view(e: &Env, pid: &str) -> Value {
    let views = e.json_at("views/views.json");
    let md5 = views["people"][pid]["files"]["events.json"]
        .as_str()
        .unwrap();
    e.json_at(&format!("views/{pid}/events.{}.json", &md5[..12]))
}

fn bills_view(e: &Env, pid: &str) -> Value {
    let views = e.json_at("views/views.json");
    let md5 = views["people"][pid]["files"]["bills.json"]
        .as_str()
        .unwrap();
    e.json_at(&format!("views/{pid}/bills.{}.json", &md5[..12]))
}

fn private_view(e: &Env, pid: &str) -> Value {
    let views = e.json_at("views/views.json");
    let md5 = views["people"][pid]["files"]["private.json"]
        .as_str()
        .unwrap();
    e.json_at(&format!("views/{pid}/private.{}.json", &md5[..12]))
}

/// The ids of a `private.json` kind, each with its area and where it came
/// from.
fn private_rows(v: &Value, kind: &str) -> Vec<(String, String, String)> {
    v[kind]
        .as_object()
        .unwrap_or_else(|| panic!("no {kind}: {v}"))
        .iter()
        .map(|(id, w)| {
            (
                id.clone(),
                w["area"].as_str().unwrap().to_string(),
                w["from"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

fn bill_row(v: &Value, id: &str) -> Value {
    v["bills"]
        .get(id)
        .unwrap_or_else(|| panic!("no bill {id}: {v}"))
        .clone()
}

/// The folder's write lock, held as another medrec writing it would.
fn hold_folder_lock(e: &Env) -> std::fs::File {
    let mut lock = None;
    walk(&e.root.join("cache/medrec/locks"), &mut |p| {
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        if name.ends_with(".lock") && !name.starts_with("refresh-") {
            lock = Some(p.to_path_buf());
        }
    });
    let held = std::fs::OpenOptions::new()
        .write(true)
        .open(lock.expect("a write left the folder's lock file"))
        .unwrap();
    held.lock().unwrap();
    held
}

#[test]
fn a_bill_marked_paid_from_the_app_or_a_terminal_is_settled_in_the_bills_view() {
    let e = Env::new("paid");
    setup(&e);
    let (alex, _) = with_event_fixtures(&e);
    let aid = alex["id"].as_str().unwrap();
    e.ok(&["views", "write"]);
    let bill = "d_hearingbillaaaaaaaaa";
    let b = bill_row(&bills_view(&e, aid), bill);
    assert_eq!(
        (&b["amount_due"], &b["settled"]),
        (&json!("50.00"), &json!(false)),
        "{b}"
    );

    // Mistakes change nothing.
    let dir = e.records().join(alex["folder"].as_str().unwrap());
    let file = dir.join("marks.json");
    for (args, code) in [
        (
            vec!["bills", "paid", bill, "--on", "2026-03-20", "--amount", "0"],
            2,
        ),
        (
            vec![
                "bills", "paid", bill, "--on", "March 20", "--amount", "40.00",
            ],
            2,
        ),
        (
            vec![
                "bills",
                "paid",
                "d_hipnoteaaaaaaaaaaaaa",
                "--on",
                "2026-03-20",
                "--amount",
                "40.00",
            ],
            2,
        ),
        (
            vec![
                "bills",
                "paid",
                "d_nosuchdocumentaaaaaa",
                "--on",
                "2026-03-20",
                "--amount",
                "40.00",
            ],
            4,
        ),
        (
            vec![
                "bills",
                "paid",
                "r_hipdoctoraaaaaaaaaaa",
                "--on",
                "2026-03-20",
                "--amount",
                "40.00",
            ],
            2,
        ),
        (vec!["bills", "undo", "o_aaaaaaaaaaaaaaaaaaaa"], 4),
        (vec!["bills", "undo", "not-a-payment"], 2),
    ] {
        let (got, text) = e.code(&args);
        assert_eq!(got, Some(code), "{args:?}: {text}");
    }
    assert!(!file.exists(), "no payment was written");

    // Half from a terminal; the same again is unchanged.
    let first = e.ok(&[
        "bills",
        "paid",
        bill,
        "--on",
        "2026-03-20",
        "--amount",
        "$20.00",
    ]);
    assert_eq!(first["status"], "recorded", "{first}");
    assert_eq!(
        e.ok(&[
            "bills",
            "paid",
            bill,
            "--on",
            "2026-03-20",
            "--amount",
            "20.00"
        ])["status"],
        "unchanged"
    );

    // The rest from the app, and a request naming a paper that is no bill.
    // While another writer holds the folder, the request waits.
    let held = hold_folder_lock(&e);
    decision_request(
        &e,
        "x_paidaaaaaaaaaaaaaaaa",
        "paid",
        json!({"person": aid, "document": bill, "on": "2026-04-01", "amount": "30.00"}),
    );
    let v = e.ok(&["refresh", "--scheduled"]);
    drop(held);
    assert!(v["requests"].as_array().unwrap().is_empty(), "{v}");
    assert!(e
        .records()
        .join("_requests/20261124T120000Z_x_paidaaaaaaaaaaaaaaaa.json")
        .exists());
    let m: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert_eq!(m["payments"].as_array().unwrap().len(), 1, "{m}");
    decision_request(
        &e,
        "x_paidbbbbbbbbbbbbbbbb",
        "paid",
        json!({"person": aid, "document": "d_hipnoteaaaaaaaaaaaaa", "on": "2026-04-01", "amount": "20.00"}),
    );
    e.ok(&["refresh", "--scheduled"]);
    let done = e.json_at("_requests/done/20261124T120000Z_x_paidaaaaaaaaaaaaaaaa.json");
    assert_eq!(done["handled"]["outcome"], "ok", "{done}");
    assert_eq!(
        done["paid"],
        json!({"person": aid, "document": bill, "on": "2026-04-01", "amount": "30.00"})
    );
    let refused = e.json_at("_requests/done/20261124T120000Z_x_paidbbbbbbbbbbbbbbbb.json");
    assert_eq!(refused["handled"]["outcome"], "invalid", "{refused}");
    assert!(refused["handled"]["message"]
        .as_str()
        .unwrap()
        .contains("not a bill"));

    let b = bill_row(&bills_view(&e, aid), bill);
    assert_eq!(
        (&b["paid_total"], &b["left"], &b["settled"]),
        (&json!("50.00"), &json!("0.00"), &json!(true)),
        "{b}"
    );
    let by: Vec<&str> = b["paid"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["on"].as_str().unwrap())
        .collect();
    assert_eq!(by, ["2026-03-20", "2026-04-01"]);
    let listed = e.ok(&["bills", "list", "--person", "Alex"]);
    let item = listed["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["document"] == bill)
        .unwrap();
    assert_eq!(item["settled"], true, "{listed}");

    // Both payments are listed; one undone leaves the bill half paid. The
    // payments are in marks.json, the event decisions in overrides.json, so
    // a medrec before 0.9.1 rewriting overrides.json never drops them.
    e.ok(&[
        "events",
        "apart",
        "r_hipdoctoraaaaaaaaaaa",
        "r_hipimageaaaaaaaaaaaa",
    ]);
    let p = e.ok(&["bills", "payments"]);
    assert_eq!(p["items"].as_array().unwrap().len(), 2, "{p}");
    e.ok(&["bills", "undo", first["id"].as_str().unwrap()]);
    let b = bill_row(&bills_view(&e, aid), bill);
    assert_eq!(
        (&b["left"], &b["settled"]),
        (&json!("20.00"), &json!(false)),
        "{b}"
    );
    let o: Value =
        serde_json::from_slice(&std::fs::read(dir.join("overrides.json")).unwrap()).unwrap();
    let m: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert_eq!(o["events"].as_array().unwrap().len(), 1, "{o}");
    assert!(o.get("payments").is_none(), "{o}");
    assert_eq!(
        (&m["schema"], m["payments"].as_array().unwrap().len()),
        (&json!("medrec-marks/v1"), 1),
        "{m}"
    );

    // A marks file that does not read: the views say so and show the bill
    // unpaid, the list says so, and nothing writes over it.
    std::fs::write(&file, b"{not json").unwrap();
    e.ok(&["views", "write"]);
    let v = bills_view(&e, aid);
    assert_eq!(v["marks_unreadable"], true, "{v}");
    assert_eq!(bill_row(&v, bill)["left"], "50.00");
    let listed = e.ok(&["bills", "list", "--person", "Alex"]);
    assert_eq!(listed["marks_unreadable"], json!([aid]), "{listed}");
    let out = e.run(&["bills", "list", "--person", "Alex"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{err}");
    assert!(err.contains("marks.json does not read"), "{err}");
    let (got, text) = e.code(&["bills", "paid", bill, "--on", "2026-04-02", "--amount", "1"]);
    assert_ne!(got, Some(0), "{text}");
    assert_eq!(std::fs::read(&file).unwrap(), b"{not json");
    let _ = std::fs::remove_dir_all(&e.root);
}

fn holding<'a>(v: &'a Value, id: &str) -> &'a Value {
    v["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|ev| {
            ev["members"]
                .as_array()
                .unwrap()
                .iter()
                .any(|m| m["record"] == id || m["document"] == id)
        })
        .unwrap()
}

/// Alex's records the invented parent's (tests/fixtures/events), Robin a
/// child of Alex's with none: the people, as household.json has them.
fn with_event_fixtures(e: &Env) -> (Value, Value) {
    let h = e.json_at("household.json");
    let person = |name: &str| {
        h["people"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["name"] == name)
            .unwrap()
            .clone()
    };
    let (alex, robin) = (person("Alex Testperson"), person("Robin Testperson"));
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/events");
    for (from, to) in [
        ("parent.index.json", "index.json"),
        ("parent.text.json", "text.json"),
    ] {
        let mut v: Value =
            serde_json::from_str(&std::fs::read_to_string(fixtures.join(from)).unwrap()).unwrap();
        v["person"] = alex["id"].clone();
        std::fs::write(
            e.records().join(alex["folder"].as_str().unwrap()).join(to),
            serde_json::to_vec(&v).unwrap(),
        )
        .unwrap();
    }
    (alex, robin)
}

/// Alex's records are the invented adult's of `tests/fixtures/private`.
fn with_private_fixtures(e: &Env) -> Value {
    let h = e.json_at("household.json");
    let alex = h["people"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "Alex Testperson")
        .unwrap()
        .clone();
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/private");
    for (from, to) in [
        ("adult.index.json", "index.json"),
        ("adult.text.json", "text.json"),
    ] {
        let mut v: Value =
            serde_json::from_str(&std::fs::read_to_string(fixtures.join(from)).unwrap()).unwrap();
        v["person"] = alex["id"].clone();
        std::fs::write(
            e.records().join(alex["folder"].as_str().unwrap()).join(to),
            serde_json::to_vec(&v).unwrap(),
        )
        .unwrap();
    }
    alex
}

#[test]
fn private_items_follow_the_rules_until_the_household_marks_them() {
    let e = Env::new("private");
    setup(&e);
    let alex = with_private_fixtures(&e);
    let aid = alex["id"].as_str().unwrap();
    let robin = e.json_at("household.json")["people"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "Robin Testperson")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    e.ok(&["views", "write"]);
    let (smoking, varenicline, visit, screen, letter) = (
        "r_smokingaaaaaaaaaaaaa",
        "r_vareniclineaaaaaaaaa",
        "r_quitvisitaaaaaaaaaaa",
        "r_screenaaaaaaaaaaaaaa",
        "d_quitnoticeaaaaaaaaaa",
    );
    let (migraine, sumatriptan) = ("r_migraineaaaaaaaaaaaa", "r_sumatriptanaaaaaaaaa");
    let area = || "substance_use".to_string();
    let v = private_view(&e, aid);
    assert_eq!(v["schema"], "medrec-private/v1");
    assert_eq!(v["person"], aid);
    assert_eq!(
        private_rows(&v, "records"),
        vec![
            (visit.into(), area(), "provider".into()),
            (screen.into(), area(), "test".into()),
            (smoking.into(), area(), "code".into()),
            (varenicline.into(), area(), "medicine".into()),
        ]
    );
    assert_eq!(
        private_rows(&v, "documents"),
        vec![(letter.into(), area(), "provider".into())]
    );
    let events = private_rows(&v, "events");
    assert_eq!(events.len(), 1, "the group visit's event: {v}");
    assert_eq!(
        (&events[0].1, &events[0].2),
        (&area(), &"members".to_string())
    );
    let visit_event = events[0].0.clone();
    assert!(events_view(&e, aid).get("private").is_none());
    // Views a build of the same version wrote before private.json existed
    // are made again.
    let mut index = e.json_at("views/views.json");
    index["people"][aid]["files"]
        .as_object_mut()
        .unwrap()
        .remove("private.json");
    std::fs::write(
        e.records().join("views/views.json"),
        serde_json::to_vec(&index).unwrap(),
    )
    .unwrap();
    let w = e.ok(&["views", "write"]);
    assert_eq!(w["people_rebuilt"], 1, "{w}");
    assert_eq!(private_view(&e, aid)["person"], aid);
    // Robin has nothing private, and a view all the same.
    let r = private_view(&e, &robin);
    assert!(r["records"].as_object().unwrap().is_empty(), "{r}");

    let listed = e.ok(&["private", "list"]);
    let items = listed["items"].as_array().unwrap();
    assert_eq!(items.len(), 6, "{listed}");
    assert_eq!(items[0]["id"], letter.to_string(), "newest first: {listed}");
    assert!(items
        .iter()
        .all(|i| i["person"] == aid && i["area"] == "substance_use"));
    assert_eq!(
        items
            .iter()
            .filter(|i| i["kind"] == "event")
            .map(|i| i["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec![visit_event.as_str()]
    );

    // Mistakes change nothing.
    let file = e
        .records()
        .join(alex["folder"].as_str().unwrap())
        .join("marks.json");
    for (args, code) in [
        (vec!["private", "mark", "not-an-id"], 2),
        (vec!["private", "mark", "o_aaaaaaaaaaaaaaaaaaaa"], 2),
        (vec!["private", "mark", "r_nosuchrecordaaaaaaaa"], 4),
        (vec!["private", "reset", "e_nosucheventaaaaaaaaa"], 4),
    ] {
        let (got, text) = e.code(&args);
        assert_eq!(got, Some(code), "{args:?}: {text}");
    }
    assert!(!file.exists(), "no mark was written");

    // A terminal marks one medicine private and another not; the same again
    // is unchanged.
    let m = e.ok(&["private", "mark", sumatriptan]);
    assert_eq!(
        (&m["status"], &m["person"]),
        (&json!("recorded"), &json!(aid)),
        "{m}"
    );
    assert_eq!(
        e.ok(&["private", "mark", sumatriptan])["status"],
        "unchanged"
    );
    assert_eq!(
        e.ok(&["private", "unmark", varenicline])["status"],
        "recorded"
    );
    e.ok(&["views", "write"]);
    let v = private_view(&e, aid);
    let records = private_rows(&v, "records");
    assert!(
        records.contains(&(sumatriptan.into(), "household".into(), "household".into())),
        "{v}"
    );
    assert!(!records.iter().any(|r| r.0 == varenicline), "{v}");

    // The app marks a condition private and names an item that is not the
    // person's; an unmarked event's members stay private by the event.
    decision_request(
        &e,
        "x_privateaaaaaaaaaaaaa",
        "private",
        json!({"person": aid, "item": migraine, "private": true}),
    );
    decision_request(
        &e,
        "x_privatebbbbbbbbbbbbb",
        "private",
        json!({"person": aid, "item": "r_nosuchrecordaaaaaaaa", "private": true}),
    );
    decision_request(
        &e,
        "x_privateccccccccccccc",
        "private",
        json!({"person": aid, "item": visit_event, "private": false}),
    );
    e.ok(&["refresh", "--scheduled"]);
    let done = e.json_at("_requests/done/20261124T120000Z_x_privateaaaaaaaaaaaaa.json");
    assert_eq!(done["handled"]["outcome"], "ok", "{done}");
    assert_eq!(
        done["private"],
        json!({"person": aid, "item": migraine, "private": true})
    );
    let refused = e.json_at("_requests/done/20261124T120000Z_x_privatebbbbbbbbbbbbb.json");
    assert_eq!(refused["handled"]["outcome"], "invalid", "{refused}");
    assert!(refused["handled"]["message"]
        .as_str()
        .unwrap()
        .contains("not one of the person's"));
    let marks: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert_eq!(marks["schema"], "medrec-marks/v1");
    let by_app = marks["private"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["item"] == migraine)
        .unwrap();
    assert_eq!(
        (&by_app["private"], &by_app["request"], &by_app["by"]),
        (
            &json!(true),
            &json!("x_privateaaaaaaaaaaaaa"),
            &json!("Alex Testperson")
        ),
        "{by_app}"
    );
    let v = private_view(&e, aid);
    let records = private_rows(&v, "records");
    assert!(
        records.contains(&(migraine.into(), "household".into(), "household".into())),
        "{v}"
    );
    assert!(
        v["events"].get(&visit_event).is_none(),
        "the event is unmarked: {v}"
    );
    // Its visit is private by its place still.
    assert!(
        records.contains(&(visit.into(), area(), "provider".into())),
        "{v}"
    );

    // Reset gives the rules back.
    assert_eq!(
        e.ok(&["private", "reset", varenicline])["status"],
        "recorded"
    );
    assert_eq!(
        e.ok(&["private", "reset", varenicline])["status"],
        "unchanged"
    );
    e.ok(&["views", "write"]);
    let records = private_rows(&private_view(&e, aid), "records");
    assert!(records.contains(&(varenicline.into(), area(), "medicine".into())));

    // A marks.json that does not read is left out, and said so; no mark is
    // written over it.
    std::fs::write(&file, b"{not json").unwrap();
    e.ok(&["views", "write"]);
    let v = private_view(&e, aid);
    assert_eq!(v["marks_unreadable"], true, "{v}");
    assert!(!private_rows(&v, "records").iter().any(|r| r.0 == migraine));
    let listed = e.ok(&["private", "list", "--person", "Alex"]);
    assert_eq!(listed["marks_unreadable"], json!([aid]), "{listed}");
    let (code, _) = e.code(&["private", "mark", sumatriptan]);
    assert_ne!(code, Some(0));
    assert_eq!(std::fs::read(&file).unwrap(), b"{not json");
}

/// A person's `providers.json`.
fn provider_view(e: &Env, pid: &str) -> Value {
    let views = e.json_at("views/views.json");
    let md5 = views["people"][pid]["files"]["providers.json"]
        .as_str()
        .unwrap();
    e.json_at(&format!("views/{pid}/providers.{}.json", &md5[..12]))
}

/// What the household says of providers, at a terminal and from the app.
#[test]
fn the_household_says_who_a_printed_name_is() {
    let e = Env::new("labels");
    setup(&e);
    let alex = with_private_fixtures(&e);
    let aid = alex["id"].as_str().unwrap();
    e.ok(&["views", "write"]);
    let (visit, summary) = ("r_headvisitaaaaaaaaaaa", "d_headsummaryaaaaaaaaa");
    let practice = "Sample Headache Clinic";
    let file = e
        .records()
        .join(alex["folder"].as_str().unwrap())
        .join("marks.json");
    let index = std::fs::read(file.with_file_name("index.json")).unwrap();

    // Mistakes change nothing, and repeat no name.
    let set = ["labels", "set", "--person", "Alex", "--printed"];
    let of =
        |rest: &[&'static str]| -> Vec<&'static str> { set.iter().chain(rest).copied().collect() };
    for (args, code) in [
        (of(&[practice]), 2),
        (of(&[practice, "--role", " "]), 2),
        (of(&[practice, "--phone", "none"]), 2),
        (of(&[practice, "--role", "clinic", "--item", "x"]), 2),
        (of(&["Invented Name", "--role", "clinic"]), 4),
        (
            of(&[
                practice,
                "--role",
                "clinic",
                "--item",
                "r_magnesiumaaaaaaaaaaa",
            ]),
            4,
        ),
        (
            vec!["labels", "same", "--person", "Alex", "--printed", practice],
            2,
        ),
        (
            vec![
                "labels",
                "wrong",
                "--person",
                "No One",
                "--printed",
                practice,
            ],
            4,
        ),
        (vec!["labels", "undo", "r_headvisitaaaaaaaaaaa"], 2),
        (vec!["labels", "undo", "o_nosuchidaaaaaaaaaaaa"], 4),
    ] {
        let (got, text) = e.code(&args);
        assert_eq!(got, Some(code), "{args:?}: {text}");
        assert!(!text.contains("Invented Name"), "{text}");
    }
    assert!(!file.exists(), "nothing was written");

    let said = e.ok(&of(&[
        practice,
        "--name",
        "Headache doctors",
        "--phone",
        "555-010-0142",
    ]));
    assert_eq!(
        (&said["status"], &said["kind"], &said["person"]),
        (&json!("recorded"), &json!("set"), &json!(aid)),
        "{said}"
    );
    let first = said["said"]["id"].as_str().unwrap().to_string();
    let again = e.ok(&of(&[
        practice,
        "--name",
        "Headache doctors",
        "--phone",
        "555-010-0142",
    ]));
    assert_eq!(
        (&again["status"], &again["said"]["id"]),
        (&json!("unchanged"), &json!(first))
    );
    e.ok(&["views", "write"]);
    let v = provider_view(&e, aid);
    let id = v["by_name"][practice].as_str().unwrap().to_string();
    let label = &v["labels"][&id];
    assert_eq!(
        (&label["name"], &label["phone"], &label["confidence"]),
        (
            &json!("Headache doctors"),
            &json!("555-010-0142"),
            &json!("household")
        ),
        "{v}"
    );
    assert_eq!(v["overrides"][0]["id"], json!(first), "{v}");

    // The app says the visit's is something else, and a name nothing prints.
    decision_request(
        &e,
        "x_providersaaaaaaaaaaa",
        "provider",
        json!({"person": aid, "printed": [practice], "set": {"role": "Substance use counselor"}, "item": visit}),
    );
    decision_request(
        &e,
        "x_providersbbbbbbbbbbb",
        "provider",
        json!({"person": aid, "printed": ["Invented Name"], "wrong": true}),
    );
    decision_request(
        &e,
        "x_providersccccccccccc",
        "provider",
        json!({"person": aid, "printed": ["Invented Name"], "maybe": true}),
    );
    e.ok(&["refresh", "--scheduled"]);
    let at = |id: &str| format!("_requests/done/20261124T120000Z_{id}.json");
    let done = e.json_at(&at("x_providersaaaaaaaaaaa"));
    assert_eq!(done["handled"]["outcome"], "ok", "{done}");
    assert_eq!(done["provider"], json!({"person": aid, "kind": "set"}));
    for id in ["x_providersbbbbbbbbbbb", "x_providersccccccccccc"] {
        let refused = e.json_at(&at(id));
        assert_eq!(refused["handled"]["outcome"], "invalid", "{refused}");
    }
    for id in [
        "x_providersaaaaaaaaaaa",
        "x_providersbbbbbbbbbbb",
        "x_providersccccccccccc",
    ] {
        let text = e.json_at(&at(id)).to_string();
        for s in ["Invented Name", practice, "Substance use counselor"] {
            assert!(!text.contains(s), "{s}: {text}");
        }
    }
    let marks: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    // Asked before the first was said at the terminal, it is listed before it.
    let by_app = &marks["providers"][0];
    assert_eq!(
        (&by_app["at"], &marks["providers"][1]["id"]),
        (&json!("2026-11-24T12:00:00Z"), &json!(first))
    );
    assert_eq!(
        (
            &by_app["set"],
            &by_app["item"],
            &by_app["by"],
            &by_app["request"]
        ),
        (
            &json!({"role": "Substance use counselor"}),
            &json!(visit),
            &json!("Alex Testperson"),
            &json!("x_providersaaaaaaaaaaa")
        ),
        "{marks}"
    );
    // A household label in a private area makes its item private, and the
    // paper the visit links with it; the private part of the file has it.
    let private = private_view(&e, aid);
    assert!(private_rows(&private, "records").contains(&(
        visit.into(),
        "substance_use".into(),
        "provider".into()
    )));
    assert!(private["documents"].get(summary).is_some(), "{private}");
    let v = provider_view(&e, aid);
    assert!(!json!([v["labels"], v["overrides"]])
        .to_string()
        .contains("Substance use counselor"));
    assert_eq!(v["private"]["overrides"][0]["item"], visit, "{v}");

    // Listed newest first, with the person's own.
    let listed = e.ok(&["labels", "list", "--person", "Alex"]);
    let items = listed["items"].as_array().unwrap();
    assert_eq!(items.len(), 2, "{listed}");
    assert!(items
        .iter()
        .all(|i| i["kind"] == "set" && i["person"] == aid));
    let (_, table) = e.code(&["labels", "list"]);
    assert!(
        table.contains("Headache doctors") && table.contains(practice),
        "{table}"
    );

    // Undone, the rules are back; and no record was edited.
    let undone = by_app["id"].as_str().unwrap();
    assert_eq!(e.ok(&["labels", "undo", undone])["kind"], "undo");
    assert_eq!(e.code(&["labels", "undo", undone]).0, Some(4));
    e.ok(&["views", "write"]);
    let private = private_view(&e, aid);
    assert!(private["records"].get(visit).is_none());
    assert!(private["documents"].get(summary).is_none());
    assert_eq!(
        std::fs::read(file.with_file_name("index.json")).unwrap(),
        index
    );

    // Served again, or for both people who asked, an undo is done.
    decision_request(
        &e,
        "x_providersddddddddddd",
        "provider",
        json!({"person": aid, "undo": undone}),
    );
    e.ok(&["refresh", "--scheduled"]);
    let done = e.json_at(&at("x_providersddddddddddd"));
    assert_eq!(done["handled"]["outcome"], "ok", "{done}");
    assert_eq!(done["handled"]["message"], "there was nothing to undo");

    // A marks.json that does not read is not written over, and a request
    // that says something stays pending: its done/ file would not keep it.
    let held = std::fs::read(&file).unwrap();
    let mut odd: Value = serde_json::from_slice(&held).unwrap();
    odd["providers"][0]["printed"] = json!(practice);
    let odd = serde_json::to_vec(&odd).unwrap();
    std::fs::write(&file, &odd).unwrap();
    assert_ne!(e.code(&of(&[practice, "--role", "clinic"])).0, Some(0));
    let now = medrec::model::now_rfc3339();
    let stamp: String = now.chars().filter(|c| *c != '-' && *c != ':').collect();
    let at = |id: &str| format!("_requests/done/{stamp}_{id}.json");
    let wrong = json!({"person": aid, "printed": [practice], "wrong": true});
    let id = "x_providerseeeeeeeeeee";
    decision_request_at(&e, id, "provider", wrong.clone(), &now);
    let pending = e.records().join(format!("_requests/{stamp}_{id}.json"));
    for _ in 0..2 {
        let pass = e.ok(&["refresh", "--scheduled"]);
        assert_eq!(
            pass["requests"],
            json!([{"request": id, "outcome": "retrying",
                "cause": "reading the person's marks.json: other"}])
        );
        assert!(pending.exists());
        assert!(!e.records().join(at(id)).exists());
    }
    assert_eq!(std::fs::read(&file).unwrap(), odd);
    // For a day, and then what it answers repeats nothing of the marks.
    let mut first = 0;
    e.local(&mut |l| {
        let t = &mut l["failing"][id];
        first = t.as_i64().unwrap();
        *t = json!(first - 24 * 3600);
    });
    assert!(first > 0);
    let pass = e.ok(&["refresh", "--scheduled"]);
    assert_eq!(pass["requests"][0]["outcome"], "failed", "{pass}");
    assert!(!pass.to_string().contains(practice), "{pass}");
    assert!(!pending.exists());
    let failed = e.json_at(&at(id));
    assert_eq!(failed["handled"]["outcome"], "failed", "{failed}");
    assert!(!failed.to_string().contains(practice), "{failed}");
    e.local(&mut |l| assert!(l.get("failing").is_none(), "{l}"));

    // Written once the marks read again.
    let id = "x_providersfffffffffff";
    decision_request_at(&e, id, "provider", wrong, &now);
    e.ok(&["refresh", "--scheduled"]);
    std::fs::write(&file, &held).unwrap();
    e.ok(&["refresh", "--scheduled"]);
    assert_eq!(e.json_at(&at(id))["handled"]["outcome"], "ok");
    e.local(&mut |l| assert!(l.get("failing").is_none(), "{l}"));
}

/// A request can be served late, or again: it is what was said when it was
/// asked, and undoes nothing said or taken back since.
#[test]
fn what_a_provider_request_says_is_as_of_when_it_was_asked() {
    let e = Env::new("labels-late");
    setup(&e);
    let alex = with_private_fixtures(&e);
    let aid = alex["id"].as_str().unwrap();
    let robin = e.json_at("household.json")["people"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "Robin Testperson")
        .unwrap()["id"]
        .clone();
    let (practice, center) = ("Sample Headache Clinic", "Sample Addiction Services");
    let file = e
        .records()
        .join(alex["folder"].as_str().unwrap())
        .join("marks.json");
    let hour = |h: u32| format!("2026-11-24T{h:02}:00:00Z");
    let ask = |id: &str, h: u32, body: Value| {
        decision_request_at(&e, id, "provider", body, &hour(h));
    };
    let outcome = |id: &str, h: u32| -> Value {
        e.json_at(&format!("_requests/done/20261124T{h:02}0000Z_{id}.json"))["handled"]["outcome"]
            .clone()
    };
    let rows = |pass: &Value| -> Vec<(String, String)> {
        let text = |v: &Value| v.as_str().unwrap().to_string();
        pass["requests"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| (text(&r["request"]), text(&r["outcome"])))
            .collect()
    };
    let say = |name: &str, printed: &str| {
        e.ok(&[
            "labels",
            "set",
            "--person",
            "Alex",
            "--printed",
            printed,
            "--name",
            name,
        ])["said"]["id"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let names = || -> Vec<Value> {
        let marks: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        marks["providers"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|c| c["set"]["name"].clone())
            .collect()
    };
    let row = |id: &str, o: &str| (id.to_string(), o.to_string());
    // To the next second, the least two times are told apart by.
    let wait = || {
        let now = medrec::refresh::time::now();
        while medrec::refresh::time::now() <= now {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    };

    // One that cannot be written holds back the person's later ones, and
    // nobody else's.
    let (first, second, other) = (
        "x_lateaaaaaaaaaaaaaaaa",
        "x_latebbbbbbbbbbbbbbbb",
        "x_latecccccccccccccccc",
    );
    ask(
        first,
        10,
        json!({"person": aid, "printed": [practice], "set": {"name": "Asked first"}}),
    );
    ask(
        second,
        11,
        json!({"person": aid, "printed": [center], "set": {"name": "Asked second"}}),
    );
    ask(
        other,
        12,
        json!({"person": robin, "undo": "o_nothingaaaaaaaaaaaaa"}),
    );
    std::fs::write(&file, b"{not json").unwrap();
    let pass = e.ok(&["refresh", "--scheduled"]);
    assert_eq!(
        rows(&pass),
        [
            row(first, "retrying"),
            row(second, "waiting"),
            row(other, "ok")
        ]
    );

    // Said since at a terminal, that stands; the other is recorded.
    std::fs::remove_file(&file).unwrap();
    say("Said after", practice);
    let pass = e.ok(&["refresh", "--scheduled"]);
    assert_eq!(rows(&pass), [row(first, "skipped"), row(second, "ok")]);
    assert_eq!(outcome(first, 10), "skipped");
    assert_eq!(names(), [json!("Asked second"), json!("Said after")]);

    // Of some of the same names, it is said of the others alone.
    let both = "x_latedddddddddddddddd";
    ask(
        both,
        13,
        json!({"person": aid, "printed": [practice, center], "set": {"name": "Of both"}}),
    );
    e.ok(&["refresh", "--scheduled"]);
    assert_eq!(outcome(both, 13), "ok");
    assert_eq!(
        names(),
        [json!("Asked second"), json!("Of both"), json!("Said after")]
    );
    let v = provider_view(&e, aid);
    let shown = |printed: &str| {
        let part = if v["by_name"].get(printed).is_some() {
            &v
        } else {
            &v["private"]
        };
        let id = part["by_name"][printed].as_str().unwrap().to_string();
        part["labels"][&id]["name"].clone()
    };
    assert_eq!(shown(practice), "Said after", "{v}");
    assert_eq!(shown(center), "Of both", "{v}");

    // Served again after it was taken back (its answer was never written), it
    // is not said again.
    let last = "x_lateeeeeeeeeeeeeeeee";
    let body = json!({"person": aid, "printed": [center], "set": {"name": "Asked last"}});
    ask(last, 14, body.clone());
    e.ok(&["refresh", "--scheduled"]);
    assert_eq!(outcome(last, 14), "ok");
    let marks: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    let id = marks["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["request"] == last)
        .unwrap()["id"]
        .as_str()
        .unwrap();
    e.ok(&["labels", "undo", id]);
    std::fs::remove_file(
        e.records()
            .join(format!("_requests/done/20261124T140000Z_{last}.json")),
    )
    .unwrap();
    ask(last, 14, body.clone());
    let held = std::fs::read(&file).unwrap();
    e.ok(&["refresh", "--scheduled"]);
    assert_eq!(outcome(last, 14), "skipped");
    assert_eq!(std::fs::read(&file).unwrap(), held);
    // Asked again after that, it is.
    wait();
    let again = "x_lateffffffffffffffff";
    let now = medrec::model::now_rfc3339();
    decision_request_at(&e, again, "provider", body, &now);
    let pass = e.ok(&["refresh", "--scheduled"]);
    assert_eq!(rows(&pass), [row(again, "ok")]);
    let marks: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert!(marks.get("providers_undone").is_none(), "{marks}");
    assert_eq!(names().last().unwrap(), "Asked last");

    // The order is the store's. A device whose clock is ahead has no last word
    // when it is tried again, and one whose clock is behind is heard.
    let (ahead, behind) = ("x_lategggggggggggggggg", "x_latehhhhhhhhhhhhhhhh");
    let of = |name: &str| json!({"person": aid, "printed": [practice], "set": {"name": name}});
    let held = std::fs::read(&file).unwrap();
    let soon = "2099-01-01T00:00:00Z";
    let now = medrec::model::now_rfc3339();
    decision_request_by(&e, ahead, "provider", of("Clock ahead"), &now, soon);
    std::fs::write(&file, b"{not json").unwrap();
    let pass = e.ok(&["refresh", "--scheduled"]);
    assert_eq!(rows(&pass), [row(ahead, "retrying")]);
    std::fs::write(&file, &held).unwrap();
    wait();
    say("Said since", practice);
    let pass = e.ok(&["refresh", "--scheduled"]);
    assert_eq!(rows(&pass), [row(ahead, "skipped")]);
    assert_eq!(names().last().unwrap(), "Said since");
    let now = medrec::model::now_rfc3339();
    decision_request_by(
        &e,
        behind,
        "provider",
        of("Clock behind"),
        &now,
        "2026-11-24T09:00:00Z",
    );
    let pass = e.ok(&["refresh", "--scheduled"]);
    assert_eq!(rows(&pass), [row(behind, "ok")]);
    assert_eq!(names().last().unwrap(), "Clock behind");

    // Said again is said later: what was asked before that does not count.
    let between = "x_lateiiiiiiiiiiiiiiii";
    let first = say("Said twice", practice);
    wait();
    let now = medrec::model::now_rfc3339();
    decision_request_at(&e, between, "provider", of("Asked between"), &now);
    wait();
    assert_eq!(say("Said twice", practice), first);
    let pass = e.ok(&["refresh", "--scheduled"]);
    assert_eq!(rows(&pass), [row(between, "skipped")]);
    assert_eq!(names().last().unwrap(), "Said twice");

    // What a later medrec took back is kept, and reads.
    let mut marks: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    let later = json!([{"printed": [practice], "kind": "moved", "at": "2026-01-01T00:00:00Z",
        "to": "Example"}]);
    marks["providers_undone"] = later.clone();
    std::fs::write(&file, serde_json::to_vec(&marks).unwrap()).unwrap();
    say("Said last", practice);
    let marks: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert_eq!(marks["providers_undone"], later);
    assert_eq!(names().last().unwrap(), "Said last");
}

#[test]
fn an_event_mark_follows_its_members_when_the_event_takes_a_new_id() {
    let e = Env::new("private-follow");
    setup(&e);
    let alex = with_private_fixtures(&e);
    let aid = alex["id"].as_str().unwrap();
    e.ok(&["views", "write"]);
    let visit = "r_quitvisitaaaaaaaaaaa";
    let event_of = |e: &Env| -> String {
        events_view(e, aid)["events"]
            .as_array()
            .unwrap()
            .iter()
            .find(|ev| {
                ev["members"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|m| m["record"] == visit)
            })
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let file = e
        .records()
        .join(alex["folder"].as_str().unwrap())
        .join("marks.json");
    let event_marks = || -> Vec<Value> {
        let m: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        m["private"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|x| x["item"].as_str().unwrap().starts_with("e_"))
            .cloned()
            .collect()
    };

    // The portal visit's event, marked not private, keeps its members.
    let first = event_of(&e);
    assert_eq!(e.ok(&["private", "unmark", &first])["status"], "recorded");
    assert_eq!(e.ok(&["private", "unmark", &first])["status"], "unchanged");
    let marks = event_marks();
    assert_eq!(marks.len(), 1, "{marks:?}");
    assert_eq!(marks[0]["members"], json!([visit]));

    // The practice's claim for that visit arrives and becomes the event's
    // anchor: a new id, and the mark follows.
    let index = e
        .records()
        .join(alex["folder"].as_str().unwrap())
        .join("index.json");
    let mut idx: Value = serde_json::from_slice(&std::fs::read(&index).unwrap()).unwrap();
    let mut claim: Value = serde_json::from_str::<Value>(
        &std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/events/parent.index.json"),
        )
        .unwrap(),
    )
    .unwrap()["records"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == "r_hipdoctoraaaaaaaaaaa")
        .unwrap()
        .clone();
    claim["id"] = json!("r_quitclaimaaaaaaaaaaa");
    claim["key"] = json!("claim|example|quitclaim|2025-02-20");
    claim["date"] = json!("2025-02-20");
    claim["display"] = json!("Office visit · Example Recovery Center");
    claim["fields"]["provider"] = json!("Example Recovery Center");
    claim["fields"]["billable_period"] = json!({"start": "2025-02-20", "end": "2025-02-20"});
    idx["records"].as_array_mut().unwrap().push(claim);
    std::fs::write(&index, serde_json::to_vec(&idx).unwrap()).unwrap();
    e.ok(&["views", "write"]);
    let second = event_of(&e);
    assert_ne!(first, second, "the claim is the anchor now");
    let v = private_view(&e, aid);
    assert!(v["events"].get(&second).is_none(), "the mark followed: {v}");
    assert!(v["events"].get(&first).is_none(), "{v}");

    // Marking the new event replaces the followed mark; the same again
    // changes nothing; a reset drops it, and again changes nothing.
    assert_eq!(e.ok(&["private", "unmark", &second])["status"], "recorded");
    let marks = event_marks();
    assert_eq!(marks.len(), 1, "{marks:?}");
    assert_eq!(marks[0]["item"], second.as_str());
    let mut members: Vec<&str> = marks[0]["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m.as_str().unwrap())
        .collect();
    members.sort();
    assert_eq!(members, vec!["r_quitclaimaaaaaaaaaaa", visit]);
    assert_eq!(e.ok(&["private", "unmark", &second])["status"], "unchanged");
    assert_eq!(e.ok(&["private", "reset", &second])["status"], "recorded");
    assert!(event_marks().is_empty());
    assert_eq!(e.ok(&["private", "reset", &second])["status"], "unchanged");
    e.ok(&["views", "write"]);
    let v = private_view(&e, aid);
    assert_eq!(
        v["events"][&second]["from"], "members",
        "back to the rules: {v}"
    );
}

#[test]
fn a_decision_is_kept_replaced_or_undone_and_a_broken_file_never_stops_the_views() {
    let e = Env::new("decisions");
    setup(&e);
    let (alex, _) = with_event_fixtures(&e);
    let aid = alex["id"].as_str().unwrap();
    e.ok(&["views", "write"]);
    let file = e
        .records()
        .join(alex["folder"].as_str().unwrap())
        .join("overrides.json");
    let (doc, lab) = ("r_hipdoctoraaaaaaaaaaa", "r_hipimageaaaaaaaaaaaa");

    // Mistakes change nothing.
    for (args, code) in [
        (vec!["events", "apart", doc, doc], 2),
        (vec!["events", "apart", doc, "r_nosucharecordaaaaaaa"], 4),
        (vec!["events", "together", "d_nosuchadocumentaaaaa", lab], 4),
        (vec!["events", "undo", "o_aaaaaaaaaaaaaaaaaaaa"], 4),
        (vec!["events", "undo", "not-a-decision"], 2),
        (vec!["events", "show", "e_nosuchaneventaaaaaaa"], 4),
    ] {
        let (got, text) = e.code(&args);
        assert_eq!(got, Some(code), "{args:?}: {text}");
    }
    assert!(!file.exists(), "no decision was written");

    // The same decision again is unchanged, and the file with it; the other
    // kind for the same two replaces it.
    assert_eq!(e.ok(&["events", "apart", doc, lab])["status"], "recorded");
    let once = std::fs::read(&file).unwrap();
    assert_eq!(e.ok(&["events", "apart", lab, doc])["status"], "unchanged");
    assert_eq!(std::fs::read(&file).unwrap(), once);
    let t = e.ok(&["events", "together", doc, lab]);
    assert_eq!(t["status"], "recorded");
    let d = e.ok(&["events", "decisions"]);
    let items = d["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{d}");
    assert_eq!(
        (&items[0]["kind"], &items[0]["id"]),
        (&json!("together"), &t["id"])
    );
    e.ok(&["events", "undo", t["id"].as_str().unwrap()]);
    assert!(e.ok(&["events", "decisions"])["items"]
        .as_array()
        .unwrap()
        .is_empty());

    // A decisions file that does not read leaves the events without it and
    // says so; every other view is still written, and no decision is
    // written over it.
    std::fs::write(&file, b"{not json").unwrap();
    e.ok(&["views", "write"]);
    let v = events_view(&e, aid);
    assert_eq!(v["overrides_unreadable"], true, "{v}");
    assert!(!v["events"].as_array().unwrap().is_empty());
    let listed = e.ok(&["events", "list", "--person", "Alex"]);
    assert!(!listed["items"].as_array().unwrap().is_empty());
    let (code, text) = e.code(&["events", "apart", doc, lab]);
    assert_ne!(code, Some(0), "{text}");
    assert_eq!(std::fs::read(&file).unwrap(), b"{not json");
    let _ = std::fs::remove_dir_all(&e.root);
}

#[test]
fn an_event_request_records_the_households_decision_and_the_views_follow() {
    let e = Env::new("events");
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
    let (alex, robin) = with_event_fixtures(&e);
    let aid = alex["id"].as_str().unwrap();
    e.ok(&["views", "write"]);

    // Every person has an events view; Alex's names Robin's index, a
    // child's, among what it was made from.
    let views = e.json_at("views/views.json");
    let entry = &views["people"][aid];
    assert_eq!(entry["overrides"], Value::Null, "{entry}");
    let rid = robin["id"].as_str().unwrap();
    let robin_dir = e.records().join(robin["folder"].as_str().unwrap());
    assert_eq!(
        entry["linked"][rid],
        json!({
            "index": md5_of(&robin_dir.join("index.json")),
            "text": md5_of(&robin_dir.join("text.json")),
        }),
        "{entry}"
    );
    let before = events_view(&e, aid);
    assert_eq!(before["schema"], "medrec-events/v1");
    assert!(events_view(&e, rid)["events"].is_array());
    let listed = e.ok(&["events", "list", "--person", "Alex"]);
    assert_eq!(
        listed["items"].as_array().unwrap().len(),
        before["events"].as_array().unwrap().len()
    );
    let (doc, lab) = ("r_hipdoctoraaaaaaaaaaa", "r_hipimageaaaaaaaaaaaa");
    let visit = holding(&before, doc)["id"].as_str().unwrap().to_string();
    assert_eq!(holding(&before, lab)["id"], visit.as_str());
    let shown = e.ok(&["events", "show", &visit]);
    assert_eq!(shown["person"], aid);

    // "Not the same visit", from the app: the next pass records it and the
    // views follow.
    event_request(
        &e,
        "x_eventaaaaaaaaaaaaaaa",
        json!({"person": aid, "apart": [doc, lab]}),
    );
    // A member that is not the person's is refused.
    event_request(
        &e,
        "x_eventbbbbbbbbbbbbbbb",
        json!({"person": aid, "together": [doc, "r_notalexsaaaaaaaaaaaa"]}),
    );
    e.ok(&["refresh", "--scheduled"]);
    let done = e.json_at("_requests/done/20261124T120000Z_x_eventaaaaaaaaaaaaaaa.json");
    assert_eq!(done["handled"]["outcome"], "ok", "{done}");
    assert_eq!(done["event"], json!({"person": aid, "apart": [doc, lab]}));
    let refused = e.json_at("_requests/done/20261124T120000Z_x_eventbbbbbbbbbbbbbbb.json");
    assert_eq!(refused["handled"]["outcome"], "invalid", "{refused}");
    assert!(
        refused["handled"]["message"]
            .as_str()
            .unwrap()
            .contains("not one of the person's"),
        "{refused}"
    );
    let o = e.json_at(&format!(
        "{}/overrides.json",
        alex["folder"].as_str().unwrap()
    ));
    assert_eq!(o["schema"], "medrec-overrides/v1");
    let decisions = o["events"].as_array().unwrap();
    assert_eq!(decisions.len(), 1);
    assert_eq!(
        (
            decisions[0]["kind"].as_str(),
            decisions[0]["request"].as_str(),
            decisions[0]["by"].as_str()
        ),
        (
            Some("apart"),
            Some("x_eventaaaaaaaaaaaaaaa"),
            Some("Alex Testperson")
        )
    );
    let after = events_view(&e, aid);
    assert_eq!(after["overrides"], 1);
    assert_eq!(
        holding(&after, doc)["id"],
        visit.as_str(),
        "the anchor stays"
    );
    assert_ne!(holding(&after, lab)["id"], visit.as_str());
    assert!(e.json_at("views/views.json")["people"][aid]["overrides"].is_string());

    // From a terminal: the decisions, undone, and made again the other way.
    let d = e.ok(&["events", "decisions", "--person", "Alex"]);
    let id = d["items"][0]["id"].as_str().unwrap().to_string();
    e.ok(&["events", "undo", &id]);
    assert_eq!(holding(&events_view(&e, aid), lab)["id"], visit.as_str());
    let t = e.ok(&[
        "events",
        "together",
        "r_earvisitaaaaaaaaaaaa",
        "r_daytestaaaaaaaaaaaaa",
    ]);
    assert_eq!(t["status"], "recorded");
    assert_eq!(
        e.ok(&[
            "events",
            "together",
            "r_earvisitaaaaaaaaaaaa",
            "r_daytestaaaaaaaaaaaaa"
        ])["status"],
        "unchanged"
    );
    assert_eq!(
        holding(&events_view(&e, aid), "r_daytestaaaaaaaaaaaaa")["id"],
        holding(&events_view(&e, aid), "r_earvisitaaaaaaaaaaaa")["id"]
    );
    let (code, _) = e.code(&[
        "events",
        "apart",
        "r_earvisitaaaaaaaaaaaa",
        "r_nosucharecordaaaaaaa",
    ]);
    assert_eq!(code, Some(4));
    let _ = std::fs::remove_dir_all(&e.root);
}

fn md5_of(path: &Path) -> String {
    medrec::hash::md5_hex(&std::fs::read(path).unwrap())
}
