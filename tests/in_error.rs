//! What a source says is entered in error (docs/data-contract.md, "Entered in
//! error"), through the binary, on synthetic data only.

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

    fn ok(&self, args: &[&str]) -> Value {
        let path = std::env::var("PATH").unwrap_or_default();
        let out = Command::cargo_bin("medrec")
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
            .unwrap();
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
        let r = self.ok(&[
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
        r["records_updated"].as_u64().unwrap()
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
        }
    });
    let path = e.raw("two", "Binary/bin-0002.json");
    let mut v: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    v["data"] = "not base64!".into();
    std::fs::write(&path, v.to_string()).unwrap();
    let r = e.ok(&["reindex"]);
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
