//! The whole flow, through the binary, on a local records folder with
//! synthetic data only (`medrec::testdata`): init, people, intake of scans
//! (duplicates, near-duplicates, review, the organize-scans hand-off),
//! imaging, an import from a fake provider, the views, notes, verify and
//! backup. Offline; no Keychain; no Drive.

use std::path::{Path, PathBuf};
use std::process::Output;

use assert_cmd::Command;
use serde_json::Value;

struct Env {
    root: PathBuf,
    path: String,
}

impl Env {
    fn new(tag: &str) -> Env {
        let root = std::env::temp_dir().join(format!("medrec-flow-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let path = medrec::testdata::no_viewer_path(&std::env::var("PATH").unwrap_or_default());
        let e = Env { root, path };
        e.ok(&["profiles", "add", "home"]);
        e
    }

    fn run(&self, args: &[&str]) -> Output {
        self.run_env(args, &[])
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
            .env("MEDREC_VIEWER", "none")
            .env_remove("NO_COLOR");
        for (k, v) in env {
            c.env(k, v);
        }
        c.args(args).output().unwrap()
    }

    /// `--json` run that must succeed; its document.
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
}

/// A test's `open` of a paper or a study launches nothing on the machine:
/// `MEDREC_VIEWER=none` holds it (`launched: false`), and the first `open` and
/// `xdg-open` on its `PATH` are the tests' own, which only say what they were
/// given. That file is a check at best: a viewer is started and not waited
/// for, so one could write after the read.
fn launches_nothing(e: &Env, args: &[&str]) {
    let launched = medrec::testdata::no_viewer_dir().join("launched");
    let before = std::fs::read_to_string(&launched).unwrap_or_default();
    let open = e.ok(args);
    assert_eq!(open["launched"], false, "{args:?}: {open}");
    let after = std::fs::read_to_string(&launched).unwrap_or_default();
    assert_eq!(before, after, "{args:?} ran a viewer");
    for name in ["open", "xdg-open"] {
        let found = Command::new("sh")
            .args(["-c", &format!("command -v {name}")])
            .env("PATH", &e.path)
            .output()
            .unwrap();
        let found = String::from_utf8_lossy(&found.stdout).trim().to_string();
        let ours = medrec::testdata::no_viewer_dir().join(name);
        assert_eq!(PathBuf::from(found), ours, "{name}");
    }
}

fn has(tool: &str) -> bool {
    medrec::proc::which(tool).is_some()
}

fn ids_of(v: &Value) -> Vec<String> {
    v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap().to_string())
        .collect()
}

fn setup(e: &Env) {
    let r = e.ok(&["init", e.records().to_str().unwrap()]);
    assert_eq!(r["status"], "created");
    assert_eq!(
        e.ok(&["init", e.records().to_str().unwrap()])["status"],
        "existing"
    );
    e.ok(&[
        "people",
        "add",
        "Alex Testperson",
        "--birth-date",
        "1990-01-01",
    ]);
    let robin = e.ok(&[
        "people",
        "add",
        "Robin Testperson",
        "--birth-date",
        "2020-02-02",
        "--guardian",
        "Alex",
    ]);
    assert_ne!(robin["adult"], true);
    assert_eq!(robin["guardians"].as_array().unwrap().len(), 1);
}

#[test]
fn init_refuses_a_folder_holding_other_files() {
    let e = Env::new("init");
    let d = e.root.join("busy");
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("taxes.pdf"), b"%PDF-1.4").unwrap();
    assert_eq!(e.run(&["init", d.to_str().unwrap()]).status.code(), Some(2));
    assert!(e
        .run(&["init", d.to_str().unwrap(), "--allow-other-files"])
        .status
        .success());
    let _ = std::fs::remove_dir_all(&e.root);
}

#[test]
fn scans_file_duplicates_are_caught_and_review_finishes_the_rest() {
    if !has("pdftotext") {
        eprintln!("skipped: pdftotext (poppler) is not installed");
        return;
    }
    let e = Env::new("scans");
    setup(&e);
    let scans = e.root.join("scans");
    medrec::testdata::write_scans(&scans);
    let f = |n: &str| scans.join(n).to_string_lossy().into_owned();
    let files = [
        "lab_robin.pdf",
        "lab_robin_copy.pdf",
        "lab_robin_rescan.pdf",
        "visit_alex.pdf",
        "eob_alex.pdf",
        "ambiguous.pdf",
    ];
    let mut args = vec!["intake".to_string()];
    args.extend(files.iter().map(|n| f(n)));
    let a: Vec<&str> = args.iter().map(String::as_str).collect();

    let dry = e.ok(&[&a[..], &["--dry-run"]].concat());
    assert_eq!(dry["counts"]["would_file"], 4, "{dry}");
    assert_eq!(dry["counts"]["duplicate"], 1);
    assert!(!e.records().join("Robin Testperson/Documents").exists());

    let r = e.ok(&a);
    assert_eq!(
        (
            r["counts"]["filed"].as_u64(),
            r["counts"]["duplicate"].as_u64(),
            r["counts"]["review"].as_u64()
        ),
        (Some(4), Some(1), Some(1)),
        "{r}"
    );
    let item = |src: &str| {
        r["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["source"] == src)
            .unwrap()
            .clone()
    };
    assert_eq!(item("lab_robin.pdf")["doc_type"], "lab_report");
    assert_eq!(item("lab_robin.pdf")["date"], "2026-03-04");
    assert_eq!(
        item("eob_alex.pdf")["path"],
        "Alex Testperson/Claims/2026/2026-02-10_eob_example_health_plan_eob_alex.pdf"
    );
    assert_eq!(
        item("lab_robin_rescan.pdf")["near_duplicates"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    // Named files are copied, never moved, unless --move.
    assert!(scans.join("lab_robin.pdf").exists());

    // The ambiguous page: review it, then file it by hand.
    let q = ids_of(&e.ok(&["review", "list"]));
    assert_eq!(q.len(), 1);
    assert_eq!(e.run(&["review", "file", &q[0]]).status.code(), Some(2));
    let filed = e.ok(&[
        "review",
        "file",
        &q[0],
        "--person",
        "Alex",
        "--type",
        "letter",
        "--date",
        "2026-01-20",
    ]);
    assert_eq!(filed["status"], "filed");
    assert!(e.records().join(filed["path"].as_str().unwrap()).exists());
    assert!(ids_of(&e.ok(&["review", "list"])).is_empty());

    // organize-scans hands over values medrec disagrees with: held.
    let variant = e.root.join("variant.pdf");
    let mut bytes = std::fs::read(scans.join("lab_robin.pdf")).unwrap();
    bytes.extend_from_slice(b"\n% variant\n");
    std::fs::write(&variant, bytes).unwrap();
    let v = variant.to_str().unwrap();
    let held = e.ok(&[
        "intake",
        v,
        "--person",
        "Alex",
        "--type",
        "letter",
        "--date",
        "2026-01-01",
        "--classified-by",
        "organize-scans",
        "--move",
    ]);
    let i = &held["items"][0];
    assert_eq!(i["status"], "review");
    assert!(
        i["reasons"].to_string().contains(
            "organize-scans said person `Alex Testperson`; medrec found `Robin Testperson`"
        ),
        "{i}"
    );
    assert!(
        !variant.exists(),
        "a moved file goes once it is held and read back"
    );
    let q = ids_of(&e.ok(&["review", "list"]));
    assert_eq!(e.run(&["review", "drop", &q[0]]).status.code(), Some(6));
    e.ok(&["review", "drop", &q[0], "--force"]);

    // The inbox is drained: files move.
    std::fs::copy(
        scans.join("visit_alex.pdf"),
        e.records().join("inbox/scan_0002.pdf"),
    )
    .unwrap();
    // A name with a backslash is a file's own.
    std::fs::copy(
        scans.join("visit_alex.pdf"),
        e.records().join("inbox/scan\\0003.pdf"),
    )
    .unwrap();
    let inbox = e.ok(&["intake", "--inbox"]);
    let items = inbox["items"].as_array().unwrap();
    assert_eq!(items.len(), 2, "{inbox}");
    assert!(items.iter().all(|i| i["status"] == "duplicate"), "{inbox}");
    assert!(!e.records().join("inbox/scan_0002.pdf").exists());
    assert!(
        !e.records().join("inbox/scan\\0003.pdf").exists(),
        "{inbox}"
    );

    let v = e.ok(&["verify"]);
    assert_eq!(v["missing"].as_array().unwrap().len(), 0);
    let _ = std::fs::remove_dir_all(&e.root);
}

#[test]
fn imaging_discs_add_only_new_images() {
    let e = Env::new("imaging");
    setup(&e);
    let dicom = e.root.join("dicom");
    medrec::testdata::write_dicom(&dicom);
    let cd = |n: &str| dicom.join(n).to_string_lossy().into_owned();
    let first = e.ok(&["images", "intake", &cd("cd1")]);
    assert_eq!(first["studies"][0]["status"], "filed");
    assert_eq!(first["studies"][0]["instances_new"], 3);
    let second = e.ok(&["images", "intake", &cd("cd2")]);
    assert_eq!(second["studies"][0]["status"], "added");
    assert_eq!(
        (
            second["studies"][0]["instances_new"].as_u64(),
            second["studies"][0]["instances_existing"].as_u64()
        ),
        (Some(3), Some(3))
    );
    let again = e.ok(&["images", "intake", &cd("cd1/DICOMDIR")]);
    assert_eq!(again["studies"][0]["status"], "unchanged");
    if dicom.join("ct.zip").exists() {
        let ct = e.ok(&["images", "intake", &cd("ct.zip")]);
        assert_eq!(ct["source"], "zip");
        assert_eq!(ct["studies"][0]["person_name"], "Alex Testperson");
    }
    let list = e.ok(&["images", "list", "--person", "Robin"]);
    let study = list["items"][0]["id"].as_str().unwrap().to_string();
    assert_eq!(list["items"][0]["instances"], 6);
    let open = e.ok(&["images", "open", &study, "--no-launch"]);
    assert_eq!(open["launched"], false);
    launches_nothing(&e, &["images", "open", &study]);
    let opened = PathBuf::from(open["path"].as_str().unwrap());
    assert!(opened.join("S002/00003.dcm").exists());
    let out = e.root.join("export");
    let ex = e.ok(&["images", "export", &study, "-o", out.to_str().unwrap()]);
    assert_eq!(ex["files"], 7);
    assert_eq!(
        e.run(&["images", "export", &study, "-o", out.to_str().unwrap()])
            .status
            .code(),
        Some(2)
    );
    let v = e.ok(&["verify"]);
    assert!(v["study_files"].as_u64().unwrap() >= 6);
    assert!(v["unindexed"].as_array().unwrap().is_empty(), "{v}");
    let _ = std::fs::remove_dir_all(&e.root);
}

/// `verify --events` finds an event and a record that say what none of their own
/// raw files does.
#[test]
fn verify_compares_events_with_their_raw_files() {
    let base = Env::new("verify-events");
    setup(&base);
    let e = provider_env(&base, medrec::testdata::STREAM);
    let import = ["import", "--from", "fakeprov", "--from-profile", "adult1"];
    e.ok(&[&import[..], &["--patient", "pat-0001", "--person", "Robin"]].concat());
    let path = e.records().join("Robin Testperson/index.json");
    let filed: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let visit = filed["records"].as_array().unwrap().iter();
    let at = visit.enumerate().find(|(_, r)| r["kind"] == "encounter");
    let (at_index, visit) = at.unwrap();
    let id = visit["id"].as_str().unwrap();
    let compared = || -> (Option<i32>, Value) {
        let out = e.run(&["--json", "verify", "--events"]);
        let v: Value = serde_json::from_slice(&out.stdout).unwrap();
        let people = v["people"].as_array().unwrap().iter();
        let some: Vec<&Value> = people.filter(|p| p["events"] != 0).collect();
        assert_eq!(some.len(), 1, "{v}");
        (out.status.code(), some[0].clone())
    };
    let (code, v) = compared();
    assert_eq!(code, Some(0), "{v}");
    assert!(v["records"].as_u64() > Some(1), "{v}");
    let plain = e.ok(&["verify"]);
    assert!(plain.get("people").is_none() && plain["index_unread"] == serde_json::json!([]));
    let event = e.ok(&["events", "list", "--person", "Robin"]);
    let event = event["items"][0]["id"].as_str().unwrap().to_string();
    const LISTS: [&str; 7] = [
        "place_not_given",
        "places_differ",
        "name_not_given",
        "day_not_given",
        "source_not_its_own",
        "resource_not_its_own",
        "unread",
    ];
    let raw = e
        .records()
        .join(visit["provenance"][0]["raw_path"].as_str().unwrap());
    let its = std::fs::read_to_string(&raw).unwrap();
    let other = its.replace("enc-0001", "enc-other");
    let other = other.replace("Example Health Clinic", "Elsewhere Clinic");
    let other = other.replace("Example Health", "Elsewhere Clinic");
    assert_ne!(other, its);
    std::fs::write(raw.with_file_name("enc-other.json"), &other).unwrap();
    type Change = fn(&mut Value);
    let cases: [(Change, &str); 8] = [
        (
            |r| r["fields"]["service_provider"] = "Elsewhere Clinic".into(),
            "place_not_given",
        ),
        (
            |r| {
                let mut p = r["provenance"][0].clone();
                let path = p["raw_path"].as_str().unwrap();
                let path = path.replace("enc-0001", "enc-other");
                p["raw_path"] = path.into();
                p["source_resource"]["id"] = "enc-other".into();
                r["provenance"].as_array_mut().unwrap().push(p);
            },
            "places_differ",
        ),
        (
            |r| r["display"] = "Elsewhere visit".into(),
            "name_not_given",
        ),
        (|r| r["date"] = "2001-01-01".into(), "day_not_given"),
        (
            |r| r["provenance"][0]["source"]["organization_id"] = "elsewhere".into(),
            "source_not_its_own",
        ),
        (
            |r| r["provenance"][0]["source_resource"]["id"] = "elsewhere".into(),
            "resource_not_its_own",
        ),
        // One of two files is not there: what only it may say is not
        // compared, the name, the day and the place.
        (
            |r| {
                let mut p = r["provenance"][0].clone();
                let path = p["raw_path"]
                    .as_str()
                    .unwrap()
                    .replace("enc-0001", "enc-gone");
                p["raw_path"] = path.into();
                r["provenance"].as_array_mut().unwrap().push(p);
                r["display"] = "Elsewhere visit".into();
                r["date"] = "2001-01-01".into();
                r["fields"]["service_provider"] = "Elsewhere Clinic".into();
            },
            "unread",
        ),
        // A path an edited index makes absolute is a file that is not there.
        (
            |r| {
                let path = r["provenance"][0]["raw_path"].as_str().unwrap();
                r["provenance"][0]["raw_path"] = format!("/{path}").into();
            },
            "unread",
        ),
    ];
    for (change, list) in cases {
        let mut index = filed.clone();
        change(&mut index["records"][at_index]);
        std::fs::write(&path, serde_json::to_string(&index).unwrap()).unwrap();
        let (code, v) = compared();
        assert_eq!(code, Some(1), "{list}: {v}");
        assert!(v.get("not_compared").is_none(), "{v}");
        for l in LISTS {
            let of = if l == "place_not_given" {
                event.as_str()
            } else {
                id
            };
            let want = if l == list { vec![of] } else { vec![] };
            assert_eq!(v[l], serde_json::json!(want), "{list}: {v}");
        }
    }
    std::fs::write(&path, serde_json::to_string(&filed).unwrap()).unwrap();
    std::fs::write(&raw, "{").unwrap();
    let (code, v) = compared();
    assert_eq!(
        (code, &v["unread"]),
        (Some(1), &serde_json::json!([id])),
        "{v}"
    );
    std::fs::write(&raw, &its).unwrap();
    assert_eq!(compared().0, Some(0));
    // A place no file gives but the source's name, and files whose place is a
    // records connection's or a room number: none of it is a difference.
    let resource: Value = serde_json::from_str(&its).unwrap();
    assert_eq!(resource["serviceProvider"]["display"], "Example Health");
    let at = |name: &str, place: Option<&str>| {
        let mut r = resource.clone();
        match place {
            Some(p) => r["serviceProvider"]["display"] = p.into(),
            None => drop(r.as_object_mut().unwrap().remove("serviceProvider")),
        }
        let text = serde_json::to_string(&r).unwrap();
        assert!(!text.contains("\"Example Health\""), "{name}");
        std::fs::write(raw.with_file_name(format!("{name}.json")), text).unwrap();
        let mut p = visit["provenance"][0].clone();
        let path = p["raw_path"].as_str().unwrap().replace("enc-0001", name);
        p["raw_path"] = path.into();
        p
    };
    let no_place = at("enc-noplace", None);
    let connection = at("enc-connection", Some("Example Health (all practices)"));
    let room = at("enc-room", Some("214"));
    let mut index = filed.clone();
    index["records"][at_index]["provenance"] = serde_json::json!([no_place]);
    std::fs::write(&path, serde_json::to_string(&index).unwrap()).unwrap();
    let (code, v) = compared();
    assert_eq!(code, Some(0), "by the source's name: {v}");
    let mut index = filed.clone();
    index["records"][at_index]["provenance"] = serde_json::json!([room, connection]);
    std::fs::write(&path, serde_json::to_string(&index).unwrap()).unwrap();
    let (code, v) = compared();
    assert_eq!(code, Some(0), "a connection and a room: {v}");
    // A person whose files do not read is not compared; the others are.
    std::fs::write(&path, serde_json::to_string(&filed).unwrap()).unwrap();
    let text = e.records().join("Robin Testperson/text.json");
    let kept = std::fs::read(&text).ok();
    std::fs::write(&text, "{").unwrap();
    let out = e.run(&["--json", "verify", "--events"]);
    let v: Value = serde_json::from_slice(&out.stdout).expect("one JSON document");
    assert_eq!(out.status.code(), Some(1), "{v}");
    let robin = v["people"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["not_compared"] == true);
    assert!(
        robin.is_some_and(|p| p["events"] == 0 && p["error_code"] == "other"),
        "{v}"
    );
    match kept {
        Some(t) => std::fs::write(&text, t).unwrap(),
        None => std::fs::remove_file(&text).unwrap(),
    }
    assert_eq!(compared().0, Some(0));
    // An index that does not read is in the report, and the rest is checked.
    std::fs::write(&path, "{").unwrap();
    let out = e.run(&["--json", "verify", "--events"]);
    let v: Value = serde_json::from_slice(&out.stdout).expect("one JSON document");
    assert_eq!(out.status.code(), Some(1), "{v}");
    let unread = v["index_unread"].as_array().unwrap();
    assert_eq!(unread.len(), 1, "{v}");
    let people = v["people"].as_array().unwrap();
    let robin = people.iter().find(|p| p["person"] == unread[0]);
    assert!(robin.is_some_and(|p| p["not_compared"] == true), "{v}");
    let said_by = e.run(&["verify", "--events"]);
    let said = String::from_utf8_lossy(&said_by.stdout).to_string();
    assert!(
        said.contains("index that does not read") && said.contains("not compared (other)"),
        "{said}"
    );
    let why = String::from_utf8_lossy(&said_by.stderr).to_string();
    assert!(
        why.contains("does not read") && why.contains("index.json"),
        "{why}"
    );
    std::fs::write(&path, serde_json::to_string(&filed).unwrap()).unwrap();
    assert_eq!(compared().0, Some(0));
    let out = e.run(&["verify", "--events"]);
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        text.contains("events of") && !text.contains("Clinic"),
        "{text}"
    );
    let _ = std::fs::remove_dir_all(&base.root);
}

fn provider_env(e: &Env, stream: &str) -> Env {
    let dir = medrec::testdata::write_provider(&e.root.join("provider"), stream);
    Env {
        root: e.root.clone(),
        path: format!("{}:{}", dir.display(), e.path),
    }
}

#[test]
fn an_import_files_records_and_documents_and_is_idempotent() {
    let base = Env::new("import");
    setup(&base);
    let e = provider_env(&base, medrec::testdata::STREAM);
    e.ok(&["config", "set", "providers", "fakeprov"]);
    let providers = e.ok(&["providers", "list"]);
    assert!(providers["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["cli"] == "fakeprov" && p["status"] == "ok"));

    // Whose records: required the first time, and the birth date must agree.
    assert_eq!(
        e.run(&[
            "import",
            "--from",
            "fakeprov",
            "--from-profile",
            "adult1",
            "--patient",
            "pat-0001"
        ])
        .status
        .code(),
        Some(2)
    );
    assert_eq!(
        e.run(&[
            "import",
            "--from",
            "fakeprov",
            "--from-profile",
            "adult1",
            "--patient",
            "pat-0001",
            "--person",
            "Alex"
        ])
        .status
        .code(),
        Some(2)
    );
    let r = e.ok(&[
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
    assert_eq!(r["resources"], 14);
    assert_eq!(r["records_added"], 9);
    assert_eq!(r["documents_added"], 2);
    assert_eq!(r["skipped"], 1);
    let raw = e
        .records()
        .join("Robin Testperson/Records/fakeprov-example-health");
    let obs = std::fs::read_to_string(raw.join("Observation/obs-0001.json")).unwrap();
    assert!(
        medrec::testdata::STREAM.contains(obs.trim_end()),
        "kept verbatim"
    );
    assert!(
        e.records()
            .join("Robin Testperson/Claims/2026")
            .read_dir()
            .unwrap()
            .count()
            == 1
    );

    let again = e.ok(&[
        "import",
        "--from",
        "fakeprov",
        "--from-profile",
        "adult1",
        "--patient",
        "pat-0001",
    ]);
    assert_eq!(
        (
            again["records_unchanged"].as_u64(),
            again["resources_written"].as_u64(),
            again["documents_added"].as_u64()
        ),
        (Some(9), Some(0), Some(0))
    );

    // A result corrected at the source, as a new version last updated
    // later: updated, the old version kept.
    let changed = medrec::testdata::STREAM
        .replace("\"value\":12.5", "\"value\":12.7")
        .replace(
            "\"id\":\"obs-0001\",\"meta\":{\"versionId\":\"1\",\"lastUpdated\":\"2026-03-05T11:00:00Z\"",
            "\"id\":\"obs-0001\",\"meta\":{\"versionId\":\"2\",\"lastUpdated\":\"2026-03-06T11:00:00Z\"",
        );
    let e2 = provider_env(&base, &changed);
    let upd = e2.ok(&[
        "import",
        "--from",
        "fakeprov",
        "--from-profile",
        "adult1",
        "--patient",
        "pat-0001",
    ]);
    assert_eq!(
        (
            upd["records_updated"].as_u64(),
            upd["resources_written"].as_u64()
        ),
        (Some(1), Some(1)),
        "{upd}"
    );
    let kept: Vec<String> = raw
        .join("Observation")
        .read_dir()
        .unwrap()
        .map(|d| d.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(kept.len(), 2, "{kept:?}");
    assert!(kept.iter().any(|k| k.starts_with("obs-0001~")));

    // A stream cut short writes nothing; a sign-in failure says how to sign in.
    let cut: String = changed.lines().take(5).map(|l| format!("{l}\n")).collect();
    let e3 = provider_env(&base, &cut);
    let before = std::fs::read_to_string(e.records().join("Robin Testperson/index.json")).unwrap();
    assert_eq!(
        e3.run(&[
            "import",
            "--from",
            "fakeprov",
            "--from-profile",
            "adult1",
            "--patient",
            "pat-0001"
        ])
        .status
        .code(),
        Some(5)
    );
    // A trailer that says the export was not complete: refused too.
    let partial = changed.replace("\"complete\":true", "\"complete\":false");
    assert_ne!(partial, changed);
    let e4 = provider_env(&base, &partial);
    assert_eq!(
        e4.run(&[
            "import",
            "--from",
            "fakeprov",
            "--from-profile",
            "adult1",
            "--patient",
            "pat-0001"
        ])
        .status
        .code(),
        Some(5)
    );
    let auth = e2.run_env(
        &[
            "import",
            "--from",
            "fakeprov",
            "--from-profile",
            "adult1",
            "--patient",
            "pat-0001",
            "--org",
            "example-health",
        ],
        &[("FAKEPROV_EXIT", "3")],
    );
    assert_eq!(auth.status.code(), Some(3));
    assert!(
        String::from_utf8_lossy(&auth.stderr).contains("fakeprov auth login --org example-health")
    );
    // A provider that names its own sign-in command: that one, once.
    let own = "fakeprov auth login --org example-health --label robin";
    let auth = e2.run_env(
        &[
            "import",
            "--from",
            "fakeprov",
            "--from-profile",
            "adult1",
            "--patient",
            "pat-0001",
        ],
        &[
            ("FAKEPROV_EXIT", "3"),
            (
                "FAKEPROV_STDERR",
                &format!("error: not signed in for this patient \u{2014} run `{own}`"),
            ),
        ],
    );
    assert_eq!(auth.status.code(), Some(3));
    let stderr = String::from_utf8_lossy(&auth.stderr);
    assert!(
        stderr.contains(&format!("run `{own}`, then import again")),
        "{stderr}"
    );
    assert_eq!(stderr.matches("auth login").count(), 1, "{stderr}");
    assert_eq!(
        std::fs::read_to_string(e.records().join("Robin Testperson/index.json")).unwrap(),
        before
    );

    // The views read what the import brought.
    let tl = e.ok(&["timeline", "--person", "Robin"]);
    let kinds: Vec<&str> = tl["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["kind"].as_str().unwrap())
        .collect();
    for k in [
        "encounter",
        "result",
        "report",
        "condition",
        "medication",
        "allergy",
        "immunization",
        "claim",
        "coverage",
        "document",
    ] {
        assert!(kinds.contains(&k), "timeline lacks {k}: {kinds:?}");
    }
    assert_eq!(e.ok(&["results"])["items"].as_array().unwrap().len(), 2);
    assert_eq!(
        e.ok(&["results", "--code", "718-7"])["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        e.ok(&["results", "--code", "hemoglobin"])["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(e.ok(&["results", "--code", "2345-7"])["items"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(
        e.ok(&["meds", "--active"])["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(e.ok(&["conditions"])["items"][0]["code"], "J45.909");
    assert_eq!(e.ok(&["allergies"])["items"].as_array().unwrap().len(), 1);
    assert_eq!(
        e.ok(&["immunizations"])["items"].as_array().unwrap().len(),
        1
    );
    let claims = e.ok(&["timeline", "--kind", "claim"]);
    assert_eq!(
        claims["items"][0]["detail"], "billed 150.0 USD, plan paid 110.0 USD, your share 25.0 USD",
        "the payer gives no member liability, so the share is its parts"
    );
    let hits = e.ok(&["search", "hemo"]);
    assert!(hits["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|h| h["kind"] == "result"));
    let eobs = e.ok(&["documents", "list", "--type", "eob"]);
    assert_eq!(eobs["items"].as_array().unwrap().len(), 1);
    let doc = eobs["items"][0]["id"].as_str().unwrap().to_string();
    launches_nothing(&e, &["documents", "open", &doc]);
    let open = e.ok(&["documents", "open", &doc, "--no-launch"]);
    let copy = PathBuf::from(open["path"].as_str().unwrap());
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(&copy).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::metadata(copy.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );

    // Notes.
    let rec = e.ok(&["results"])["items"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    e.ok(&[
        "note",
        "add",
        "Robin",
        "Recheck the hemoglobin at the next visit.",
        "--tag",
        "follow-up",
        "--about",
        &rec,
    ]);
    assert_eq!(
        e.run(&[
            "note",
            "add",
            "Robin",
            "x",
            "--about",
            "d_aaaaaaaaaaaaaaaaaaaa"
        ])
        .status
        .code(),
        Some(4)
    );
    let notes = e.ok(&["note", "list", "--tag", "follow-up"]);
    assert_eq!(notes["items"].as_array().unwrap().len(), 1);
    assert!(
        std::fs::read_to_string(e.records().join("Robin Testperson/notes.md"))
            .unwrap()
            .contains("Recheck the hemoglobin")
    );
    assert!(e.ok(&["search", "recheck"])["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|h| h["kind"] == "note"));

    // Verify catches a changed file; backup copies and checks everything.
    assert_eq!(
        e.ok(&["verify", "--deep"])["changed"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    let eob_path = e.records().join(eobs["items"][0]["path"].as_str().unwrap());
    let original = std::fs::read(&eob_path).unwrap();
    std::fs::write(&eob_path, b"%PDF-1.4 tampered").unwrap();
    let bad = e.run(&["--json", "verify"]);
    assert_eq!(bad.status.code(), Some(1));
    std::fs::write(&eob_path, original).unwrap();
    let b = e.ok(&["backup", "--to", e.root.join("backup").to_str().unwrap()]);
    assert_eq!(b["files"], b["verified"]);
    assert!(b["mismatched"].as_array().unwrap().is_empty());
    let shown = e.ok(&["people", "show", "Robin"]);
    assert_eq!(shown["documents"], 2);
    assert_eq!(shown["sources"].as_array().unwrap().len(), 1);
    let _ = std::fs::remove_dir_all(Path::new(&e.root));
}

#[test]
fn a_local_folder_is_owner_only_and_lists_take_a_limit() {
    use std::os::unix::fs::PermissionsExt;
    let e = Env::new("modes");
    setup(&e);
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&e.records()), 0o700);
    assert_eq!(mode(&e.records().join("household.json")), 0o600);

    assert_eq!(
        e.ok(&["people", "list"])["items"].as_array().unwrap().len(),
        2
    );
    let one = e.ok(&["people", "list", "--limit", "1"]);
    assert_eq!(one["items"].as_array().unwrap().len(), 1);

    // A backup into a folder others can read ends owner-only, files it
    // already held included.
    let target = e.root.join("backup");
    std::fs::create_dir_all(target.join("old")).unwrap();
    std::fs::write(target.join("old/left.txt"), b"x").unwrap();
    let set =
        |p: &Path, m: u32| std::fs::set_permissions(p, std::fs::Permissions::from_mode(m)).unwrap();
    set(&target, 0o755);
    set(&target.join("old"), 0o755);
    set(&target.join("old/left.txt"), 0o644);
    let b = e.ok(&["backup", "--to", target.to_str().unwrap()]);
    assert_eq!(b["files"], b["verified"]);
    let mut open = Vec::new();
    let mut stack = vec![target.clone()];
    while let Some(d) = stack.pop() {
        if mode(&d) != 0o700 {
            open.push(d.clone());
        }
        for f in std::fs::read_dir(&d).unwrap().flatten() {
            if f.file_type().unwrap().is_dir() {
                stack.push(f.path());
            } else if mode(&f.path()) != 0o600 {
                open.push(f.path());
            }
        }
    }
    assert!(open.is_empty(), "not owner-only: {open:?}");

    let folder_mode = |e: &Env| -> String {
        let out = e.run(&["--json", "doctor"]);
        let v: Value = serde_json::from_slice(&out.stdout).expect("one JSON document");
        v["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["check"] == "folder mode")
            .map(|c| c["status"].as_str().unwrap().to_string())
            .expect("a folder mode check")
    };
    assert_eq!(folder_mode(&e), "ok");
    std::fs::set_permissions(e.records(), std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(folder_mode(&e), "warn");
    let _ = std::fs::remove_dir_all(Path::new(&e.root));
}

/// `doctor` names the `open` commands a `none` viewer holds, and where the
/// viewer is from. It starts no viewer.
#[test]
fn doctor_says_which_open_a_none_viewer_holds() {
    let e = Env::new("doctor-viewer");
    setup(&e);
    let viewer = |env: &[(&str, &str)]| -> String {
        let out = e.run_env(&["--json", "doctor"], env);
        let v: Value = serde_json::from_slice(&out.stdout).expect("one JSON document");
        let checks = v["checks"].as_array().unwrap();
        let c = checks.iter().find(|c| c["check"] == "viewer");
        c.expect("a viewer check")["detail"]
            .as_str()
            .unwrap()
            .to_string()
    };
    // The harness's MEDREC_VIEWER=none holds both, and says so.
    let both = viewer(&[]);
    assert!(
        both.contains("`images open` and `documents open`"),
        "{both}"
    );
    assert!(both.contains("MEDREC_VIEWER"), "{both}");
    // `config viewer none` holds studies alone.
    e.ok(&["config", "set", "viewer", "none"]);
    let studies = viewer(&[("MEDREC_VIEWER", "")]);
    assert!(
        studies.contains("papers still open in the system's viewer"),
        "{studies}"
    );
    assert!(!studies.contains("MEDREC_VIEWER"), "{studies}");
    let _ = std::fs::remove_dir_all(Path::new(&e.root));
}

#[test]
fn a_reimport_names_the_patient_by_the_providers_label() {
    let base = Env::new("label");
    setup(&base);
    let e = provider_env(&base, medrec::testdata::STREAM);
    let first = [
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
    assert_eq!(e.ok(&first)["records_added"], 9);
    // The synthetic header labels its patient "Robin Testperson".
    let by_label = [
        "import",
        "--from",
        "fakeprov",
        "--from-profile",
        "adult1",
        "--patient",
        "Robin Testperson",
    ];
    let again = e.ok(&by_label);
    assert_eq!(
        (
            again["records_unchanged"].as_u64(),
            again["resources_written"].as_u64()
        ),
        (Some(9), Some(0)),
        "{again}"
    );
    // The label now answering with another patient's records: refused, and
    // nothing written.
    let other = medrec::testdata::STREAM.replacen(
        "\"patient\":{\"id\":\"pat-0001\"",
        "\"patient\":{\"id\":\"pat-0002\"",
        1,
    );
    assert_ne!(other, medrec::testdata::STREAM);
    let before = std::fs::read_to_string(e.records().join("household.json")).unwrap();
    let e2 = provider_env(&base, &other);
    let out = e2.run(&[
        "--json",
        "import",
        "--from",
        "fakeprov",
        "--from-profile",
        "adult1",
        "--patient",
        "Robin Testperson",
    ]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stdout).contains("has not imported as"));
    assert_eq!(
        std::fs::read_to_string(e.records().join("household.json")).unwrap(),
        before
    );
    let _ = std::fs::remove_dir_all(Path::new(&e.root));
}

/// Sources that a CLI, an organization and a FHIR base do not tell apart:
/// each person's records have the name of the source they came from, and no
/// other's, whichever was imported first.
#[test]
fn sources_with_one_key_keep_their_own_names() {
    let base = Env::new("shared-key");
    setup(&base);
    let e = provider_env(&base, medrec::testdata::STREAM);
    medrec::testdata::write_shared_key(&base.root.join("provider"));
    let streams = medrec::testdata::shared_key_streams();
    for (patient, person, _, _) in &streams {
        e.ok(&[
            "import",
            "--from",
            "fakeprov",
            "--from-profile",
            "adult1",
            "--patient",
            patient,
            "--person",
            person,
        ]);
    }
    let people = e.ok(&["people", "list"]);
    let views = e.records().join("views");
    let read = |path: PathBuf| -> Value {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    };
    let index = read(views.join("views.json"));
    for who in ["Alex", "Robin"] {
        let person = people["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["name"] == format!("{who} Testperson"))
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        let view = |name: &str| {
            let md5 = index["people"][&person]["files"][name].as_str().unwrap();
            read(
                views
                    .join(&person)
                    .join(medrec::viewfiles::stored_name(name, md5)),
            )
        };
        let own: Vec<(&str, &str)> = streams
            .iter()
            .filter(|(_, p, _, _)| *p == who)
            .map(|(patient, _, name, _)| (*patient, *name))
            .collect();
        assert_eq!(own.len(), 2);

        // Each visit has its source's name, and the reading of its source.
        let events = view("events.json");
        for (patient, name) in &own {
            let reading = format!("Reading at {patient}");
            let visits: Vec<&Value> = events["events"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|ev| {
                    let mut members = ev["members"].as_array().unwrap().iter();
                    members.any(|m| m["title"] == *reading)
                })
                .collect();
            assert_eq!(visits.len(), 1, "{events}");
            assert_eq!(visits[0]["place"], *name, "{events}");
            let titles = visits[0]["members"].as_array().unwrap().iter();
            let readings = titles.filter(|m| m["title"].as_str().unwrap().starts_with("Reading"));
            assert_eq!(readings.count(), 1, "{events}");
        }

        // The same names on the record lists, the timeline and the views.
        let mut all: Vec<&str> = own.iter().map(|(_, name)| *name).collect();
        all.sort();
        let names = |v: &Value, at: &str| -> Vec<String> {
            let mut named: Vec<String> = v
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r.pointer(at).unwrap().as_str().unwrap().to_string())
                .collect();
            named.sort();
            named
        };
        let rows = e.ok(&["results", "--person", who]);
        assert_eq!(names(&rows["items"], "/sources/0"), all, "{rows}");
        let visits = e.ok(&["timeline", "--person", who, "--kind", "encounter"]);
        assert_eq!(names(&visits["items"], "/source"), all, "{visits}");
        assert_eq!(
            names(
                &view("records-encounter-0.json"),
                "/provenance/0/source/label"
            ),
            all
        );
        assert_eq!(names(&view("summary.json")["sources"], "/source"), all);
    }
    let _ = std::fs::remove_dir_all(&e.root);
}

#[test]
fn ids_past_64_characters_and_absolute_references_are_imported() {
    // Epic's unconstrained FHIR ids run past FHIR R4's 64 characters.
    let doc = "D0c-".repeat(50);
    let bin = "B1n.".repeat(50);
    let base_url = "https://fhir.example.org/api/FHIR/R4";
    let stream = medrec::testdata::STREAM
        .replace(
            "\"DocumentReference/doc-0002\"",
            &format!("\"{base_url}/DocumentReference/{doc}/_history/1\""),
        )
        .replace(
            "\"Binary/bin-0002\"",
            &format!("\"{base_url}/Binary/{bin}\""),
        )
        .replace("doc-0002", &doc)
        .replace("bin-0002", &bin);
    assert_eq!(stream.matches(&format!("{base_url}/")).count(), 2);

    let base = Env::new("long-ids");
    setup(&base);
    let e = provider_env(&base, &stream);
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
    let r = e.ok(&import);
    assert_eq!(
        (
            r["resources"].as_u64(),
            r["records_added"].as_u64(),
            r["documents_added"].as_u64(),
            r["skipped"].as_u64()
        ),
        (Some(14), Some(9), Some(2), Some(1)),
        "{r}"
    );
    // The EOB's document is found through its absolute, versioned reference.
    assert_eq!(
        e.records()
            .join("Robin Testperson/Claims/2026")
            .read_dir()
            .unwrap()
            .count(),
        1
    );
    let raw = e
        .records()
        .join("Robin Testperson/Records/fakeprov-example-health/DocumentReference");
    let names: Vec<String> = raw
        .read_dir()
        .unwrap()
        .map(|d| d.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names.len(), 2, "{names:?}");
    assert!(names.iter().all(|n| n.len() <= 64), "{names:?}");
    assert!(names.iter().any(|n| n.starts_with("id-")), "{names:?}");

    let again = e.ok(&import);
    assert_eq!(
        (
            again["resources_written"].as_u64(),
            again["documents_added"].as_u64()
        ),
        (Some(0), Some(0)),
        "{again}"
    );
    let v = e.ok(&["verify", "--deep"]);
    assert_eq!(v["ok"], 11, "{v}");
    assert!(
        [&v["missing"], &v["changed"], &v["unindexed"]]
            .iter()
            .all(|a| a.as_array().is_some_and(|a| a.is_empty())),
        "{v}"
    );
    let _ = std::fs::remove_dir_all(Path::new(&e.root));
}

#[test]
fn the_birth_date_checks_have_their_own_flag() {
    let base = Env::new("birth");
    setup(&base);
    let e = provider_env(&base, medrec::testdata::STREAM);
    let stdout = |args: &[&str]| -> (Option<i32>, Value) {
        let mut a = vec!["--json"];
        a.extend_from_slice(args);
        let out = e.run(&a);
        (
            out.status.code(),
            serde_json::from_slice(&out.stdout).expect("one JSON document"),
        )
    };
    // The synthetic source's patient is Robin's age, not Alex's.
    let import = [
        "import",
        "--from",
        "fakeprov",
        "--from-profile",
        "adult1",
        "--patient",
        "pat-0001",
        "--person",
        "Alex",
    ];
    let (code, v) = stdout(&import);
    assert_eq!(code, Some(2));
    assert!(v.to_string().contains("--allow-birth-date-mismatch"), "{v}");
    let mut allowed = import.to_vec();
    allowed.push("--allow-birth-date-mismatch");
    assert_eq!(e.ok(&allowed)["records_added"], 9);

    let dicom = e.root.join("dicom");
    medrec::testdata::write_dicom(&dicom);
    let cd1 = dicom.join("cd1").to_string_lossy().into_owned();
    let (_, v) = stdout(&["images", "intake", &cd1, "--person", "Alex"]);
    assert_eq!(v["studies"][0]["status"], "skipped");
    let v = e.ok(&[
        "images",
        "intake",
        &cd1,
        "--person",
        "Alex",
        "--allow-birth-date-mismatch",
        "--dry-run",
    ]);
    assert_eq!(v["studies"][0]["status"], "would_file");
    let _ = std::fs::remove_dir_all(Path::new(&e.root));
}

#[test]
fn a_scan_under_tmp_is_read_by_ocr() {
    if !has("tesseract") || !has("pdftoppm") {
        eprintln!("skipped: tesseract or pdftoppm is not installed");
        return;
    }
    let e = Env::new("ocr-tmp");
    setup(&e);
    let scans = e.root.join("scans");
    medrec::testdata::write_scans(&scans);
    let png = scans.join("scan_visit_alex.png");
    // A literal /tmp path: on macOS, the image library tesseract reads with
    // rewrites a leading /tmp to the per-user temporary directory.
    let under_tmp = PathBuf::from(format!("/tmp/medrec-flow-ocr-{}.png", std::process::id()));
    std::fs::copy(&png, &under_tmp).unwrap();
    let r = e.ok(&["intake", under_tmp.to_str().unwrap()]);
    let _ = std::fs::remove_file(&under_tmp);
    assert_eq!(r["items"][0]["status"], "filed", "{r}");
    let _ = std::fs::remove_dir_all(Path::new(&e.root));
}

#[test]
fn a_type_the_source_withheld_is_reported_as_possibly_incomplete() {
    // A search answered with an issue saying it left results out: one
    // `forbidden` skip per type, with neither id nor search.
    let withheld = concat!(
        r#"{"resource_type":"Condition","reason":"some categories were left out","code":"forbidden"},"#,
        r#"{"resource_type":"Observation","reason":"some categories were left out","code":"forbidden"},"#,
        // A reason and a type carrying escape sequences and a newline, and
        // a reason that is only whitespace.
        r#"{"resource_type":"Proc\u001b]0;x\u0007edure","reason":"\u001b[2Jcleared\nscreen\u202e","code":"forbidden"},"#,
        r#"{"resource_type":"Immunization","reason":"  ","code":"forbidden"}"#
    );
    let stream =
        medrec::testdata::STREAM.replace("\"skipped\":[", &format!("\"skipped\":[{withheld},"));
    assert_ne!(stream, medrec::testdata::STREAM);

    let base = Env::new("withheld");
    setup(&base);
    let e = provider_env(&base, &stream);
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
    let out = e.run(&import);
    assert_eq!(out.status.code(), Some(0));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("1 other resource or search was skipped")
            && text.contains(concat!(
                "withheld some resources of these types, so their records may be incomplete:\n",
                "  Condition, Observation: some categories were left out\n",
                "  Proc]0;xedure: [2Jclearedscreen\n",
                "  Immunization: no reason given\n"
            )),
        "{text}"
    );
    assert!(
        !text
            .chars()
            .any(|c| c.is_control() && c != '\n' || c == '\u{202e}'),
        "{text:?}"
    );
    // The Condition it did serve is filed, and a re-import says the same.
    let r = e.ok(&import);
    assert_eq!(
        r["withheld_types"],
        serde_json::json!(["Condition", "Immunization", "Observation", "Proc]0;xedure"]),
        "{r}"
    );
    assert_eq!(
        r["withheld"],
        serde_json::json!([
            {"reason": "some categories were left out", "resource_types": ["Condition", "Observation"]},
            {"reason": "[2Jclearedscreen", "resource_types": ["Proc]0;xedure"]},
            {"reason": "", "resource_types": ["Immunization"]}
        ])
    );
    assert_eq!(
        (r["skipped"].as_u64(), r["records_unchanged"].as_u64()),
        (Some(5), Some(9))
    );
    let _ = std::fs::remove_dir_all(&e.root);
}

#[test]
fn a_reports_and_a_medias_binary_are_filed_with_their_date_and_title() {
    use base64::Engine;
    let b64 = |b: Vec<u8>| base64::engine::general_purpose::STANDARD.encode(b);
    let report_pdf = b64(medrec::pdfgen::text_pdf(&["Example Health", "CBC report"]));
    let media_pdf = b64(medrec::pdfgen::text_pdf(&[
        "Example Health",
        "Synthetic photo page",
    ]));
    let stream = medrec::testdata::STREAM
        .replace(
            "\"conclusion\":",
            r#""category":[{"coding":[{"system":"http://terminology.hl7.org/CodeSystem/v2-0074","code":"LAB"}]}],"presentedForm":[{"contentType":"application/pdf","url":"Binary/bin-0003"}],"conclusion":"#,
        )
        .replace(
            "{\"schema\":\"records-stream-end/v1\"",
            &format!(
                concat!(
                    r#"{{"resourceType":"Binary","id":"bin-0003","contentType":"application/pdf","data":"{}"}}"#, "\n",
                    r#"{{"resourceType":"Media","id":"med-0001","status":"completed","subject":{{"reference":"Patient/pat-0001"}},"createdDateTime":"2026-03-01T08:00:00Z","content":{{"contentType":"application/pdf","url":"Binary/bin-0004","title":"Synthetic photo"}}}}"#, "\n",
                    r#"{{"resourceType":"Binary","id":"bin-0004","contentType":"application/pdf","data":"{}"}}"#, "\n",
                    r#"{{"schema":"records-stream-end/v1""#
                ),
                report_pdf, media_pdf
            ),
        )
        .replace("\"Binary\":2", "\"Binary\":4,\"Media\":1");
    assert_eq!(
        stream.lines().count(),
        medrec::testdata::STREAM.lines().count() + 3
    );

    let base = Env::new("report-media");
    setup(&base);
    let e = provider_env(&base, &stream);
    let r = e.ok(&[
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
    assert_eq!(
        (r["resources"].as_u64(), r["documents_added"].as_u64()),
        (Some(17), Some(4)),
        "{r}"
    );

    let labs = e.ok(&["documents", "list", "--type", "lab_report"]);
    let labs = labs["items"].as_array().unwrap();
    assert_eq!(labs.len(), 1);
    assert_eq!(
        (labs[0]["date"].as_str(), labs[0]["title"].as_str()),
        (Some("2026-03-04"), Some("CBC"))
    );
    let other = e.ok(&["documents", "list", "--type", "other"]);
    let media: Vec<&Value> = other["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|d| d["title"] == "Synthetic photo")
        .collect();
    assert_eq!(media.len(), 1, "{other}");
    assert_eq!(media[0]["date"], "2026-03-01");

    let index: Value = serde_json::from_str(
        &std::fs::read_to_string(e.records().join("Robin Testperson/index.json")).unwrap(),
    )
    .unwrap();
    let mut sources: Vec<&str> = index["documents"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|d| d["provenance"]["source_resource"]["type"].as_str())
        .collect();
    sources.sort_unstable();
    assert_eq!(
        sources,
        [
            "DiagnosticReport",
            "DocumentReference",
            "DocumentReference",
            "Media"
        ]
    );
    let _ = std::fs::remove_dir_all(&e.root);
}

/// An import of `pat-0001` as Robin whose provider holds its export open
/// until `<hold>/go` exists; it returns once the export has started.
fn held_import(e: &Env, hold: &Path) -> std::process::Child {
    std::fs::create_dir_all(hold).unwrap();
    let mut c = std::process::Command::new(env!("CARGO_BIN_EXE_medrec"));
    c.env("XDG_CONFIG_HOME", e.root.join("config"))
        .env("TZ", "America/New_York")
        .env("XDG_CACHE_HOME", e.root.join("cache"))
        .env("HOME", &e.root)
        .env("PATH", &e.path)
        .env("MEDREC_PROFILE", "home")
        .env("MEDREC_KEYCHAIN_SERVICE", "piekstra.medrec.hermetic-tests")
        .env("MEDREC_VIEWER", "none")
        .env("FAKEPROV_HOLD", hold)
        .args([
            "--json",
            "import",
            "--from",
            "fakeprov",
            "--from-profile",
            "adult1",
            "--patient",
            "pat-0001",
            "--person",
            "Robin",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let child = c.spawn().unwrap();
    let started = std::time::Instant::now();
    while !hold.join("exporting").exists() {
        assert!(started.elapsed().as_secs() < 30, "the export never started");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    child
}

#[test]
fn an_import_takes_the_folder_lock_only_after_the_export() {
    let base = Env::new("import-lock");
    setup(&base);
    let e = provider_env(&base, medrec::testdata::STREAM);
    let hold = e.root.join("hold");
    let import = held_import(&e, &hold);
    // While the provider exports, other writers have the folder, and one
    // changes the household the import read before its export.
    let during = std::time::Instant::now();
    e.ok(&[
        "people",
        "add",
        "Sam Testperson",
        "--birth-date",
        "1991-03-03",
    ]);
    e.ok(&["note", "add", "Robin", "Written while an export ran."]);
    assert!(
        during.elapsed().as_secs() < 10,
        "a writer waited on the import's export"
    );
    std::fs::write(hold.join("go"), "").unwrap();
    let out = import.wait_with_output().unwrap();
    let (code, text) = said(&out);
    assert_eq!(code, Some(0), "{text}");
    let r: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(r["records_added"].as_u64().unwrap() > 0, "{r}");
    // The household holds both: the person added during the export, and the
    // source the import recorded.
    let h: Value =
        serde_json::from_slice(&std::fs::read(e.records().join("household.json")).unwrap())
            .unwrap();
    assert!(h["people"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["name"] == "Sam Testperson"));
    assert!(h["sources"]
        .as_array()
        .unwrap()
        .iter()
        .any(|s| s["cli"] == "fakeprov"));
    e.ok(&["verify"]);
    let _ = std::fs::remove_dir_all(&e.root);
}

#[test]
fn an_import_refuses_a_folder_replaced_during_its_export() {
    let base = Env::new("import-replaced");
    setup(&base);
    let e = provider_env(&base, medrec::testdata::STREAM);
    let hold = e.root.join("hold");
    let import = held_import(&e, &hold);
    // The records folder is replaced at the same place, with the same person.
    std::fs::remove_dir_all(e.records()).unwrap();
    e.ok(&["init", e.records().to_str().unwrap()]);
    e.ok(&[
        "people",
        "add",
        "Robin Testperson",
        "--birth-date",
        "2020-02-02",
    ]);
    std::fs::write(hold.join("go"), "").unwrap();
    let (code, text) = said(&import.wait_with_output().unwrap());
    assert_eq!(code, Some(1), "{text}");
    assert!(
        text.contains("was replaced while this command ran"),
        "{text}"
    );
    // Nothing went into the new folder.
    assert!(!std::fs::read_dir(e.records())
        .unwrap()
        .flatten()
        .any(|d| d.file_name().to_string_lossy().contains("example-health")));
    let records = e.ok(&["timeline", "--person", "Robin"]);
    assert_eq!(records["items"].as_array().unwrap().len(), 0);
    let _ = std::fs::remove_dir_all(&e.root);
}

/// `medrec <args>` output, stdout and stderr together, and its exit code.
fn said(out: &Output) -> (Option<i32>, String) {
    (
        out.status.code(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

#[test]
fn an_import_runs_through_the_providers_profile() {
    let base = Env::new("import-profile");
    setup(&base);
    let e = provider_env(&base, medrec::testdata::STREAM);
    let import = |extra: &[&str], env: &[(&str, &str)]| {
        let mut a = vec![
            "import",
            "--from",
            "fakeprov",
            "--patient",
            "pat-0001",
            "--person",
            "Robin",
        ];
        a.extend_from_slice(extra);
        said(&e.run_env(&a, env))
    };
    let args = || std::fs::read_to_string(e.root.join("provider/export-args")).unwrap_or_default();

    // No --from-profile: exit 2 listing the provider's profiles, before the
    // export runs.
    let (code, text) = import(&[], &[]);
    assert_eq!(code, Some(2), "{text}");
    assert!(text.contains("--from-profile <NAME>"), "{text}");
    assert!(
        text.contains("fakeprov's profiles: adult1, adult2"),
        "{text}"
    );
    assert_eq!(args(), "");
    // A name that is not a profile name is refused as clap parses it.
    assert_eq!(import(&["--from-profile", "Adult1"], &[]).0, Some(2));
    // The provider refuses a profile it does not have.
    let (code, text) = import(&["--from-profile", "adult3"], &[]);
    assert_eq!(code, Some(2), "{text}");
    assert!(text.contains("adult1, adult2"), "{text}");

    // The named profile goes to the export as --profile.
    let v = e.ok(&[
        "import",
        "--from",
        "fakeprov",
        "--from-profile",
        "adult2",
        "--patient",
        "pat-0001",
        "--person",
        "Robin",
    ]);
    assert_eq!(v["records_added"], 9);
    assert!(args().contains("--profile adult2"), "{}", args());
    // The provider's profile name stays in this machine's config: none of
    // it reaches the records folder.
    let mut stack = vec![e.records()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).unwrap() {
            let p = entry.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else if let Ok(t) = std::fs::read_to_string(&p) {
                assert!(!t.contains("adult2"), "{}", p.display());
            }
        }
    }

    // A provider that signs in without listing login-profiles predates them:
    // refused rather than run unprofiled.
    let (code, text) = import(
        &["--from-profile", "adult1"],
        &[("FAKEPROV_INFO", "unprofiled")],
    );
    assert_eq!(code, Some(2), "{text}");
    assert!(text.contains("predates login profiles"), "{text}");
    let list = e.ok(&["config", "set", "providers", "fakeprov"]);
    assert!(list.is_object());
    let out = e.run_env(
        &["--json", "providers", "list"],
        &[("FAKEPROV_INFO", "unprofiled")],
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let row = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["cli"] == "fakeprov")
        .unwrap()
        .clone();
    assert_eq!(
        (&row["status"], &row["login_profiles"]),
        (&"outdated".into(), &false.into())
    );

    // One that needs no login takes no profile, and gets none.
    let no_login = [("FAKEPROV_INFO", "no-login")];
    assert_eq!(import(&["--from-profile", "adult1"], &no_login).0, Some(2));
    let (code, text) = import(&[], &no_login);
    assert_eq!(code, Some(0), "{text}");
    assert!(!args().contains("--profile"), "{}", args());
    let _ = std::fs::remove_dir_all(&e.root);
}

#[test]
fn profiles_keep_records_folders_apart_and_the_old_folder_moves_into_one() {
    let e = Env::new("profiles");
    setup(&e);
    let second = e.root.join("second");
    e.ok(&["profiles", "add", "away"]);
    let away = [("MEDREC_PROFILE", "away")];
    let out = e.run_env(&["--json", "init", second.to_str().unwrap()], &away);
    assert!(out.status.success(), "{}", said(&out).1);
    let out = e.run_env(&["--json", "people", "list"], &away);
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["items"].as_array().unwrap().len(), 0);
    assert_eq!(
        e.ok(&["people", "list"])["items"].as_array().unwrap().len(),
        2
    );
    // --profile wins over the environment.
    let out = e.run_env(&["--json", "--profile", "home", "people", "list"], &away);
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["items"].as_array().unwrap().len(), 2);
    // Without either, nothing runs.
    let (code, text) = said(&e.run_env(&["people", "list"], &[("MEDREC_PROFILE", "")]));
    assert_eq!(code, Some(2), "{text}");
    assert!(text.contains("configured: away, home"), "{text}");

    // A config from before profiles: one top-level records folder.
    let config = e.root.join("config/medrec/config.json");
    let old = serde_json::json!({ "folder": e.records().to_str().unwrap(), "viewer": "none" });
    std::fs::write(&config, old.to_string()).unwrap();
    let (code, text) = said(&e.run_env(&["people", "list"], &[("MEDREC_PROFILE", "")]));
    assert_eq!(code, Some(2), "{text}");
    assert!(text.contains("medrec profiles migrate <NAME>"), "{text}");
    let v = e.ok(&["profiles", "migrate", "home"]);
    assert_eq!(v["status"], "migrated");
    // The file as it was is kept, for a medrec from before profiles.
    let backup = std::path::PathBuf::from(v["backup"].as_str().unwrap());
    assert_eq!(
        backup,
        e.root.join("config/medrec/config.json.pre-profiles")
    );
    let was: Value = serde_json::from_str(&std::fs::read_to_string(&backup).unwrap()).unwrap();
    assert_eq!(was, old);
    {
        use std::os::unix::fs::PermissionsExt;
        // Both name records folders and a person id: owner-only.
        for f in [&backup, &config] {
            let mode = std::fs::metadata(f).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "{}", f.display());
        }
    }
    // With a profile named, config show gives its settings where they were.
    let shown = e.ok(&["config", "show"]);
    assert_eq!(
        (&shown["profile"], &shown["folder"]),
        (&"home".into(), &e.records().to_str().unwrap().into())
    );
    assert_eq!(
        e.ok(&["people", "list"])["items"].as_array().unwrap().len(),
        2
    );
    let kept: Value = serde_json::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap();
    assert!(kept.get("folder").is_none(), "{kept}");
    assert_eq!(kept["viewer"], "none");
    assert_eq!(
        kept["profiles"]["home"]["folder"],
        e.records().to_str().unwrap()
    );
    assert_eq!(
        e.ok(&["profiles", "migrate", "home"])["status"],
        "nothing-to-migrate"
    );
    // Migrating into a profile with another records folder is refused.
    std::fs::write(
        &config,
        serde_json::json!({
            "folder": second.to_str().unwrap(),
            "profiles": { "home": { "folder": e.records().to_str().unwrap() } }
        })
        .to_string(),
    )
    .unwrap();
    assert_eq!(
        e.run(&["profiles", "migrate", "home"]).status.code(),
        Some(2)
    );
    // Removing a profile leaves its records folder as it is.
    e.ok(&["profiles", "migrate", "away"]);
    e.ok(&["profiles", "remove", "away", "--force"]);
    assert!(second.join("household.json").exists());
    let _ = std::fs::remove_dir_all(&e.root);
}

#[test]
fn a_reimport_under_another_name_for_the_organization_finds_the_person() {
    let base = Env::new("import-alias");
    setup(&base);
    let e = provider_env(&base, medrec::testdata::STREAM);
    let import = |extra: &[&str]| {
        let mut a = vec![
            "import",
            "--from",
            "fakeprov",
            "--from-profile",
            "adult1",
            "--patient",
            "pat-0001",
        ];
        a.extend_from_slice(extra);
        said(&e.run(&a))
    };
    let args = e.root.join("provider/export-args");
    // The provider takes `--org health` for its organization `example-health`.
    let (code, text) = import(&["--org", "health", "--person", "Robin"]);
    assert_eq!(code, Some(0), "{text}");
    for org in ["health", "example-health"] {
        let v = e.ok(&[
            "import",
            "--from",
            "fakeprov",
            "--from-profile",
            "adult1",
            "--patient",
            "pat-0001",
            "--org",
            org,
        ]);
        assert_eq!(v["records_unchanged"], 9, "--org {org}");
    }
    // A patient never imported from this CLI is refused before the export runs.
    std::fs::remove_file(&args).unwrap();
    let out = e.run(&[
        "import",
        "--from",
        "fakeprov",
        "--from-profile",
        "adult1",
        "--patient",
        "pat-0009",
        "--org",
        "health",
    ]);
    assert_eq!(out.status.code(), Some(2));
    assert!(!args.exists(), "the export ran");
    let _ = std::fs::remove_dir_all(&e.root);
}

#[test]
fn an_import_from_downloaded_files_runs_with_files_and_no_profile() {
    let base = Env::new("import-files");
    setup(&base);
    let e = provider_env(&base, medrec::testdata::STREAM);
    let statements = e.root.join("statements");
    std::fs::create_dir_all(&statements).unwrap();
    let dir = statements.to_str().unwrap();
    // The provider's own profile variable is set, as it may be in a shell.
    let files = [("FAKEPROV_INFO", "files"), ("FAKEPROV_PROFILE", "adult1")];
    let import = |extra: &[&str], env: &[(&str, &str)]| {
        let mut a = vec![
            "import",
            "--from",
            "fakeprov",
            "--patient",
            "pat-0001",
            "--person",
            "Robin",
        ];
        a.extend_from_slice(extra);
        said(&e.run_env(&a, env))
    };
    let read =
        |f: &str| std::fs::read_to_string(e.root.join("provider").join(f)).unwrap_or_default();

    // One source per import: refused as the arguments are parsed.
    let (code, text) = import(&["--from-files", dir, "--from-profile", "adult1"], &files);
    assert_eq!(code, Some(2), "{text}");
    assert!(text.contains("cannot be used with"), "{text}");
    assert_eq!(read("export-args"), "");
    // A provider that reads no downloaded files is refused before its export.
    let (code, text) = import(&["--from-files", dir], &[]);
    assert_eq!(code, Some(2), "{text}");
    assert!(text.contains("reads no downloaded files"), "{text}");
    assert_eq!(read("export-args"), "");
    // So is a directory that is not there.
    let missing = e.root.join("nowhere");
    let (code, text) = import(&["--from-files", missing.to_str().unwrap()], &files);
    assert_eq!(code, Some(2), "{text}");
    assert_eq!(read("export-args"), "");
    // Neither source: the refusal names both.
    let (code, text) = import(&[], &files);
    assert_eq!(code, Some(2), "{text}");
    assert!(
        text.contains("--from-files <DIR>") && text.contains("adult1, adult2"),
        "{text}"
    );

    // The files go to the export as --files, with no profile in its
    // arguments or its environment.
    let (code, text) = import(&["--from-files", dir], &files);
    assert_eq!(code, Some(0), "{text}");
    let args = read("export-args");
    let absolute = std::fs::canonicalize(&statements).unwrap();
    assert!(
        args.contains(&format!("--files {}", absolute.display())),
        "{args}"
    );
    assert!(!args.contains("--profile"), "{args}");
    assert_eq!(read("export-env").trim(), "profile= files=");
    // The same records through the login are unchanged, and the files
    // variable stays out of a login export's environment.
    let out = e.run_env(
        &[
            "--json",
            "import",
            "--from",
            "fakeprov",
            "--from-profile",
            "adult1",
            "--patient",
            "pat-0001",
        ],
        &[("FAKEPROV_FILES", dir)],
    );
    let (code, text) = said(&out);
    assert_eq!(code, Some(0), "{text}");
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["records_unchanged"], 9);
    assert_eq!(read("export-env").trim(), "profile= files=");
    e.ok(&["config", "set", "providers", "fakeprov"]);
    let out = e.run_env(&["--json", "providers", "list"], &files);
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(v["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["cli"] == "fakeprov" && p["records_files"] == true));
    let _ = std::fs::remove_dir_all(&e.root);
}

/// The synthetic stream read at `pulled`, its Observation at `value` and
/// last updated at `updated`, or undated.
fn with_observation(value: f64, updated: Option<&str>, pulled: &str) -> String {
    medrec::testdata::STREAM
        .lines()
        .map(|l| {
            let mut v: Value = serde_json::from_str(l).unwrap();
            if v["schema"] == "records-stream/v1" {
                v["pulled_at"] = serde_json::json!(pulled);
            }
            if v["resourceType"] == "Observation" {
                v["valueQuantity"]["value"] = serde_json::json!(value);
                let meta = v["meta"].as_object_mut().unwrap();
                match updated {
                    Some(u) => meta.insert("lastUpdated".into(), serde_json::json!(u)),
                    None => meta.remove("lastUpdated"),
                };
            }
            format!("{}\n", serde_json::to_string(&v).unwrap())
        })
        .collect()
}

#[test]
fn an_older_version_does_not_replace_the_one_filed() {
    let base = Env::new("import-older");
    setup(&base);
    let pulled = "2026-03-06T12:00:00Z";
    let e = provider_env(
        &base,
        &with_observation(13.0, Some("2026-03-06T11:00:00Z"), pulled),
    );
    let serve = |value: f64, updated: Option<&str>, pulled: &str| {
        std::fs::write(
            e.root.join("provider").join("synthetic.ndjson"),
            with_observation(value, updated, pulled),
        )
        .unwrap()
    };
    let import = || {
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
        ])
    };
    let value = || {
        let r = e.ok(&["results", "--code", "718-7"]);
        r["items"][0]["detail"].as_str().unwrap_or("").to_string()
    };
    let dir = e
        .records()
        .join("Robin Testperson/Records/fakeprov-example-health/Observation");
    let raw = || -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|d| d.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    };
    let current = || std::fs::read_to_string(dir.join("obs-0001.json")).unwrap();
    import();
    assert!(value().starts_with("13"), "{}", value());
    assert_eq!(raw(), ["obs-0001.json"]);

    // A version last updated before it, in another zone, changes nothing
    // filed; it goes beside the current one.
    serve(11.0, Some("2026-03-01T09:00:00-05:00"), pulled);
    let r = import();
    assert_eq!(r["records_older"], 1, "{r}");
    assert_eq!(r["records_updated"], 0, "{r}");
    assert!(value().starts_with("13"), "{}", value());
    assert_eq!(raw().len(), 2);
    assert!(current().contains("2026-03-06T11:00:00Z"));
    // Served again, it is already kept.
    assert_eq!(import()["records_older"], 1);
    assert_eq!(raw().len(), 2);
    // The same instant is not later: the version held stays.
    serve(12.0, Some("2026-03-06T06:00:00-05:00"), pulled);
    assert_eq!(import()["records_older"], 1);
    assert!(value().starts_with("13"), "{}", value());

    // A later version replaces it.
    serve(14.0, Some("2026-03-07T11:00:00Z"), pulled);
    let r = import();
    assert_eq!(r["records_updated"], 1, "{r}");
    assert_eq!(r["records_older"], 0, "{r}");
    assert!(value().starts_with("14"), "{}", value());

    // A version the server sent undated is as of the read: one read before
    // the held version loses, one after it wins.
    serve(15.0, None, "2026-03-07T09:00:00Z");
    assert_eq!(import()["records_older"], 1);
    serve(15.0, None, "2026-03-08T12:00:00Z");
    assert_eq!(import()["records_updated"], 1);
    assert!(value().starts_with("15"), "{}", value());
    // Read again unchanged, later: a version dated between the two reads is
    // older than the second.
    serve(15.0, None, "2026-03-20T12:00:00Z");
    let r = import();
    assert_eq!(r["records_updated"], 0, "{r}");
    assert_eq!(r["records_older"], 0, "{r}");
    serve(16.0, Some("2026-03-10T00:00:00Z"), "2026-03-21T12:00:00Z");
    assert_eq!(import()["records_older"], 1);
    assert!(value().starts_with("15"), "{}", value());

    // Stamped again on a later read, the same content is the same version:
    // its copy is overwritten in place, and no version is added.
    let files = raw().len();
    serve(15.0, Some("2026-03-22T12:00:00Z"), "2026-03-22T12:00:00Z");
    let r = import();
    assert_eq!(r["records_updated"], 0, "{r}");
    assert_eq!(r["records_older"], 0, "{r}");
    assert_eq!(r["resources_written"], 1, "{r}");
    assert_eq!(raw().len(), files);
    assert!(current().contains("2026-03-22T12:00:00Z"));
    // An earlier stamp on the same content changes nothing.
    serve(15.0, Some("2026-03-21T12:00:00Z"), "2026-03-23T12:00:00Z");
    let r = import();
    assert_eq!(r["resources_written"], 0, "{r}");
    assert_eq!(r["records_older"], 0, "{r}");
    assert!(current().contains("2026-03-22T12:00:00Z"));
    // Other content dated before the stamp now held is older.
    serve(17.0, Some("2026-03-22T06:00:00Z"), "2026-03-24T12:00:00Z");
    assert_eq!(import()["records_older"], 1);
    assert!(value().starts_with("15"), "{}", value());
    assert_eq!(raw().len(), files + 1);

    // A refresh cut short leaves the held copy as `~refresh-<tag>`, the tag
    // starting the MD5 of the copy it made room for. The next import removes
    // it when the current copy is that one, and otherwise puts it back.
    let good = std::fs::read(dir.join("obs-0001.json")).unwrap();
    let tag = &medrec::hash::md5_hex(&good)[..8];
    let left = dir.join(format!("obs-0001~refresh-{tag}.json"));
    std::fs::write(&left, b"{\"resourceType\":\"Observation\"}\n").unwrap();
    import();
    assert!(!left.exists());
    assert_eq!(std::fs::read(dir.join("obs-0001.json")).unwrap(), good);
    // The current copy missing: the held one goes back.
    let left = dir.join("obs-0001~refresh-0000abcd.json");
    std::fs::rename(dir.join("obs-0001.json"), &left).unwrap();
    import();
    assert!(!left.exists());
    assert_eq!(std::fs::read(dir.join("obs-0001.json")).unwrap(), good);
    assert_eq!(raw().len(), files + 1);
    // A current copy that is not the one made room for (an upload that did
    // not read back): the held one goes back, and that copy stays beside it.
    std::fs::rename(dir.join("obs-0001.json"), &left).unwrap();
    std::fs::write(dir.join("obs-0001.json"), b"{\"resourceType\":\"Obs\n").unwrap();
    import();
    assert!(!left.exists());
    assert_eq!(std::fs::read(dir.join("obs-0001.json")).unwrap(), good);
    assert_eq!(raw().len(), files + 2);

    let v = e.ok(&["verify"]);
    assert_eq!(v["missing"].as_array().unwrap().len(), 0, "{v}");
    assert_eq!(v["changed"].as_array().unwrap().len(), 0, "{v}");
    let _ = std::fs::remove_dir_all(&e.root);
}

/// The synthetic stream with its Observation served with no id, beside a
/// second one with no id, both last updated at `updated`.
fn with_observations_without_ids(updated: &str) -> String {
    let mut out = String::new();
    for l in medrec::testdata::STREAM.lines() {
        let mut v: Value = serde_json::from_str(l).unwrap();
        if v["schema"] == "records-stream-end/v1" {
            v["counts"]["Observation"] = serde_json::json!(2);
        }
        if v["resourceType"] == "Observation" {
            v.as_object_mut().unwrap().remove("id");
            v["meta"]["lastUpdated"] = serde_json::json!(updated);
            let mut other = v.clone();
            other["code"]["coding"][0]["code"] = serde_json::json!("4544-3");
            other["code"]["text"] = serde_json::json!("Hematocrit");
            other["valueQuantity"]["value"] = serde_json::json!(40.0);
            out.push_str(&format!("{}\n", serde_json::to_string(&v).unwrap()));
            out.push_str(&format!("{}\n", serde_json::to_string(&other).unwrap()));
            continue;
        }
        out.push_str(&format!("{}\n", serde_json::to_string(&v).unwrap()));
    }
    out
}

#[test]
fn resources_with_no_id_are_filed_apart_and_kept_when_stamped_again() {
    let base = Env::new("import-noid");
    setup(&base);
    let e = provider_env(
        &base,
        &with_observations_without_ids("2026-03-05T11:00:00Z"),
    );
    let import = || {
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
        ])
    };
    let dir = e
        .records()
        .join("Robin Testperson/Records/fakeprov-example-health/Observation");
    let files = || {
        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|d| d.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    };
    let first = import();
    let names = files();
    assert_eq!(names.len(), 2, "{names:?}");
    assert!(names.iter().all(|n| n.starts_with("noid-")), "{names:?}");
    let results = e.ok(&["results"]);
    let readings = results["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["kind"] == "result")
        .count();
    assert_eq!(readings, 2, "{results}");

    // Served again, then stamped again on a later read: the same two files,
    // and no record changes.
    let again = import();
    assert_eq!(again["resources_written"], 0, "{again}");
    assert_eq!(again["records_added"], 0, "{again}");
    std::fs::write(
        e.root.join("provider").join("synthetic.ndjson"),
        with_observations_without_ids("2026-03-09T11:00:00Z"),
    )
    .unwrap();
    let stamped = import();
    assert_eq!(stamped["resources_written"], 2, "{stamped}");
    assert_eq!(stamped["records_added"], 0, "{stamped}");
    assert_eq!(stamped["records_updated"], 0, "{stamped}");
    assert_eq!(files(), names);
    assert_eq!(
        stamped["records_unchanged"], first["records_added"],
        "{stamped}"
    );

    let v = e.ok(&["verify"]);
    assert_eq!(v["missing"].as_array().unwrap().len(), 0, "{v}");
    assert_eq!(v["changed"].as_array().unwrap().len(), 0, "{v}");
    let _ = std::fs::remove_dir_all(&e.root);
}

/// The synthetic stream with the synthetic fill, and with its prescription,
/// the MedicationRequest as `mr-0001` written by an `Example Prescriber`,
/// or without it.
fn with_fill(prescription: bool) -> String {
    let fill: Value =
        serde_json::from_str(include_str!("fixtures/fhir/medication-dispense.json")).unwrap();
    let mut out = String::new();
    for l in medrec::testdata::STREAM.lines() {
        let mut v: Value = serde_json::from_str(l).unwrap();
        if v["schema"] == "records-stream-end/v1" {
            v["counts"]["MedicationDispense"] = serde_json::json!(1);
            if !prescription {
                v["counts"]
                    .as_object_mut()
                    .unwrap()
                    .remove("MedicationRequest");
            }
            out.push_str(&format!("{}\n", serde_json::to_string(&fill).unwrap()));
        }
        if v["resourceType"] == "MedicationRequest" {
            if !prescription {
                continue;
            }
            v["id"] = serde_json::json!("mr-0001");
            v["requester"] = serde_json::json!({"display": "Example Prescriber"});
        }
        out.push_str(&format!("{}\n", serde_json::to_string(&v).unwrap()));
    }
    out
}

#[test]
fn a_fill_names_the_prescriber_of_the_prescription_it_points_at() {
    let base = Env::new("import-prescriber");
    setup(&base);
    let e = provider_env(&base, &with_fill(true));
    let import = || {
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
        ])
    };
    let fill = || {
        let meds = e.ok(&["meds"]);
        meds["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["fields"]["source"] == "dispense")
            .cloned()
            .unwrap_or_else(|| panic!("{meds}"))
    };
    import();
    assert_eq!(fill()["fields"]["prescriber"], "Example Prescriber");
    // Imported again, it is the same fill.
    let again = import();
    assert_eq!(again["records_updated"], 0, "{again}");
    assert_eq!(fill()["fields"]["prescriber"], "Example Prescriber");
    // A later stream with the fill and not its prescription: the request
    // filed before still names the prescriber.
    std::fs::write(
        e.root.join("provider").join("synthetic.ndjson"),
        with_fill(false),
    )
    .unwrap();
    let partial = import();
    assert_eq!(partial["records_updated"], 0, "{partial}");
    assert_eq!(fill()["fields"]["prescriber"], "Example Prescriber");
    let _ = std::fs::remove_dir_all(&e.root);
}

#[test]
fn a_document_rendered_again_on_a_later_export_is_not_filed_twice() {
    let base = Env::new("rendered-again");
    setup(&base);
    let finding = "Hemoglobin was 13.1 and the other routine blood tests were in range.";
    let corrected = "Hemoglobin was 13.4 and the other routine blood tests were in range.";
    let serve = |rendering: &str, finding: &str, version: &str| {
        medrec::testdata::write_provider(
            &base.root.join("provider"),
            &medrec::testdata::stream_with_summary(rendering, finding, version),
        )
    };
    let e = provider_env(
        &base,
        &medrec::testdata::stream_with_summary("rendering-a", finding, "1"),
    );
    let import = |person: bool| {
        let mut a = vec![
            "import",
            "--from",
            "fakeprov",
            "--from-profile",
            "adult1",
            "--patient",
            "pat-0001",
        ];
        if person {
            a.extend_from_slice(&["--person", "Robin"]);
        }
        let v = e.ok(&a);
        (
            v["documents_added"].as_u64().unwrap(),
            v["documents_duplicate"].as_u64().unwrap(),
            v["documents_unchanged"].as_u64().unwrap(),
        )
    };
    // Each filed document's renderings, in filing order.
    let renderings = || {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(e.records()).unwrap() {
            let index = entry.unwrap().path().join("index.json");
            if let Ok(t) = std::fs::read_to_string(&index) {
                let v: Value = serde_json::from_str(&t).unwrap();
                for d in v["documents"].as_array().unwrap() {
                    out.push(d["renderings"].as_array().map_or(0, Vec::len));
                }
            }
        }
        out
    };
    assert_eq!(import(true), (2, 0, 0));

    // The next export renders the summary again: a new timestamp and id,
    // other bytes, the same text. The copy filed before stays the only one,
    // and the rendering is noted on it.
    serve("rendering-b", finding, "1");
    assert_eq!(import(false), (0, 1, 1));
    assert_eq!(renderings().iter().sum::<usize>(), 1);
    // The same rendering served again, byte for byte, is a duplicate and
    // noted once.
    assert_eq!(import(false), (0, 2, 0));
    assert_eq!(renderings().iter().sum::<usize>(), 1);

    // One corrected digit, at the same version, is an amended document.
    serve("rendering-c", corrected, "1");
    assert_eq!(import(false), (1, 1, 0));
    // The same text under a new version is a new version.
    serve("rendering-d", corrected, "2");
    assert_eq!(import(false), (1, 1, 0));
    // A rendering of the latest version is that version again.
    serve("rendering-e", corrected, "2");
    assert_eq!(import(false), (0, 1, 1));
    assert_eq!(renderings().iter().sum::<usize>(), 2);

    let listed = e.ok(&["documents", "list"]);
    assert_eq!(listed["items"].as_array().unwrap().len(), 4);
    let v = e.ok(&["verify"]);
    assert!(v["missing"].as_array().unwrap().is_empty(), "{v}");
    let _ = std::fs::remove_dir_all(&e.root);
}

#[test]
fn a_document_filed_twice_merges_into_the_one_it_renders_again() {
    let base = Env::new("merge");
    setup(&base);
    let finding = "Hemoglobin was 13.1 and the other routine blood tests were in range.";
    let e = provider_env(
        &base,
        &medrec::testdata::stream_with_summary("rendering-a", finding, "1"),
    );
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
    // The person's folder, and the summary filed for it.
    let folder = std::fs::read_dir(e.records())
        .unwrap()
        .map(|d| d.unwrap().path())
        .find(|p| {
            p.join("index.json").is_file() && {
                let v: Value =
                    serde_json::from_str(&std::fs::read_to_string(p.join("index.json")).unwrap())
                        .unwrap();
                !v["documents"].as_array().unwrap().is_empty()
            }
        })
        .unwrap();
    let read = |f: &str| -> Value {
        serde_json::from_str(&std::fs::read_to_string(folder.join(f)).unwrap()).unwrap()
    };
    let mut index = read("index.json");
    let mut text = read("text.json");
    let docs = index["documents"].as_array().unwrap().clone();
    let summary = docs
        .iter()
        .find(|d| d["media_type"] == "application/xml")
        .unwrap()
        .clone();
    let pdf = docs
        .iter()
        .find(|d| d["media_type"] == "application/pdf")
        .unwrap()
        .clone();
    let kept = summary["id"].as_str().unwrap().to_string();

    // A copy as an import before 0.2.3 filed it: the summary rendered
    // again, other bytes and the same text, named by a record.
    let original = std::fs::read(e.records().join(summary["path"].as_str().unwrap())).unwrap();
    let mut bytes = original.clone();
    bytes.extend_from_slice(b"\n<!-- rendering-b -->\n");
    let copy_path = summary["path"].as_str().unwrap().replace(".xml", "-2.xml");
    std::fs::write(e.records().join(&copy_path), &bytes).unwrap();
    let mut copy = summary.clone();
    copy["id"] = "d_copy0001".into();
    copy["title"] = "Psychotherapy summary".into();
    copy["original_name"] = "psychotherapy_summary.xml".into();
    copy["path"] = copy_path.clone().into();
    copy["sha256"] = medrec::hash::sha256_hex(&bytes).into();
    copy["md5"] = medrec::hash::md5_hex(&bytes).into();
    copy["size"] = (bytes.len() as u64).into();
    copy["provenance"]["fetched_at"] = "2026-03-06T09:00:00Z".into();
    index["documents"].as_array_mut().unwrap().push(copy);
    index["records"][0]["documents"] = serde_json::json!(["d_copy0001"]);
    // A study whose report it is.
    index["studies"] = serde_json::json!([{
        "id": "s_study0001",
        "study_uid": "2.25.1",
        "path": "Imaging/none",
        "series": [],
        "instances": 0,
        "size": 0,
        "report": "d_copy0001",
        "filed_at": "2026-03-06T09:00:00Z",
        "provenance": []
    }]);
    text["texts"]["d_copy0001"] = text["texts"][&kept].clone();
    std::fs::write(folder.join("index.json"), index.to_string()).unwrap();
    std::fs::write(folder.join("text.json"), text.to_string()).unwrap();
    e.ok(&[
        "note",
        "add",
        "Robin",
        "About the copy",
        "--about",
        "d_copy0001",
    ]);

    // Refusals come before the prompt: with no terminal and no --force, a
    // merge that would be refused is that refusal, not exit 6.
    let (code, said_) = said(&e.run(&[
        "--json",
        "documents",
        "merge",
        pdf["id"].as_str().unwrap(),
        "--into",
        &kept,
    ]));
    assert_eq!(code, Some(2), "{said_}");
    assert!(said_.contains("is not a rendering of"), "{said_}");
    let (code, said_) = said(&e.run(&["documents", "merge", &kept, "--into", &kept]));
    assert_eq!(code, Some(2), "{said_}");
    let (code, said_) = said(&e.run(&["documents", "merge", "d_nope0001", "--into", &kept]));
    assert_eq!(code, Some(4), "{said_}");
    assert!(
        said_.contains("`d_nope0001`") && !said_.contains(&format!("`{kept}`")),
        "{said_}"
    );
    // A merge that would go ahead asks, and with no terminal it is exit 6.
    let gate = e.run(&["documents", "merge", "d_copy0001", "--into", &kept]);
    assert_eq!(gate.status.code(), Some(6));

    let m = e.ok(&[
        "documents",
        "merge",
        "d_copy0001",
        "--into",
        &kept,
        "--force",
    ]);
    assert_eq!(m["document"], kept.as_str());
    assert_eq!(m["file_removed"], true);
    assert_eq!(m["references_moved"], 3);
    assert!(!e.records().join(&copy_path).exists());
    let index = read("index.json");
    let docs = index["documents"].as_array().unwrap();
    assert!(docs.iter().all(|d| d["id"] != "d_copy0001"));
    let kept_doc = docs.iter().find(|d| d["id"] == kept.as_str()).unwrap();
    assert_eq!(
        kept_doc["renderings"][0]["sha256"],
        medrec::hash::sha256_hex(&bytes)
    );
    assert_eq!(index["records"][0]["documents"], serde_json::json!([kept]));
    assert_eq!(index["studies"][0]["report"], kept.as_str());
    // What the copy was called holds it private, and so does its file's name.
    assert_eq!(kept_doc["title"], summary["title"]);
    assert_eq!(kept_doc["original_name"], summary["original_name"]);
    let given = serde_json::json!({
        "title": "Psychotherapy summary",
        "name": "psychotherapy_summary.xml"
    });
    let mut kept_given = kept_doc["given"][0].clone();
    kept_given.as_object_mut().unwrap().remove("raw_path");
    assert_eq!(kept_given, given);
    let held = || {
        let private = e.ok(&["private", "list"]);
        let mut rows = private["items"].as_array().unwrap().iter();
        rows.any(|r| r["id"] == kept.as_str() && r["given"] == true)
    };
    assert!(held());
    for (left, private) in [("name", true), ("raw_path", false)] {
        let mut index = read("index.json");
        for d in index["documents"].as_array_mut().unwrap() {
            let given = d.get_mut("given").and_then(Value::as_array_mut);
            for g in given.into_iter().flatten() {
                g.as_object_mut().unwrap().retain(|k, _| k == left);
            }
        }
        std::fs::write(folder.join("index.json"), index.to_string()).unwrap();
        assert_eq!(held(), private, "{left}");
    }
    assert!(read("text.json")["texts"].get("d_copy0001").is_none());
    let notes = e.ok(&["note", "list"]);
    assert_eq!(notes["items"][0]["about"], serde_json::json!([kept]));
    let v = e.ok(&["verify"]);
    assert!(v["missing"].as_array().unwrap().is_empty(), "{v}");
    assert!(v["unindexed"].as_array().unwrap().is_empty(), "{v}");
    let _ = std::fs::remove_dir_all(&e.root);
}

/// The synthetic stream with a billing CLI's statement: a DocumentReference
/// typed `bill` by text, dated the statement date, and its PDF.
fn with_statement(lines: &[&str]) -> String {
    use base64::Engine;
    let pdf = base64::engine::general_purpose::STANDARD.encode(medrec::pdfgen::text_pdf(lines));
    medrec::testdata::STREAM
        .replace(
            "{\"schema\":\"records-stream-end/v1\"",
            &format!(
                concat!(
                    r#"{{"resourceType":"DocumentReference","id":"stmt-0001","status":"current","type":{{"text":"bill"}},"subject":{{"reference":"Patient/pat-0001"}},"date":"2026-11-02T00:00:00Z","content":[{{"attachment":{{"contentType":"application/pdf","url":"Binary/bin-0009","title":"Statement"}}}}]}}"#, "\n",
                    r#"{{"resourceType":"Binary","id":"bin-0009","contentType":"application/pdf","data":"{}"}}"#, "\n",
                    r#"{{"schema":"records-stream-end/v1""#
                ),
                pdf
            ),
        )
        .replace("\"DocumentReference\":2,\"Binary\":2", "\"DocumentReference\":3,\"Binary\":3")
}

#[test]
fn a_statement_filed_by_hand_and_the_portals_copy_are_one_statement() {
    if !has("pdftotext") {
        eprintln!("skipped: pdftotext (poppler) is not installed");
        return;
    }
    let base = Env::new("statements");
    setup(&base);
    // The scan the user files: a masked account, each field on its line.
    let scan = base.root.join("statement-scan.pdf");
    std::fs::write(
        &scan,
        medrec::pdfgen::text_pdf(&[
            "Example Medical Group",
            "STATEMENT",
            "Statement date: Nov 2, 2026",
            "Guarantor Account #: ****0123",
            "Patient: Robin Testperson",
            "Balance due: $1,234.56",
            "Please pay by 12/01/2026",
        ]),
    )
    .unwrap();
    let filed = base.ok(&[
        "intake",
        scan.to_str().unwrap(),
        "--person",
        "Robin",
        "--type",
        "bill",
        "--date",
        "2026-10-14",
        "--org",
        "Example Medical Group",
    ]);
    let first = filed["items"][0]["document"].as_str().unwrap().to_string();
    assert_eq!(
        filed["items"][0]["same_statement_as"],
        Value::Null,
        "{filed}"
    );
    let bills = base.ok(&["documents", "list", "--type", "bill"]);
    let st = &bills["items"][0]["statement"];
    assert_eq!(
        (
            st["statement_date"].as_str(),
            st["amount_due"]["amount"].as_str(),
            st["due_date"].as_str(),
            st["account"].as_str()
        ),
        (
            Some("2026-11-02"),
            Some("1234.56"),
            Some("2026-12-01"),
            Some("****0123")
        ),
        "{bills}"
    );

    // The billing CLI exports the portal's PDF of the same statement later.
    let e = provider_env(
        &base,
        &with_statement(&[
            "Example Medical Group",
            "Patient Statement",
            "Statement Date: 11/02/2026",
            "Account Number: EX-000123",
            "Amount Due: $1,234.56",
            "Due Date: 12/01/2026",
        ]),
    );
    let import = || {
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
        ])
    };
    let r = import();
    assert_eq!(r["documents_same_statement"], 1, "{r}");
    let bills = e.ok(&["documents", "list", "--type", "bill"]);
    let items = bills["items"].as_array().unwrap();
    assert_eq!(items.len(), 2, "{bills}");
    let portal = items.iter().find(|d| d["id"] != first.as_str()).unwrap();
    assert_eq!(portal["same_statement_as"], first.as_str(), "{portal}");
    assert_eq!(portal["statement"]["account"], "EX-000123");
    assert!(
        portal["path"]
            .as_str()
            .unwrap()
            .starts_with("Robin Testperson/Claims/2026/"),
        "{portal}"
    );
    // Imported again, nothing more is filed.
    let again = import();
    assert_eq!(
        (
            again["documents_added"].as_u64(),
            again["documents_same_statement"].as_u64()
        ),
        (Some(0), Some(0)),
        "{again}"
    );
    let _ = std::fs::remove_dir_all(&base.root);
}

/// The synthetic stream with a second, lab line on the claim (CPT 80053,
/// dated the day of the visit and its results), a member-liability total,
/// a benefit period and a diagnosis.
fn with_lab_line() -> String {
    medrec::testdata::STREAM
        .lines()
        .map(|l| {
            let mut v: Value = serde_json::from_str(l).unwrap();
            if v["resourceType"] == "ExplanationOfBenefit" {
                let usd = |a: f64| serde_json::json!({"value": a, "currency": "USD"});
                let adj = |code: &str, text: &str, a: f64| {
                    serde_json::json!({"category": {"coding": [{"code": code}], "text": text}, "amount": usd(a)})
                };
                v["item"].as_array_mut().unwrap().push(serde_json::json!({
                    "sequence": 2,
                    "productOrService": {"coding": [{"system": "http://www.ama-assn.org/go/cpt", "code": "80053", "display": "Chemistry tests"}]},
                    "servicedDate": "2026-03-04",
                    "adjudication": [adj("submitted", "Billed", 40.0), adj("benefit", "Plan paid", 30.0), adj("copay", "Copay", 5.5)],
                }));
                v["total"]
                    .as_array_mut()
                    .unwrap()
                    .push(adj("memberliability", "You owe", 30.5));
                v["benefitPeriod"] = serde_json::json!({"start": "2026-01-01", "end": "2026-12-31"});
                v["diagnosis"] = serde_json::json!([{
                    "sequence": 1,
                    "diagnosisCodeableConcept": {"coding": [{"system": "http://hl7.org/fhir/sid/icd-10-cm", "code": "Z00.00", "display": "General adult examination"}]},
                }]);
            }
            format!("{}\n", serde_json::to_string(&v).unwrap())
        })
        .collect()
}

#[test]
fn a_claim_is_titled_priced_by_category_and_linked_to_its_visit_and_results() {
    let base = Env::new("claims");
    setup(&base);
    let e = provider_env(&base, &with_lab_line());
    let import = || {
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
        ])
    };
    import();
    let index: Value = serde_json::from_str(
        &std::fs::read_to_string(e.records().join("Robin Testperson/index.json")).unwrap(),
    )
    .unwrap();
    let records = index["records"].as_array().unwrap();
    let id_of = |kind: &str| -> Vec<String> {
        records
            .iter()
            .filter(|r| r["kind"] == kind)
            .map(|r| r["id"].as_str().unwrap().to_string())
            .collect()
    };
    let claim = records.iter().find(|r| r["kind"] == "claim").unwrap();
    let f = &claim["fields"];
    assert_eq!(
        claim["display"],
        "Office visit and Chemistry tests · Example Health"
    );
    assert_eq!(f["type"], "professional", "the type stays in fields.type");
    assert_eq!(f["category"], "mixed");
    assert_eq!(f["items"][0]["category"], "visit");
    assert_eq!(f["items"][1]["category"], "lab");
    assert_eq!(f["items"][0]["amounts"]["copay"]["amount"], "25.0");
    let amount = |k: &str| f["amounts"][k]["amount"].as_str().unwrap_or("").to_string();
    assert_eq!(amount("billed"), "150.0", "a total wins over the lines");
    assert_eq!(amount("plan_paid"), "140.00", "no total: the lines' sum");
    assert_eq!(amount("copay"), "30.50");
    assert_eq!(amount("you_owe"), "30.5");
    assert_eq!(amount("allowed"), "", "missing means unknown");
    assert_eq!(f["benefit_period"]["start"], "2026-01-01");
    assert_eq!(f["diagnoses"][0]["code"]["code"], "Z00.00");
    assert_eq!(f["visit"], id_of("encounter")[0].as_str());
    let mut labs = id_of("result");
    labs.extend(id_of("report"));
    let mut linked: Vec<String> = f["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    labs.sort();
    linked.sort();
    assert_eq!(linked, labs);
    assert_eq!(f["items"][0]["records"][0], f["visit"]);
    assert_eq!(
        f["items"][1]["records"].as_array().unwrap().len(),
        labs.len()
    );

    // The links are not the claim's own content: importing again leaves it
    // unchanged.
    let again = import();
    assert_eq!(again["records_updated"], 0, "{again}");

    let claims = e.ok(&["timeline", "--kind", "claim"]);
    assert_eq!(
        claims["items"][0]["detail"],
        "billed 150.0 USD, plan paid 140.00 USD, you owe 30.5 USD"
    );
    // "blood tests" is in no record's text, and names the lab results and
    // the claim for them.
    let hits = e.ok(&["search", "blood tests"]);
    let kinds: Vec<&str> = hits["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["kind"].as_str().unwrap())
        .collect();
    assert!(
        kinds.contains(&"result") && kinds.contains(&"claim"),
        "{kinds:?}"
    );
    assert!(!kinds.contains(&"encounter"), "{kinds:?}");

    // A claim filed in an earlier shape, which no source serves again, is
    // shaped again from its raw file.
    let path = e.records().join("Robin Testperson/index.json");
    let mut old: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let want = old.clone();
    for r in old["records"].as_array_mut().unwrap() {
        if r["kind"] == "claim" {
            r["display"] = serde_json::json!("Professional");
            let f = r["fields"].as_object_mut().unwrap();
            for k in [
                "amounts",
                "category",
                "benefit_period",
                "diagnoses",
                "visit",
                "results",
            ] {
                f.remove(k);
            }
        }
    }
    std::fs::write(&path, serde_json::to_string(&old).unwrap()).unwrap();
    let r = e.ok(&["reindex"]);
    assert_eq!(
        (r["claims"].as_u64(), r["reshaped"].as_u64()),
        (Some(1), Some(1)),
        "{r}"
    );
    let now: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let claim_of = |v: &Value| {
        v["records"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["kind"] == "claim")
            .cloned()
            .unwrap()
    };
    assert_eq!(claim_of(&now)["display"], claim_of(&want)["display"]);
    assert_eq!(
        claim_of(&now)["fields"],
        claim_of(&want)["fields"],
        "links included"
    );
    assert_eq!(
        e.ok(&["reindex"])["reshaped"],
        0,
        "a second run changes nothing"
    );
    // One that is shaped again with the links it had has none made otherwise.
    assert_eq!(r["links_updated"], 1, "{r}");
    assert!(claim_of(&want)["fields"]["visit"].is_string());
    let mut old = now.clone();
    let records = old["records"].as_array_mut().unwrap();
    let claim = records.iter_mut().find(|r| r["kind"] == "claim").unwrap();
    claim["display"] = serde_json::json!("Professional");
    // With a line it has no more, that says none.
    let lines = claim["fields"]["items"].as_array_mut().unwrap();
    lines.push(serde_json::json!({"service": "Example service"}));
    std::fs::write(&path, serde_json::to_string(&old).unwrap()).unwrap();
    let r = e.ok(&["reindex"]);
    assert_eq!(
        (r["reshaped"].as_u64(), r["links_updated"].as_u64()),
        (Some(1), Some(0)),
        "{r}"
    );
}

/// The synthetic stream with another claim in place of its own, whose totals
/// leave a line out (tests/fixtures/fhir/left-out-line-eob.json).
fn with_left_out_line() -> String {
    let eob: Value =
        serde_json::from_str(include_str!("fixtures/fhir/left-out-line-eob.json")).unwrap();
    medrec::testdata::STREAM
        .lines()
        .map(|l| {
            let v: Value = serde_json::from_str(l).unwrap();
            let v = if v["resourceType"] == "ExplanationOfBenefit" {
                eob.clone()
            } else {
                v
            };
            format!("{}\n", serde_json::to_string(&v).unwrap())
        })
        .collect()
}

/// The raw files of a record, and of many, by the names of their elements and
/// how many of each, and what a later version changed, with no value.
#[test]
fn the_raw_files_of_a_record_are_counted_by_the_names_of_their_elements() {
    let base = Env::new("raw-files");
    setup(&base);
    let import = |version: u32, mark: bool| {
        let stream: String = with_left_out_line()
            .lines()
            .map(|l| {
                let mut v: Value = serde_json::from_str(l).unwrap();
                if v["resourceType"] == "ExplanationOfBenefit" {
                    v["meta"] = serde_json::json!({"versionId": version.to_string(),
                        "lastUpdated": format!("2026-03-0{version}T00:00:00Z")});
                    if mark {
                        let of = v["item"][0]["adjudication"].as_array_mut().unwrap();
                        of.push(
                            serde_json::json!({"category": {"coding": [{"code": "not-counted"}]}}),
                        );
                    }
                }
                format!("{v}\n")
            })
            .collect();
        provider_env(&base, &stream).ok(&[
            "import",
            "--from",
            "fakeprov",
            "--from-profile",
            "adult1",
            "--patient",
            "pat-0001",
            "--person",
            "Robin",
        ])
    };
    import(1, false);
    import(2, true);
    import(3, true);
    let index = std::fs::read(base.records().join("Robin Testperson/index.json")).unwrap();
    let index: Value = serde_json::from_slice(&index).unwrap();
    let records = index["records"].as_array().unwrap().iter();
    let claims: Vec<&Value> = records.filter(|r| r["kind"] == "claim").collect();
    let id = claims[0]["id"].as_str().unwrap().to_string();

    // As its source sent it, each version that is kept.
    let raw = base.ok(&["records", "raw", &id]);
    assert_eq!(raw["schema"], "record-raw/v1");
    let files = raw["files"].as_array().unwrap();
    assert_eq!(files.len(), 3, "{raw}");
    let versions: Vec<&Value> = files
        .iter()
        .map(|f| &f["resource"]["meta"]["versionId"])
        .collect();
    assert_eq!(versions, ["1", "2", "3"]);
    assert_eq!(files[2]["version"], "current");
    assert!(files[..2].iter().all(|f| f["version"] != "current"));
    assert!(raw.get("notes").is_none());

    // What the later one changed, and no value of either.
    let no_value = |text: &str| {
        for word in [
            "eob-rehab",
            "Sample",
            "Example Health",
            "97110",
            "200",
            "2024",
            "2026-03",
            "Robin",
        ] {
            assert!(!text.contains(word), "{word}: {text}");
        }
    };
    let changes = base.ok(&["records", "raw", &id, "--changes"]);
    let of = &changes["files"][0];
    assert_eq!(
        (&of["cli"], &of["versions"]),
        (&"fakeprov".into(), &3.into()),
        "{changes}"
    );
    assert_eq!((&of["ordered"], &of["unread"]), (&true.into(), &0.into()));
    let steps = of["steps"].as_array().unwrap();
    let ends: Vec<(&Value, &Value)> = steps.iter().map(|s| (&s["from"], &s["to"])).collect();
    let named = |i: usize| &files[i]["version"];
    assert_eq!(ends, [(named(0), named(1)), (named(1), named(2))]);
    let last = steps[1]["changed"].as_array().unwrap().iter();
    let last: Vec<&Value> = last.map(|c| &c["path"]).collect();
    assert_eq!(last, ["meta.lastUpdated", "meta.versionId"]);
    let changed = steps[0]["changed"].as_array().unwrap();
    let row = |path: &str| {
        changed
            .iter()
            .find(|c| c["path"] == path)
            .cloned()
            .unwrap_or_default()
    };
    let code = "item[].adjudication[].category.coding[].code";
    assert_eq!(
        row(code),
        serde_json::json!({"path": code, "added": 1, "removed": 0})
    );
    let at = "meta.versionId";
    assert_eq!(
        row(at),
        serde_json::json!({"path": at, "added": 1, "removed": 1})
    );
    no_value(&changes["files"].to_string());

    let fields = base.ok(&["records", "raw", &id, "--fields"]);
    let word = |v: &Value, word: &str| -> Value {
        let words = v["types"][0]["words"].as_array().unwrap().iter();
        let mut of = words.filter(|w| w["path"] == code && w["word"] == word);
        of.next().map_or(Value::Null, |w| w["n"].clone())
    };
    assert_eq!(word(&fields, "not-counted"), 1, "{fields}");
    no_value(&fields["types"].to_string());

    // Of many, each file once.
    let all = base.ok(&["records", "fields", "--person", "Robin", "--kind", "claim"]);
    assert_eq!(all["schema"], "raw-fields/v1");
    let counts = ["records", "without_raw", "unread"].map(|k| all[k].as_u64().unwrap());
    assert_eq!(counts, [claims.len() as u64, 0, 0]);
    assert_eq!(all["types"].as_array().unwrap().len(), 1, "{all}");
    assert_eq!(all["types"][0]["resource_type"], "ExplanationOfBenefit");
    assert_eq!(word(&all, "not-counted"), 1, "{all}");
    // Of each the current version alone.
    assert_eq!(all["types"][0]["files"], claims.len() as u64);
    let submitted = word(&fields, "submitted");
    assert!(submitted.as_u64() > Some(0), "{fields}");
    assert_eq!(word(&all, "submitted"), submitted);
    no_value(&all.to_string());
    let alex = base.ok(&["records", "fields", "--person", "Alex"]);
    assert_eq!(
        (&alex["records"], &alex["types"]),
        (&0.into(), &serde_json::json!([]))
    );
    let two = ["records", "fields", "--kind", "claim", "--kind", "result"];
    assert_eq!(base.ok(&two)["types"].as_array().unwrap().len(), 2);
    let unknown = base.run(&["records", "fields", "--kind", "claims"]);
    assert_eq!(unknown.status.code(), Some(2));
    let every = base.ok(&["records", "fields"]);
    assert!(every["types"].as_array().unwrap().len() > 4, "{every}");
    no_value(&every.to_string());
    // Nor any text, date or id of the records.
    let mut said: Vec<String> = Vec::new();
    for r in index["records"].as_array().unwrap() {
        for k in ["display", "date"] {
            said.extend(r[k].as_str().map(str::to_string));
        }
        let of = &r["provenance"][0]["source_resource"]["id"];
        said.extend(of.as_str().map(str::to_string));
        let texts = r["fields"].as_object().unwrap().values();
        said.extend(texts.filter_map(|v| Some(v.as_str()?.to_string())));
    }
    said.retain(|w| w.len() > 5);
    assert!(said.len() > 20, "{said:?}");
    let text = every.to_string();
    for w in &said {
        let quoted = format!("\"{w}\"");
        assert!(!text.contains(&quoted), "{w}: {text}");
    }
    for args in [
        &["records", "fields", "--kind", "claim"][..],
        &["records", "raw", &id, "--fields"],
        &["records", "raw", &id, "--changes"],
    ] {
        let out = base.run(args);
        let text = String::from_utf8(out.stdout).unwrap();
        assert!(text.contains(code), "{text}");
        no_value(&text);
    }

    // An earlier one that does not say when it was made: each is compared with
    // the current one.
    let undated = base.records().join(files[0]["path"].as_str().unwrap());
    let undated = undated.with_file_name(format!(
        "{}~0undated.json",
        files[2]["path"]
            .as_str()
            .unwrap()
            .rsplit('/')
            .next()
            .unwrap()
            .trim_end_matches(".json")
    ));
    let mut of = files[0]["resource"].clone();
    of.as_object_mut().unwrap().remove("meta");
    std::fs::write(&undated, of.to_string()).unwrap();
    let changes = base.ok(&["records", "raw", &id, "--changes"]);
    let of = &changes["files"][0];
    assert_eq!(
        (&of["versions"], &of["ordered"]),
        (&4.into(), &false.into()),
        "{of}"
    );
    let steps = of["steps"].as_array().unwrap();
    assert_eq!(steps.len(), 3);
    assert!(steps.iter().all(|s| s["to"] == "current"), "{of}");
    let says = "their order is not known";
    let text =
        || String::from_utf8(base.run(&["records", "raw", &id, "--changes"]).stdout).unwrap();
    assert!(text().contains(says), "{}", text());
    let path = base.records().join(files[2]["path"].as_str().unwrap());
    let current = std::fs::read(&path).unwrap();
    std::fs::write(&path, "{").unwrap();
    let changes = base.ok(&["records", "raw", &id, "--changes"]);
    let of = &changes["files"][0];
    assert_eq!(
        (&of["ordered"], &of["steps"]),
        (&false.into(), &serde_json::json!([]))
    );
    assert!(!text().contains(says), "{}", text());
    std::fs::write(&path, current).unwrap();
    std::fs::remove_file(&undated).unwrap();

    // One file that does not read, or is not there, is counted, and named when
    // it is asked for.
    let rel = files[2]["path"].as_str().unwrap();
    let path = base.records().join(rel);
    for left in [Some(b"{".as_slice()), Some(b"{\"id\": \"\xff\"}"), None] {
        match left {
            Some(bytes) => std::fs::write(&path, bytes).unwrap(),
            None => std::fs::remove_file(&path).unwrap(),
        }
        let all = base.ok(&["records", "fields", "--kind", "claim"]);
        assert_eq!(all["unread"], 1, "{all}");
        let of = base.ok(&["records", "raw", &id, "--fields"]);
        assert_eq!(of["unread"], 1, "{of}");
        let of = base.ok(&["records", "raw", &id, "--changes"]);
        let of = &of["files"][0];
        assert_eq!(
            (&of["versions"], &of["unread"]),
            (&2.into(), &1.into()),
            "{of}"
        );
        // The earlier ones are in their order all the same.
        let steps = of["steps"].as_array().unwrap();
        let ends: Vec<(&Value, &Value)> = steps.iter().map(|s| (&s["from"], &s["to"])).collect();
        assert_eq!(
            (&of["ordered"], ends),
            (&true.into(), vec![(named(0), named(1))])
        );
        let out = base.run(&["records", "raw", &id]);
        assert_eq!(out.status.code(), Some(1));
        let says = String::from_utf8(out.stderr).unwrap();
        assert!(
            says.contains(rel) && says.contains("does not read"),
            "{says}"
        );
    }
    // The answers with no value name no file in an error either.
    let of_one = |how| ["records", "raw", id.as_str(), how];
    let no_name = |args: [&str; 4]| {
        let out = base.run(&args);
        let how = args.join(" ");
        assert_eq!(out.status.code(), Some(1), "{how}");
        let says = String::from_utf8(out.stderr).unwrap();
        assert!(says.contains("could not be read"), "{how}: {says}");
        for text in [String::from_utf8(out.stdout).unwrap(), says] {
            no_value(&text);
        }
    };
    std::fs::write(&path, "{}").unwrap();
    let may = |mode: u32| {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
    };
    may(0o000);
    let all = ["records", "fields", "--kind", "claim"];
    for args in [of_one("--changes"), of_one("--fields"), all] {
        no_name(args);
    }
    may(0o600);
    let dir = path.parent().unwrap();
    let aside = dir.with_extension("aside");
    std::fs::rename(dir, &aside).unwrap();
    std::fs::write(dir, "not a folder").unwrap();
    for args in [of_one("--changes"), of_one("--fields"), all] {
        no_name(args);
    }
    // Nor who can be meant.
    let out = base.run(&["records", "fields", "--person", "Testperson"]);
    assert_eq!(out.status.code(), Some(4));
    let says = String::from_utf8(out.stderr).unwrap();
    assert!(!says.contains("Robin") && !says.contains("Alex"), "{says}");

    // A record that no raw file is kept of.
    let mut index = index.clone();
    let mut without = claims[0].clone();
    without["id"] = "r_withoutaaaaaaaaaaaaa".into();
    without["key"] = "without".into();
    for p in without["provenance"].as_array_mut().unwrap() {
        p.as_object_mut().unwrap().remove("raw_path");
    }
    index["records"].as_array_mut().unwrap().push(without);
    let file = base.records().join("Robin Testperson/index.json");
    std::fs::write(file, index.to_string()).unwrap();
    for how in [None, Some("--fields"), Some("--changes")] {
        let args = ["records", "raw", "r_withoutaaaaaaaaaaaaa"];
        let args: Vec<&str> = args.into_iter().chain(how).collect();
        assert_eq!(base.run(&args).status.code(), Some(4), "{how:?}");
    }
    std::fs::remove_file(dir).unwrap();
    let many = base.ok(&["records", "fields", "--kind", "claim"]);
    assert_eq!(
        (&many["records"], &many["without_raw"]),
        (&2.into(), &1.into())
    );
    let none = base.run(&["records", "raw", "r_aaaaaaaaaaaaaaaaaaaa"]);
    assert_eq!(none.status.code(), Some(4));
}

/// What each record says of the providers it prints, by its kind and field.
fn providers_of(index: &Value) -> Vec<String> {
    let mut out: Vec<String> = index["records"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|r| {
            let kind = r["kind"].as_str().unwrap().to_string();
            let date = r["date"].as_str().unwrap_or("").to_string();
            r["providers"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .map(move |p| {
                    format!(
                        "{kind} {date} {} {} {}",
                        p["field"].as_str().unwrap(),
                        p["npi"].as_str().unwrap_or("-"),
                        p["state"].as_str().unwrap_or("-")
                    )
                })
        })
        .collect();
    out.sort();
    out
}

/// A record keeps the NPI and the state its source gives for each provider
/// it prints, through a later import that no longer serves the practitioner,
/// and `reindex` gives them to records that lack them.
#[test]
fn a_record_keeps_who_its_source_says_a_provider_is() {
    let base = Env::new("provider-refs");
    setup(&base);
    let e = provider_env(&base, medrec::testdata::PROVIDERS_STREAM);
    let import = |e: &Env| {
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
        ])
    };
    import(&e);
    let path = e.records().join("Robin Testperson/index.json");
    let read =
        || -> Value { serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap() };
    let want = [
        "claim 2026-02-10 facility - TX",
        "claim 2026-02-10 provider 0000000022 -",
        "encounter 2026-02-10 location - TX",
        "encounter 2026-02-10 practitioner 0000000006 FL",
        "encounter 2026-02-10 service_provider 0000000022 GA",
        "encounter 2026-02-17 location - GA",
        "immunization 2026-02-10 performer 0000000048 -",
        "medication 2026-02-10 prescriber 0000000014 TX",
        "medication 2026-02-11 pharmacy 0000000030 -",
        "medication 2026-02-11 prescriber 0000000014 TX",
        "medication 2026-02-12 prescriber 0000000006 FL",
        "medication 2026-02-13 pharmacy - TX",
        "medication 2026-02-13 prescriber 0000000006 FL",
        "procedure 2026-02-10 performer 0000000006 FL",
    ];
    assert_eq!(providers_of(&read()), want);

    // Served again unchanged, nothing is written; and a later stream that
    // serves the claim and the visits but not the practitioners reads them
    // from their own raw files.
    let again = import(&e);
    assert_eq!(again["records_updated"], 0, "{again}");
    let dropped = [
        "Practitioner",
        "PractitionerRole",
        "Organization",
        "Location",
        "MedicationRequest",
    ];
    let later: String = medrec::testdata::PROVIDERS_STREAM
        .lines()
        .filter_map(|l| {
            let mut v: Value = serde_json::from_str(l).unwrap();
            if dropped.contains(&v["resourceType"].as_str().unwrap_or("")) {
                return None;
            }
            if let Some(counts) = v.get_mut("counts").and_then(Value::as_object_mut) {
                counts.retain(|k, _| !dropped.contains(&k.as_str()));
            }
            if v["id"] == "enc-a" {
                v["meta"]["versionId"] = "2".into();
            }
            Some(format!("{}\n", serde_json::to_string(&v).unwrap()))
        })
        .collect();
    let e2 = provider_env(&base, &later);
    import(&e2);
    assert_eq!(providers_of(&read()), want);

    // An index without them, as an earlier medrec wrote it.
    let mut old = read();
    for r in old["records"].as_array_mut().unwrap() {
        r.as_object_mut().unwrap().remove("providers");
    }
    std::fs::write(&path, serde_json::to_string(&old).unwrap()).unwrap();
    assert!(providers_of(&read()).is_empty());
    let r = e2.ok(&["reindex"]);
    assert_eq!(r["providers_updated"], 9, "{r}");
    assert_eq!(providers_of(&read()), want);
    assert_eq!(e2.ok(&["reindex"])["providers_updated"], 0);

    // A raw file that no longer reads takes nothing from a record, and
    // neither does one that is gone.
    let raw = e
        .records()
        .join("Robin Testperson/Records/fakeprov-example-health");
    std::fs::write(raw.join("Practitioner/prac-one.json"), "{").unwrap();
    std::fs::remove_file(raw.join("Location/loc-two.json")).unwrap();
    assert_eq!(e2.ok(&["reindex"])["providers_updated"], 0);
    assert_eq!(providers_of(&read()), want);
}

/// Two sources state one fact and print it alike; one identifies its
/// providers and the other does not. Neither takes from the other, whichever
/// imports last.
#[test]
fn two_sources_of_one_record_keep_what_either_says_of_its_providers() {
    let base = Env::new("provider-two");
    setup(&base);
    let only = |v: &Value, kinds: &[&str]| {
        v["resourceType"]
            .as_str()
            .is_none_or(|t| t == "Patient" || kinds.contains(&t))
    };
    let stream = |org: &str, kinds: &[&str]| -> String {
        medrec::testdata::PROVIDERS_STREAM
            .lines()
            .filter_map(|l| {
                let mut v: Value = serde_json::from_str(l).unwrap();
                if !only(&v, kinds) {
                    return None;
                }
                if let Some(counts) = v.get_mut("counts").and_then(Value::as_object_mut) {
                    counts.retain(|k, _| k == "Patient" || kinds.contains(&k.as_str()));
                }
                if v["organization"].is_object() {
                    v["organization"]["id"] = org.into();
                    v["organization"]["fhir_base"] = format!("https://{org}.example.org").into();
                }
                Some(format!("{}\n", serde_json::to_string(&v).unwrap()))
            })
            .collect()
    };
    let bare = stream("one", &["Encounter"]);
    let full = stream(
        "two",
        &["Encounter", "Practitioner", "Organization", "Location"],
    );
    let import = |s: &str| {
        provider_env(&base, s).ok(&[
            "import",
            "--from",
            "fakeprov",
            "--from-profile",
            "adult1",
            "--patient",
            "pat-0001",
            "--person",
            "Robin",
        ])
    };
    let path = base.records().join("Robin Testperson/index.json");
    let read =
        || -> Value { serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap() };
    let want = [
        "encounter 2026-02-10 location - TX",
        "encounter 2026-02-10 practitioner 0000000006 FL",
        "encounter 2026-02-10 service_provider 0000000022 GA",
        "encounter 2026-02-17 location - GA",
    ];
    import(&bare);
    assert!(providers_of(&read()).is_empty());
    import(&full);
    assert_eq!(read()["records"].as_array().unwrap().len(), 2);
    assert_eq!(providers_of(&read()), want);
    for s in [&full, &bare, &full] {
        let r = import(s);
        assert_eq!(r["records_updated"], 0, "{r}");
        assert_eq!(providers_of(&read()), want);
    }
    assert_eq!(base.ok(&["reindex"])["providers_updated"], 0);
    assert_eq!(providers_of(&read()), want);
}

/// What each source says of a condition and of a medicine, kept through
/// imports in any order and given back by `reindex`.
#[test]
fn a_record_keeps_what_each_source_says_of_its_status() {
    const STATUS: &str = include_str!("fixtures/fhir/status.json");
    let base = Env::new("status-words");
    setup(&base);
    let stream = |org: &str, version: u32, clinical: &str, medicine: &str| -> String {
        let mut resources: Vec<Value> = serde_json::from_str(STATUS).unwrap();
        for r in &mut resources {
            if r["id"] == "cond-period" {
                r["clinicalStatus"]["coding"][0]["code"] = clinical.into();
                r["meta"] = serde_json::json!({"versionId": version.to_string(),
                    "lastUpdated": format!("2026-02-0{version}T00:00:00Z")});
            }
            if r["id"] == "med-prescription" {
                r["status"] = medicine.into();
            }
            let of = if r["resourceType"] == "AllergyIntolerance" {
                "patient"
            } else {
                "subject"
            };
            r[of] = serde_json::json!({"reference": "Patient/pat-0001"});
        }
        let mut counts = serde_json::Map::from_iter([("Patient".to_string(), 1.into())]);
        for r in &resources {
            let n = counts
                .entry(r["resourceType"].as_str().unwrap())
                .or_insert(0.into());
            *n = (n.as_u64().unwrap() + 1).into();
        }
        let mut out: Vec<Value> = Vec::new();
        for l in medrec::testdata::PROVIDERS_STREAM.lines() {
            let mut v: Value = serde_json::from_str(l).unwrap();
            if v["organization"].is_object() {
                v["organization"]["id"] = org.into();
                v["organization"]["fhir_base"] = format!("https://{org}.example.org").into();
            }
            if v.get("counts").is_some() {
                v["counts"] = counts.clone().into();
                out.append(&mut resources);
            }
            if v["resourceType"].as_str().is_none_or(|t| t == "Patient") {
                out.push(v);
            }
        }
        out.iter().map(|v| format!("{v}\n")).collect()
    };
    let import = |s: &str| {
        provider_env(&base, s).ok(&[
            "import",
            "--from",
            "fakeprov",
            "--from-profile",
            "adult1",
            "--patient",
            "pat-0001",
            "--person",
            "Robin",
        ])
    };
    let path = base.records().join("Robin Testperson/index.json");
    let read =
        || -> Value { serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap() };
    let record = |v: &Value, display: &str| -> Value {
        let all = v["records"].as_array().unwrap();
        all.iter()
            .find(|r| r["display"] == display)
            .cloned()
            .unwrap()
    };
    let said = |display: &str| -> Vec<String> {
        let r = record(&read(), display);
        let words = r["words"].as_array().cloned().unwrap_or_default();
        let word = |w: &Value| {
            let of = w["raw_path"]
                .as_str()
                .unwrap()
                .split('/')
                .nth(2)
                .unwrap()
                .to_string();
            format!("{of} {}", w["status"].as_str().unwrap_or("-"))
        };
        std::iter::once(r["status"].as_str().unwrap().to_string())
            .chain(words.iter().map(word))
            .collect()
    };
    let one = stream("one", 1, "resolved", "active");
    let two = stream("two", 1, "active", "stopped");
    import(&one);
    let r = import(&two);
    assert_eq!(r["records_added"], 0, "{r}");
    let condition = ["resolved", "fakeprov-one resolved", "fakeprov-two active"];
    let medicine = ["active", "fakeprov-one active", "fakeprov-two stopped"];
    assert_eq!(said("Sample stiffness"), condition);
    assert_eq!(said("Sample syrup"), medicine);
    // Each served again as it was writes nothing.
    for s in [&one, &two, &one] {
        let r = import(s);
        assert_eq!(r["records_updated"], 0, "{r}");
        assert_eq!(said("Sample stiffness"), condition);
    }
    assert_eq!(base.ok(&["reindex"])["statuses_updated"], 0);

    // A change at one is its word, and writes the record.
    let newer = stream("two", 2, "inactive", "stopped");
    let changed = ["inactive", "fakeprov-one resolved", "fakeprov-two inactive"];
    let before = std::fs::read(&path).unwrap();
    import(&newer);
    assert_eq!(said("Sample stiffness"), changed);
    // An import cut short after its raw files, and run again; and with a
    // reindex between, which reads what the source says and writes no field
    // of it.
    for reindexed in [false, true] {
        std::fs::write(&path, &before).unwrap();
        if reindexed {
            assert!(base.ok(&["reindex"])["statuses_updated"].as_u64() > Some(0));
            assert_eq!(said("Sample stiffness")[1..], changed[1..]);
            assert_eq!(said("Sample stiffness")[0], condition[0]);
        } else {
            assert_eq!(said("Sample stiffness"), condition);
        }
        import(&newer);
        assert_eq!(said("Sample stiffness"), changed);
    }
    let r = import(&newer);
    assert_eq!(r["records_updated"], 0, "{r}");
    import(&one);
    assert_eq!(said("Sample stiffness"), changed);
    assert_eq!(base.ok(&["reindex"])["statuses_updated"], 0);
    assert_eq!(said("Sample stiffness"), changed);

    // An index an earlier medrec wrote.
    let full = read();
    let mut old = full.clone();
    for r in old["records"].as_array_mut().unwrap() {
        r.as_object_mut().unwrap().remove("words");
        r.as_object_mut().unwrap().remove("fields_from");
        let kind = r["kind"].as_str().unwrap().to_string();
        let f = r["fields"].as_object_mut().unwrap();
        for k in [
            "onset",
            "recorded",
            "categories",
            "abatement_text",
            "start",
            "end",
            "end_of",
            "status_reason",
            "reported",
        ] {
            f.remove(k);
        }
        if kind == "allergy" {
            f.remove("verification_status");
        }
    }
    std::fs::write(&path, serde_json::to_string(&old).unwrap()).unwrap();
    let kept = std::fs::read(&path).unwrap();
    let r = base.ok(&["reindex", "--dry-run"]);
    assert_eq!(
        (r["statuses_updated"].as_u64(), r["dry_run"].as_bool()),
        (Some(7), Some(true)),
        "{r}"
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        kept,
        "a dry run writes nothing"
    );
    let r = base.ok(&["reindex"]);
    assert_eq!(r["statuses_updated"], 7, "{r}");
    assert!(r.get("dry_run").is_none());
    // All but which version import read in.
    let mut unversioned = full.clone();
    for r in unversioned["records"].as_array_mut().unwrap() {
        for w in r["words"].as_array_mut().into_iter().flatten() {
            assert!(w.as_object_mut().unwrap().remove("version").is_some());
        }
    }
    assert_eq!(read()["records"], unversioned["records"]);
    assert_eq!(base.ok(&["reindex"])["statuses_updated"], 0);
    // Or the next import of each, which serves the same versions: with no
    // writer named, the first to come writes, and then none changes it.
    std::fs::write(&path, serde_json::to_string(&old).unwrap()).unwrap();
    import(&newer);
    import(&one);
    let words = |v: &Value| -> Vec<Value> {
        let records = v["records"].as_array().unwrap();
        records.iter().map(|r| r["words"].clone()).collect()
    };
    assert_eq!(words(&read()), words(&full));
    assert_eq!(said("Sample stiffness"), changed);
    for s in [&newer, &one, &newer] {
        let r = import(s);
        assert_eq!(r["records_updated"], 0, "{r}");
    }
    assert_eq!(base.ok(&["reindex"])["statuses_updated"], 0);
    let full = read();

    // What was said stays where its raw file no longer reads.
    let raw = base.records().join("Robin Testperson/Records/fakeprov-two");
    std::fs::write(raw.join("Condition/cond-period.json"), "{").unwrap();
    std::fs::remove_file(raw.join("MedicationRequest/med-prescription.json")).unwrap();
    assert_eq!(base.ok(&["reindex"])["statuses_updated"], 0);
    assert_eq!(read()["records"], full["records"]);
}

/// A result is imported with its parts, its exact value, its categories and
/// the results it lists, and `reindex` gives them to results and reports that
/// lack them.
#[test]
fn a_results_parts_and_categories_come_with_an_import_and_by_reindex() {
    const READINGS: &str = include_str!("fixtures/fhir/readings.json");
    let base = Env::new("readings");
    setup(&base);
    let cases: Vec<Value> = serde_json::from_str(READINGS).unwrap();
    let mut resources: Vec<Value> = cases.into_iter().map(|c| c["resource"].clone()).collect();
    // Each of its own day, so of its own record.
    for (i, r) in resources.iter_mut().enumerate() {
        r["effectiveDateTime"] = format!("2024-04-{:02}T09:30:00Z", i + 1).into();
    }
    let refs = |ids: [&str; 2]| {
        ids.map(|id| serde_json::json!({"reference": format!("Observation/{id}")}))
    };
    resources.push(serde_json::json!({
        "resourceType": "Observation", "id": "panel-1", "status": "final",
        "code": {"text": "Example heading"}, "effectiveDateTime": "2024-04-21T09:30:00Z",
        "hasMember": refs(["lt-1", "range-1"]),
    }));
    resources.push(serde_json::json!({
        "resourceType": "DiagnosticReport", "id": "report-1", "status": "final",
        "code": {"text": "Example panel"}, "effectiveDateTime": "2024-04-30T09:30:00Z",
        "result": refs(["sum-1", "not-filed"]),
        "category": [{"coding": [
            {"system": "http://terminology.hl7.org/CodeSystem/v2-0074", "code": "LAB"}]}],
    }));
    let observations = resources.len() - 1;
    for r in &mut resources {
        r["subject"] = serde_json::json!({"reference": "Patient/pat-0001"});
    }
    let mut lines: Vec<Value> = Vec::new();
    for l in medrec::testdata::PROVIDERS_STREAM.lines() {
        let mut v: Value = serde_json::from_str(l).unwrap();
        if v.get("counts").is_some() {
            v["counts"] = serde_json::json!({"Patient": 1, "Observation": observations,
                "DiagnosticReport": 1});
            lines.append(&mut resources);
        }
        if v["resourceType"].as_str().is_none_or(|t| t == "Patient") {
            lines.push(v);
        }
    }
    let stream: String = lines.iter().map(|v| format!("{v}\n")).collect();
    provider_env(&base, &stream).ok(&[
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
    let path = base.records().join("Robin Testperson/index.json");
    let read =
        || -> Value { serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap() };
    let whole = read();
    let of_day = |v: &Value, day: &str| -> Value {
        let mut all = v["records"].as_array().unwrap().iter();
        all.find(|r| r["date"] == day).unwrap()["fields"].clone()
    };
    let first = of_day(&whole, "2024-04-01");
    assert_eq!(
        first["value"],
        serde_json::json!({"value": "128/76", "unit": "mmHg"})
    );
    assert_eq!(first["components"].as_array().unwrap().len(), 2, "{first}");
    assert_eq!(first["categories"], serde_json::json!(["vital-signs"]));
    assert_eq!(
        of_day(&whole, "2024-04-30")["categories"],
        serde_json::json!(["LAB"])
    );
    let shown = base.ok(&["results", "--person", "Robin"]).to_string();
    for text in ["128/76", "<0.5", "1:40"] {
        assert!(shown.contains(text), "{text}: {shown}");
    }
    let id_of = |id: &str| -> Value {
        let mut all = whole["records"].as_array().unwrap().iter();
        let its = |r: &&Value| r["provenance"][0]["source_resource"]["id"] == id;
        all.find(its).unwrap()["id"].clone()
    };
    let heading = of_day(&whole, "2024-04-21");
    let want = serde_json::json!([id_of("lt-1"), id_of("range-1")]);
    assert_eq!(heading["members"], want);
    let report = of_day(&whole, "2024-04-30");
    assert_eq!(report["members"], serde_json::json!([id_of("sum-1")]));
    assert_eq!(report["results"], 2);
    let rows = base.ok(&["results", "--person", "Robin"]);
    let mut rows = rows["items"].as_array().unwrap().iter();
    let row = rows.find(|r| r["display"] == "Example heading").unwrap();
    assert_eq!(row["detail"], "panel", "{row}");
    // A part with no value is in what the source gave, and nowhere else.
    const ABSENT: &str = "Sample wristband reading";
    let lists = |of: &Value| -> Vec<Value> {
        let all = of["records"].as_array().unwrap().iter();
        let says = all.filter(|r| r.to_string().contains(ABSENT));
        says.cloned().collect()
    };
    let has = lists(&whole);
    assert_eq!(has.len(), 1);
    let gave = has[0]["given"][0]["fields"]["components"].as_array();
    assert_eq!(gave.unwrap().len(), 3, "{}", has[0]);
    let mut of = has[0].clone();
    of.as_object_mut().unwrap().remove("given");
    assert!(!of.to_string().contains(ABSENT), "{of}");
    for args in [
        &["results", "--person", "Robin"] as &[&str],
        &["timeline", "--person", "Robin"],
        &["search", "wristband"],
    ] {
        let v = base.ok(args);
        assert!(!v.to_string().contains(ABSENT), "{args:?}: {v}");
    }
    assert_eq!(
        base.ok(&["search", "wristband"])["items"],
        serde_json::json!([])
    );
    for f in std::fs::read_dir(base.records().join("views"))
        .unwrap()
        .flatten()
    {
        let files = std::fs::read_dir(f.path()).into_iter().flatten().flatten();
        for file in files
            .map(|f| f.path())
            .chain([f.path()])
            .filter(|p| p.is_file())
        {
            let text = String::from_utf8_lossy(&std::fs::read(&file).unwrap()).to_lowercase();
            assert!(!text.contains("wristband"), "{}", file.display());
        }
    }
    let r = base.ok(&["reindex"]);
    assert_eq!(
        (&r["results_updated"], &r["given_updated"]),
        (&0.into(), &0.into())
    );

    // An index without them, as an earlier medrec wrote it.
    let mut old = read();
    let mut lacking = 0;
    let without = |v: &mut Value| {
        let absent = v.to_string().contains(ABSENT);
        absent && v.as_object_mut().unwrap().remove("given").is_some()
    };
    let less = old["records"].as_array_mut().unwrap().iter_mut();
    let given = less.map(without).filter(|had| *had).count();
    assert_eq!(given, 1);
    for r in old["records"].as_array_mut().unwrap() {
        let listing = r.as_object_mut().unwrap().remove("lists").is_some();
        let fields = r["fields"].as_object_mut().unwrap();
        fields.remove("members");
        let parts = fields.remove("components").is_some();
        let had = fields.len();
        fields.retain(|k, v| k != "categories" && !(k == "value" && parts && v.is_string()));
        lacking += usize::from(listing || parts || fields.len() < had);
    }
    std::fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();
    let r = base.ok(&["reindex", "--dry-run"]);
    assert_eq!(r["results_updated"], lacking, "{r}");
    assert_eq!(read()["records"], old["records"]);
    assert_eq!(base.ok(&["reindex"])["results_updated"], lacking);
    assert_eq!(read()["records"], whole["records"]);
    assert_eq!(base.ok(&["reindex"])["results_updated"], 0);

    // A raw file that no longer reads takes nothing from a result.
    let raw = base
        .records()
        .join("Robin Testperson/Records/fakeprov-example-health/Observation");
    std::fs::write(raw.join("bp-1.json"), "{").unwrap();
    assert_eq!(base.ok(&["reindex"])["results_updated"], 0);
    assert_eq!(of_day(&read(), "2024-04-01"), first);
    let _ = std::fs::remove_dir_all(&base.root);
}

/// An import says how many of the records its source gave before it does not
/// have, and the names of what changed in those it updated.
#[test]
fn an_import_counts_what_is_not_served_and_names_what_changed() {
    let base = Env::new("not-served");
    setup(&base);
    let import = |stream: &[Value], more: &[&str]| -> Value {
        let stream: String = stream.iter().map(|v| format!("{v}\n")).collect();
        let args = ["import", "--from", "fakeprov", "--from-profile", "adult1"];
        let who = ["--patient", "pat-0001", "--person", "Robin"];
        let args: Vec<&str> = args.iter().chain(&who).chain(more).copied().collect();
        provider_env(&base, &stream).ok(&args)
    };
    let all: Vec<Value> = medrec::testdata::STREAM
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let first = import(&all, &[]);
    assert_eq!(first["records_not_served"], 0, "{first}");
    assert_eq!(first["not_served"], serde_json::json!({}));
    assert_eq!(first["updated"], serde_json::json!([]));

    // Without its claim, and with a later word of its condition.
    let is = |v: &Value, t: &str| v["resourceType"] == t;
    let mut less: Vec<Value> = all.clone();
    less.retain(|v| !is(v, "ExplanationOfBenefit"));
    for v in &mut less {
        if is(v, "Condition") {
            v["clinicalStatus"] = serde_json::json!({"coding": [{"code": "resolved"}]});
            v["meta"]["lastUpdated"] = "2026-03-06T00:00:00Z".into();
        }
        if let Some(n) = v.get_mut("counts") {
            n.as_object_mut().unwrap().remove("ExplanationOfBenefit");
        }
    }
    let second = import(&less, &[]);
    assert_eq!(second["records_not_served"], 1, "{second}");
    assert_eq!(second["not_served"], serde_json::json!({"claim": 1}));
    let updated = second["updated"].as_array().unwrap();
    assert_eq!(updated.len(), 1, "{second}");
    assert_eq!(updated[0]["kind"], "condition");
    let changed: Vec<&str> = updated[0]["changed"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(changed, ["status", "words"]);
    // It is kept, and counted by each import from then on.
    let path = base.records().join("Robin Testperson/index.json");
    let index: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let kept = index["records"].as_array().unwrap();
    assert!(kept.iter().any(|r| r["kind"] == "claim"));
    let runs = index["imports"].as_array().unwrap();
    assert_eq!(runs.last().unwrap()["records_not_served"], 1);
    let third = import(&less, &[]);
    assert_eq!(third["records_not_served"], 1);
    assert_eq!(third["updated"], serde_json::json!([]));

    // Where a type was withheld none of it is counted, and an export of what
    // changed since a day tells nothing.
    let mut withheld = less.clone();
    withheld.last_mut().unwrap()["skipped"] = serde_json::json!([
        {"resource_type": "ExplanationOfBenefit", "reason": "withheld", "code": "forbidden"}]);
    assert_eq!(import(&withheld, &[])["records_not_served"], 0);
    let since = import(&less, &["--since", "2024-01-01"]);
    assert!(since["records_not_served"].is_null(), "{since}");
    assert!(since["not_served"].is_null());
    let mut partial = less.clone();
    partial[0]["since"] = "2024-01-01".into();
    let since = import(&partial, &[]);
    assert!(since["records_not_served"].is_null(), "{since}");
    assert!(since["not_served"].is_null());
    assert_eq!(import(&all, &[])["records_not_served"], 0);

    // Nor do downloaded files, which are what is in a directory; and what only
    // they gave is none that a sign-in stopped giving.
    let statements = base.root.join("statements");
    std::fs::create_dir_all(&statements).unwrap();
    let dir = statements.to_str().unwrap();
    let at = |stream: &[Value], minute: u32| -> Vec<Value> {
        let mut stream = stream.to_vec();
        stream[0]["pulled_at"] = format!("2026-03-07T09:{minute:02}:00Z").into();
        stream
    };
    let files = |stream: &[Value]| -> Value {
        let stream: String = stream.iter().map(|v| format!("{v}\n")).collect();
        let args = ["--json", "import", "--from", "fakeprov"];
        let args = [&args[..], &["--from-files", dir, "--patient", "pat-0001"]].concat();
        let e = provider_env(&base, &stream);
        let out = e.run_env(&args, &[("FAKEPROV_INFO", "files")]);
        assert!(out.status.success(), "{:?}", said(&out));
        serde_json::from_slice(&out.stdout).unwrap()
    };
    let r = files(&at(&less, 1));
    assert!(r["records_not_served"].is_null(), "{r}");
    assert!(r["not_served"].is_null());
    let index: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let runs = index["imports"].as_array().unwrap();
    assert_eq!(runs.last().unwrap()["from_files"], true);
    assert_eq!(runs[0]["from_files"], false);
    assert_eq!(import(&at(&less, 2), &[])["records_not_served"], 1);
    let more = |version: u32| -> Vec<Value> {
        let mut stream = all.clone();
        let another = stream.iter().find(|v| is(v, "AllergyIntolerance"));
        let mut another = another.unwrap().clone();
        another["id"] = "another".into();
        another["code"] = serde_json::json!({"text": "Example pollen"});
        another["meta"]["lastUpdated"] = format!("2026-03-0{version}T00:00:00Z").into();
        stream.insert(1, another);
        let counts = stream.last_mut().unwrap()["counts"].as_object_mut();
        let n = &mut counts.unwrap()["AllergyIntolerance"];
        *n = (n.as_u64().unwrap() + 1).into();
        stream
    };
    assert_eq!(files(&at(&more(1), 3))["records_added"], 1);
    assert_eq!(import(&at(&all, 4), &[])["records_not_served"], 0);
    import(&at(&more(2), 5), &[]);
    let r = import(&at(&all, 6), &[]);
    assert_eq!(r["not_served"], serde_json::json!({"allergy": 1}), "{r}");
    // A sign-in through a provider that also reads files is one.
    let stream: String = at(&less, 7).iter().map(|v| format!("{v}\n")).collect();
    let args = ["--json", "import", "--from", "fakeprov"];
    let args = [
        &args[..],
        &["--from-profile", "adult1", "--patient", "pat-0001"],
    ]
    .concat();
    let out = provider_env(&base, &stream).run_env(&args, &[("FAKEPROV_INFO", "files")]);
    assert!(out.status.success(), "{:?}", said(&out));
    let r: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(r["records_not_served"].is_u64(), "{r}");
    let index: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let last = index["imports"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(last["from_files"], false);
    // One that signs in nowhere reads the files its environment names.
    let stream: String = at(&less, 8).iter().map(|v| format!("{v}\n")).collect();
    let args = ["--json", "import", "--from", "fakeprov"];
    let args = [&args[..], &["--patient", "pat-0001"]].concat();
    let env = [("FAKEPROV_INFO", "files-only"), ("FAKEPROV_FILES", dir)];
    let out = provider_env(&base, &stream).run_env(&args, &env);
    assert!(out.status.success(), "{:?}", said(&out));
    let r: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(r["records_not_served"].is_null(), "{r}");
    let index: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let last = index["imports"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(last["from_files"], true);
    let _ = std::fs::remove_dir_all(&base.root);
}

#[test]
fn a_claim_line_its_totals_leave_out_is_marked_and_kept_through_reindex() {
    let base = Env::new("claim-lines");
    setup(&base);
    let e = provider_env(&base, &with_left_out_line());
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
    let path = e.records().join("Robin Testperson/index.json");
    let claim_of = |v: &Value| {
        v["records"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["kind"] == "claim")
            .cloned()
            .unwrap()
    };
    let read =
        || -> Value { serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap() };
    let want = read();
    let f = &claim_of(&want)["fields"];
    let counted: Vec<bool> = f["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["counted"] != false)
        .collect();
    assert_eq!(counted, [true, true, false, true]);
    assert_eq!(f["items"][0]["revenue"]["code"], "0420");
    assert_eq!(
        f["payer_notes"][0],
        "P7 -- A sample note about the whole claim."
    );

    // Readers give the claim's totals.
    let claims = e.ok(&["timeline", "--kind", "claim"]);
    assert_eq!(
        claims["items"][0]["detail"],
        "billed 390.0 USD, plan paid 186.0 USD, you owe 42.0 USD"
    );
    // A line's processing note is searchable.
    let hits = e.ok(&["search", "session limit"]);
    assert_eq!(hits["items"][0]["kind"], "claim", "{hits}");

    // A claim filed by 0.8.1, without these fields, takes them on reindex.
    let mut old = want.clone();
    for r in old["records"].as_array_mut().unwrap() {
        if r["kind"] == "claim" {
            let f = r["fields"].as_object_mut().unwrap();
            f.remove("payer_notes");
            for i in f["items"].as_array_mut().unwrap() {
                let i = i.as_object_mut().unwrap();
                for k in ["counted", "payer_notes", "revenue"] {
                    i.remove(k);
                }
            }
        }
    }
    std::fs::write(&path, serde_json::to_string(&old).unwrap()).unwrap();
    let r = e.ok(&["reindex"]);
    assert_eq!(r["reshaped"], 1, "{r}");
    assert_eq!(claim_of(&read())["fields"], claim_of(&want)["fields"]);
    assert_eq!(claim_of(&read())["display"], claim_of(&want)["display"]);
    assert_eq!(e.ok(&["reindex"])["reshaped"], 0);
    let _ = std::fs::remove_dir_all(&e.root);
}

/// Whether the views are those of every person's current `index.json`.
fn views_current(e: &Env) -> bool {
    let h: Value =
        serde_json::from_str(&std::fs::read_to_string(e.records().join("household.json")).unwrap())
            .unwrap();
    let v: Value = serde_json::from_str(
        &std::fs::read_to_string(e.records().join("views/views.json")).unwrap(),
    )
    .unwrap();
    h["people"].as_array().unwrap().iter().all(|p| {
        let index = e
            .records()
            .join(p["folder"].as_str().unwrap())
            .join("index.json");
        !index.exists() || v["people"][p["id"].as_str().unwrap()]["index"] == md5_of(&index)
    })
}

#[test]
fn a_list_files_each_line_with_its_own_values_and_the_views_once() {
    if !has("pdftotext") {
        eprintln!("skipped: pdftotext (poppler) is not installed");
        return;
    }
    let e = Env::new("lists");
    setup(&e);
    let scans = e.root.join("scans");
    medrec::testdata::write_scans(&scans);
    let f = |n: &str| scans.join(n).to_string_lossy().into_owned();
    let list = e.root.join("intake.ndjson");
    let line = |v: Value| format!("{}\n", serde_json::to_string(&v).unwrap());
    let lines = [
        line(
            serde_json::json!({"file": f("visit_alex.pdf"), "person": "Alex", "type": "visit_summary", "date": "2026-02-01", "org": "Example Health"}),
        ),
        line(
            serde_json::json!({"file": f("lab_robin.pdf"), "person": "Robin", "type": "lab_report", "date": "2026-03-04"}),
        ),
        line(serde_json::json!({"file": f("ambiguous.pdf")})),
        line(serde_json::json!({"file": f("not-there.pdf")})),
    ];
    // A bad line anywhere stops the list before anything is filed.
    std::fs::write(
        &list,
        format!(
            "{}{}",
            lines[0],
            line(serde_json::json!({"file": f("eob_alex.pdf"), "type": "statement"}))
        ),
    )
    .unwrap();
    let bad = e.run(&["--json", "intake", "--from", list.to_str().unwrap()]);
    assert_eq!(bad.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&bad.stdout).contains("list line 2: unknown document type"));
    assert!(!e.records().join("Alex Testperson/Documents").exists());
    // The list and the flags it replaces do not mix.
    assert_eq!(
        e.run(&[
            "intake",
            "--from",
            list.to_str().unwrap(),
            "--person",
            "Alex"
        ])
        .status
        .code(),
        Some(2)
    );

    std::fs::write(&list, lines.concat()).unwrap();
    // The missing file fails as the command would, and the rest are filed.
    let out = e.run(&["--json", "intake", "--from", list.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1));
    let r: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        (
            r["counts"]["filed"].as_u64(),
            r["counts"]["review"].as_u64(),
            r["counts"]["failed"].as_u64()
        ),
        (Some(2), Some(1), Some(1)),
        "{r}"
    );
    let failed = &r["items"][3];
    assert_eq!(
        (&failed["line"], &failed["status"], &failed["error_code"]),
        (
            &Value::from(4),
            &Value::from("failed"),
            &Value::from("not_found")
        ),
        "{failed}"
    );
    assert_eq!(
        r["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["line"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        [1, 2, 3, 4]
    );
    let by = |src: &str| {
        r["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["source"] == src)
            .unwrap()
            .clone()
    };
    assert_eq!(by("visit_alex.pdf")["person_name"], "Alex Testperson");
    assert_eq!(by("lab_robin.pdf")["date"], "2026-03-04");
    assert!(views_current(&e), "the views follow the whole list");

    // The held page and an item that is not held: the first is filed, the
    // second reported, and the command exits 1.
    let q = ids_of(&e.ok(&["review", "list"]));
    assert_eq!(q.len(), 1);
    let rl = e.root.join("review.ndjson");
    std::fs::write(
        &rl,
        [
            line(serde_json::json!({"id": "rv_not_held"})),
            line(serde_json::json!({"id": q[0], "person": "Alex", "type": "letter", "date": "2026-01-20"})),
        ]
        .concat(),
    )
    .unwrap();
    let out = e.run(&["--json", "review", "file", "--from", rl.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["schema"], "review-filed-list/v1");
    assert_eq!(v["counts"], serde_json::json!({"failed": 1, "filed": 1}));
    assert_eq!(v["items"][0]["line"], 1);
    assert_eq!(v["items"][0]["error_code"], "not_found", "{v}");
    assert_eq!(v["items"][1]["status"], "filed");
    assert!(e
        .records()
        .join(v["items"][1]["path"].as_str().unwrap())
        .exists());
    assert!(ids_of(&e.ok(&["review", "list"])).is_empty());
    assert!(views_current(&e));

    // A plain intake with a file that fails still brings the views up to
    // date for the one it filed, before it exits 1.
    // The claim's folder refuses writes, so the EOB fails to upload.
    use std::os::unix::fs::PermissionsExt;
    let claims = e.records().join("Alex Testperson/Claims/2026");
    std::fs::create_dir_all(&claims).unwrap();
    std::fs::set_permissions(&claims, std::fs::Permissions::from_mode(0o555)).unwrap();
    if std::fs::write(claims.join("probe"), b"").is_ok() {
        // Permission bits do not bind this user (root): nothing would fail.
        eprintln!("skipped the refused-upload case: the folder accepted a write");
        let _ = std::fs::remove_dir_all(&e.root);
        return;
    }
    let out = e.run(&[
        "--json",
        "intake",
        &f("lab_robin_rescan.pdf"),
        &f("eob_alex.pdf"),
    ]);
    std::fs::set_permissions(&claims, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        out.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        (
            v["counts"]["filed"].as_u64(),
            v["counts"]["failed"].as_u64()
        ),
        (Some(1), Some(1)),
        "{v}"
    );
    assert!(views_current(&e));
    let _ = std::fs::remove_dir_all(&e.root);
}

/// MD5 (hex) of a file, as Drive reports it.
fn md5_of(path: &Path) -> String {
    medrec::hash::md5_hex(&std::fs::read(path).unwrap())
}

#[test]
fn the_views_follow_every_write_and_only_files_that_change_are_written() {
    let base = Env::new("views");
    setup(&base);
    let e = provider_env(&base, medrec::testdata::STREAM);
    e.ok(&["config", "set", "providers", "fakeprov"]);
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

    // The import brought them up to date: views.json records the files
    // as they are now.
    let records = e.records();
    let read = |p: &Path| -> Value { serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap() };
    let index = read(&records.join("views/views.json"));
    assert_eq!(index["schema"], "medrec-views/v2");
    assert_eq!(index["medrec"], env!("CARGO_PKG_VERSION"));
    assert_eq!(index["household"], md5_of(&records.join("household.json")));
    let household = read(&records.join("household.json"));
    let people = household["people"].as_array().unwrap();
    assert_eq!(index["people"].as_object().unwrap().len(), people.len());
    let id_of = |name: &str| {
        people.iter().find(|p| p["name"] == name).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string()
    };
    for p in people {
        let id = p["id"].as_str().unwrap();
        let folder = records.join(p["folder"].as_str().unwrap());
        let entry = &index["people"][id];
        for (field, file) in [("index", "index.json"), ("text", "text.json")] {
            let path = folder.join(file);
            if path.is_file() {
                assert_eq!(entry[field], md5_of(&path), "{field} of {id}");
            } else {
                assert!(entry[field].is_null(), "{field} of {id}");
            }
        }
    }
    let robin = id_of("Robin Testperson");
    let dir = records.join("views").join(&robin);
    // `files` is every view in the person's directory, each stored under
    // its name with its MD5's first 12 hex, and holding those bytes.
    let files = index["people"][&robin]["files"].as_object().unwrap();
    let on_disk = std::fs::read_dir(&dir).unwrap().count();
    assert_eq!(files.len(), on_disk);
    for (name, md5) in files {
        let stored = medrec::viewfiles::stored_name(name, md5.as_str().unwrap());
        assert_eq!(md5_of(&dir.join(&stored)), *md5, "{name}");
    }
    assert!(files.contains_key("summary.json") && files.contains_key("search-items.bin"));
    assert!(index["people"][&robin]["records"]["result"]["shards"]
        .as_array()
        .is_some_and(|s| !s.is_empty()));

    // Views of the v1 layout (plain names, from the connector's own writer)
    // are removed by the names their views.json gave.
    let current = std::fs::read(records.join("views/views.json")).unwrap();
    for plain in ["summary.json", "records-result-0.json"] {
        std::fs::write(dir.join(plain), "[]").unwrap();
    }
    let mut v1 = index.clone();
    v1["schema"] = "medrec-views/v1".into();
    v1["people"][&robin]["files"] =
        serde_json::json!({"summary.json": "0", "records-result-0.json": "0"});
    std::fs::write(records.join("views/views.json"), v1.to_string()).unwrap();
    let upgraded = e.ok(&["views", "write"]);
    assert_eq!(upgraded["files_removed"], 2, "{upgraded}");
    assert_eq!(upgraded["files_written"], 0, "{upgraded}");
    assert!(!dir.join("summary.json").exists());
    assert_eq!(
        std::fs::read(records.join("views/views.json")).unwrap(),
        current
    );

    // Nothing changed since: nothing is made or written.
    let w = e.ok(&["views", "write"]);
    assert_eq!(w["schema"], "views-write/v1");
    assert_eq!(
        (w["people_rebuilt"].as_u64(), w["files_written"].as_u64()),
        (Some(0), Some(0)),
        "{w}"
    );
    // Views another build of this version wrote are made again: a rule can
    // change between them.
    let mut other: Value = serde_json::from_slice(&current).unwrap();
    assert_eq!(other["reader"], medrec::viewfiles::READER, "{other}");
    other["build"] = "0123456789abcdef0123456789abcdef01234567".into();
    std::fs::write(records.join("views/views.json"), other.to_string()).unwrap();
    let again = e.ok(&["views", "write"]);
    let rebuilt = again["people_rebuilt"].as_u64();
    assert_eq!(rebuilt, Some(people.len() as u64), "{again}");
    let now: Value =
        serde_json::from_slice(&std::fs::read(records.join("views/views.json")).unwrap()).unwrap();
    assert_eq!(
        now.get("build"),
        serde_json::from_slice::<Value>(&current)
            .unwrap()
            .get("build")
    );

    // Made again from the same files, every view keeps its name. A file of
    // a view's shape that no view names is removed; anything else is left.
    let stray = dir.join("records-result-99.0123456789ab.json");
    std::fs::write(&stray, "[]").unwrap();
    let mine = dir.join("my notes.txt");
    std::fs::write(&mine, "not a view").unwrap();
    let all = e.ok(&["views", "write", "--all"]);
    assert_eq!(
        all["people_rebuilt"].as_u64(),
        Some(people.len() as u64),
        "{all}"
    );
    assert_eq!(all["files_written"], 0, "{all}");
    assert_eq!(all["files_removed"], 1, "{all}");
    assert!(all["files_unchanged"].as_u64().unwrap() > 10, "{all}");
    assert!(!stray.exists());
    assert!(mine.exists());

    // A person who left household.json leaves no views behind.
    let alex = id_of("Alex Testperson");
    let alex_views = || {
        std::fs::read_dir(records.join("views").join(&alex))
            .map(|d| d.count())
            .unwrap_or(0)
    };
    assert!(alex_views() > 0);
    let mut h = household.clone();
    h["people"]
        .as_array_mut()
        .unwrap()
        .retain(|p| p["id"] != alex.as_str());
    // Robin's guardian is the person who left; the views do not mind.
    std::fs::write(
        records.join("household.json"),
        serde_json::to_string_pretty(&h).unwrap(),
    )
    .unwrap();
    let left = e.ok(&["views", "write"]);
    assert_eq!(left["people"], 1, "{left}");
    assert!(left["files_removed"].as_u64().unwrap() > 0, "{left}");
    assert_eq!(alex_views(), 0);
    assert!(read(&records.join("views/views.json"))["people"]
        .get(&alex)
        .is_none());

    // A person named "Views" gets another folder; a household whose person
    // folder is `views` (made before 0.8.0) gets no views, and their files
    // are left alone.
    let views_person = e.ok(&["people", "add", "Views", "--birth-date", "1980-03-03"]);
    let shown = e.ok(&["people", "show", "Views"]);
    let folder = shown["folder"].as_str().unwrap_or_default().to_string();
    assert!(
        !folder.eq_ignore_ascii_case("views"),
        "{views_person} {shown}"
    );
    let mut h = read(&records.join("household.json"));
    for p in h["people"].as_array_mut().unwrap() {
        if p["name"] == "Views" {
            p["folder"] = "views".into();
        }
    }
    std::fs::write(
        records.join("household.json"),
        serde_json::to_string_pretty(&h).unwrap(),
    )
    .unwrap();
    std::fs::write(records.join("views/notes.md"), "# Notes: Views\n").unwrap();
    let refused = e.run(&["--json", "views", "write", "--all"]);
    assert_ne!(refused.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&refused.stdout).contains("where medrec writes the views"));
    assert!(records.join("views/notes.md").is_file());
    assert!(mine.exists());
    let _ = std::fs::remove_dir_all(&e.root);
}

#[test]
fn init_refuses_to_adopt_a_folder_holding_a_views_folder() {
    let e = Env::new("adopt-views");
    let d = e.root.join("theirs");
    std::fs::create_dir_all(d.join("Views")).unwrap();
    std::fs::write(d.join("Views/holiday.txt"), "their own file").unwrap();
    let out = e.run(&["--json", "init", d.to_str().unwrap(), "--allow-other-files"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stdout).contains("where medrec writes its views"));
    assert!(d.join("Views/holiday.txt").is_file());

    // An empty one too, flag or not: it lists no files.
    let empty = e.root.join("empty");
    std::fs::create_dir_all(empty.join("views")).unwrap();
    for args in [vec!["--allow-other-files"], vec![]] {
        let mut cmd = vec!["--json", "init", empty.to_str().unwrap()];
        cmd.extend(args);
        let out = e.run(&cmd);
        assert_eq!(out.status.code(), Some(2), "{cmd:?}");
        assert!(!empty.join("household.json").exists());
    }
    let _ = std::fs::remove_dir_all(&e.root);
}
