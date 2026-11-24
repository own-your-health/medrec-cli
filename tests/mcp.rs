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
            .env("TZ", "America/New_York")
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
    let v = assert_fits(&t, RECORDS, &r);
    assert!(v["items"][0]["fields"].is_object());
    // The member's share once, labelled as medrec's sum: the payer gave
    // no figure of its own.
    let detail = v["items"][0]["detail"].as_str().unwrap();
    assert!(detail.contains("your share"), "{detail}");
    assert!(!detail.contains("member owes"), "{detail}");

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

#[test]
fn events_answer_what_happened_with_each_members_title_and_date() {
    let e = Env::new("events");
    household(&e);
    // Alex's records are the invented parent's (tests/fixtures/events).
    let h: Value =
        serde_json::from_slice(&std::fs::read(e.records().join("household.json")).unwrap())
            .unwrap();
    let alex = h["people"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "Alex Testperson")
        .unwrap()
        .clone();
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
    let t = e.tools();
    assert_conforms(&t);

    let ev = |args: Value| {
        let mut a = args;
        a["kind"] = json!("events");
        let r = call(&t, RECORDS, a);
        assert_fits(&t, RECORDS, &r).clone()
    };
    let v = ev(json!({"person": "Alex"}));
    let items = v["items"].as_array().unwrap();
    let er = items
        .iter()
        .find(|i| i["kind"] == "er")
        .expect("the ER visit");
    assert_eq!(
        (&er["you_owe"], &er["date"], &er["end"], &er["status"]),
        (
            &json!("260.00 USD"),
            &json!("2026-10-07"),
            &json!("2026-10-08"),
            &json!("complete")
        ),
        "{er}"
    );
    assert_eq!(er["source"], "Example General Hospital", "{er}");
    assert!(er.get("fields").is_none(), "concise leaves the members out");
    let starts: Vec<&str> = items.iter().map(|i| i["date"].as_str().unwrap()).collect();
    assert!(starts.windows(2).all(|w| w[0] >= w[1]), "newest first");
    // What is still expected, and a grouping that is not sure, in words.
    assert!(
        items.iter().any(|i| i["status"] == "waiting"
            && i["detail"]
                .as_str()
                .unwrap_or("")
                .contains("still expected:")),
        "{v}"
    );
    assert!(
        items.iter().any(|i| i["detail"]
            .as_str()
            .unwrap_or("")
            .contains("grouping unsure")),
        "{v}"
    );

    // Words of the event; dates that overlap its days.
    let v = ev(json!({"name": "amoxicillin"}));
    assert_eq!(v["returned"], 1, "{v}");
    assert_eq!(v["items"][0]["kind"], "fill", "{v}");
    let v = ev(json!({"since": "2026-10-08", "until": "2026-10-08"}));
    assert!(
        v["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["id"] == er["id"]),
        "an event spanning the day: {v}"
    );

    // One event in full: its money and members, each with its title and
    // date, from events.json alone.
    let v = ev(json!({"id": er["id"]}));
    assert_eq!(v["returned"], 1, "{v}");
    let f = &v["items"][0]["fields"];
    assert_eq!(f["money"]["discount"], "8955.00", "{f}");
    assert_eq!(f["confidence"], "sure", "{f}");
    let members = f["members"].as_array().unwrap();
    assert!(members.len() >= 4, "{f}");
    assert!(
        members
            .iter()
            .all(|m| m["title"].is_string() && m["date"].is_string()),
        "{f}"
    );

    // Mistakes are errors naming the fix.
    for (args, want) in [
        (
            json!({"kind": "events", "id": er["id"], "person": "Alex"}),
            "usage:",
        ),
        (
            json!({"kind": "events", "id": "x_notaneventatall"}),
            "usage:",
        ),
        (json!({"kind": "claims", "id": er["id"]}), "usage:"),
        (
            json!({"kind": "events", "id": "e_aaaaaaaaaaaaaaaaaaaa"}),
            "not_found:",
        ),
        (
            json!({"kind": "events", "person": "Nobody Atall"}),
            "not_found:",
        ),
    ] {
        let r = call(&t, RECORDS, args.clone());
        assert!(
            r.is_error() && r.text().starts_with(want),
            "{args}: {}",
            r.text()
        );
    }

    // A bill past its due date says so until it is paid, then what is left
    // once something is paid, and that it is paid.
    let bill = "d_urologybillaaaaaaaaa";
    let detail = || -> String {
        let r = call(&t, RECORDS, json!({"kind": "bills", "person": "Alex"}));
        let v = assert_fits(&t, RECORDS, &r).clone();
        v["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["id"] == bill)
            .unwrap_or_else(|| panic!("no {bill}: {v}"))["detail"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let summary_bills = || -> Vec<String> {
        let r = call(&t, SUMMARY, json!({"person": "Alex"}));
        let v = assert_fits(&t, SUMMARY, &r).clone();
        v["summary"]["sections"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["section"] == "bills")
            .map(|s| {
                s["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|i| i["id"].as_str().unwrap_or("").to_string())
                    .collect()
            })
            .unwrap_or_default()
    };
    assert_eq!(detail(), "Bill, past due");
    assert!(
        summary_bills().iter().any(|b| b == bill),
        "{:?}",
        summary_bills()
    );
    e.ok(&[
        "bills",
        "paid",
        bill,
        "--on",
        "2026-03-20",
        "--amount",
        "15.00",
    ]);
    assert_eq!(detail(), "Bill, past due, 25.00 left");
    e.ok(&[
        "bills",
        "paid",
        bill,
        "--on",
        "2026-03-25",
        "--amount",
        "25.00",
    ]);
    assert_eq!(detail(), "Bill, paid", "a settled bill is never past due");
    assert!(
        !summary_bills().iter().any(|b| b == bill),
        "a settled bill is not one to pay: {:?}",
        summary_bills()
    );
    let _ = std::fs::remove_dir_all(&e.root);
}

/// One JSON-RPC exchange over the server's stdio: send `msg`, and for a
/// request, read lines until its answer.
#[test]
fn private_items_stay_out_of_household_wide_answers_unless_asked_about() {
    let e = Env::new("private");
    household(&e);
    // Alex's records are the invented adult's (tests/fixtures/private).
    let h: Value =
        serde_json::from_slice(&std::fs::read(e.records().join("household.json")).unwrap())
            .unwrap();
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
    let (depression, sertraline, letter) = (
        "r_depressionaaaaaaaaaa",
        "r_sertralineaaaaaaaaaa",
        "d_therapyletteraaaaaaa",
    );
    let t = e.tools();
    let ask = |tool: &str, args: Value| {
        let r = call(&t, tool, args);
        assert!(!r.is_error(), "{tool}: {}", r.text());
        assert_fits(&t, tool, &r).clone()
    };
    let ids = |v: &Value| -> Vec<String> {
        v["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["id"].as_str().unwrap_or("").to_string())
            .collect()
    };
    let left_out = |v: &Value| -> Option<String> {
        v["warnings"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|w| w["code"] == "private-left-out")
            .map(|w| w["message"].as_str().unwrap().to_string())
    };
    let item = |v: &Value, id: &str| -> Value {
        v["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["id"] == id)
            .unwrap_or_else(|| panic!("no {id}: {v}"))
            .clone()
    };

    // Household-wide: left out, and the warning says how many.
    let v = ask(RECORDS, json!({"kind": "conditions"}));
    assert!(!ids(&v).contains(&depression.to_string()), "{v}");
    assert!(
        ids(&v).contains(&"r_hypertensionaaaaaaaa".to_string()),
        "{v}"
    );
    let warn = left_out(&v).expect("a private-left-out warning");
    assert!(warn.starts_with("1 private item left out"), "{warn}");
    let v = ask(RECORDS, json!({"kind": "medications", "name": "tablet"}));
    assert!(
        !ids(&v).contains(&sertraline.to_string()),
        "a form names no medicine: {v}"
    );
    assert!(left_out(&v).is_some(), "{v}");
    let v = ask(RECORDS, json!({"kind": "events"}));
    assert!(
        v["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i["source"] != "Example Behavioral Health"),
        "{v}"
    );
    assert!(left_out(&v).is_some(), "{v}");
    let v = ask(SEARCH, json!({"query": "session"}));
    assert!(!ids(&v).contains(&letter.to_string()), "{v}");
    assert!(left_out(&v).is_some(), "{v}");
    let v = ask(SEARCH, json!({"query": "recheck"}));
    assert!(left_out(&v).is_none(), "nothing private matched: {v}");

    // A page counts what was left out between it and the page before; the
    // pages add up to the answer's whole count.
    let whole = ask(RECORDS, json!({"kind": "timeline"}));
    let count = |v: &Value| -> usize {
        left_out(v).map_or(0, |w| w.split(' ').next().unwrap().parse().unwrap())
    };
    let total = count(&whole);
    assert!(total >= 2, "{whole}");
    let (mut sum, mut pages, mut cursor) = (0, 0, None::<String>);
    loop {
        let mut args = json!({"kind": "timeline", "limit": 2});
        if let Some(c) = &cursor {
            args["cursor"] = json!(c);
        }
        let v = ask(RECORDS, args);
        sum += count(&v);
        pages += 1;
        match v["next_cursor"].as_str() {
            Some(c) => cursor = Some(c.to_string()),
            None => break,
        }
    }
    assert!(pages > 1);
    assert_eq!(sum, total, "{pages} pages");

    // About the person, or naming the item: in, and marked.
    let v = ask(RECORDS, json!({"kind": "conditions", "person": "Alex"}));
    assert_eq!(item(&v, depression)["private"], "mental_health");
    assert!(item(&v, "r_hypertensionaaaaaaaa").get("private").is_none());
    assert!(left_out(&v).is_none(), "{v}");
    let v = ask(
        RECORDS,
        json!({"kind": "medications", "name": "sertraline"}),
    );
    assert_eq!(item(&v, sertraline)["private"], "mental_health");
    let v = ask(SEARCH, json!({"query": "sertraline"}));
    assert_eq!(item(&v, sertraline)["private"], "mental_health");
    let v = ask(SEARCH, json!({"query": "session", "person": "Alex"}));
    assert_eq!(item(&v, letter)["private"], "mental_health");
    let v = ask(RECORDS, json!({"kind": "events", "name": "behavioral"}));
    let ev = &v["items"][0];
    assert_eq!(
        (&ev["source"], &ev["private"]),
        (&json!("Example Behavioral Health"), &json!("mental_health")),
        "{v}"
    );
    let v = ask(DOCUMENT, json!({"id": letter}));
    assert_eq!(v["private"], "mental_health");
    let v = ask(DOCUMENT, json!({"id": "d_checkupavsaaaaaaaaaa"}));
    assert!(v.get("private").is_none(), "{v}");
    let v = ask(SUMMARY, json!({"person": "Alex"}));
    let lines: Vec<&Value> = v["summary"]["sections"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|s| s["items"].as_array().unwrap())
        .collect();
    let marked: Vec<&str> = lines
        .iter()
        .filter(|l| l["private"] == "mental_health")
        .map(|l| l["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        marked,
        vec![sertraline, depression, "r_therapyvisitaaaaaaaa"],
        "{v}"
    );
    assert!(lines.iter().filter(|l| l.get("private").is_none()).count() >= 3);

    // A marks.json that does not read: any of Alex's items may be one the
    // household marked, so a household-wide answer holds them all back.
    std::fs::write(
        e.records()
            .join(alex["folder"].as_str().unwrap())
            .join("marks.json"),
        b"{not json",
    )
    .unwrap();
    let v = ask(RECORDS, json!({"kind": "conditions"}));
    assert!(
        v["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i["person"] != "Alex Testperson"),
        "{v}"
    );
    assert!(left_out(&v).unwrap().starts_with("2 private items"), "{v}");
    let unreadable: Vec<&Value> = v["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|w| w["code"] == "private-marks-unreadable")
        .collect();
    assert_eq!(unreadable.len(), 1, "{v}");
    let v = ask(RECORDS, json!({"kind": "conditions", "person": "Alex"}));
    assert_eq!(item(&v, depression)["private"], "mental_health");
    assert!(item(&v, "r_hypertensionaaaaaaaa").get("private").is_none());

    // A second Alex Testperson, born another day, whose marks read: only
    // the first one's records and events are held back.
    e.ok(&[
        "people",
        "add",
        "Alex Testperson",
        "--birth-date",
        "1985-05-05",
    ]);
    let h: Value =
        serde_json::from_slice(&std::fs::read(e.records().join("household.json")).unwrap())
            .unwrap();
    let other = h["people"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "Alex Testperson" && p["id"] != alex["id"])
        .unwrap()
        .clone();
    let mut v: Value = serde_json::from_str(
        &std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/events/portal.index.json"),
        )
        .unwrap(),
    )
    .unwrap();
    v["person"] = other["id"].clone();
    std::fs::write(
        e.records()
            .join(other["folder"].as_str().unwrap())
            .join("index.json"),
        serde_json::to_vec(&v).unwrap(),
    )
    .unwrap();
    let v = ask(RECORDS, json!({"kind": "medications"}));
    assert!(
        ids(&v).contains(&"r_amoxfillaaaaaaaaaaaa".to_string()),
        "{v}"
    );
    assert!(
        !ids(&v).contains(&"r_lisinoprilaaaaaaaaaa".to_string()),
        "{v}"
    );
    let v = ask(RECORDS, json!({"kind": "events"}));
    let shown: Vec<&Value> = v["items"].as_array().unwrap().iter().collect();
    assert!(
        shown.iter().any(|i| i["source"] == "Example Pharmacy"),
        "the other Alex's fills: {v}"
    );
    assert!(
        shown
            .iter()
            .all(|i| i["source"] != "Example Family Practice"),
        "the first Alex's events are held back: {v}"
    );
    let warned = v["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|w| w["code"] == "private-marks-unreadable")
        .count();
    assert_eq!(warned, 1, "{v}");
}

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
        .env("TZ", "America/New_York")
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

/// Serves `mcp` with `env`, calls medrec_summary `calls` times in that one
/// server and returns each answer with how long it took.
fn call_summary(e: &Env, env: &[(&str, &str)], calls: usize) -> Vec<(Value, std::time::Duration)> {
    let bin = assert_cmd::cargo::cargo_bin("medrec");
    let mut cmd = std::process::Command::new(bin);
    cmd.env("XDG_CONFIG_HOME", e.root.join("config"))
        .env("TZ", "America/New_York")
        .env("XDG_CACHE_HOME", e.root.join("cache"))
        .env("HOME", &e.root)
        .env("PATH", &e.path)
        .env("MEDREC_PROFILE", "home")
        .env("MEDREC_KEYCHAIN_SERVICE", "piekstra.medrec.hermetic-tests")
        .env_remove("MEDREC_DRIVE_PROFILE");
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    rpc(
        &mut stdin,
        &mut stdout,
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18", "capabilities": {},
            "clientInfo": {"name": "medrec-test", "version": "0"}}}),
    )
    .unwrap();
    rpc(
        &mut stdin,
        &mut stdout,
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    );
    let mut answers = Vec::new();
    for id in 0..calls {
        let started = std::time::Instant::now();
        let r = rpc(
            &mut stdin,
            &mut stdout,
            json!({"jsonrpc": "2.0", "id": 2 + id, "method": "tools/call",
                   "params": {"name": SUMMARY, "arguments": {}}}),
        )
        .unwrap();
        answers.push((r, started.elapsed()));
    }
    drop(stdin);
    let _ = child.wait();
    answers
}

#[test]
fn a_drive_folders_keychain_dialog_nobody_answers_ends_the_call_with_auth() {
    let e = Env::new("keychain");
    e.ok(&[
        "config",
        "set",
        "folder",
        "gdrive:home-drive/1Synthetic_folder",
    ]);
    // A sealed entry in medrec's vault, so reading the sign-in needs the
    // vault's key; the test keychain holds none.
    let vault = e.root.join(".config/medrec/vault");
    std::fs::create_dir_all(&vault).unwrap();
    std::fs::write(vault.join("another-entry.pkv"), b"sealed").unwrap();
    let log = e.root.join("keychain-reads");
    // The first read waits 5 s on a dialog; each call waits 2 s for it.
    let calls = call_summary(
        &e,
        &[
            ("MEDREC_TEST_KEYCHAIN_DELAY_SECS", "5"),
            ("MEDREC_TEST_KEYCHAIN_LOG", log.to_str().unwrap()),
            ("MEDREC_TEST_AUTH_WAIT_SECS", "2"),
        ],
        3,
    );
    // The first two calls end with `auth` while the dialog is open. The
    // second waited on the same dialog, never raising another.
    for (r, took) in &calls[..2] {
        assert_eq!(r["result"]["isError"], true, "{r}");
        assert!(r.to_string().contains("Always Allow"), "{r}");
        assert!(*took < std::time::Duration::from_secs(4), "{took:?}");
    }
    // Answered during the third: there is no sign-in on this machine, so
    // the call says how to sign in.
    let (r, _) = &calls[2];
    assert_eq!(r["result"]["isError"], true, "{r}");
    assert!(
        r.to_string()
            .contains("medrec drive login --drive-profile home-drive"),
        "{r}"
    );
    let reads = std::fs::read_to_string(&log).unwrap().lines().count();
    assert_eq!(reads, 1, "one read, which every call waited on");
    let _ = std::fs::remove_dir_all(&e.root);
}
