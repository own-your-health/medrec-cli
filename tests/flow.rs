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
        let path = std::env::var("PATH").unwrap_or_default();
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
            .env("XDG_CACHE_HOME", self.root.join("cache"))
            .env("HOME", &self.root)
            .env("PATH", &self.path)
            .env("MEDREC_PROFILE", "home")
            .env("MEDREC_KEYCHAIN_SERVICE", "piekstra.medrec.hermetic-tests")
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
    let inbox = e.ok(&["intake", "--inbox"]);
    assert_eq!(inbox["items"][0]["status"], "duplicate");
    assert!(!e.records().join("inbox/scan_0002.pdf").exists());

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
        .env("XDG_CACHE_HOME", e.root.join("cache"))
        .env("HOME", &e.root)
        .env("PATH", &e.path)
        .env("MEDREC_PROFILE", "home")
        .env("MEDREC_KEYCHAIN_SERVICE", "piekstra.medrec.hermetic-tests")
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
                    "productOrService": {"coding": [{"system": "http://www.ama-assn.org/go/cpt", "code": "80053", "display": "Metabolic panel"}]},
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
        "Office visit and Metabolic panel · Example Health"
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
