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
            &json!("161.00 USD"),
            &json!("2024-10-08"),
            &Value::Null,
            &json!("complete")
        ),
        "{er}"
    );
    assert_eq!(er["source"], "Sample Valley Hospital", "{er}");
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
    let v = ev(json!({"name": "amlodipine"}));
    assert_eq!(v["returned"], 1, "{v}");
    assert_eq!(v["items"][0]["kind"], "fill", "{v}");
    let v = ev(json!({"since": "2022-03-04", "until": "2022-03-04"}));
    assert_eq!(
        v["items"][0]["source"], "Example Harbor Hospital",
        "an event spanning the day: {v}"
    );

    // One event in full: its money and members, each with its title and
    // date, from events.json alone.
    let v = ev(json!({"id": er["id"]}));
    assert_eq!(v["returned"], 1, "{v}");
    let f = &v["items"][0]["fields"];
    assert_eq!(f["money"]["discount"], "1625.00", "{f}");
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
    let bill = "d_hearingbillaaaaaaaaa";
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
        "30.00",
    ]);
    assert_eq!(detail(), "Bill, past due, 20.00 left");
    e.ok(&[
        "bills",
        "paid",
        bill,
        "--on",
        "2026-03-25",
        "--amount",
        "20.00",
    ]);
    assert_eq!(detail(), "Bill, paid", "a settled bill is never past due");
    assert!(
        !summary_bills().iter().any(|b| b == bill),
        "a settled bill is not one to pay: {:?}",
        summary_bills()
    );
    let _ = std::fs::remove_dir_all(&e.root);
}

/// A letter only a private record links, and studies by their report, stay
/// out of household-wide answers; an unreported study does too.
#[test]
fn linked_letters_and_studies_follow_what_is_private() {
    let e = Env::new("private-links");
    household(&e);
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
    let mut v: Value =
        serde_json::from_str(&std::fs::read_to_string(fixtures.join("adult.index.json")).unwrap())
            .unwrap();
    v["person"] = alex["id"].clone();
    // A letter in no event, linked by the smoking record alone.
    let mut letter = v["documents"][0].clone();
    letter["id"] = json!("d_linkedletteraaaaaaaa");
    letter["title"] = json!("Zebrafish letter");
    letter.as_object_mut().unwrap().remove("organization");
    v["documents"].as_array_mut().unwrap().push(letter);
    let smoking = v["records"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|r| r["id"] == "r_smokingaaaaaaaaaaaaa")
        .unwrap();
    smoking["documents"] = json!(["d_linkedletteraaaaaaaa"]);
    let study = |id: &str, title: &str, report: Option<&str>| {
        let mut s = json!({"id": id, "study_uid": id, "path": format!("Imaging/{id}/"),
            "date": "2026-10-30", "description": title, "body_part": "kneecap", "series": [], "instances": 0, "size": 0,
            "filed_at": "2026-11-20T10:00:00Z", "provenance": []});
        if let Some(r) = report {
            s["report"] = json!(r);
        }
        s
    };
    v["studies"] = json!([
        study(
            "m_privateaaaaaaaaaaaaa",
            "Quokka picture",
            Some("d_linkedletteraaaaaaaa")
        ),
        study(
            "m_openaaaaaaaaaaaaaaaa",
            "Wombat picture",
            Some("d_headsummaryaaaaaaaaa")
        ),
        study("m_unreportedaaaaaaaaaa", "Numbat picture", None),
    ]);
    std::fs::write(
        e.records()
            .join(alex["folder"].as_str().unwrap())
            .join("index.json"),
        serde_json::to_vec(&v).unwrap(),
    )
    .unwrap();
    std::fs::write(
        e.records()
            .join(alex["folder"].as_str().unwrap())
            .join("text.json"),
        serde_json::to_vec(&json!({"schema": "medrec-text/v1", "person": alex["id"],
            "texts": {"d_linkedletteraaaaaaaa": "kneecap notes"}}))
        .unwrap(),
    )
    .unwrap();
    let t = e.tools();
    let ask = |args: Value| {
        let r = call(&t, SEARCH, args);
        assert!(!r.is_error(), "{}", r.text());
        assert_fits(&t, SEARCH, &r).clone()
    };
    let found = |v: &Value, id: &str| -> Option<Value> {
        v["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["id"] == id)
            .cloned()
    };
    let left_out = |v: &Value| {
        v["warnings"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|w| w["code"] == "private-left-out")
    };
    for (word, id, held) in [
        ("zebrafish", "d_linkedletteraaaaaaaa", true),
        ("quokka", "m_privateaaaaaaaaaaaaa", true),
        ("numbat", "m_unreportedaaaaaaaaaa", true),
        ("wombat", "m_openaaaaaaaaaaaaaaaa", false),
    ] {
        // Not named, household-wide.
        let v = ask(json!({"query": "kneecap"}));
        assert_eq!(found(&v, id).is_none(), held, "{id}: {v}");
        // About the person, all of it.
        let v = ask(json!({"query": word, "person": "Alex"}));
        let i = found(&v, id).unwrap_or_else(|| panic!("no {id}: {v}"));
        let marked = i.get("private").is_some();
        assert_eq!(marked, matches!(word, "zebrafish" | "quokka"), "{id}: {i}");
        // Named: included.
        assert!(found(&ask(json!({"query": word})), id).is_some(), "{id}");
    }
    let v = ask(json!({"query": "kneecap"}));
    assert!(left_out(&v), "{v}");
    assert_eq!(v["items"].as_array().unwrap().len(), 1, "{v}");
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
    let (smoking, varenicline, letter) = (
        "r_smokingaaaaaaaaaaaaa",
        "r_vareniclineaaaaaaaaa",
        "d_quitnoticeaaaaaaaaaa",
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
    assert!(!ids(&v).contains(&smoking.to_string()), "{v}");
    assert!(
        ids(&v).contains(&"r_migraineaaaaaaaaaaaa".to_string()),
        "{v}"
    );
    let warn = left_out(&v).expect("a private-left-out warning");
    assert!(warn.starts_with("1 private item left out"), "{warn}");
    let v = ask(RECORDS, json!({"kind": "medications", "name": "tablet"}));
    assert!(
        !ids(&v).contains(&varenicline.to_string()),
        "a form names no medicine: {v}"
    );
    assert!(left_out(&v).is_some(), "{v}");
    let v = ask(RECORDS, json!({"kind": "events"}));
    assert!(
        v["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i["source"] != "Example Recovery Center"),
        "{v}"
    );
    assert!(left_out(&v).is_some(), "{v}");
    let v = ask(SEARCH, json!({"query": "cedar"}));
    assert!(!ids(&v).contains(&letter.to_string()), "{v}");
    assert!(left_out(&v).is_some(), "{v}");
    let v = ask(SEARCH, json!({"query": "diary"}));
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
    assert_eq!(item(&v, smoking)["private"], "substance_use");
    assert!(item(&v, "r_migraineaaaaaaaaaaaa").get("private").is_none());
    assert!(left_out(&v).is_none(), "{v}");
    let v = ask(
        RECORDS,
        json!({"kind": "medications", "name": "varenicline"}),
    );
    assert_eq!(item(&v, varenicline)["private"], "substance_use");
    let v = ask(SEARCH, json!({"query": "varenicline"}));
    assert_eq!(item(&v, varenicline)["private"], "substance_use");
    let v = ask(SEARCH, json!({"query": "cedar", "person": "Alex"}));
    assert_eq!(item(&v, letter)["private"], "substance_use");
    let v = ask(RECORDS, json!({"kind": "events", "name": "recovery"}));
    let ev = &v["items"][0];
    assert_eq!(
        (&ev["source"], &ev["private"]),
        (&json!("Example Recovery Center"), &json!("substance_use")),
        "{v}"
    );
    let v = ask(DOCUMENT, json!({"id": letter}));
    assert_eq!(v["private"], "substance_use");
    let v = ask(DOCUMENT, json!({"id": "d_headsummaryaaaaaaaaa"}));
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
        .filter(|l| l["private"] == "substance_use")
        .map(|l| l["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        marked,
        vec![
            varenicline,
            smoking,
            "r_screenaaaaaaaaaaaaaa",
            "r_quitvisitaaaaaaaaaaa"
        ],
        "{v}"
    );
    assert!(lines.iter().filter(|l| l.get("private").is_none()).count() >= 3);

    // A series of tests holding one private result is private: a
    // household-wide answer leaves the whole series out and counts it once,
    // even when its latest result is not the private one.
    let folder = e.records().join(alex["folder"].as_str().unwrap());
    let mut idx: Value =
        serde_json::from_slice(&std::fs::read(folder.join("index.json")).unwrap()).unwrap();
    let mut earlier = idx["records"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == "r_magnesiumaaaaaaaaaaa")
        .unwrap()
        .clone();
    earlier["id"] = json!("r_magnesiumearlieraaaa");
    earlier["key"] = json!("result|magnesium|2024-09-10");
    earlier["date"] = json!("2024-09-10T15:10:00Z");
    idx["records"].as_array_mut().unwrap().push(earlier);
    std::fs::write(folder.join("index.json"), serde_json::to_vec(&idx).unwrap()).unwrap();
    let series = |v: &Value| -> Option<Value> {
        v["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["title"] == "Magnesium")
            .cloned()
    };
    let v = ask(RECORDS, json!({"kind": "tests"}));
    assert_eq!(series(&v).unwrap()["results"], 2, "{v}");
    let held = count(&v);
    std::fs::write(
        folder.join("marks.json"),
        serde_json::to_vec(&json!({
            "schema": "medrec-marks/v1", "person": alex["id"], "payments": [],
            "private": [{"id": "o_magnesiummarkaaaaaaa", "item": "r_magnesiumearlieraaaa",
                         "private": true, "at": "2026-11-20T10:00:00Z"}]}))
        .unwrap(),
    )
    .unwrap();
    let v = ask(RECORDS, json!({"kind": "tests"}));
    assert!(series(&v).is_none(), "{v}");
    assert_eq!(count(&v), held + 1, "{v}");
    // About Alex, the series is there, marked with its private result's
    // area.
    let v = ask(RECORDS, json!({"kind": "tests", "person": "Alex"}));
    let s = series(&v).expect("Alex's own answer holds the series");
    assert_eq!(
        (&s["results"], &s["private"]),
        (&json!(2), &json!("household")),
        "{v}"
    );
    // Naming the test includes it too.
    let v = ask(RECORDS, json!({"kind": "tests", "name": "magnesium"}));
    assert!(series(&v).is_some(), "{v}");
    std::fs::remove_file(folder.join("marks.json")).unwrap();

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
    assert_eq!(item(&v, smoking)["private"], "substance_use");
    assert!(item(&v, "r_migraineaaaaaaaaaaaa").get("private").is_none());

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
        ids(&v).contains(&"r_inhaleraaaaaaaaaaaaa".to_string()),
        "{v}"
    );
    assert!(
        !ids(&v).contains(&"r_sumatriptanaaaaaaaaa".to_string()),
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
            .all(|i| i["source"] != "Sample Headache Clinic"),
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

/// The folder with Alex's records those of tests/fixtures/standing, at its
/// places.
fn standing_household(e: &Env) {
    fixture_household(e, "standing", &["index.json"]);
}

/// The folder with these files of a fixture as Alex's, at the places of
/// tests/fixtures/standing. Alex's folder.
fn fixture_household(e: &Env, fixture: &str, files: &[&str]) -> PathBuf {
    household(e);
    let read = |p: &Path| -> Value { serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap() };
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let path = e.records().join("household.json");
    let mut h = read(&path);
    let alex = h["people"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "Alex Testperson")
        .unwrap()
        .clone();
    for s in read(&fixtures.join("standing/household.json"))["sources"]
        .as_array()
        .unwrap()
    {
        let mut s = s.clone();
        s["person"] = alex["id"].clone();
        h["sources"].as_array_mut().unwrap().push(s);
    }
    std::fs::write(&path, serde_json::to_vec(&h).unwrap()).unwrap();
    let dir = e.records().join(alex["folder"].as_str().unwrap());
    for f in files {
        let mut v = read(&fixtures.join(fixture).join(f));
        v["person"] = alex["id"].clone();
        std::fs::write(dir.join(f), serde_json::to_vec(&v).unwrap()).unwrap();
    }
    dir
}

fn h_id(e: &Env, name: &str) -> String {
    let h = std::fs::read(e.records().join("household.json")).unwrap();
    let h: Value = serde_json::from_slice(&h).unwrap();
    let mut people = h["people"].as_array().unwrap().iter();
    let found = people.find(|p| p["name"] == name).unwrap();
    found["id"].as_str().unwrap().to_string()
}

const ERRORS: &[&str] = &["index.json", "marks.json", "overrides.json"];
const SKIN_VISIT: &str = "r_skinvisitaaaaaaaaaaa";
const SKIN_BILL: &str = "d_skinbillaaaaaaaaaaaa";
const EYE_BILL: &str = "d_eyebillaaaaaaaaaaaaa";
const LETTER: &str = "d_letteraaaaaaaaaaaaaa";
const EAST: &str = "Sample Clinic East: entered-in-error (read 2024-07-03)";
const NORTH: &str = "Example Health: current (read 2024-07-02)";

/// A page without what is entered in error says how many it leaves out, of
/// those the answer could show; each is there when asked for by its id.
#[test]
fn what_is_entered_in_error_is_counted_and_opened_by_its_id() {
    let e = Env::new("errors");
    let dir = fixture_household(&e, "errors", ERRORS);
    let t = e.tools();
    let ask = |tool: &str, args: Value| {
        let r = call(&t, tool, args);
        assert!(!r.is_error(), "{tool}: {}", r.text());
        assert_fits(&t, tool, &r).clone()
    };
    let ids = |v: &Value| -> Vec<String> {
        let items = v["items"].as_array().unwrap().iter();
        items
            .map(|i| i["id"].as_str().unwrap().to_string())
            .collect()
    };
    let warned = |v: &Value, code: &str| -> Option<String> {
        let w = v["warnings"].as_array().into_iter().flatten();
        let found = w.into_iter().find(|w| w["code"] == code)?;
        Some(found["message"].as_str().unwrap().to_string())
    };
    let item = |v: &Value, id: &str| -> Value {
        let mut items = v["items"].as_array().unwrap().iter();
        items.find(|i| i["id"] == id).unwrap().clone()
    };
    let hidden = [SKIN_BILL, LETTER, "d_footbillaaaaaaaaaaaa"];

    // Of the household, a private one is not counted; of the person, it is.
    for (person, errors, bills, events) in [
        (
            None,
            "2 entered in error, not shown",
            "1 bill entered in error, with a payment you marked",
            "3 entered in error, not shown",
        ),
        (
            Some("Alex"),
            "3 entered in error, not shown",
            "2 bills entered in error, 1 with a payment you marked",
            "4 entered in error, not shown",
        ),
    ] {
        let of = |kind: &str| {
            let mut args = json!({"kind": kind});
            if let Some(p) = person {
                args["person"] = p.into();
            }
            ask(RECORDS, args)
        };
        for (kind, line) in [("documents", errors), ("bills", bills), ("events", events)] {
            let v = of(kind);
            assert!(!ids(&v).iter().any(|i| hidden.contains(&i.as_str())), "{v}");
            assert_eq!(
                warned(&v, "entered-in-error").as_deref(),
                Some(line),
                "{kind} {person:?}"
            );
            let private = warned(&v, "private-left-out").is_some();
            assert_eq!(private, person.is_none(), "{kind}");
        }
        let v = of("bills");
        let bill = item(&v, EYE_BILL);
        assert_eq!(bill["standing"], "sources differ");
        assert_eq!(bill["says"], json!(format!("{NORTH}; {EAST}")));
        // The visit keeps its event, which says what it lost.
        let v = of("events");
        let visits = v["items"].as_array().unwrap().iter();
        let visits: Vec<&Value> = visits
            .filter(|i| i["person"] == "Alex Testperson")
            .collect();
        assert_eq!(visits.len(), 1, "{v}");
        assert_eq!(visits[0]["you_owe"], "40.00 USD");
        let detail = visits[0]["detail"].as_str().unwrap();
        assert!(
            detail.ends_with("2 entered in error, not shown"),
            "{detail}"
        );
    }
    let v = ask(RECORDS, json!({"kind": "results", "person": "Alex"}));
    let differs = item(&v, "r_leveldiffersaaaaaaaa");
    assert_eq!(differs["standing"], "sources differ");
    assert_eq!(differs["status"], "final");
    assert_eq!(
        warned(&v, "entered-in-error").unwrap(),
        "1 entered in error, not shown"
    );
    let v = ask(SEARCH, json!({"query": "example letter"}));
    assert_eq!(ids(&v), Vec::<String>::new());
    assert_eq!(
        warned(&v, "entered-in-error").unwrap(),
        "1 entered in error, not shown"
    );

    // The summary of the person says the same of each section.
    let v = ask(SUMMARY, json!({"person": "Alex"}));
    let note = |section: &str| {
        let sections = v["summary"]["sections"].as_array().unwrap();
        let of = sections.iter().find(|s| s["section"] == section);
        of.unwrap()["note"].as_str().unwrap_or_default().to_string()
    };
    for section in ["immunizations", "latest_results", "recent_visits"] {
        assert_eq!(note(section), "1 entered in error, not shown", "{section}");
    }
    assert_eq!(
        note("bills"),
        "2 bills entered in error, 1 with a payment you marked"
    );

    // Opened by its id, one says what each source says and what is kept.
    let hint = |id: &str| {
        let v = ask(DOCUMENT, json!({"id": id}));
        v["hint"].as_str().unwrap().to_string()
    };
    let out = format!("Entered in error ({EAST}): it is in no list, event, bill or total.");
    assert!(hint(SKIN_BILL).starts_with(&format!(
        "{out} Kept and not applied: 1 payment on it. No text"
    )));
    assert!(hint(LETTER).starts_with(&format!(
        "{out} Kept and not applied: 1 decision about its event, 1 correction of a provider it \
         names. No text"
    )));
    assert!(hint("d_footbillaaaaaaaaaaaa").starts_with(&format!("{out} No text")));
    let differ = format!("Its sources differ ({NORTH}; {EAST}). No text");
    assert!(hint(EYE_BILL).starts_with(&differ), "{}", hint(EYE_BILL));
    let events = e.ok(&["events", "list", "--person", "Alex"]);
    let visit = events["items"][0]["id"].as_str().unwrap().to_string();
    assert_eq!(events["items"].as_array().unwrap().len(), 1);
    let v = ask(RECORDS, json!({"kind": "events", "id": visit}));
    let fields = &v["items"][0]["fields"];
    assert_eq!(fields["in_error"], json!([LETTER, SKIN_BILL]));
    assert_eq!(fields["not_applied"][0]["id"], "o_togetheraaaaaaaaaaaa");
    let views: Value = {
        e.ok(&["views", "write"]);
        let pid = h_id(&e, "Alex Testperson");
        let dir = e.records().join("views").join(pid);
        let file = std::fs::read_dir(dir).unwrap().find_map(|f| {
            let f = f.unwrap().path();
            let name = f.file_name()?.to_string_lossy().into_owned();
            name.starts_with("events.").then_some(f)
        });
        serde_json::from_slice(&std::fs::read(file.unwrap()).unwrap()).unwrap()
    };
    let lost = views["events_in_error"].as_array().unwrap();
    assert_eq!(lost.len(), 4);
    for l in lost {
        let v = ask(RECORDS, json!({"kind": "events", "id": l["id"]}));
        assert_eq!(v["items"][0]["standing"], "entered in error");
        assert_eq!(warned(&v, "entered-in-error"), None);
    }

    // Back as it was once its source no longer says so.
    let path = dir.join("index.json");
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, text.replace("entered-in-error", "current")).unwrap();
    for kind in ["documents", "bills", "events", "results"] {
        let v = ask(RECORDS, json!({"kind": kind, "person": "Alex"}));
        assert_eq!(warned(&v, "entered-in-error"), None, "{kind}");
    }
    let v = ask(RECORDS, json!({"kind": "bills", "person": "Alex"}));
    let detail = item(&v, SKIN_BILL)["detail"].to_string();
    assert!(detail.contains("25.00 left"), "{detail}");
    let v = ask(RECORDS, json!({"kind": "events", "id": visit}));
    let members = v["items"][0]["fields"]["members"].to_string();
    assert!(members.contains(LETTER), "{members}");
}

/// The commands say the same, and what the household decided of what is in
/// error is kept, not applied and said so.
#[test]
fn the_commands_count_what_is_entered_in_error_and_keep_what_was_decided() {
    let e = Env::new("errors-cmd");
    let dir = fixture_household(&e, "errors", ERRORS);
    let notes = |v: &Value| -> Vec<String> {
        let n = v["notes"].as_array().into_iter().flatten();
        n.map(|n| n.as_str().unwrap().to_string()).collect()
    };
    for (args, line) in [
        (
            vec!["documents", "list", "--person", "Alex"],
            "3 entered in error, not shown",
        ),
        (
            vec!["events", "list", "--person", "Alex"],
            "4 entered in error, not shown",
        ),
        (
            vec!["bills", "list", "--person", "Alex"],
            "2 bills entered in error, 1 with a payment you marked",
        ),
        (
            vec!["private", "list", "--person", "Alex"],
            "2 entered in error, not shown",
        ),
        (
            vec!["bills", "list"],
            "Alex Testperson: 2 bills entered in error, 1 with a payment you marked",
        ),
    ] {
        let v = e.ok(&args);
        assert_eq!(notes(&v), [line], "{args:?}");
        for i in v["items"].as_array().unwrap() {
            let of = ["id", "document", "item"].map(|k| i[k].as_str().unwrap_or_default());
            assert!(!of.contains(&SKIN_BILL) && !of.contains(&LETTER), "{i}");
        }
        let out = e.run(&args);
        let said = String::from_utf8_lossy(&out.stderr);
        assert!(said.contains(line), "{args:?}: {said}");
    }
    let v = e.ok(&["bills", "list", "--person", "Alex"]);
    let calls = v["worth_a_call"].as_array().unwrap();
    assert!(calls.iter().all(|c| c["bill"] != SKIN_BILL), "{v}");
    let v = e.ok(&["documents", "list", "--person", "Alex"]);
    assert_eq!(v["items"][0]["standing"], "sources differ");
    assert_eq!(v["items"][0]["says"], json!([NORTH, EAST]));

    let kept = |args: &[&str], id: &str| {
        let v = e.ok(args);
        let mut items = v["items"].as_array().unwrap().iter();
        items.find(|i| i["id"] == id).unwrap()["applied"] == false
    };
    for (args, id) in [
        (
            ["bills", "payments", "--person", "Alex"],
            "o_paidaaaaaaaaaaaaaaaa",
        ),
        (
            ["events", "decisions", "--person", "Alex"],
            "o_togetheraaaaaaaaaaaa",
        ),
        (
            ["labels", "list", "--person", "Alex"],
            "o_deskaaaaaaaaaaaaaaaa",
        ),
    ] {
        assert!(kept(&args, id), "{args:?}");
    }
    // So is what it decides now.
    let paid = ["bills", "paid", SKIN_BILL, "--on", "2024-05-07"];
    let v = e.ok(&[&paid[..], &["--amount", "5.00"]].concat());
    assert_eq!(v["applied"], false);
    let v = e.ok(&["events", "apart", SKIN_VISIT, SKIN_BILL]);
    assert_eq!(v["applied"], false);
    let v = e.ok(&[
        "bills",
        "paid",
        EYE_BILL,
        "--on",
        "2024-07-09",
        "--amount",
        "5.00",
    ]);
    assert!(v.get("applied").is_none());

    let path = dir.join("index.json");
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, text.replace("entered-in-error", "current")).unwrap();
    assert!(!kept(
        &["bills", "payments", "--person", "Alex"],
        "o_paidaaaaaaaaaaaaaaaa"
    ));
    assert!(!kept(
        &["events", "decisions", "--person", "Alex"],
        "o_togetheraaaaaaaaaaaa"
    ));
    let v = e.ok(&["bills", "list", "--person", "Alex"]);
    assert_eq!(notes(&v), Vec::<String>::new());
    let bills = v["items"].as_array().unwrap();
    let skin = bills.iter().find(|b| b["document"] == SKIN_BILL).unwrap();
    assert_eq!(skin["paid_total"], "20.00");
}

#[test]
fn what_is_current_is_by_every_sources_word() {
    let e = Env::new("standing");
    standing_household(&e);
    let t = e.tools();
    let ask = |tool: &str, args: Value| {
        let r = call(&t, tool, args);
        assert!(!r.is_error(), "{tool}: {}", r.text());
        assert_fits(&t, tool, &r).clone()
    };
    let ids = |v: &Value| -> Vec<String> {
        let items = v["items"].as_array().unwrap().iter();
        items
            .map(|i| i["id"].as_str().unwrap().to_string())
            .collect()
    };
    let item = |v: &Value, id: &str| -> Value {
        let mut items = v["items"].as_array().unwrap().iter();
        let found = items.find(|i| i["id"].as_str().unwrap().starts_with(&format!("r_{id}aa")));
        found.unwrap_or_else(|| panic!("no {id}: {v}")).clone()
    };
    let warned = |v: &Value, code: &str| -> Vec<String> {
        let all = v["warnings"].as_array().into_iter().flatten();
        all.filter(|w| w["code"] == code)
            .map(|w| w["message"].as_str().unwrap().to_string())
            .collect()
    };
    // The reader's lists of the fixture, as it has them after every date.
    let cases: Value = serde_json::from_str(include_str!("fixtures/standing/lists.json")).unwrap();
    let want = |kind: &str| -> Vec<String> {
        let cases = cases["cases"].as_array().unwrap();
        let whole = cases.iter().filter(|c| c["hidden"] == json!([]));
        let c = whole
            .filter(|c| c["kind"] == kind)
            .max_by_key(|c| c["day"].as_str().unwrap())
            .unwrap();
        ["current", "on_hold", "ended_by_date"]
            .iter()
            .flat_map(|g| c["lists"][g].as_array().into_iter().flatten())
            .map(|l| l["records"][0].as_str().unwrap().to_string())
            .collect()
    };
    let in_error = |v: &Value| -> usize {
        let said = warned(v, "entered-in-error");
        let n = |m: &String| m.split(' ').next().unwrap().parse::<usize>().unwrap();
        said.iter().map(n).sum()
    };
    for (kind, of, errors) in [
        ("conditions", "condition", 1),
        ("allergies", "allergy", 1),
        ("medications", "medication", 2),
    ] {
        let args = json!({"kind": kind, "person": "Alex", "active": true, "limit": 50});
        let v = ask(RECORDS, args);
        assert_eq!((ids(&v), in_error(&v)), (want(of), errors), "{kind}");
        // Page by page, the same, and each left out is counted once.
        let (mut paged, mut counted) = (Vec::new(), 0);
        let mut args = json!({"kind": kind, "person": "Alex", "active": true, "limit": 2});
        loop {
            let v = ask(RECORDS, args.clone());
            paged.extend(ids(&v));
            counted += in_error(&v);
            match v["next_cursor"].as_str() {
                Some(c) => args["cursor"] = c.into(),
                None => break,
            }
        }
        assert_eq!((paged, counted), (want(of), errors), "{kind}");
    }

    let v = ask(
        RECORDS,
        json!({"kind": "conditions", "person": "Alex", "active": true, "limit": 50}),
    );
    let one = item(&v, "bothsay");
    assert_eq!(one["standing"], "current, sources differ");
    assert_eq!(
        one["says"],
        "Example Health: active (read 2024-05-02); Sample Clinic East: resolved (read 2024-05-03)"
    );
    assert_eq!(
        item(&v, "tworecords")["standing"],
        "current, sources differ"
    );
    assert_eq!(
        item(&v, "again")["says"],
        "Example Health: active (read 2024-05-02)"
    );
    assert_eq!(
        item(&v, "provisional")["says"],
        "Example Health: active, provisional (read 2024-05-02)"
    );
    assert_eq!(
        warned(&v, "no-problem-list"),
        ["No problem list from Example Walk-in; diagnoses from visits are listed under All"]
    );
    let v = ask(
        RECORDS,
        json!({"kind": "allergies", "person": "Alex", "active": true}),
    );
    let one = item(&v, "oneheard");
    assert_eq!(one["standing"], "current, not every source heard");
    assert_eq!(
        one["says"],
        "Sample Clinic East: inactive (read 2024-05-03); Example Health: not heard"
    );
    assert_eq!(
        item(&v, "nut")["says"],
        "Example Health: active (read 2024-05-02); Sample Clinic East: active (read 2024-05-03)"
    );

    // All of them, each with what it is.
    let v = ask(
        RECORDS,
        json!({"kind": "conditions", "person": "Alex", "limit": 50}),
    );
    assert!(warned(&v, "no-problem-list").is_empty());
    assert_eq!(item(&v, "visitonly")["standing"], "diagnosis from a visit");
    assert_eq!(item(&v, "refuted")["standing"], "refuted");
    assert_eq!(
        item(&v, "abated")["says"],
        "Example Health: no status, ended 2022-05 (read 2024-05-02)"
    );
    let v = ask(
        RECORDS,
        json!({"kind": "medications", "person": "Alex", "limit": 50}),
    );
    assert_eq!(
        item(&v, "ends")["standing"],
        "ended by its date 2024-03-31 (prescription), filled 2024-05-02"
    );
    assert_eq!(item(&v, "onhold")["standing"], "on hold");
    assert_eq!(item(&v, "draft")["standing"], "draft");
    assert_eq!(item(&v, "nottaken")["standing"], "ended");
    assert!(item(&v, "fillafter").get("standing").is_none());
    assert!(!ids(&v).iter().any(|i| i.starts_with("r_error")));

    // What is entered in error, of any kind, is out of the timeline and the
    // search too.
    let args = json!({"kind": "timeline", "person": "Alex", "limit": 100});
    let v = ask(RECORDS, args);
    assert!(!ids(&v).iter().any(|i| i == "r_erroraaaaaaaaaaaaaaa"));
    assert_eq!(in_error(&v), 5);
    let v = ask(SEARCH, json!({"query": "sample burn", "person": "Alex"}));
    assert_eq!((ids(&v).len(), in_error(&v)), (0, 1), "{v}");
    let v = ask(RECORDS, json!({"kind": "results", "person": "Alex"}));
    assert_eq!((ids(&v).len(), in_error(&v)), (0, 1), "{v}");
    let v = ask(RECORDS, json!({"kind": "tests", "person": "Alex"}));
    assert_eq!((ids(&v).len(), in_error(&v)), (0, 0), "{v}");
    let v = ask(
        RECORDS,
        json!({"kind": "medications", "person": "Alex", "limit": 50}),
    );
    assert_eq!(in_error(&v), 2);
    assert!(!ids(&v).iter().any(|i| i == "r_fillerroraaaaaaaaaaa"));

    // The summary has the same lists.
    let v = ask(SUMMARY, json!({"person": "Alex"}));
    let section = |name: &str| -> Value {
        let mut all = v["summary"]["sections"].as_array().unwrap().iter();
        all.find(|s| s["section"] == name)
            .unwrap_or_else(|| panic!("no {name}"))
            .clone()
    };
    let names = |s: &Value| -> Vec<String> {
        let items = s["items"].as_array().unwrap().iter();
        items
            .map(|i| i["name"].as_str().unwrap().to_string())
            .collect()
    };
    let s = section("conditions");
    assert_eq!(
        s["items"].as_array().unwrap().len(),
        want("condition").len()
    );
    assert_eq!(
        s["note"],
        "No problem list from Example Walk-in; diagnoses from visits are listed under All. 1 entered in error, not shown"
    );
    assert_eq!(s["items"][1]["standing"], "current, sources differ");
    assert!(s["items"][1].get("detail").is_none());
    assert_eq!(names(&section("medications_on_hold")), ["Sample patch"]);
    assert_eq!(
        names(&section("medications_ended_by_date")),
        ["Sample capsule", "Sample drops", "Sample cream"]
    );

    // The view's, of which a reader makes the same lists on its own day.
    e.ok(&["views", "write"]);
    let view = |name: &str| -> Value {
        let path = find(&e.records().join("views"), &format!("{name}.json"))
            .unwrap_or_else(|| panic!("no {name}"));
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
    };
    let pid = h_id(&e, "Alex Testperson");
    let dir = e.records().join("views").join(&pid);
    let stored = |name: &str| -> Value {
        let index = view("views");
        let md5 = index["people"][&pid]["files"][name]
            .as_str()
            .unwrap()
            .to_string();
        let path = dir.join(medrec::viewfiles::stored_name(name, &md5));
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
    };
    let (summary, st) = (stored("summary.json"), stored("standing.json"));
    assert_eq!(view("views")["people"][&pid]["in_error"], st["in_error"]);
    let st: medrec::standing::Standings = serde_json::from_value(st).unwrap();
    for (name, of) in [
        ("conditions", "condition"),
        ("allergies", "allergy"),
        ("medications", "medication"),
    ] {
        let lists = st.lists(of, &|_| true, &medrec::model::today());
        let lines = lists
            .current
            .iter()
            .chain(&lists.on_hold)
            .chain(&lists.ended_by_date);
        let firsts: Vec<String> = lines.map(|l| l.records[0].clone()).collect();
        let held = summary["standing"][name].as_array().unwrap();
        assert!(firsts.iter().all(|id| held.iter().any(|i| i["id"] == **id)));
        // The older lists are as they were.
        assert!(summary[name]["items"].as_array().unwrap().len() <= 25);
        assert_eq!(firsts, want(of), "{name}");
    }

    // A record the answer may not show makes no line differ, and is in no
    // line's words.
    let v = ask(
        RECORDS,
        json!({"kind": "conditions", "active": true, "limit": 50}),
    );
    assert_eq!(
        item(&v, "tworecords")["standing"],
        "current, sources differ"
    );
    let told = warned(&v, "no-problem-list");
    assert_eq!(told.len(), 1, "{v}");
    assert!(
        told[0].starts_with("Alex Testperson: No problem list from"),
        "{told:?}"
    );
    e.ok(&["private", "mark", "r_tworecordsotheraaaaa"]);
    e.ok(&["private", "mark", "r_fillafteraaaaaaaaaaa"]);
    let v = ask(
        RECORDS,
        json!({"kind": "conditions", "active": true, "limit": 50}),
    );
    assert_eq!(item(&v, "tworecords")["standing"], "current");
    assert_eq!(
        item(&v, "tworecords")["says"],
        "Example Health: active (read 2024-05-02)"
    );
    let v = ask(
        RECORDS,
        json!({"kind": "medications", "active": true, "limit": 50}),
    );
    // The last fill it may show.
    assert_eq!(
        item(&v, "ends")["standing"],
        "ended by its date 2024-03-31 (prescription), filled 2024-04-10"
    );
    let v = ask(
        RECORDS,
        json!({"kind": "conditions", "person": "Alex", "active": true, "limit": 50}),
    );
    assert_eq!(
        item(&v, "tworecords")["standing"],
        "current, sources differ"
    );
    // A question that names it lists it, and it counts on the line; one whose
    // dates leave it out does not.
    let named = json!({"kind": "conditions", "name": "rash", "active": true});
    let v = ask(RECORDS, named);
    assert_eq!(
        item(&v, "tworecords")["standing"],
        "current, sources differ"
    );
    let args = json!({"kind": "conditions", "name": "rash", "active": true, "since": "2024-04-18"});
    let v = ask(RECORDS, args);
    assert_eq!(item(&v, "tworecords")["standing"], "current");
    // One entered in error that the answer may not show is counted as private.
    let v = ask(RECORDS, json!({"kind": "results"}));
    let before = in_error(&v);
    e.ok(&["private", "mark", "r_resulterroraaaaaaaaa"]);
    let v = ask(RECORDS, json!({"kind": "results"}));
    assert_eq!((before, in_error(&v)), (1, 0));
    assert_eq!(warned(&v, "private-left-out").len(), 1, "{v}");
}

/// The commands list the same.
#[test]
fn the_record_commands_list_what_is_current_by_every_sources_word() {
    let e = Env::new("standing-commands");
    standing_household(&e);
    let v = e.ok(&["conditions", "--person", "Alex", "--active"]);
    let items = v["items"].as_array().unwrap();
    assert_eq!(items.len(), 11);
    assert_eq!(items[1]["standing"], "current, sources differ");
    assert_eq!(items[1]["says"].as_array().unwrap().len(), 2);
    assert_eq!(
        v["notes"],
        json!([
            "No problem list from Example Walk-in; diagnoses from visits are listed under All",
            "1 entered in error, not shown"
        ])
    );
    let v = e.ok(&["meds", "--person", "Alex", "--active"]);
    let standing: Vec<&str> = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["standing"].as_str().unwrap())
        .collect();
    assert_eq!(standing.len(), 11);
    assert!(standing[..7].iter().all(|s| s.starts_with("current")));
    assert_eq!(standing[7], "on hold");
    assert!(standing[8..]
        .iter()
        .all(|s| s.starts_with("ended by its date")));
    let v = e.ok(&["meds", "--person", "Alex"]);
    assert_eq!(v["items"].as_array().unwrap().len(), 17);
    assert_eq!(v["notes"], json!(["2 entered in error, not shown"]));
    let v = e.ok(&["results", "--person", "Alex"]);
    assert_eq!(v["items"], json!([]));
    assert_eq!(v["notes"], json!(["1 entered in error, not shown"]));
    // At the terminal, each source's word; the rows alone are the output,
    // and what it leaves out is said beside them.
    let text = |args: &[&str]| {
        let mut cmd = e.command();
        let out = cmd.args(args).output().unwrap();
        (
            String::from_utf8(out.stdout).unwrap(),
            String::from_utf8(out.stderr).unwrap(),
        )
    };
    let (shown, said) = text(&["conditions", "--person", "Alex", "--active"]);
    for words in [
        "current, sources differ",
        "Example Health: active (read 2024-05-02); Sample Clinic East: resolved (read 2024-05-03)",
    ] {
        assert!(shown.contains(words), "{words}: {shown}");
    }
    for (args, words) in [
        (
            &["conditions", "--person", "Alex", "--active"][..],
            "1 entered in error, not shown",
        ),
        (
            &["timeline", "--person", "Alex"],
            "5 entered in error, not shown",
        ),
        (
            &["search", "sample burn", "--person", "Alex"],
            "1 entered in error, not shown",
        ),
    ] {
        let (shown, said) = text(args);
        assert!(said.contains(words), "{words}: {said}");
        assert!(!shown.contains("not shown"), "{shown}");
    }
    assert!(said.contains("No problem list from"), "{said}");
    // `--quiet` leaves the rows.
    let (_, said) = text(&["conditions", "--person", "Alex", "--active", "--quiet"]);
    assert_eq!(said, "");
    let v = e.ok(&["timeline", "--person", "Alex"]);
    let ids: Vec<&str> = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap())
        .collect();
    assert!(!ids.contains(&"r_erroraaaaaaaaaaaaaaa"));
    assert!(ids.contains(&"r_errorbesideaaaaaaaaa"));
    assert_eq!(v["notes"], json!(["5 entered in error, not shown"]));
    let v = e.ok(&["search", "sample burn", "--person", "Alex"]);
    assert_eq!(v["items"], json!([]));
    assert_eq!(v["notes"], json!(["1 entered in error, not shown"]));
    assert!(e
        .ok(&["timeline", "--person", "Robin"])
        .get("notes")
        .is_none());
}

/// Puts `code-names.json` in a person's views, as a medrec with the tables
/// writes it.
fn put_code_names(e: &Env, pid: &str, v: &Value) {
    put_view(e, pid, "code-names.json", v);
}

/// Puts one of a person's views there.
fn put_view(e: &Env, pid: &str, name: &str, v: &Value) {
    let bytes = serde_json::to_vec(v).unwrap();
    let md5 = medrec::hash::md5_hex(&bytes);
    let stored = medrec::viewfiles::stored_name(name, &md5);
    let at = e.records().join("views").join(pid).join(stored);
    std::fs::write(at, &bytes).unwrap();
    let path = e.records().join("views/views.json");
    let mut index: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    index["people"][pid]["files"][name] = json!(md5);
    std::fs::write(path, serde_json::to_vec(&index).unwrap()).unwrap();
}

/// A label decides what is private: while a person's do not read, an answer
/// about the whole household holds all of theirs back, and says why.
#[test]
fn labels_that_do_not_read_hold_a_person_back() {
    let e = Env::new("labelslost");
    household(&e);
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
    let folder = e.records().join(alex["folder"].as_str().unwrap());
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/labels");
    let mut idx: Value =
        serde_json::from_slice(&std::fs::read(fixtures.join("index.json")).unwrap()).unwrap();
    idx["person"] = alex["id"].clone();
    std::fs::write(folder.join("index.json"), serde_json::to_vec(&idx).unwrap()).unwrap();
    e.ok(&["views", "write"]);
    let t = e.tools();
    let ask = |args: Value| {
        let r = call(&t, RECORDS, args);
        assert!(!r.is_error(), "{}", r.text());
        assert_fits(&t, RECORDS, &r).clone()
    };
    let shown = |v: &Value, id: &str| v["items"].as_array().unwrap().iter().any(|i| i["id"] == id);
    let claims = json!({"kind": "claims", "limit": 50});

    // The label makes its claim private.
    let talk = "r_talkaaaaaaaaaaaaaaaa";
    assert!(shown(&ask(claims.clone()), talk));
    let mut state: Value =
        serde_json::from_slice(&std::fs::read(fixtures.join("provider-state.json")).unwrap())
            .unwrap();
    state["person"] = alex["id"].clone();
    let at = folder.join("provider-state.json");
    std::fs::write(&at, serde_json::to_vec(&state).unwrap()).unwrap();
    let v = ask(claims.clone());
    assert!(!shown(&v, talk), "{v}");
    assert!(shown(&v, "r_officevisitaaaaaaaaa"), "{v}");

    std::fs::write(&at, "{").unwrap();
    let v = ask(claims);
    for r in ["r_talkaaaaaaaaaaaaaaaa", "r_officevisitaaaaaaaaa"] {
        assert!(!shown(&v, r), "{v}");
    }
    let codes: Vec<&str> = v["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|w| w["code"].as_str())
        .collect();
    assert!(codes.contains(&"private-labels-unreadable"), "{v}");
    assert!(!codes.contains(&"private-marks-unreadable"), "{v}");
    // About Alex, everything is there.
    let v = ask(json!({"kind": "claims", "person": "Alex", "limit": 50}));
    assert!(shown(&v, talk), "{v}");
}

/// A label stands beside what an item prints, in a list of its own: nothing a
/// source says is changed, and nothing is found by it.
#[test]
fn a_printed_provider_has_its_label_beside_it() {
    let e = Env::new("providerlabels");
    household(&e);
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
    let folder = e.records().join(alex["folder"].as_str().unwrap());
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/labels");
    for name in ["index.json", "text.json", "provider-state.json"] {
        let mut v: Value =
            serde_json::from_slice(&std::fs::read(fixtures.join(name)).unwrap()).unwrap();
        v["person"] = alex["id"].clone();
        std::fs::write(folder.join(name), serde_json::to_vec(&v).unwrap()).unwrap();
    }
    e.ok(&["views", "write"]);
    let t = e.tools();
    let ask = |tool: &str, args: Value| {
        let r = call(&t, tool, args);
        assert!(!r.is_error(), "{tool}: {}", r.text());
        assert_fits(&t, tool, &r).clone()
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
    let of = |v: &Value, id: &str| item(v, id)["provider_labels"].clone();
    let (sample, portal, talk) = (
        "r_samplevisitaaaaaaaaa",
        "r_sampleportalaaaaaaaa",
        "r_talkaaaaaaaaaaaaaaaa",
    );
    let directory = json!({"from": "directory", "date": "2026-03-08"});

    // A claim's provider: the claim's row has its title, so not the name.
    let claims = ask(RECORDS, json!({"kind": "claims"}));
    assert_eq!(
        of(&claims, sample),
        json!([{"field": "provider", "name": "Jordan Sample, MD", "role": "family doctor",
            "phone": "555-010-0101", "phone_from": directory, "confidence": "sure",
            "evidence": [{"from": "npi"}]}])
    );
    assert_eq!(item(&claims, sample)["title"], "Sample service");
    // A likely one says why; one with none has no list.
    assert_eq!(
        of(&claims, "r_onepaperaaaaaaaaaaaa")[0]["evidence"],
        json!([{"from": "only_match", "state": "FL"},
            {"from": "paper", "document": "d_billaaaaaaaaaaaaaaaa"}])
    );
    assert_eq!(of(&claims, "r_twomatchesaaaaaaaaaa"), Value::Null);

    // A private item's is in an answer about the person, and in none about the
    // household, whatever it names.
    let about = ask(RECORDS, json!({"kind": "claims", "person": "Alex"}));
    assert_eq!(of(&about, talk)[0]["role"], "psychologist");
    assert_eq!(item(&about, talk)["private"], "mental_health");
    let named = ask(RECORDS, json!({"kind": "claims", "name": "Sample service"}));
    assert_eq!(item(&named, talk)["private"], "mental_health");
    assert_eq!(of(&named, talk), Value::Null);
    assert_eq!(of(&named, sample), of(&claims, sample));

    // Every labelled field in full, as the record prints it.
    let visits = json!({"kind": "visits", "person": "Alex"});
    assert_eq!(of(&ask(RECORDS, visits.clone()), portal), Value::Null);
    let mut full = visits;
    full["response_format"] = json!("detailed");
    let full = ask(RECORDS, full);
    let fields: Vec<(Value, Value)> = of(&full, portal)
        .as_array()
        .unwrap()
        .iter()
        .map(|l| (l["field"].clone(), l["printed"].clone()))
        .collect();
    assert_eq!(
        fields,
        [
            (json!("practitioner"), json!("Jordan Sample, MD")),
            (json!("service_provider"), json!("Example Primary Care"))
        ]
    );

    // What the household says of a paper's organization is beside it, on the
    // paper, its bill, in the timeline and where it is read.
    let org = "Example Family Practice";
    e.ok(&[
        "labels",
        "set",
        "--person",
        "Alex",
        "--printed",
        org,
        "--name",
        "Our practice",
        "--phone",
        "555-010-0142",
    ]);
    let stale = ask(RECORDS, json!({"kind": "claims"}));
    assert_eq!(of(&stale, sample), of(&claims, sample), "the views follow");
    let bill = "d_billaaaaaaaaaaaaaaaa";
    for kind in ["documents", "bills", "timeline"] {
        let v = ask(RECORDS, json!({"kind": kind, "limit": 100}));
        let l = &of(&v, bill)[0];
        assert_eq!(
            (&l["field"], &l["printed"], &l["name"], &l["confidence"]),
            (
                &json!("organization"),
                &json!(org),
                &json!("Our practice"),
                &json!("household")
            ),
            "{kind}: {l}"
        );
        assert_eq!(l["phone_from"]["from"], "household");
        assert_eq!(item(&v, bill)["source"], org);
    }
    let v = ask(RECORDS, json!({"kind": "timeline", "limit": 100}));
    assert_eq!(of(&v, sample), of(&claims, sample));
    assert_eq!(of(&v, portal), Value::Null);
    let v = ask(DOCUMENT, json!({"id": bill}));
    assert_eq!(v["provider_labels"][0]["name"], "Our practice");
    assert_eq!(v["organization"], org);

    // An event's place and clinicians, and in full its members'.
    let events = ask(RECORDS, json!({"kind": "events", "limit": 100}));
    let event = |v: &Value, member: &str| -> Value {
        let all = e.ok(&["events", "list", "--person", "Alex"]);
        let id = all["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x["members"].to_string().contains(member))
            .unwrap()["id"]
            .clone();
        item(v, id.as_str().unwrap())
    };
    let place = event(&events, "r_officevisitaaaaaaaaa");
    let l = &place["provider_labels"];
    assert!(
        l.as_array()
            .unwrap()
            .iter()
            .any(|l| l["printed"] == "EXAMPLE PRIMARY CARE GROUP LLC"
                && l["name"] == "Example Primary Care"),
        "{place}"
    );
    let one = ask(
        RECORDS,
        json!({"kind": "events", "id": event(&events, sample)["id"]}),
    );
    let one = &one["items"][0];
    let mut seen = std::collections::BTreeSet::new();
    for l in one["provider_labels"].as_array().unwrap() {
        assert!(
            seen.insert((l["printed"].to_string(), l["name"].to_string())),
            "{one}"
        );
    }
    assert!(!seen.is_empty(), "{one}");

    // Nothing is found by a label, and the summary has none.
    for q in ["Our practice", "psychologist", "Avery"] {
        let v = ask(SEARCH, json!({"query": q}));
        assert!(v["items"].as_array().unwrap().is_empty(), "{v}");
        for kind in ["claims", "documents", "events", "timeline"] {
            let v = ask(RECORDS, json!({"kind": kind, "name": q}));
            assert!(v["items"].as_array().unwrap().is_empty(), "{kind}: {v}");
        }
    }
    let v = ask(SUMMARY, json!({"person": "Alex"}));
    assert!(!v.to_string().contains("provider_labels"), "{v}");

    // Views older than the folder's files label nothing.
    let marks = folder.join("marks.json");
    let mut v: Value = serde_json::from_slice(&std::fs::read(&marks).unwrap()).unwrap();
    v["later"] = json!(true);
    std::fs::write(&marks, serde_json::to_vec(&v).unwrap()).unwrap();
    let v = ask(RECORDS, json!({"kind": "claims"}));
    assert_eq!(of(&v, sample), Value::Null);

    // The labels count in a page's size.
    let mut idx: Value =
        serde_json::from_slice(&std::fs::read(folder.join("index.json")).unwrap()).unwrap();
    let records = idx["records"].as_array_mut().unwrap();
    let claim = records.iter().find(|r| r["id"] == sample).unwrap().clone();
    for n in 0..190 {
        let mut c = claim.clone();
        let tail = |n: usize| (b'a' + n as u8) as char;
        c["id"] = json!(format!(
            "r_many{}{}aaaaaaaaaaaaaa",
            tail(n / 20),
            tail(n % 20)
        ));
        c["key"] = json!(format!("claim|many{n}"));
        records.push(c);
    }
    std::fs::write(folder.join("index.json"), serde_json::to_vec(&idx).unwrap()).unwrap();
    e.ok(&["views", "write"]);
    let mut args = json!({"kind": "claims", "person": "Alex", "limit": 200});
    let mut total = 0;
    loop {
        let v = ask(RECORDS, args.clone());
        for i in v["items"].as_array().unwrap() {
            total += usize::from(i["id"].as_str().unwrap().starts_with("r_many"));
            if i["id"].as_str().unwrap().starts_with("r_many") {
                assert_eq!(i["provider_labels"], of(&claims, sample));
            }
        }
        match v["next_cursor"].as_str() {
            Some(c) => {
                assert_eq!(v["truncated"], true);
                args["cursor"] = json!(c);
            }
            None => break,
        }
    }
    assert_eq!(total, 190);
    let _ = std::fs::remove_dir_all(&e.root);
}

/// An event that fits a page only without its labels is still answered, with
/// those that fit, and says so.
#[test]
fn a_label_yields_to_a_record_it_does_not_fit_beside() {
    let e = Env::new("labelsyield");
    household(&e);
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
    let folder = e.records().join(alex["folder"].as_str().unwrap());
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/labels");
    let sample = "r_samplevisitaaaaaaaaa";
    let t = e.tools();
    // Visits of one day with one clinician, each printed its own way.
    for (visits, all) in [(10, true), (120, false)] {
        for name in ["index.json", "provider-state.json"] {
            let mut v: Value =
                serde_json::from_slice(&std::fs::read(fixtures.join(name)).unwrap()).unwrap();
            v["person"] = alex["id"].clone();
            if name == "index.json" {
                let records = v["records"].as_array_mut().unwrap();
                let claim = records.iter().find(|r| r["id"] == sample).unwrap().clone();
                let mut facility = claim.clone();
                facility["id"] = json!("r_facilityaaaaaaaaaaaa");
                facility["key"] = json!("claim|facility");
                facility["fields"]["type"] = json!("institutional");
                facility["fields"]["provider"] = json!("EXAMPLE SURGERY CENTER");
                facility["providers"] = json!([]);
                records.push(facility);
                for n in 0..visits {
                    let mut c = claim.clone();
                    let at = |n: u8| (b'a' + n) as char;
                    let (a, b) = (at(n / 20), at(n % 20));
                    c["id"] = json!(format!("r_many{a}{b}aaaaaaaaaaaaaa"));
                    c["key"] = json!(format!("claim|many{n}"));
                    c["fields"]["provider"] = json!(format!("J SAMPLE {a}{b}").to_uppercase());
                    records.push(c);
                }
            }
            std::fs::write(folder.join(name), serde_json::to_vec(&v).unwrap()).unwrap();
        }
        e.ok(&["views", "write"]);
        let events = e.ok(&["events", "list", "--person", "Alex"]);
        let one = events["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x["members"].to_string().contains("r_manyaa"))
            .unwrap();
        let check = |v: &Value| {
            let item = v["items"]
                .as_array()
                .unwrap()
                .iter()
                .find(|i| i["id"] == one["id"])
                .unwrap_or_else(|| panic!("no {}", one["id"]));
            let labels = item["provider_labels"].as_array().unwrap().len();
            let names = one["clinicians"].as_array().unwrap().len();
            assert!(names + 5 > usize::from(visits), "{names}");
            assert_eq!(item["fields"]["members"], one["members"]);
            let warned = v["warnings"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|w| w["code"] == "provider-labels-left-out");
            assert_eq!(warned, !all, "{}", v["warnings"]);
            assert_eq!(labels >= names, all, "{labels} of {names}");
            assert!(labels > 0);
        };
        let ask = |args: Value| {
            let r = call(&t, RECORDS, args);
            assert!(!r.is_error(), "{visits}: {}", r.text());
            assert_fits(&t, RECORDS, &r).clone()
        };
        check(&ask(json!({"kind": "events", "id": one["id"]})));
        // And a detailed list reads past it.
        let mut args = json!({"kind": "events", "person": "Alex", "response_format": "detailed"});
        let mut seen = 0;
        loop {
            let v = ask(args.clone());
            let items = v["items"].as_array().unwrap();
            seen += items.len();
            if items.iter().any(|i| i["id"] == one["id"]) {
                check(&v);
            }
            match v["next_cursor"].as_str() {
                Some(c) => args["cursor"] = json!(c),
                None => break,
            }
        }
        assert_eq!(seen, events["items"].as_array().unwrap().len());
    }
    let _ = std::fs::remove_dir_all(&e.root);
}

#[test]
fn a_condition_has_its_plain_name_from_the_folders_views() {
    let e = Env::new("codenames");
    household(&e);
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
    let (pid, folder) = (
        alex["id"].as_str().unwrap().to_string(),
        alex["folder"].as_str().unwrap().to_string(),
    );
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/private");
    for (from, to) in [
        ("adult.index.json", "index.json"),
        ("adult.text.json", "text.json"),
    ] {
        let mut v: Value =
            serde_json::from_str(&std::fs::read_to_string(fixtures.join(from)).unwrap()).unwrap();
        v["person"] = alex["id"].clone();
        std::fs::write(
            e.records().join(&folder).join(to),
            serde_json::to_vec(&v).unwrap(),
        )
        .unwrap();
    }
    e.ok(&["views", "write"]);
    put_code_names(
        &e,
        &pid,
        &json!({"schema": "medrec-code-names/v1", "person": pid,
            "table": {"id": "icd10cm", "version": "FY2027", "date": "2026-06-01"},
            "codes": {"icd10cm|G43.909": {"name": "Sample open name"}},
            "private_codes": {"icd10cm|F17.210": {"name": "Sample private name"}}}),
    );
    let (private, open) = ("r_smokingaaaaaaaaaaaaa", "r_migraineaaaaaaaaaaaa");
    let t = e.tools();
    let ask = |tool: &str, args: Value| {
        let r = call(&t, tool, args);
        assert!(!r.is_error(), "{tool}: {}", r.text());
        assert_fits(&t, tool, &r).clone()
    };
    let name_of = |v: &Value, id: &str| -> Option<String> {
        v["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["id"] == id)
            .unwrap_or_else(|| panic!("no {id}: {v}"))["plain_name"]
            .as_str()
            .map(str::to_string)
    };

    // About one person: both, with the source's own words beside them.
    let v = ask(RECORDS, json!({"kind": "conditions", "person": "Alex"}));
    assert_eq!(name_of(&v, open).as_deref(), Some("Sample open name"));
    assert_eq!(name_of(&v, private).as_deref(), Some("Sample private name"));
    let row = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == open)
        .unwrap();
    assert_eq!(row["title"], "Migraine headache");
    let v = ask(SUMMARY, json!({"person": "Alex"}));
    let lines = v["summary"]["sections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["section"] == "conditions")
        .unwrap()["items"]
        .clone();
    assert!(
        lines
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l["id"] == open && l["plain_name"] == "Sample open name"),
        "{lines}"
    );
    assert!(!v.to_string().contains("name_key"), "{v}");

    // About the household: only the codes an open item uses, and a private
    // item the question names keeps the source's words.
    let v = ask(RECORDS, json!({"kind": "conditions"}));
    assert_eq!(name_of(&v, open).as_deref(), Some("Sample open name"));
    let v = ask(
        RECORDS,
        json!({"kind": "conditions", "name": "nicotine dependence"}),
    );
    assert_eq!(name_of(&v, private), None);

    // A name is not a word a question can find: no row, and no private
    // item opened.
    for q in ["Sample private name", "Sample open name"] {
        let v = ask(RECORDS, json!({"kind": "conditions", "name": q}));
        assert!(v["items"].as_array().unwrap().is_empty(), "{v}");
        let v = ask(SEARCH, json!({"query": q}));
        assert!(v["items"].as_array().unwrap().is_empty(), "{v}");
    }

    // Views older than the folder's files name nothing.
    let marks = e.records().join(&folder).join("marks.json");
    let mark = |item: &str| {
        std::fs::write(
            &marks,
            serde_json::to_vec(
                &json!({"schema": "medrec-marks/v1", "person": pid, "private": [
                {"id": "m_aaaaaaaaaaaaaaaaaaaa", "item": item, "private": true,
                 "at": "2026-11-24T00:00:00Z"}]}),
            )
            .unwrap(),
        )
        .unwrap();
    };
    mark(open);
    let v = ask(RECORDS, json!({"kind": "conditions", "person": "Alex"}));
    assert_eq!(name_of(&v, open), None);

    // A pass on a machine with no table keeps the names, split by what is
    // private now, and adds none.
    let view = || -> Option<Value> {
        let index: Value =
            serde_json::from_slice(&std::fs::read(e.records().join("views/views.json")).unwrap())
                .unwrap();
        let md5 = index["people"][&pid]["files"]["code-names.json"].as_str()?;
        let stored = medrec::viewfiles::stored_name("code-names.json", md5);
        let bytes = std::fs::read(e.records().join("views").join(&pid).join(stored)).unwrap();
        Some(serde_json::from_slice(&bytes).unwrap())
    };
    e.ok(&["views", "write"]);
    let v = view().expect("the names are kept");
    assert_eq!(v["table"]["version"], "FY2027");
    assert_eq!(v["codes"], json!({}));
    assert_eq!(
        v["private_codes"],
        json!({"icd10cm|F17.210": {"name": "Sample private name"},
               "icd10cm|G43.909": {"name": "Sample open name"}})
    );
    let v = ask(RECORDS, json!({"kind": "conditions", "person": "Alex"}));
    assert_eq!(name_of(&v, open).as_deref(), Some("Sample open name"));
    let v = ask(
        RECORDS,
        json!({"kind": "conditions", "name": "migraine headache"}),
    );
    assert_eq!(name_of(&v, open), None);

    // Nobody's views are made again for the table this machine lacks.
    let path = e.records().join("views/views.json");
    let mut index: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    index["people"][&pid]["code_table"] = json!("icd10cm FY2027");
    std::fs::write(&path, serde_json::to_vec(&index).unwrap()).unwrap();
    assert_eq!(e.ok(&["views", "write"])["people_rebuilt"], 0);
    let after: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(
        after["people"][&pid]["code_table"], "icd10cm FY2027",
        "a person who stands keeps the table recorded"
    );
    // Made again without the table, they keep the names and say no table
    // made them: a machine with one makes them again.
    e.ok(&["views", "write", "--all"]);
    let after: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert!(after["people"][&pid].get("code_table").is_none());
    assert!(view().is_some());

    // A last view whose bytes are not the ones recorded is not carried.
    let files = std::fs::read_dir(e.records().join("views").join(&pid)).unwrap();
    for f in files.flatten() {
        if f.file_name().to_string_lossy().starts_with("code-names.") {
            std::fs::write(f.path(), b"{}").unwrap();
        }
    }
    e.ok(&["views", "write", "--all"]);
    assert!(view().is_none());
    let _ = std::fs::remove_dir_all(&e.root);
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

const HEAR_VISIT: &str = "r_hearvisitaaaaaaaaaaa";
const HEAR_BILL: &str = "d_hearbillaaaaaaaaaaaa";

/// What a source gave holds an item private wherever it is: in the index's
/// views, its event, its bill, the search, each total of the household and
/// every answer, as the household's own mark does. Only that opens it.
#[test]
fn what_a_source_gave_is_private_everywhere() {
    let e = Env::new("given");
    let dir = fixture_household(&e, "given", &["index.json"]);
    let held: Value =
        serde_json::from_slice(&std::fs::read(dir.join("index.json")).unwrap()).unwrap();
    let mut open = held.clone();
    open["records"][0]
        .as_object_mut()
        .unwrap()
        .remove("given")
        .unwrap();
    let t = e.tools();
    let alex = h_id(&e, "Alex Testperson");
    let kinds = [
        "timeline",
        "events",
        "claims",
        "bills",
        "documents",
        "conditions",
        "results",
    ];
    // Everything the household can be shown of it, each answer fitting.
    let answers = || -> BTreeMap<String, Value> {
        let mut out = BTreeMap::new();
        let mut ask = |name: String, tool: &str, args: Value| {
            let r = call(&t, tool, args);
            assert!(!r.is_error(), "{tool}: {}", r.text());
            out.insert(name, assert_fits(&t, tool, &r).clone());
        };
        for kind in kinds {
            ask(kind.into(), RECORDS, json!({ "kind": kind }));
            let detailed = json!({"kind": kind, "response_format": "detailed"});
            ask(format!("{kind}, detailed"), RECORDS, detailed);
        }
        ask("search".into(), SEARCH, json!({"query": "professional"}));
        ask("bills, search".into(), SEARCH, json!({"query": "bill"}));
        ask("summary".into(), SUMMARY, json!({}));
        out
    };
    // The views a reader makes its pages and totals of, and what is private.
    let views = || -> (BTreeMap<String, Value>, Vec<String>, String) {
        e.ok(&["views", "write"]);
        let mut out = BTreeMap::new();
        for f in std::fs::read_dir(e.records().join("views").join(&alex)).unwrap() {
            let path = f.unwrap().path();
            let name = path.file_name().unwrap().to_str().unwrap().to_string();
            let (stem, ext) = (
                name.split('.').next().unwrap(),
                name.rsplit('.').next().unwrap(),
            );
            if ext == "json" {
                let v: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
                out.insert(stem.to_string(), v);
            }
        }
        let private = out.remove("private").unwrap();
        let of = ["records", "documents", "events"].iter();
        let ids = of.flat_map(|k| private[*k].as_object().into_iter().flatten());
        let ids = ids.map(|(id, _)| id.clone()).collect();
        (out, ids, private["records"][HEAR_VISIT].to_string())
    };
    let put = |v: &Value| std::fs::write(dir.join("index.json"), v.to_string()).unwrap();

    // With nothing given, it is the household's to see.
    put(&open);
    let (seen, (files, none, _)) = (answers(), views());
    assert!(none.is_empty(), "{none:?}");
    for kind in ["claims", "timeline", "search"] {
        assert!(seen[kind].to_string().contains(HEAR_VISIT), "{kind}");
    }
    for kind in ["bills", "documents", "timeline", "bills, search"] {
        assert!(seen[kind].to_string().contains(HEAR_BILL), "{kind}");
    }
    for name in [
        "bills",
        "documents",
        "events",
        "records-claim-0",
        "summary",
        "timeline-0",
    ] {
        assert!(files.contains_key(name), "{name}");
    }

    // The household's own mark: what private is, everywhere.
    e.ok(&["private", "mark", HEAR_VISIT]);
    let (marked, (marked_files, marked_ids, _)) = (answers(), views());
    e.ok(&["private", "reset", HEAR_VISIT]);
    assert_eq!(marked_ids.len(), 3, "{marked_ids:?}");
    assert_eq!((answers(), views().0), (seen.clone(), files.clone()));

    // What a source gave is the same, and no answer or view has it.
    put(&held);
    let (kept, (kept_files, kept_ids, why)) = (answers(), views());
    assert_eq!(kept, marked);
    assert_eq!(
        (kept_files.clone(), kept_ids.clone()),
        (marked_files, marked_ids)
    );
    assert_eq!(
        why,
        json!({"area": "mental_health", "from": "provider", "given": true}).to_string()
    );
    for (name, v) in kept.iter().chain(&kept_files) {
        assert!(!v.to_string().contains("Counseling"), "{name}");
    }
    for (name, v) in &kept {
        for id in &kept_ids {
            assert!(!v.to_string().contains(id.as_str()), "{name}: {id}");
        }
    }
    // Of the person, it is there, marked, with the values that stand.
    for (kind, id) in [("claims", HEAR_VISIT), ("bills", HEAR_BILL)] {
        let r = call(&t, RECORDS, json!({"kind": kind, "person": "Alex"}));
        let v = assert_fits(&t, RECORDS, &r).clone();
        let mut items = v["items"].as_array().unwrap().iter();
        let i = items.find(|i| i["id"] == id).unwrap();
        assert_eq!(i["private"], "mental_health", "{kind}");
        assert!(!v.to_string().contains("Counseling"), "{v}");
    }
    let list = e.ok(&["private", "list"]);
    let mut rows = list["items"].as_array().unwrap().iter();
    let row = rows.find(|i| i["id"] == HEAR_VISIT).unwrap();
    assert_eq!(
        (&row["from"], &row["given"]),
        (&json!("provider"), &json!(true))
    );

    // Only the household opens it.
    e.ok(&["private", "unmark", HEAR_VISIT]);
    let (opened, (opened_files, opened_ids, _)) = (answers(), views());
    assert!(opened_ids.is_empty(), "{opened_ids:?}");
    assert_eq!((opened, opened_files), (seen, files));
}
