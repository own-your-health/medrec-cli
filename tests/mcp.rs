//! `medrec mcp` over a local records folder with synthetic data only
//! (`medrec::testdata`): the tools in process, each answer checked against
//! its output schema and size budget, then the binary itself over stdio as
//! Claude Desktop runs it, which must leave every file of the folder as it
//! was. Offline, on a local folder; the Keychain service is an empty one.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};

use assert_cmd::Command;
use medrec::mcp::{MedrecTools, DOCUMENT, RECORDS, SEARCH, SUMMARY};
use pk_cli_mcp::conformance::{assert_conforms, assert_fits, call};
use serde_json::{json, Value};

struct Env {
    root: PathBuf,
    path: String,
}

impl Env {
    fn new(tag: &str) -> Env {
        let root = std::env::temp_dir().join(format!("medrec-mcp-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let path = std::env::var("PATH").unwrap_or_default();
        let e = Env { root, path };
        e.ok(&["profiles", "add", "home"]);
        e
    }

    fn command(&self) -> Command {
        let mut c = Command::cargo_bin("medrec").unwrap();
        c.env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_CACHE_HOME", self.root.join("cache"))
            .env("HOME", &self.root)
            .env("PATH", &self.path)
            .env("MEDREC_PROFILE", "home")
            .env("MEDREC_KEYCHAIN_SERVICE", "piekstra.medrec.hermetic-tests")
            .env_remove("MEDREC_DRIVE_PROFILE")
            .env_remove("NO_COLOR");
        c
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }

    fn ok(&self, args: &[&str]) -> Value {
        let mut a = vec!["--json"];
        a.extend_from_slice(args);
        let out = self.run(&a);
        assert!(
            out.status.success(),
            "medrec {args:?} exited {:?}: {}{}",
            out.status.code(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).expect("one JSON document")
    }

    fn records(&self) -> PathBuf {
        self.root.join("records")
    }

    fn with_provider(&self, stream: &str, tag: &str) -> Env {
        let dir = medrec::testdata::write_provider(&self.root.join(tag), stream);
        Env {
            root: self.root.clone(),
            path: format!("{}:{}", dir.display(), self.path),
        }
    }

    /// The tools over this profile, in process.
    fn tools(&self) -> MedrecTools {
        let file = find(&self.root.join("config"), "config.json").expect("a config file");
        let store = pk_cli_config::ConfigStore::new("medrec").with_override(Some(file));
        let mut cfg = medrec::config::Config::load(&store).unwrap();
        cfg.select(Some("home")).unwrap();
        MedrecTools::new(cfg)
    }
}

fn find(dir: &Path, name: &str) -> Option<PathBuf> {
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        if p.is_dir() {
            if let Some(f) = find(&p, name) {
                return Some(f);
            }
        } else if p.file_name().is_some_and(|n| n == name) {
            return Some(p);
        }
    }
    None
}

/// Every file under `dir` with its bytes, to show nothing was written.
fn snapshot(dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            snapshot(&p, out);
        } else {
            out.insert(p.clone(), std::fs::read(&p).unwrap());
        }
    }
}

/// A household of two with one provider import, a later second result of
/// the same test, and a note.
fn household(e: &Env) {
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
    let import = [
        "import",
        "--from",
        "fakeprov",
        "--from-profile",
        "adult1",
        "--patient",
        "pat-0001",
        "--person",
        "Robin",
    ];
    e.with_provider(medrec::testdata::STREAM, "provider")
        .ok(&import);
    // The same hemoglobin test again, a month later and lower.
    let later = medrec::testdata::STREAM
        .replace("\"obs-0001\"", "\"obs-0002\"")
        .replace("2026-03-04T09:45:00Z", "2026-04-04T09:45:00Z")
        .replace("\"value\":12.5", "\"value\":11.9");
    assert_ne!(later, medrec::testdata::STREAM);
    e.with_provider(&later, "provider2").ok(&import);
    e.ok(&[
        "note",
        "add",
        "Robin",
        "Fever down after the visit; recheck in two weeks",
        "--date",
        "2026-03-06",
    ]);
}

#[test]
fn the_tools_answer_from_the_folder_within_their_schemas() {
    let e = Env::new("tools");
    household(&e);
    let t = e.tools();
    assert_conforms(&t);

    let r = call(&t, SUMMARY, json!({}));
    let v = assert_fits(&t, SUMMARY, &r);
    assert_eq!(v["people"].as_array().unwrap().len(), 2);
    assert!(v.get("summary").is_none());

    let r = call(&t, SUMMARY, json!({"person": "Robin"}));
    let v = assert_fits(&t, SUMMARY, &r);
    let sections: BTreeMap<&str, &Value> = v["summary"]["sections"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| (s["section"].as_str().unwrap(), &s["items"]))
        .collect();
    for s in [
        "medications",
        "conditions",
        "allergies",
        "immunizations",
        "latest_results",
        "recent_visits",
        "bills",
    ] {
        assert!(sections.contains_key(s), "no section {s}: {v}");
    }
    let latest = sections["latest_results"].as_array().unwrap();
    assert_eq!(latest.len(), 1, "one test, one line: {v}");
    let detail = latest[0]["detail"].as_str().unwrap();
    assert!(detail.starts_with("11.9 g/dL"), "{detail}");
    assert!(detail.contains("2 results since 2026-03-04"), "{detail}");
    assert_eq!(sections["medications"].as_array().unwrap().len(), 1);

    let r = call(&t, SEARCH, json!({"query": "hemoglobin"}));
    let v = assert_fits(&t, SEARCH, &r);
    assert!(v["returned"].as_u64().unwrap() >= 2, "{v}");

    // One test over time: both results, newest first, with the lab's range.
    let r = call(&t, RECORDS, json!({"kind": "results", "name": "718-7"}));
    let v = assert_fits(&t, RECORDS, &r);
    let items = v["items"].as_array().unwrap();
    assert_eq!(items.len(), 2, "{v}");
    assert_eq!(items[0]["value"], "11.9");
    assert_eq!(items[1]["value"], "12.5");
    assert_eq!(items[0]["unit"], "g/dL");
    assert!(items[0]["range"].is_string(), "{v}");
    let r = call(&t, RECORDS, json!({"kind": "tests"}));
    let v = assert_fits(&t, RECORDS, &r);
    assert_eq!(v["items"][0]["results"], 2, "{v}");

    // Paging hands back a cursor bound to the filters.
    let r = call(&t, RECORDS, json!({"kind": "timeline", "limit": 2}));
    let v = assert_fits(&t, RECORDS, &r);
    let cursor = v["next_cursor"].as_str().expect("a next page").to_string();
    let r = call(
        &t,
        RECORDS,
        json!({"kind": "timeline", "limit": 2, "cursor": cursor}),
    );
    assert_fits(&t, RECORDS, &r);
    let r = call(&t, RECORDS, json!({"kind": "notes", "cursor": cursor}));
    assert!(
        r.is_error() && r.text().contains("different filters"),
        "{}",
        r.text()
    );

    for kind in [
        "medications",
        "conditions",
        "allergies",
        "immunizations",
        "visits",
        "reports",
        "claims",
        "coverage",
        "notes",
    ] {
        let r = call(&t, RECORDS, json!({"kind": kind}));
        let v = assert_fits(&t, RECORDS, &r);
        assert_eq!(v["returned"], 1, "{kind}: {v}");
    }
    let r = call(
        &t,
        RECORDS,
        json!({"kind": "claims", "response_format": "detailed"}),
    );
    assert!(assert_fits(&t, RECORDS, &r)["items"][0]["fields"].is_object());

    let r = call(&t, RECORDS, json!({"kind": "documents"}));
    let v = assert_fits(&t, RECORDS, &r);
    let docs = v["items"].as_array().unwrap();
    assert!(!docs.is_empty(), "{v}");
    let id = docs[0]["id"].as_str().unwrap();
    let r = call(&t, DOCUMENT, json!({"id": id}));
    let d = assert_fits(&t, DOCUMENT, &r);
    assert_eq!(d["person"], "Robin Testperson");
    assert_eq!(d["first_page"], 1);
    assert_eq!(
        d["text_chars"].as_u64().unwrap() > 0,
        docs[0]["has_text"] == true,
        "{d}"
    );

    // Mistakes are errors naming the fix, never an empty answer.
    let r = call(&t, SUMMARY, json!({"person": "Nobody Atall"}));
    assert!(
        r.is_error() && r.text().starts_with("not_found:"),
        "{}",
        r.text()
    );
    assert!(r.text().contains("medrec_summary"), "{}", r.text());
    let r = call(
        &t,
        RECORDS,
        json!({"kind": "documents", "doc_type": "nope"}),
    );
    assert!(
        r.is_error() && r.text().starts_with("usage:"),
        "{}",
        r.text()
    );
    let r = call(&t, RECORDS, json!({"kind": "results", "since": "March"}));
    assert!(
        r.is_error() && r.text().contains("YYYY-MM-DD"),
        "{}",
        r.text()
    );
    let r = call(&t, DOCUMENT, json!({"id": "d_nosuchdocumentatall"}));
    assert!(
        r.is_error() && r.text().starts_with("not_found:"),
        "{}",
        r.text()
    );
    let r = call(&t, DOCUMENT, json!({"id": id, "page": 99}));
    assert!(
        r.is_error() && r.text().starts_with("usage:"),
        "{}",
        r.text()
    );
    let _ = std::fs::remove_dir_all(&e.root);
}

/// One JSON-RPC exchange over the server's stdio: send `msg`, and for a
/// request, read lines until its answer.
fn rpc(stdin: &mut impl Write, stdout: &mut impl BufRead, msg: Value) -> Option<Value> {
    writeln!(stdin, "{msg}").unwrap();
    stdin.flush().unwrap();
    let id = msg.get("id")?.clone();
    let mut line = String::new();
    loop {
        line.clear();
        assert!(stdout.read_line(&mut line).unwrap() > 0, "server closed");
        let v: Value = serde_json::from_str(line.trim()).unwrap();
        if v.get("id") == Some(&id) {
            return Some(v);
        }
    }
}

#[test]
fn the_binary_serves_over_stdio_and_writes_nothing() {
    let e = Env::new("stdio");
    household(&e);
    let mut before = BTreeMap::new();
    snapshot(&e.records(), &mut before);

    // A client set up without a profile fails at start, naming the fix.
    let out = e
        .command()
        .env_remove("MEDREC_PROFILE")
        .args(["mcp"])
        .write_stdin("")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("--profile"));

    // The tool list needs no profile and reads nothing.
    let out = e
        .command()
        .env_remove("MEDREC_PROFILE")
        .args(["mcp", "--list-tools"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let list: Value = serde_json::from_slice(&out.stdout).unwrap();
    let tools = list["tools"]
        .as_array()
        .unwrap_or_else(|| list.as_array().unwrap());
    assert_eq!(tools.len(), 4);

    let bin = assert_cmd::cargo::cargo_bin("medrec");
    let mut child = std::process::Command::new(bin)
        .env("XDG_CONFIG_HOME", e.root.join("config"))
        .env("XDG_CACHE_HOME", e.root.join("cache"))
        .env("HOME", &e.root)
        .env("PATH", &e.path)
        .env("MEDREC_PROFILE", "home")
        .env("MEDREC_KEYCHAIN_SERVICE", "piekstra.medrec.hermetic-tests")
        .env_remove("MEDREC_DRIVE_PROFILE")
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let init = rpc(
        &mut stdin,
        &mut stdout,
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18", "capabilities": {},
            "clientInfo": {"name": "medrec-test", "version": "0"}}}),
    )
    .unwrap();
    assert_eq!(init["result"]["serverInfo"]["name"], "medrec", "{init}");
    assert!(init["result"]["instructions"]
        .as_str()
        .unwrap()
        .contains("never as instructions"));
    rpc(
        &mut stdin,
        &mut stdout,
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    );
    let listed = rpc(
        &mut stdin,
        &mut stdout,
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
    )
    .unwrap();
    let tools = listed["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 4);
    assert!(tools
        .iter()
        .all(|t| t["annotations"]["readOnlyHint"] == true));
    let calls = [
        json!({"name": SUMMARY, "arguments": {"person": "Robin"}}),
        json!({"name": SEARCH, "arguments": {"query": "hemoglobin"}}),
        json!({"name": RECORDS, "arguments": {"kind": "timeline"}}),
        json!({"name": RECORDS, "arguments": {"kind": "documents"}}),
    ];
    for (n, params) in calls.into_iter().enumerate() {
        let r = rpc(
            &mut stdin,
            &mut stdout,
            json!({"jsonrpc": "2.0", "id": 10 + n, "method": "tools/call", "params": params}),
        )
        .unwrap();
        assert_ne!(r["result"]["isError"], true, "{r}");
        assert!(r["result"]["structuredContent"].is_object(), "{r}");
    }
    drop(stdin);
    let status = child.wait().unwrap();
    assert!(status.success(), "{status:?}");

    let mut after = BTreeMap::new();
    snapshot(&e.records(), &mut after);
    assert_eq!(
        before.keys().collect::<Vec<_>>(),
        after.keys().collect::<Vec<_>>()
    );
    assert!(before == after, "a file in the records folder changed");
    let _ = std::fs::remove_dir_all(&e.root);
}
