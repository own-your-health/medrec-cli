//! What a source says is entered in error or about somebody else, and what one
//! gave and takes back (docs/data-contract.md, "Entered in error", "About
//! another person" and "What a source gave"), through the binary, on synthetic
//! data only.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use medrec::model::{Document, PersonIndex, Record};
use medrec::standing::{document_differs, document_in_error, record_differs, record_in_error};
use serde_json::{json, Value};

struct Env {
    root: PathBuf,
}

impl Env {
    fn new(tag: &str) -> Env {
        let root = std::env::temp_dir().join(format!("medrec-error-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let e = Env { root };
        e.ok(&["profiles", "add", "home"]);
        e.ok(&["init", e.records().to_str().unwrap()]);
        e.ok(&[
            "people",
            "add",
            "Robin Testperson",
            "--birth-date",
            "2020-02-02",
        ]);
        e
    }

    fn records(&self) -> PathBuf {
        self.root.join("records")
    }

    fn folder(&self) -> PathBuf {
        self.records().join("Robin Testperson")
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        let path = std::env::var("PATH").unwrap_or_default();
        Command::cargo_bin("medrec")
            .unwrap()
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_CACHE_HOME", self.root.join("cache"))
            .env("HOME", &self.root)
            .env("TZ", "America/New_York")
            .env(
                "PATH",
                format!("{}:{path}", self.root.join("provider").display()),
            )
            .env("MEDREC_PROFILE", "home")
            .env("MEDREC_KEYCHAIN_SERVICE", "piekstra.medrec.hermetic-tests")
            .arg("--json")
            .args(args)
            .output()
            .unwrap()
    }

    fn ok(&self, args: &[&str]) -> Value {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "medrec {args:?}: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap()
    }

    /// Import a source's stream. How many records that changes.
    fn import(&self, stream: &str) -> u64 {
        medrec::testdata::write_provider(&self.root.join("provider"), stream);
        self.ok(IMPORT)["records_updated"].as_u64().unwrap()
    }

    /// The same, cut short once the raw files are written: the person's folder
    /// takes no index.
    fn import_cut_short(&self, stream: &str) {
        use std::os::unix::fs::PermissionsExt;
        medrec::testdata::write_provider(&self.root.join("provider"), stream);
        let was = std::fs::metadata(self.folder()).unwrap().permissions();
        let closed = std::fs::Permissions::from_mode(0o500);
        std::fs::set_permissions(self.folder(), closed).unwrap();
        let out = self.run(IMPORT);
        std::fs::set_permissions(self.folder(), was).unwrap();
        assert!(!out.status.success());
        assert!(self.raw("one", UNFINISHED).exists());
    }

    /// A raw file of the source `org`.
    fn raw(&self, org: &str, sub: &str) -> PathBuf {
        let records = self.folder().join("Records");
        let mut dirs = std::fs::read_dir(records).unwrap();
        let of = format!("-{org}");
        let dir = dirs.find_map(|d| {
            let d = d.unwrap().path();
            d.to_string_lossy().ends_with(&of).then_some(d)
        });
        dir.unwrap().join(sub)
    }

    fn unread(&self) -> u64 {
        self.ok(&["reindex"])["papers_unread"].as_u64().unwrap()
    }

    /// What `reindex` changes of what sources say: records, papers.
    fn reindex(&self) -> (u64, u64) {
        let r = self.ok(&["reindex"]);
        let n = |k: &str| r[k].as_u64().unwrap();
        (n("statuses_updated"), n("papers_updated"))
    }

    fn index(&self) -> PersonIndex {
        let text = std::fs::read_to_string(self.folder().join("index.json")).unwrap();
        serde_json::from_str(&text).unwrap()
    }

    fn edit(&self, change: impl Fn(&mut Value)) {
        let path = self.folder().join("index.json");
        let mut v: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        change(&mut v);
        std::fs::write(path, v.to_string()).unwrap();
    }

    /// What is private, by id.
    fn private(&self) -> BTreeMap<String, Value> {
        let v = self.ok(&["private", "list"]);
        let items = v["items"].as_array().unwrap().iter();
        let by_id = |i: &Value| (i["id"].as_str().unwrap().to_string(), i.clone());
        items.map(by_id).collect()
    }

    fn visit(&self) -> Record {
        let records = self.index().records;
        let mut visits = records.into_iter().filter(|r| r.kind == "encounter");
        let v = visits.next().unwrap();
        assert!(visits.next().is_none());
        v
    }

    /// The paper `doc-0001` is of.
    fn paper(&self) -> Document {
        let documents = self.index().documents;
        let mut papers = documents
            .into_iter()
            .filter(|d| d.title.as_deref() == Some("Visit summary"));
        let p = papers.next().unwrap();
        assert!(papers.next().is_none());
        p
    }
}

const IMPORT: &[&str] = &[
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

/// What marks an import that has not finished, beside the raw files.
const UNFINISHED: &str = "importing.json";

/// The synthetic stream as an organization serves it, each resource as
/// `change` leaves it, or left out.
fn stream(org: &str, change: impl Fn(&str, &mut Value) -> bool) -> String {
    stream_of(medrec::testdata::STREAM, org, change)
}

fn stream_of(base: &str, org: &str, change: impl Fn(&str, &mut Value) -> bool) -> String {
    let mut counts = serde_json::Map::new();
    let mut lines = Vec::new();
    let mut add = |v: Value| {
        if let Some(t) = v["resourceType"].as_str() {
            let n = counts.get(t).and_then(Value::as_u64).unwrap_or(0);
            counts.insert(t.into(), (n + 1).into());
        }
        lines.push(v);
    };
    for l in base.lines() {
        let mut v: Value = serde_json::from_str(l).unwrap();
        if v["organization"].is_object() {
            v["organization"]["id"] = org.into();
            v["organization"]["fhir_base"] = format!("https://{org}.example.org").into();
        }
        let id = v["id"].as_str().unwrap_or_default().to_string();
        if v["resourceType"].is_null() || change(&id, &mut v) {
            add(v);
        }
    }
    let mut end = lines.pop().unwrap();
    end["counts"] = counts.into();
    lines.push(end);
    lines.iter().map(|v| format!("{v}\n")).collect()
}

/// A resource's status, in a version of its own.
fn say(v: &mut Value, status: &str, version: u32) {
    v["status"] = status.into();
    v["meta"] = json!({"versionId": version.to_string(),
        "lastUpdated": format!("2026-03-05T1{version}:00:00Z")});
}

/// The stream, with one resource's status.
fn saying(org: &str, resource: &str, status: &str, version: u32) -> String {
    stream(org, |id, v| {
        if id == resource {
            say(v, status, version);
        }
        true
    })
}

fn statuses(words: &[medrec::model::Word]) -> Vec<&str> {
    words.iter().filter_map(|w| w.status.as_deref()).collect()
}

/// Whether each time a source said so has ended.
fn ended(errors: &[medrec::model::InError]) -> Vec<bool> {
    errors.iter().map(|e| e.until.is_some()).collect()
}

#[test]
fn a_record_comes_back_when_a_later_version_no_longer_says_so() {
    let e = Env::new("back");
    e.import(&stream("one", |_, _| true));
    let v = e.visit();
    assert!(v.words.is_empty() && v.errors.is_empty());

    let id = v.id;
    assert_eq!(
        e.import(&saying("one", "enc-0001", "entered-in-error", 2)),
        1
    );
    let v = e.visit();
    assert!(v.id == id && record_in_error(&v) && !record_differs(&v));
    assert_eq!(statuses(&v.words), ["entered-in-error"]);
    assert_eq!(ended(&v.errors), [false]);
    // Served again as it was.
    assert_eq!(
        e.import(&saying("one", "enc-0001", "entered-in-error", 2)),
        0
    );
    assert_eq!(e.reindex(), (0, 0));
    assert_eq!(e.visit().errors, v.errors);

    assert_eq!(e.import(&saying("one", "enc-0001", "finished", 3)), 1);
    let v = e.visit();
    assert!(v.id == id && !record_in_error(&v) && !record_differs(&v));
    assert_eq!(statuses(&v.words), ["finished"]);
    assert_eq!(ended(&v.errors), [true]);
    assert_eq!(e.reindex(), (0, 0));

    // And again: each time is one of its history.
    e.import(&saying("one", "enc-0001", "entered-in-error", 4));
    assert_eq!(ended(&e.visit().errors), [true, false]);
    assert_eq!(e.reindex(), (0, 0));
}

#[test]
fn a_record_an_earlier_medrec_saved_gains_its_word_and_its_history() {
    let e = Env::new("earlier");
    e.import(&saying("one", "enc-0001", "entered-in-error", 1));
    let full = e.visit();
    assert!(record_in_error(&full));
    e.edit(|v| {
        for r in v["records"].as_array_mut().unwrap() {
            r.as_object_mut().unwrap().remove("words");
            r.as_object_mut().unwrap().remove("errors");
        }
    });
    // Its own status says so already.
    assert!(record_in_error(&e.visit()));
    assert_eq!(e.reindex().1, 0);
    let v = e.visit();
    assert_eq!(v.errors, full.errors);
    assert_eq!(statuses(&v.words), ["entered-in-error"]);
}

/// What the sources `one` and `two` say of the visit, one after the other,
/// with the class each gives it and the one the record shows.
#[test]
fn a_record_stands_while_one_source_states_it_and_shows_that_ones_values() {
    let visit = |org: &str, status: &str, version: u32| {
        stream(org, |id, v| {
            if id == "enc-0001" {
                say(v, status, version);
                v["class"] = json!({"code": "AMB", "display": format!("class {org}")});
            }
            id != "obs-0001"
        })
    };
    let shows = |e: &Env| {
        let v = e.visit();
        let class = v.fields["class"].as_str().unwrap().to_string();
        (class, record_in_error(&v), record_differs(&v))
    };
    let was = |class: &str, error: bool, differs: bool| (class.to_string(), error, differs);

    let e = Env::new("two");
    e.import(&visit("one", "finished", 1));
    e.import(&visit("two", "finished", 1));
    assert_eq!(shows(&e), was("class one", false, false));
    // The papers both serve gain the first one's word.
    assert_eq!(e.reindex(), (0, 2));
    assert_eq!(e.reindex(), (0, 0));

    // The one whose values it shows says it is entered in error.
    e.import(&visit("one", "entered-in-error", 2));
    assert_eq!(shows(&e), was("class two", false, true));
    assert_eq!(statuses(&e.visit().words), ["entered-in-error", "finished"]);
    for s in [
        visit("one", "entered-in-error", 2),
        visit("two", "finished", 1),
    ] {
        assert_eq!(e.import(&s), 0);
        assert_eq!(shows(&e), was("class two", false, true));
    }
    assert_eq!(e.reindex(), (0, 0));

    // Then both do, and one no longer does.
    e.import(&visit("two", "entered-in-error", 2));
    assert!(record_in_error(&e.visit()));
    assert_eq!(e.reindex(), (0, 0));
    e.import(&visit("one", "finished", 3));
    assert_eq!(shows(&e), was("class one", false, true));
    assert_eq!(ended(&e.visit().errors), [true, false]);
    assert_eq!(e.reindex(), (0, 0));
    assert_eq!(shows(&e), was("class one", false, true));
}

#[test]
fn a_source_that_states_a_record_gives_its_values_whoever_came_first() {
    let visit = |org: &str, status: &str| {
        stream(org, |id, v| {
            if id == "enc-0001" {
                v["status"] = status.into();
                v["class"] = json!({"code": "AMB", "display": format!("class {org}")});
            }
            true
        })
    };
    for first in ["one", "two"] {
        let e = Env::new(&format!("first-{first}"));
        let error = visit("one", "entered-in-error");
        let stands = visit("two", "finished");
        let (a, b) = match first {
            "one" => (&error, &stands),
            _ => (&stands, &error),
        };
        e.import(a);
        e.import(b);
        let v = e.visit();
        assert_eq!(v.fields["class"], "class two");
        assert!(record_differs(&v) && !record_in_error(&v));
        assert_eq!(e.reindex(), (0, 2));
        assert_eq!(e.reindex(), (0, 0));
        // As an index an earlier medrec saved, which named no writer.
        e.edit(|v| {
            for r in v["records"].as_array_mut().unwrap() {
                let r = r.as_object_mut().unwrap();
                r.remove("words");
                r.remove("errors");
                r.remove("fields_from");
            }
        });
        e.reindex();
        let v = e.visit();
        assert_eq!(v.fields["class"], "class two");
        assert!(record_differs(&v) && !record_in_error(&v));
    }
}

/// A source's word of a paper is that of what describes it.
#[test]
fn a_paper_is_in_error_by_what_describes_it_and_comes_back() {
    for by in ["status", "docStatus"] {
        let e = Env::new(&format!("paper-{by}"));
        let described = |value: Option<&str>, version: u32| {
            stream("one", |id, v| {
                if id == "doc-0001" {
                    say(v, "current", version);
                    if let Some(s) = value {
                        v[by] = s.into();
                    }
                }
                // A Binary of a later export may not be in it.
                id != "bin-0001" || version == 1
            })
        };
        e.import(&described(None, 1));
        let p = e.paper();
        assert!(p.words.is_empty() && p.errors.is_empty());
        e.import(&described(Some("entered-in-error"), 2));
        let p = e.paper();
        assert!(document_in_error(&p) && !document_differs(&p), "{by}");
        assert_eq!(ended(&p.errors), [false]);
        assert_eq!(e.reindex(), (0, 0));
        e.import(&described(None, 3));
        let p = e.paper();
        assert!(!document_in_error(&p) && !document_differs(&p));
        assert_eq!(ended(&p.errors), [true]);
        assert_eq!(e.reindex(), (0, 0));
        assert_eq!(ended(&e.paper().errors), [true]);
    }
}

#[test]
fn a_paper_stands_while_anything_its_source_has_describes_it() {
    let e = Env::new("describes");
    let with = |doc: &str, report: &str, carried: &[&str]| {
        stream("one", |id, v| {
            match id {
                "doc-0001" => say(v, doc, if doc == "current" { 1 } else { 2 }),
                "rep-0001" => {
                    say(v, report, if report == "final" { 1 } else { 2 });
                    v["presentedForm"] =
                        json!([{"contentType": "application/pdf", "url": "Binary/bin-0001"}]);
                }
                _ => {}
            }
            carried.is_empty() || v["resourceType"] == "Patient" || carried.contains(&id)
        })
    };
    e.import(&with("current", "final", &[]));
    e.import(&with("entered-in-error", "final", &[]));
    assert!(!document_in_error(&e.paper()));
    // One that carries only what says so.
    e.import(&with("entered-in-error", "final", &["doc-0001"]));
    assert!(!document_in_error(&e.paper()));
    e.import(&with("entered-in-error", "entered-in-error", &["rep-0001"]));
    assert!(document_in_error(&e.paper()));
    assert_eq!(e.reindex(), (0, 0));
}

#[test]
fn a_paper_another_source_serves_stands_and_is_filed_once() {
    let e = Env::new("served");
    e.import(&stream("one", |_, _| true));
    let id = e.paper().id;
    e.import(&stream("two", |_, _| true));
    let p = e.paper();
    assert_eq!(p.also_from.len(), 1);
    assert_eq!(statuses(&p.words), ["current"]);
    e.import(&saying("one", "doc-0001", "entered-in-error", 2));
    let p = e.paper();
    assert!(!document_in_error(&p) && document_differs(&p));
    assert_eq!(statuses(&p.words), ["entered-in-error", "current"]);
    assert_eq!(e.reindex(), (0, 0));
    e.import(&saying("two", "doc-0001", "entered-in-error", 2));
    assert!(document_in_error(&e.paper()));
    e.import(&saying("two", "doc-0001", "current", 3));
    let p = e.paper();
    assert!(!document_in_error(&p) && document_differs(&p));
    assert_eq!((p.id, p.also_from.len()), (id, 1));
    assert_eq!(e.reindex(), (0, 0));
}

/// One source's two Binaries of the same bytes are told apart.
#[test]
fn a_paper_stands_while_its_source_serves_it_by_another_binary() {
    let twice = |first: &str, second: &str| {
        let mut again = Vec::new();
        let s = stream("one", |id, v| {
            if id == "doc-0001" {
                say(v, first, if first == "current" { 1 } else { 2 });
            }
            true
        });
        for l in s.lines() {
            let mut v: Value = serde_json::from_str(l).unwrap();
            if v["counts"].is_object() {
                v["counts"]["DocumentReference"] = 3.into();
                v["counts"]["Binary"] = 3.into();
            }
            again.push(v.clone());
            if v["id"] == "doc-0001" {
                v["id"] = "doc-0003".into();
                say(&mut v, second, if second == "current" { 1 } else { 2 });
                v["content"][0]["attachment"]["url"] = "Binary/bin-0003".into();
                again.push(v.clone());
            } else if v["id"] == "bin-0001" {
                v["id"] = "bin-0003".into();
                again.push(v);
            }
        }
        again.iter().map(|v| format!("{v}\n")).collect::<String>()
    };
    let e = Env::new("binaries");
    e.import(&twice("current", "current"));
    assert_eq!(e.paper().also_from.len(), 1);
    e.import(&twice("entered-in-error", "current"));
    let p = e.paper();
    assert!(!document_in_error(&p) && document_differs(&p));
    e.import(&twice("entered-in-error", "entered-in-error"));
    assert!(document_in_error(&e.paper()));
    assert_eq!(e.reindex(), (0, 0));
}

/// A paper the household has of its own stands, whoever filed it first.
#[test]
fn a_paper_the_household_has_too_stands() {
    for household_first in [false, true] {
        let e = Env::new(&format!("household-{household_first}"));
        let scan = e.root.join("scan.pdf");
        let bytes = medrec::testdata::STREAM
            .lines()
            .map(|l| serde_json::from_str::<Value>(l).unwrap())
            .find(|v| v["id"] == "bin-0001")
            .map(|v| {
                use base64::Engine;
                let data = v["data"].as_str().unwrap().to_string();
                base64::engine::general_purpose::STANDARD
                    .decode(data)
                    .unwrap()
            })
            .unwrap();
        std::fs::write(&scan, bytes).unwrap();
        let intake = || {
            e.ok(&[
                "intake",
                scan.to_str().unwrap(),
                "--person",
                "Robin",
                "--type",
                "visit_summary",
                "--date",
                "2026-03-04",
                "--org",
                "Example Health",
            ])
        };
        if household_first {
            intake();
        }
        e.import(&stream("one", |_, _| true));
        if !household_first {
            intake();
            intake();
        }
        let papers = || {
            let all = e.index().documents;
            let sha = hash(&scan);
            let mut p = all.into_iter().filter(|d| d.sha256 == sha);
            let one = p.next().unwrap();
            assert!(p.next().is_none());
            one
        };
        assert_eq!(papers().also_from.len(), 1);
        e.import(&saying("one", "doc-0001", "entered-in-error", 2));
        let p = papers();
        assert!(!document_in_error(&p) && document_differs(&p));
        assert_eq!(e.reindex(), (0, 0));
    }
}

fn hash(p: &Path) -> String {
    medrec::hash::sha256_hex(&std::fs::read(p).unwrap())
}

/// Medrec before 0.9.9 kept no second source of a paper: `reindex` finds it,
/// and so does the import that would end the paper.
#[test]
fn a_papers_lost_source_is_found_again() {
    for by_import in [false, true] {
        let e = Env::new(&format!("lost-{by_import}"));
        e.import(&stream("one", |_, _| true));
        e.import(&stream("two", |_, _| true));
        let kept = e.paper().also_from;
        assert_eq!(kept.len(), 1);
        e.edit(|v| {
            for d in v["documents"].as_array_mut().unwrap() {
                d.as_object_mut().unwrap().remove("also_from");
                d.as_object_mut().unwrap().remove("words");
            }
        });
        if !by_import {
            assert_eq!(e.reindex(), (0, 2));
            let p = e.paper();
            assert_eq!(p.also_from[0].raw_path, kept[0].raw_path);
            assert_eq!(p.also_from[0].source, kept[0].source);
            assert_eq!(p.also_from[0].source_resource, kept[0].source_resource);
            assert_eq!(p.also_from[0].fetched_at, kept[0].fetched_at);
            assert_eq!(p.also_from[0].filed_at, kept[0].filed_at);
            let read = |p: &Document| p.words[1].read.clone();
            assert!(read(&p).is_some() && read(&p) == p.also_from[0].fetched_at);
            assert_eq!(e.reindex(), (0, 0));
        }
        e.import(&saying("one", "doc-0001", "entered-in-error", 2));
        let p = e.paper();
        assert!(!document_in_error(&p) && document_differs(&p));
        assert_eq!(p.also_from.len(), 1);
        assert_eq!(e.reindex(), (0, 0));
    }
}

/// Where one folder holds the imports of several patients, whose a raw file
/// is, and so when it was read, is not known.
#[test]
fn a_source_found_again_among_several_patients_has_no_time() {
    let e = Env::new("patients");
    e.import(&stream("one", |_, _| true));
    e.import(&stream("two", |_, _| true));
    e.edit(|v| {
        for d in v["documents"].as_array_mut().unwrap() {
            d.as_object_mut().unwrap().remove("also_from");
            d.as_object_mut().unwrap().remove("words");
        }
        let mut other = v["imports"][1].clone();
        other["id"] = "i_otheraaaaaaaaaaaaaaa".into();
        other["patient_id"] = "pat-0002".into();
        v["imports"].as_array_mut().unwrap().push(other);
    });
    assert_eq!(e.reindex(), (0, 2));
    let p = e.paper();
    assert_eq!(p.also_from[0].fetched_at, None);
    assert_eq!(p.words[1].read, None);
}

/// A word an earlier medrec did not keep was read when its source last was,
/// whenever the paper was filed.
#[test]
fn a_word_not_kept_is_dated_by_its_sources_latest_import() {
    let e = Env::new("dated");
    let pulled = |stream: &str, at: &str| {
        let (head, rest) = stream.split_once('\n').unwrap();
        let mut head: Value = serde_json::from_str(head).unwrap();
        head["pulled_at"] = at.into();
        format!("{head}\n{rest}")
    };
    let (filed, said) = ("2026-01-10T09:00:00Z", "2026-06-10T09:00:00Z");
    e.import(&pulled(&stream("one", |_, _| true), filed));
    let error = saying("one", "doc-0001", "entered-in-error", 2);
    e.import(&pulled(&error, said));
    let kept = e.paper();
    assert_eq!(kept.provenance.fetched_at.as_deref(), Some(filed));
    assert_eq!(kept.errors[0].from, said);
    e.edit(|v| {
        for d in v["documents"].as_array_mut().unwrap() {
            d.as_object_mut().unwrap().remove("errors");
            d.as_object_mut().unwrap().remove("words");
        }
    });
    assert_eq!(e.reindex(), (0, 1));
    let p = e.paper();
    assert_eq!(p.words[0].read.as_deref(), Some(said));
    assert_eq!((p.words, p.errors), (kept.words, kept.errors));
}

/// What an older version of a resource says of a paper is not said: the
/// version filed speaks, in an import as in `reindex`.
#[test]
fn an_older_version_of_what_describes_a_paper_says_nothing() {
    for by in ["doc-0001", "rep-0001"] {
        let with = |statuses: (&str, &str), version: u32, binary: bool| {
            let fine = if by == "doc-0001" { "current" } else { "final" };
            let is = if version == 1 { statuses.0 } else { statuses.1 };
            let status = if is.is_empty() { fine } else { is };
            stream("one", |id, v| {
                if id == by {
                    say(v, status, version);
                    if by == "rep-0001" {
                        v["presentedForm"] =
                            json!([{"contentType": "application/pdf", "url": "Binary/bin-0001"}]);
                    }
                }
                let other = id == "doc-0001" && by != id;
                !other && (binary || id != "bin-0001")
            })
        };
        let paper = |e: &Env| -> Document {
            let path = e.raw("one", "Binary/bin-0001.json");
            let of = |d: &Document| {
                let raw = d.provenance.raw_path.as_deref().unwrap();
                path.to_string_lossy().ends_with(raw)
            };
            e.index().documents.into_iter().find(of).unwrap()
        };
        let error = "entered-in-error";
        // The later one says so, and the older one after it does not.
        let e = Env::new(&format!("older-error-{by}"));
        e.import(&with(("", error), 1, true));
        for version in [2, 1] {
            e.import(&with(("", error), version, true));
            let p = paper(&e);
            assert!(document_in_error(&p), "{by} {version}");
            assert_eq!(ended(&p.errors), [false]);
            assert_eq!(e.reindex(), (0, 0));
        }
        // The older one says so, and the later one before it does not.
        let e = Env::new(&format!("older-fine-{by}"));
        e.import(&with((error, ""), 2, true));
        e.import(&with((error, ""), 1, true));
        let p = paper(&e);
        assert!(!document_in_error(&p) && p.errors.is_empty(), "{by}");
        assert_eq!(e.reindex(), (0, 0));
        // A paper first filed beside the older one is as the later one says.
        let e = Env::new(&format!("older-first-{by}"));
        e.import(&with(("", error), 2, false));
        e.import(&with(("", error), 1, true));
        let p = paper(&e);
        assert!(document_in_error(&p), "{by}");
        assert_eq!(ended(&p.errors), [false]);
        assert_eq!(e.reindex(), (0, 0));
    }
}

/// While a resource that may describe a paper does not read, its source says
/// of none that it is entered in error.
#[test]
fn a_raw_file_that_does_not_read_ends_no_paper() {
    for by_import in [false, true] {
        let e = Env::new(&format!("unread-{by_import}"));
        e.import(&stream("one", |_, _| true));
        let other = e.raw("one", "DocumentReference/doc-0002.json");
        let was = std::fs::read(&other).unwrap();
        std::fs::write(&other, "{").unwrap();
        let only = |status: &str, version: u32| {
            stream("one", |id, v| {
                if id == "doc-0001" {
                    say(v, status, version);
                }
                v["resourceType"] == "Patient" || id == "doc-0001"
            })
        };
        if by_import {
            e.import(&only("entered-in-error", 2));
        } else {
            let doc = e.raw("one", "DocumentReference/doc-0001.json");
            let mut v: Value = serde_json::from_slice(&std::fs::read(&doc).unwrap()).unwrap();
            say(&mut v, "entered-in-error", 2);
            std::fs::write(&doc, v.to_string()).unwrap();
        }
        assert_eq!(e.unread(), 1);
        let p = e.paper();
        assert!(!document_in_error(&p) && p.errors.is_empty());
        std::fs::write(&other, was).unwrap();
        assert_eq!(e.unread(), 0);
        assert!(document_in_error(&e.paper()));
    }
}

/// One that does not read is counted, and those beside it are found.
#[test]
fn a_binary_that_does_not_read_is_counted() {
    let e = Env::new("binary");
    e.import(&stream("one", |_, _| true));
    e.import(&stream("two", |_, _| true));
    e.edit(|v| {
        for d in v["documents"].as_array_mut().unwrap() {
            d.as_object_mut().unwrap().remove("also_from");
            d.as_object_mut().unwrap().remove("words");
            d["provenance"]["filed_by"] = "medrec 0.9.8".into();
        }
    });
    let path = e.raw("two", "Binary/bin-0002.json");
    let mut v: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    v["data"] = "not base64!".into();
    std::fs::write(&path, v.to_string()).unwrap();
    let papers = e.index().documents.len();
    let r = e.ok(&["reindex", "--dry-run"]);
    let counted = &r["people"][0]["views"]["papers"];
    let want = serde_json::json!({"filed_before": papers, "source_found": 1});
    assert_eq!((counted, r["fresh"].as_bool()), (&want, Some(true)), "{r}");
    let r = e.ok(&["reindex"]);
    assert!(r.get("fresh").is_none() && r["people"][0].get("views").is_none());
    assert_eq!(r["papers_unread"], 1);
    assert_eq!(r["papers_updated"], 1);
    assert_eq!(e.paper().also_from.len(), 1);
}

/// A resource with no id takes the place of none: a paper one describes
/// stands, and an import adds nothing to its history.
#[test]
fn a_reference_with_no_id_ends_nothing_an_earlier_one_says() {
    let e = Env::new("noid");
    let with = |status: &str, version: u32| {
        stream("one", |id, v| {
            if id == "doc-0001" {
                v.as_object_mut().unwrap().remove("id");
                say(v, status, version);
            }
            true
        })
    };
    e.import(&with("current", 1));
    for _ in 0..2 {
        e.import(&with("entered-in-error", 2));
        let p = e.paper();
        assert!(!document_in_error(&p) && p.errors.is_empty());
        assert_eq!(e.reindex(), (0, 0));
        assert!(e.paper().errors.is_empty());
    }
    // One whose only reference says so is in error, and no other paper is.
    let e = Env::new("noid-error");
    e.import(&with("entered-in-error", 1));
    let errors = |e: &Env| -> Vec<bool> {
        let papers = e.index().documents;
        papers.iter().map(document_in_error).collect()
    };
    assert!(document_in_error(&e.paper()));
    assert_eq!(errors(&e).iter().filter(|x| **x).count(), 1);
    assert_eq!(e.reindex(), (0, 0));
}

/// `reindex` reads the raw files of what several sources state, and those of
/// the rest of their type.
#[test]
fn reindex_reads_every_visits_providers_beside_one_two_sources_state() {
    let e = Env::new("providers");
    let visits = |org: &str, of: &[&str]| {
        stream_of(medrec::testdata::PROVIDERS_STREAM, org, |id, v| {
            v["resourceType"] != "Encounter" || of.contains(&id)
        })
    };
    let ids: Vec<String> = medrec::testdata::PROVIDERS_STREAM
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .filter(|v| v["resourceType"] == "Encounter")
        .map(|v| v["id"].as_str().unwrap().to_string())
        .collect();
    let all: Vec<&str> = ids.iter().map(String::as_str).collect();
    assert_eq!(all.len(), 2);
    e.import(&visits("one", &all));
    e.import(&visits("two", &all[..1]));
    let providers = || -> Vec<_> {
        let records = e.index().records;
        let of = |r: Record| (r.kind, r.providers);
        records.into_iter().map(of).collect()
    };
    let full = providers();
    let some: Vec<_> = full.iter().filter(|(_, p)| !p.is_empty()).collect();
    assert_eq!(some.iter().filter(|(k, _)| k == "encounter").count(), 2);
    e.edit(|v| {
        for r in v["records"].as_array_mut().unwrap() {
            r.as_object_mut().unwrap().remove("providers");
        }
    });
    assert_eq!(e.ok(&["reindex"])["providers_updated"], some.len());
    assert_eq!(providers(), full);
}

/// No provider of a record that stands is one only a source that says it is
/// entered in error gives, by an import as by `reindex`, and it comes back
/// with that source.
#[test]
fn a_source_in_error_says_nothing_of_the_providers() {
    let says = |org: &str, npi: Option<&str>, status: &str, version: u32| {
        stream_of(medrec::testdata::PROVIDERS_STREAM, org, |id, v| {
            if id == "enc-a" {
                say(v, status, version);
                v["class"]["display"] = format!("class {org}").into();
            }
            if id == "prac-one" {
                v["identifier"] = match npi {
                    Some(n) => json!([{"system": "http://hl7.org/fhir/sid/us-npi", "value": n}]),
                    None => json!([]),
                };
            }
            ["enc-a", "prac-one", "loc-one", "org-one"].contains(&id)
        })
    };
    let npi = |e: &Env| {
        let refs = e.visit().providers;
        let of = refs.into_iter().find(|r| r.field == "practitioner");
        of.and_then(|r| r.npi)
    };
    let is = |e: &Env, of: Option<&str>, class: &str| {
        assert_eq!(npi(e).as_deref(), of);
        assert_eq!(e.visit().fields["class"], class);
        assert_eq!(e.ok(&["reindex"])["providers_updated"], 0);
        assert_eq!(npi(e).as_deref(), of);
    };
    let (one, two) = ("0000000006", "0000000014");

    // Each gives its own of the name both print.
    let e = Env::new("npis");
    e.import(&says("one", Some(one), "finished", 1));
    e.import(&says("two", Some(two), "finished", 1));
    is(&e, Some(one), "class one");
    e.import(&says("one", Some(one), "entered-in-error", 2));
    is(&e, Some(two), "class two");
    e.import(&says("two", Some(two), "finished", 1));
    is(&e, Some(two), "class two");
    e.import(&says("one", Some(one), "finished", 3));
    is(&e, Some(one), "class one");

    // The values are another's, and one whose NPI filled them takes it back.
    let e = Env::new("npis-filled");
    e.import(&says("one", None, "finished", 1));
    e.import(&says("two", Some(two), "finished", 1));
    is(&e, Some(two), "class one");
    e.import(&says("two", Some(two), "entered-in-error", 2));
    is(&e, None, "class one");
    e.import(&says("two", Some(two), "finished", 3));
    is(&e, Some(two), "class two");

    // One that joins in error adds none.
    let e = Env::new("npis-joined");
    e.import(&says("one", None, "finished", 1));
    e.import(&says("two", Some(two), "entered-in-error", 1));
    is(&e, None, "class one");
}

// ------------------------------------------------- what a source gave

/// A place whose name says its area, and one whose name says none.
const PRIVATE: &str = "Example Counseling Group";
const OPEN: &str = "Example Walk-in Clinic";

/// The visit as a source gives it, at a place, or with no more than what it
/// is of.
fn visit_at(org: &str, place: Option<&str>, status: &str, version: u32) -> String {
    stream(org, |id, v| {
        if id == "enc-0001" {
            say(v, status, version);
            let o = v.as_object_mut().unwrap();
            o.remove("serviceProvider");
            match place {
                Some(p) => o.insert("serviceProvider".into(), json!({ "display": p })),
                None => o.remove("type").and(o.remove("period")),
            };
        }
        id != "obs-0001"
    })
}

/// A condition's code as a source gives it.
fn coded(code: &str, version: u32) -> String {
    coded_by("one", code, version)
}

fn coded_by(org: &str, code: &str, version: u32) -> String {
    stream(org, |id, v| {
        if id == "cond-0001" {
            let at = format!("2026-03-05T1{version}:00:00Z");
            v["meta"] = json!({"versionId": version.to_string(), "lastUpdated": at});
            let icd10 = "http://hl7.org/fhir/sid/icd-10-cm";
            v["code"] = json!({"coding": [{"system": icd10, "code": code}]});
        }
        id != "obs-0001"
    })
}

/// Whether the visit is private, and by a place it does not show.
fn held(e: &Env) -> Option<bool> {
    let why = e.private().remove(&e.visit().id)?;
    assert_eq!(
        (&why["area"], &why["from"]),
        (&json!("mental_health"), &json!("provider"))
    );
    Some(why["given"] == true)
}

fn shows_place(e: &Env) -> String {
    e.visit().fields["service_provider"]
        .as_str()
        .unwrap()
        .to_string()
}

/// What one source gave holds an item private beside another that gives
/// something else, and when it takes it back, says otherwise or is gone.
#[test]
fn what_one_source_gave_stays_private_whatever_is_said_later() {
    let e = Env::new("gave");
    e.import(&visit_at("two", Some(OPEN), "finished", 1));
    assert_eq!(held(&e), None);
    e.import(&visit_at("one", Some(PRIVATE), "finished", 1));
    assert_eq!((shows_place(&e).as_str(), held(&e)), (OPEN, Some(true)));
    for (status, place, version) in [
        ("entered-in-error", PRIVATE, 2),
        ("finished", PRIVATE, 3),
        ("finished", OPEN, 4),
        ("entered-in-error", OPEN, 5),
    ] {
        e.import(&visit_at("one", Some(place), status, version));
        assert_eq!(held(&e), Some(shows_place(&e) == OPEN), "{version}");
        e.reindex();
        assert_eq!(held(&e), Some(shows_place(&e) == OPEN), "{version}");
    }
    assert_eq!(shows_place(&e), OPEN);
    // Neither does the one it shows say it: only the household opens it.
    e.import(&visit_at("two", Some(OPEN), "finished", 2));
    assert_eq!(held(&e), Some(true));
    let id = e.visit().id;
    e.ok(&["private", "unmark", &id]);
    assert_eq!(held(&e), None);
    e.ok(&["private", "reset", &id]);
    assert_eq!(held(&e), Some(true));
}

/// The same of a source's own earlier version, and of one entered in error
/// that comes with nothing of what it was of.
#[test]
fn what_an_earlier_version_gave_stays_private() {
    let e = Env::new("gave-before");
    e.import(&visit_at("one", Some(PRIVATE), "finished", 1));
    assert_eq!(held(&e), Some(false));
    e.import(&visit_at("one", Some(OPEN), "finished", 2));
    assert_eq!((shows_place(&e).as_str(), held(&e)), (OPEN, Some(true)));
    assert_eq!(e.index().records.iter().flat_map(|r| &r.given).count(), 1);

    let e = Env::new("gave-bare");
    e.import(&visit_at("one", Some(PRIVATE), "finished", 1));
    let was = e.visit();
    for _ in 0..2 {
        e.import(&visit_at("one", None, "entered-in-error", 2));
        e.reindex();
        let v = e.visit();
        assert!(record_in_error(&v));
        assert_eq!(
            (v.id, v.date, v.fields),
            (was.id.clone(), was.date.clone(), was.fields.clone())
        );
    }
    e.import(&visit_at("one", Some(OPEN), "finished", 3));
    assert_eq!((shows_place(&e).as_str(), held(&e)), (OPEN, Some(true)));
}

/// One that leaves a record it was of with another's is held by what that
/// showed, whichever of the two wrote it.
#[test]
fn a_record_that_leaves_another_sources_stays_private() {
    for (first, second) in [("one", "two"), ("two", "one")] {
        let e = Env::new("gave-left");
        for org in ["one", "two"] {
            e.import(&coded_by(org, "F32.1", 1));
        }
        for org in [first, second] {
            e.import(&coded_by(org, "J45.909", 2));
            for again in [false, true] {
                if again {
                    e.reindex();
                }
                let records = e.index().records;
                let conditions = records.iter().filter(|r| r.kind == "condition");
                let private = e.private();
                let mut held = conditions.map(|r| private.contains_key(&r.id));
                assert!(held.all(|held| held), "{first} {org} {again}");
            }
        }
    }
}

/// An import cut short after its raw files are written is made good by the
/// next, which finds them there.
#[test]
fn an_import_cut_short_loses_nothing_that_was_given() {
    let visit = |place: &str, version: u32| visit_at("one", Some(place), "finished", version);
    // Its source says it is the same version, read later.
    let read = r#""pulled_at":"2026-03-05T1"#;
    let again = visit(OPEN, 1).replacen(read, &read.replace("T1", "T2"), 1);
    assert_ne!(again, visit(OPEN, 1));
    for (kind, was, is) in [
        ("encounter", visit(PRIVATE, 1), visit(OPEN, 2)),
        ("encounter", visit(PRIVATE, 1), again),
        ("condition", coded("F32.1", 1), coded("J45.909", 2)),
    ] {
        let e = Env::new("gave-cut");
        e.import(&was);
        let path = e.folder().join("index.json");
        let before = std::fs::read(&path).unwrap();
        e.import(&is);
        std::fs::write(&path, before).unwrap();
        e.import(&is);
        let records = e.index().records;
        let of = records.iter().find(|r| r.kind == kind).unwrap();
        assert_eq!(e.private()[&of.id]["given"], true, "{kind}");
        assert_eq!(of.given.len(), 1, "{kind}");
        // Once: what comes again is kept.
        assert_eq!(e.import(&is), 0, "{kind}");
    }
}

/// What an import cut short filed was given, though the index never had it:
/// by the source's next import, whatever that one serves.
#[test]
fn a_version_no_import_read_in_was_given() {
    let visit = |place: &str, version: u32| visit_at("one", Some(place), "finished", version);
    let titled = |title: &str, version: u32| {
        stream("one", |id, v| {
            if id == "doc-0001" {
                say(v, "current", version);
                v["description"] = title.into();
            }
            true
        })
    };
    let gone = |id: &str| id != "enc-0001" && id != "obs-0001";
    let secret = "Psychotherapy summary";
    let shown = |e: &Env, of: &str| {
        let idx = e.index();
        let record = idx.records.iter().find(|r| r.kind == of);
        let paper = idx.documents.iter().find(|_| of == "document");
        record.map(|r| r.id.clone()).or(paper.map(|d| d.id.clone()))
    };
    for (tag, kind, cut, served) in [
        (
            "encounter",
            "encounter",
            vec![visit(PRIVATE, 2)],
            visit(OPEN, 3),
        ),
        (
            "condition",
            "condition",
            vec![coded("F32.1", 2)],
            coded("J45.909", 3),
        ),
        (
            "paper",
            "document",
            vec![titled(secret, 2)],
            titled("Visit summary", 3),
        ),
        // Cut short twice, and the last of them served again.
        (
            "twice",
            "encounter",
            vec![visit(PRIVATE, 2), visit(OPEN, 3)],
            visit(OPEN, 3),
        ),
        // No longer served.
        (
            "gone",
            "encounter",
            vec![visit(PRIVATE, 2)],
            stream("one", |id, _| gone(id)),
        ),
    ] {
        let e = Env::new(&format!("gave-unread-{tag}"));
        e.import(&match kind {
            "encounter" => visit(OPEN, 1),
            "condition" => coded("J45.909", 1),
            _ => titled("Visit summary", 1),
        });
        let mark = e.raw("one", UNFINISHED);
        assert!(!mark.exists(), "{tag}");
        let id = shown(&e, kind).unwrap();
        for s in &cut {
            e.import_cut_short(s);
        }
        assert!(!e.private().contains_key(&id), "{tag}");
        e.import(&served);
        assert_eq!(e.private()[&id]["given"], true, "{tag}");
        assert_eq!(shown(&e, kind), Some(id), "{tag}");
        // Once: it finished, and what comes again is kept.
        assert!(!mark.exists(), "{tag}");
        assert_eq!(e.import(&served), 0, "{tag}");
        assert!(!mark.exists(), "{tag}");
    }

    // The first that reads in a resource the cut one filed, or its source.
    let e = Env::new("gave-unread-new");
    e.import(&stream("one", |id, _| gone(id)));
    e.import_cut_short(&visit(PRIVATE, 1));
    e.import(&visit(OPEN, 2));
    assert_eq!((shows_place(&e).as_str(), held(&e)), (OPEN, Some(true)));
    for (first, then) in [
        (visit(PRIVATE, 1), visit(OPEN, 2)),
        (titled(secret, 1), titled("Visit summary", 2)),
    ] {
        let e = Env::new(&format!("gave-unread-first-{}", first.contains(secret)));
        std::fs::create_dir_all(e.folder().join("Records")).unwrap();
        e.import_cut_short(&first);
        e.import(&then);
        let id = match first.contains(secret) {
            true => e.paper().id,
            false => e.visit().id,
        };
        assert_eq!(e.private()[&id]["given"], true);
        assert!(!e.raw("one", UNFINISHED).exists());
    }
}

/// What was given before medrec kept it is read from the raw files, every
/// version of them.
#[test]
fn reindex_reads_what_was_given_from_every_raw_version() {
    let forget = |e: &Env| {
        e.edit(|v| {
            for r in v["records"].as_array_mut().unwrap() {
                r.as_object_mut().unwrap().remove("given");
            }
        })
    };
    let e = Env::new("gave-raw");
    e.import(&visit_at("one", Some(PRIVATE), "finished", 1));
    e.import(&visit_at("one", Some(OPEN), "finished", 2));
    forget(&e);
    assert_eq!(held(&e), None);
    let dry = e.ok(&["reindex", "--dry-run"]);
    assert_eq!((dry["given_updated"].as_u64(), held(&e)), (Some(1), None));
    assert_eq!(e.ok(&["reindex"])["given_updated"], 1);
    assert_eq!(held(&e), Some(true));
    assert_eq!(e.ok(&["reindex"])["given_updated"], 0);

    let e = Env::new("gave-raw-two");
    e.import(&visit_at("two", Some(OPEN), "finished", 1));
    e.import(&visit_at("one", Some(PRIVATE), "finished", 1));
    forget(&e);
    assert_eq!(held(&e), None);
    assert_eq!(e.ok(&["reindex"])["given_updated"], 1);
    assert_eq!((shows_place(&e).as_str(), held(&e)), (OPEN, Some(true)));
}

/// The earlier versions of some of a source's reports are not all of them: a
/// paper is judged by the ones that stand.
#[test]
fn reindex_reads_all_that_describes_a_paper() {
    let with = |doc: &str, version: u32| {
        stream("one", |id, v| {
            match id {
                "doc-0001" => say(v, doc, version),
                "rep-0001" => {
                    say(v, "final", version);
                    v["code"]["text"] = format!("Example panel {version}").into();
                    v["presentedForm"] =
                        json!([{"contentType": "application/pdf", "url": "Binary/bin-0001"}]);
                }
                _ => {}
            }
            true
        })
    };
    let e = Env::new("gave-reports");
    e.import(&with("current", 1));
    e.import(&with("entered-in-error", 2));
    assert!(!document_in_error(&e.paper()));
    assert_eq!(e.reindex(), (0, 0));
    assert!(!document_in_error(&e.paper()));
}

/// What a resource gave goes with it to the record it is of, and the record
/// it left keeps it too.
#[test]
fn what_a_resource_gave_goes_with_it() {
    let on = |org: &str, place: &str, day: &str, version: u32| {
        stream(org, |id, v| {
            if id == "enc-0001" {
                say(v, "finished", version);
                v["serviceProvider"]["display"] = place.into();
                v["period"] = json!({"start": format!("2026-{day}T09:30:00Z")});
            }
            id != "obs-0001"
        })
    };
    // Given before, or by an older version served late.
    for versions in [[1, 0, 2], [2, 1, 3]] {
        let e = Env::new("gave-moved");
        e.import(&on("two", OPEN, "03-04", 1));
        let [first, late, moved] = versions;
        if late > 0 {
            e.import(&on("one", OPEN, "03-04", first));
        }
        e.import(&on("one", PRIVATE, "03-04", late.max(1)));
        e.import(&on("one", OPEN, "03-11", moved));
        let visits: Vec<Record> = e
            .index()
            .records
            .into_iter()
            .filter(|r| r.kind == "encounter")
            .collect();
        assert_eq!(visits.len(), 2);
        let private = e.private();
        for v in visits {
            assert_eq!(v.fields["service_provider"], OPEN);
            assert_eq!(private[&v.id]["given"], true, "{}", v.date.unwrap());
            let paths = v.given.iter().filter_map(|g| g.raw_path.as_ref());
            assert!(paths.into_iter().all(|p| !p.contains('~')), "{:?}", v.given);
        }
    }
}

/// A paper's title and its source's name are its first source's, and what
/// another gives holds it private.
#[test]
fn what_another_source_calls_a_paper_holds_it_private() {
    let called = |org: &str, title: &str, name: &str, version: u32| {
        let s = stream(org, |id, v| {
            if id == "doc-0001" {
                say(v, "current", version);
                v["description"] = title.into();
            }
            true
        });
        s.replacen(
            r#""name":"Example Health""#,
            &format!(r#""name":"{name}""#),
            1,
        )
    };
    for (title, name) in [("Psychotherapy summary", OPEN), ("Visit summary", PRIVATE)] {
        let e = Env::new(&format!("gave-paper-{}", title.len()));
        e.import(&called("two", "Visit summary", OPEN, 1));
        let id = e.paper().id;
        assert!(!e.private().contains_key(&id));
        e.import(&called("one", title, name, 1));
        assert_eq!(e.private()[&id]["given"], true, "{title}");
        assert_eq!(e.paper().organization.as_deref(), Some(OPEN));
        // It shows nothing of it, and keeps it when it is said no more.
        let shown = e.ok(&["documents", "open", &id]).to_string();
        assert!(
            !shown.contains("Psycho") && !shown.contains(PRIVATE),
            "{shown}"
        );
        e.import(&called("one", "Visit summary", OPEN, 2));
        assert_eq!(e.private()[&id]["given"], true, "{title}");
        // Nor is it lost where medrec did not keep it.
        e.edit(|v| {
            for d in v["documents"].as_array_mut().unwrap() {
                d.as_object_mut().unwrap().remove("given");
            }
        });
        assert!(!e.private().contains_key(&id));
        e.reindex();
        assert_eq!(e.private().contains_key(&id), name == OPEN, "{title}");
    }
    // Its own source's later title.
    let e = Env::new("gave-title");
    e.import(&called("two", "Visit summary", OPEN, 1));
    let id = e.paper().id;
    e.import(&called("two", "Psychotherapy summary", OPEN, 2));
    e.import(&called("two", "Visit summary", OPEN, 3));
    assert_eq!(e.private()[&id]["given"], true);
}

/// A test as a source serves it, about somebody else by its `focus`.
fn test_about(org: &str, focus: Option<Value>, version: u32) -> String {
    stream(org, |id, v| {
        if id == "obs-0001" {
            say(v, "final", version);
            if let Some(f) = &focus {
                v["focus"] = json!([f]);
            }
        }
        true
    })
}

/// The test's id, in the index; and whether it is in an answer of it.
fn the_test(e: &Env) -> (String, bool) {
    let records = e.index().records;
    let mut of = records.iter().filter(|r| r.display.contains("Hemoglobin"));
    let id = of.next().unwrap().id.clone();
    assert!(of.next().is_none());
    let seen = [
        e.ok(&["results", "--person", "Robin"]),
        e.ok(&["search", "hemoglobin"]),
        e.ok(&["timeline"]),
    ];
    let seen = seen.map(|v| v.to_string().contains(&id));
    assert!(seen.iter().all(|s| *s == seen[0]), "{seen:?}");
    (id, seen[0])
}

/// What its source says is about another person is in no list, whatever a
/// source says later; importing it again makes no other.
#[test]
fn a_test_its_source_says_is_of_another_person_is_in_no_list() {
    let parent = json!({"reference": "RelatedPerson/rp-0001", "display": "Mother"});
    let sibling = json!({"reference": "Patient/pat-0002"});
    for (n, (focus, says)) in [
        (parent.clone(), "a parent"),
        (sibling, "another person"),
        (
            json!({"type": "RelatedPerson", "identifier": {"value": "x"}}),
            "another person",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let e = Env::new(&format!("about-{n}"));
        e.import(&test_about("one", Some(focus.clone()), 1));
        let (id, seen) = the_test(&e);
        assert!(!seen, "{says}");
        let note = format!("about {says}, in Robin Testperson's chart");
        let of = |e: &Env| e.ok(&["records", "show", &id])["notes"].clone();
        assert_eq!(of(&e), json!([note]));
        let list = e.ok(&["records", "about-another"]);
        assert_eq!(list["items"].as_array().unwrap().len(), 1, "{list}");
        // Its raw files are opened as it is, and in no count of many.
        assert_eq!(e.ok(&["records", "raw", &id])["notes"], json!([note]));
        for how in ["--fields", "--changes"] {
            let notes = &e.ok(&["records", "raw", &id, how])["notes"];
            assert_eq!(*notes, json!([format!("about {says}")]));
        }
        let many = e.ok(&["records", "fields", "--kind", "result"]);
        assert_eq!((&many["records"], &many["types"]), (&json!(0), &json!([])));
        // Again; by another source with nothing of it; and by the first.
        let kept = e.index().records.len();
        e.import(&test_about("one", Some(focus), 1));
        assert_eq!(e.index().records.len(), kept);
        e.import(&test_about("one", None, 2));
        assert_eq!(e.index().records.len(), kept);
        assert_eq!(the_test(&e), (id.clone(), false), "{says}");
        assert_eq!(of(&e), json!([note]));
        // Whatever writes the index keeps it, and nothing counts as missing.
        e.ok(&["reindex"]);
        e.ok(&["views", "write"]);
        assert_eq!(the_test(&e), (id.clone(), false), "{says}");
        assert_eq!(e.index().records.len(), kept);
        let v = e.ok(&["verify"]);
        assert_eq!(v["problems"].as_array().map_or(0, Vec::len), 0, "{v}");
    }
    // Of its own subject, or of a device, it is the person's.
    for focus in [
        json!({"reference": "Patient/pat-0001"}),
        json!({"reference": "Device/d-0001"}),
    ] {
        let e = Env::new("about-own");
        e.import(&test_about("one", Some(focus), 1));
        assert!(the_test(&e).1);
    }
    // One an earlier medrec read in is, once it is imported again, or read
    // again from its raw file, and by an earlier version of it too. The
    // person's own of that day is none of it.
    for (n, (again, versions)) in [("import", 1), ("reindex", 1), ("reindex", 2)]
        .into_iter()
        .enumerate()
    {
        let e = Env::new(&format!("about-again-{n}"));
        e.import(&test_about("one", Some(parent.clone()), 1));
        if versions == 2 {
            e.import(&test_about("one", None, 2));
        }
        let (id, _) = the_test(&e);
        e.edit(|v| {
            let mut records = v["records"].as_array_mut().unwrap().iter_mut();
            let r = records.find(|r| r["id"] == id.as_str()).unwrap();
            as_filed_before(r);
        });
        assert!(the_test(&e).1);
        match again {
            "import" => drop(e.import(&test_about("one", Some(parent.clone()), 1))),
            _ => {
                let of = e.ok(&["reindex"]);
                assert_eq!(of["results_split"], 1, "{of}");
                assert_eq!(e.ok(&["reindex"])["results_split"], 0);
            }
        }
        assert_eq!(the_test(&e), (id.clone(), false), "{again}");
        let records = e.index().records;
        let theirs = records.iter().find(|r| r.id == id).unwrap();
        assert!(theirs.key.starts_with("about|result|"), "{}", theirs.key);
        e.import(&test_about("two", None, 1));
        for how in ["imported", "read again"] {
            let listed = e.ok(&["results", "--person", "Robin"]).to_string();
            assert!(listed.contains("Hemoglobin"), "{again}, {how}");
            assert!(!listed.contains(&id), "{again}, {how}");
            e.ok(&["reindex"]);
        }
    }
}

/// A record of what is about another, as an earlier medrec filed it.
fn as_filed_before(r: &mut Value) {
    r["fields"].as_object_mut().unwrap().remove("about");
    r.as_object_mut().unwrap().remove("given");
    r["key"] = own_key(r).into();
}

/// The key of one, as the person's own.
fn own_key(r: &Value) -> String {
    let key = r["key"].as_str().unwrap().strip_prefix("about|");
    let parts: Vec<&str> = key.unwrap().split('|').collect();
    parts[..4].join("|")
}

/// Nor where an import that filed the version that says so was cut short, and
/// the next serves one that does not.
#[test]
fn a_test_an_unfinished_import_says_is_of_another_is_none_of_the_persons() {
    let parent = json!({"reference": "RelatedPerson/rp-0001", "display": "Mother"});
    for own_first in [false, true] {
        let e = Env::new(&format!("about-cut-{own_first}"));
        if own_first {
            e.import(&test_about("two", None, 1));
        } else {
            e.import(&test_about("one", None, 1));
            e.edit(|v| {
                let records = v["records"].as_array_mut().unwrap();
                records.retain(|r| !r["display"].as_str().unwrap().contains("Hemoglobin"));
            });
        }
        e.import_cut_short(&test_about("one", Some(parent.clone()), 2));
        e.import(&test_about("one", None, 3));
        if !own_first {
            e.import(&test_about("two", None, 1));
        }
        let records = e.index().records;
        let of = records.iter().filter(|r| r.display.contains("Hemoglobin"));
        let (theirs, own): (Vec<&Record>, Vec<&Record>) =
            of.partition(|r| r.key.starts_with("about|"));
        assert_eq!((theirs.len(), own.len()), (1, 1), "{own_first}");
        for how in ["imported", "read again"] {
            let listed = e.ok(&["results", "--person", "Robin"]).to_string();
            assert!(listed.contains(&own[0].id), "{own_first}, {how}");
            assert!(!listed.contains(&theirs[0].id), "{own_first}, {how}");
            let list = e.ok(&["records", "about-another"]);
            assert_eq!(list["items"].as_array().unwrap().len(), 1, "{how}");
            e.ok(&["reindex"]);
        }
    }
}

/// What the person's own is called says nothing of whom it is about.
#[test]
fn a_test_called_about_something_is_the_persons() {
    for called in ["About how many servings a day", "about|result|x"] {
        let e = Env::new(&format!("about-called-{}", called.len()));
        let named = |version| {
            stream("one", |id, v| {
                if id == "obs-0001" {
                    say(v, "final", version);
                    v["code"] = json!({"text": called});
                }
                true
            })
        };
        e.import(&named(1));
        e.import(&named(1));
        e.import(&named(2));
        e.ok(&["reindex"]);
        let records = e.index().records;
        let of: Vec<&Record> = records.iter().filter(|r| r.display == called).collect();
        assert_eq!(of.len(), 1);
        assert!(of[0].given.is_empty(), "{:?}", of[0].given);
        let listed = e.ok(&["results", "--person", "Robin"]).to_string();
        assert!(listed.contains(&of[0].id), "{listed}");
        let list = e.ok(&["records", "about-another"]);
        assert!(list["items"].as_array().unwrap().is_empty(), "{list}");
    }
}

/// Two records of what is about another, of two sources, are one once each is
/// read again: with what either gave, and no resource twice.
#[test]
fn tests_about_another_read_again_are_one_record() {
    let parent = json!({"reference": "RelatedPerson/rp-0001", "display": "Mother"});
    let e = Env::new("about-join");
    e.import(&test_about("one", Some(parent.clone()), 1));
    let (id, _) = the_test(&e);
    e.edit(|v| {
        let mut records = v["records"].as_array_mut().unwrap().iter_mut();
        let r = records.find(|r| r["id"] == id.as_str()).unwrap();
        as_filed_before(r);
    });
    e.import(&test_about("two", Some(parent), 1));
    assert_eq!(e.ok(&["reindex"])["results_split"], 1);
    let (_, seen) = the_test(&e);
    assert!(!seen);
    let records = e.index().records;
    let of = records.iter().find(|r| r.display.contains("Hemoglobin"));
    let of = &of.unwrap().provenance;
    assert_eq!(of.len(), 2);
    assert_ne!(of[0].raw_path, of[1].raw_path);
}

/// Where two sources say so, it stays so once either says otherwise, or both.
#[test]
fn a_test_two_sources_say_is_of_another_person_stays_so() {
    let parent = json!({"reference": "RelatedPerson/rp-0001", "display": "Mother"});
    let seen = |e: &Env| {
        let lists = [
            e.ok(&["results", "--person", "Robin"]),
            e.ok(&["search", "hemoglobin"]),
            e.ok(&["timeline"]),
        ];
        let mut lists = lists.iter();
        lists.any(|v| v.to_string().contains("Hemoglobin"))
    };
    let count = |e: &Env| {
        let list = e.ok(&["records", "about-another"]);
        list["items"].as_array().unwrap().len()
    };
    for (n, first) in ["one", "two"].into_iter().enumerate() {
        let second = if first == "one" { "two" } else { "one" };
        let e = Env::new(&format!("about-sources-{n}"));
        for org in ["one", "two"] {
            e.import(&test_about(org, Some(parent.clone()), 1));
        }
        assert_eq!(count(&e), 1);
        for org in [first, second] {
            e.import(&test_about(org, None, 2));
            assert!(!seen(&e), "{first} {org}");
            e.ok(&["reindex"]);
            assert!(!seen(&e), "{first} {org}");
        }
        let kept = e.index().records.len();
        for org in [first, second] {
            e.import(&test_about(org, None, 2));
        }
        assert!(!seen(&e), "{first}");
        assert_eq!(e.index().records.len(), kept);
    }
}

/// The stream with a parent's test of the same code and day as the person's
/// own, before it or after it.
fn two_tests(parent_first: bool, ids: bool) -> String {
    let own = |l: &&str| l.contains("\"obs-0001\"") && l.contains("\"Observation\"");
    let line = medrec::testdata::STREAM.lines().find(own).unwrap();
    let mut mine: Value = serde_json::from_str(line).unwrap();
    let mut theirs = mine.clone();
    theirs["id"] = "obs-0001-parent".into();
    theirs["focus"] = json!([{"reference": "RelatedPerson/rp-0001", "display": "Mother"}]);
    theirs["valueQuantity"]["value"] = json!(9.9);
    if !ids {
        for v in [&mut mine, &mut theirs] {
            v.as_object_mut().unwrap().remove("id");
        }
    }
    let both = match parent_first {
        true => [theirs, mine],
        false => [mine, theirs],
    };
    let both = both.map(|v| v.to_string()).join("\n");
    let base = medrec::testdata::STREAM.replace(line, &both);
    stream_of(&base, "one", |_, _| true)
}

/// Both as an earlier medrec filed them: one record, with the first's value.
/// Whose raw file the parent's is.
fn one_record(index: &mut Value, parent_first: bool) -> Value {
    let records = index["records"].as_array_mut().unwrap();
    let at = records.iter().position(|r| !r["fields"]["about"].is_null());
    let mut theirs = records.remove(at.unwrap());
    as_filed_before(&mut theirs);
    let mut all = records.iter_mut();
    let mine = all.find(|r| r["key"] == theirs["key"]).unwrap();
    let of = theirs["provenance"].as_array_mut().unwrap().remove(0);
    let path = of["raw_path"].clone();
    let provenance = mine["provenance"].as_array_mut().unwrap();
    provenance.insert(if parent_first { 0 } else { 1 }, of);
    if parent_first {
        mine["fields"] = theirs["fields"].clone();
    }
    path
}

/// The person's own that leaves a record a parent's is in too takes nothing of
/// whom that one is about.
#[test]
fn a_test_that_leaves_a_parents_is_the_persons() {
    for parent_first in [true, false] {
        let e = Env::new(&format!("about-leaves-{parent_first}"));
        let both = two_tests(parent_first, true);
        e.import(&both);
        e.edit(|v| {
            let path = one_record(v, parent_first);
            let mut records = v["records"].as_array_mut().unwrap().iter_mut();
            let of = |r: &&mut Value| r["display"].as_str().unwrap().contains("Hemoglobin");
            let r = records.find(of).unwrap();
            let about = json!({"raw_path": path, "fields": {"about": "a parent"}});
            r["given"] = json!([about]);
        });
        // Its own source serves it of another day, and the parent's no more.
        let later = stream_of(&both, "one", |id, v| {
            if id == "obs-0001" {
                say(v, "final", 2);
                v["effectiveDateTime"] = "2026-03-01T09:00:00Z".into();
            }
            id != "obs-0001-parent"
        });
        e.import(&later);
        for how in ["imported", "read again"] {
            let v = e.ok(&["results", "--person", "Robin"]);
            let items = v["items"].as_array().unwrap().iter();
            let own = items.filter(|i| i.to_string().contains("Hemoglobin"));
            let own: Vec<String> = own.map(Value::to_string).collect();
            assert_eq!(own.len(), 1, "{how}, {parent_first}: {own:?}");
            assert!(own[0].contains("2026-03-01"), "{how}: {own:?}");
            assert!(!own[0].contains("9.9"), "{how}: {own:?}");
            let list = e.ok(&["records", "about-another"]);
            assert_eq!(list["items"].as_array().unwrap().len(), 1, "{how}");
            e.ok(&["reindex"]);
        }
    }
}

/// A parent's test is in a record of its own, whichever comes first, so the
/// person's is in their lists with its own value; and one an earlier medrec
/// filed with theirs goes to its own.
#[test]
fn a_parents_test_never_hides_the_persons_own() {
    for (n, (parent_first, ids)) in [(true, true), (false, true), (true, false), (false, false)]
        .into_iter()
        .enumerate()
    {
        let e = Env::new(&format!("about-two-{n}"));
        let check = |how: &str| {
            let of = format!("{how}, {parent_first}, {ids}");
            let v = e.ok(&["results", "--person", "Robin"]);
            let items = v["items"].as_array().unwrap().iter();
            let items = items.filter(|i| i.to_string().contains("Hemoglobin"));
            let items: Vec<String> = items.map(Value::to_string).collect();
            assert_eq!(items.len(), 1, "{of}: {items:?}");
            assert!(!items[0].contains("9.9"), "{of}: {items:?}");
            let list = e.ok(&["records", "about-another"]);
            let list = list["items"].as_array().unwrap();
            assert_eq!(list.len(), 1, "{of}");
            let shown = e.ok(&["records", "show", list[0]["id"].as_str().unwrap()]);
            assert!(shown.to_string().contains("9.9"), "{of}: {shown}");
            let records = e.index().records;
            let mut held = records.iter().flat_map(|r| &r.provenance);
            assert!(held.all(|p| p.raw_path.is_some()), "{of}");
        };
        let fold = || {
            e.edit(|v| drop(one_record(v, parent_first)));
            let list = e.ok(&["records", "about-another"]);
            assert_eq!(list["items"].as_array().unwrap().len(), 0);
        };
        e.import(&two_tests(parent_first, ids));
        check("imported");
        for own_first in [parent_first, !parent_first] {
            fold();
            e.import(&two_tests(own_first, ids));
            check("imported again");
        }
        fold();
        let dry = e.ok(&["reindex", "--dry-run"]);
        assert_eq!(dry["results_split"], 1);
        let about = &dry["people"][0]["views"]["about_another"];
        let want = serde_json::json!({"said": 1, "by_age": 0, "documents": 0});
        assert_eq!(*about, want, "{dry}");
        assert_eq!(e.ok(&["reindex"])["results_split"], 1);
        check("read again");
        assert_eq!(e.ok(&["reindex"])["results_split"], 0);
        let kept = e.index().records.len();
        e.import(&two_tests(parent_first, ids));
        assert_eq!(e.index().records.len(), kept);
        check("imported after");
    }
}

/// A test that is about a parent is still about them where the only other
/// resource of its record has no raw file.
#[test]
fn values_left_by_a_parents_test_are_never_the_persons() {
    let e = Env::new("about-left");
    e.import(&two_tests(true, true));
    e.edit(|v| {
        let records = v["records"].as_array_mut().unwrap();
        let at = records.iter().position(|r| !r["fields"]["about"].is_null());
        let mut theirs = records.remove(at.unwrap());
        let key = own_key(&theirs);
        let mine = records.iter_mut().find(|r| r["key"] == key).unwrap();
        let provenance = mine["provenance"].as_array_mut().unwrap();
        provenance[0].as_object_mut().unwrap().remove("raw_path");
        provenance.insert(0, theirs["provenance"].as_array_mut().unwrap().remove(0));
        theirs["fields"].as_object_mut().unwrap().remove("about");
        mine["fields"] = theirs["fields"].clone();
    });
    assert_eq!(e.ok(&["reindex"])["results_split"], 1);
    let v = e.ok(&["results", "--person", "Robin"]);
    assert!(!v.to_string().contains("Hemoglobin"), "{v}");
    let list = e.ok(&["records", "about-another", "--limit", "1"]);
    assert_eq!(list["items"].as_array().unwrap().len(), 1, "{list}");
    let list = e.ok(&["records", "about-another"]);
    assert_eq!(list["items"].as_array().unwrap().len(), 2, "{list}");
}
