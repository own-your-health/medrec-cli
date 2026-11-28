//! The records folder on Google Drive, against the fake Drive server: the
//! store's operations, `init`, a missing sign-in, and backups both ways.
//! Offline; the sign-in's keys are in memory, never the Keychain.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use pk_cli_auth::vault::{MemoryKeys, Vault};
use pk_cli_config::profile::ProfileName;
use pk_cli_drive::google::fake::{FakeDrive, ACCOUNT, CLIENT_JSON, ROOT_ID};
use pk_cli_drive::google::{
    DriveCli, DriveCommand, DriveSpec, LoginArgs, ProfileStore, Retry, Scope,
};

use crate::config::Config;
use crate::folder::{self, Access, Folder};
use crate::maint;
use crate::store::{self, testing, Location, Reads, Store, DEFAULT_BUDGET};

const DRIVE_PROFILE: &str = "home-drive";
const BUDGET: Duration = Duration::from_secs(60);

struct Harness {
    fake: Arc<FakeDrive>,
    dir: PathBuf,
    _drive: testing::Guard,
}

impl Harness {
    /// A fake Drive, and this thread's stores signed in to it as
    /// `home-drive` (with a read cache when `cache`).
    fn new(cache: bool) -> Harness {
        let fake = Arc::new(FakeDrive::start());
        let dir = crate::testutil::temp_dir("drive");
        let keys = Arc::new(MemoryKeys::default());
        let make = {
            let (fake, dir, keys) = (Arc::clone(&fake), dir.clone(), Arc::clone(&keys));
            move || {
                DriveCli::new(store::BIN, store::VERSION, store::DRIVE_COMMAND)
                    .unwrap()
                    .profile_flag(store::DRIVE_PROFILE_FLAG)
                    .default_scopes(&[Scope::File])
                    .with_vault(Vault::new(Arc::clone(&keys), dir.join("vault")))
                    .with_profiles(ProfileStore::at(dir.join("drive.json")))
                    .with_lock(dir.join("refresh.lock"))
                    .with_endpoints(fake.endpoints())
                    .with_retry(Retry::IMMEDIATE)
                    // The "browser" follows the fake's consent to the
                    // loopback listener the sign-in waits on.
                    .with_browser(|url| {
                        let url = url.to_string();
                        std::thread::spawn(move || {
                            let _ = reqwest::blocking::get(url);
                        });
                        true
                    })
            }
        };
        let guard = testing::use_drive(make, cache.then(|| dir.join("cache")));
        Harness {
            fake,
            dir,
            _drive: guard,
        }
    }

    fn sign_in(&self) {
        let client = self.dir.join("client.json");
        std::fs::write(&client, CLIENT_JSON).unwrap();
        store::drive_cli()
            .unwrap()
            .run(
                &DriveCommand::Login(LoginArgs {
                    client_file: Some(client.clone()),
                    wait: 1,
                    ..LoginArgs::default()
                }),
                Some(DRIVE_PROFILE),
                true,
                false,
            )
            .expect("sign-in against the fake");
        std::fs::remove_file(client).unwrap();
    }

    fn location(&self, folder: &str) -> Location {
        Location::Drive(DriveSpec::new(ProfileName::parse(DRIVE_PROFILE).unwrap(), folder).unwrap())
    }
}

#[test]
fn a_store_on_drive_writes_reads_hashes_and_lists() {
    let h = Harness::new(true);
    h.sign_in();
    let loc = h.location(ROOT_ID);
    let st = Store::open(&loc, Reads::Fresh, BUDGET).unwrap();
    st.write_text("p/a.txt", "abc").unwrap();
    assert_eq!(st.read_text("p/a.txt").unwrap().as_deref(), Some("abc"));
    assert_eq!(
        st.md5("p/a.txt").unwrap().as_deref(),
        Some("900150983cd24fb0d6963f7d28e17f72")
    );
    assert_eq!(st.md5("p/missing").unwrap(), None);
    let local = crate::testutil::temp_dir("drive-src");
    std::fs::write(local.join("scan.pdf"), b"%PDF-1.4 synthetic").unwrap();
    st.put_file(&local.join("scan.pdf"), "p/scan.pdf").unwrap();
    assert_eq!(st.read_bytes("p/scan.pdf").unwrap(), b"%PDF-1.4 synthetic");
    let hashes = st.hashes("p").unwrap();
    assert_eq!(
        hashes.iter().map(|f| f.rel.as_str()).collect::<Vec<_>>(),
        ["p/a.txt", "p/scan.pdf"]
    );
    assert!(hashes.iter().all(|f| f.md5.is_some()));
    let here = st.list_here("p").unwrap();
    assert_eq!(here.len(), 2);
    assert_eq!(st.list_here("nowhere").unwrap().len(), 0);
    // A move never replaces a file; a delete goes to the trash.
    st.write_text("p/b.txt", "b").unwrap();
    assert!(st.move_no_clobber("p/a.txt", "p/b.txt").is_err());
    assert!(st.delete("p/b.txt").unwrap());
    assert!(!st.exists("p/b.txt").unwrap());
    assert_eq!(st.account().unwrap().as_deref(), Some(ACCOUNT));
    let space = st.space().unwrap();
    assert!(space.free.unwrap() <= space.total.unwrap());
}

#[test]
fn a_tree_upload_never_replaces_a_filed_file_and_a_backup_does() {
    let h = Harness::new(false);
    h.sign_in();
    let st = Store::open(&h.location(ROOT_ID), Reads::Fresh, BUDGET).unwrap();
    let local = crate::testutil::temp_dir("drive-tree");
    std::fs::create_dir_all(local.join("s")).unwrap();
    std::fs::write(local.join("s/x.dcm"), b"x").unwrap();
    st.put_tree(&local, "q").unwrap();
    st.put_tree(&local, "q").unwrap();
    std::fs::write(local.join("s/x.dcm"), b"y").unwrap();
    assert!(st.put_tree(&local, "q").is_err());
    assert_eq!(h.fake.content("q/s/x.dcm").unwrap(), b"x");
    st.put_tree_replacing(&local, "q").unwrap();
    assert_eq!(h.fake.content("q/s/x.dcm").unwrap(), b"y");
    assert_eq!(h.fake.count("q/s", "x.dcm"), 1, "replaced in place");
    let back = crate::testutil::temp_dir("drive-fetch");
    st.fetch_tree("q", &back).unwrap();
    assert_eq!(std::fs::read(back.join("s/x.dcm")).unwrap(), b"y");
}

#[test]
fn a_folder_whose_drive_profile_is_not_signed_in_is_refused_naming_the_sign_in() {
    let h = Harness::new(false);
    let err = Store::open(&h.location(ROOT_ID), Reads::Fresh, BUDGET)
        .err()
        .unwrap();
    assert_eq!(err.exit_code(), 2, "{err}");
    let remedy = err.remedy().unwrap_or_default().to_string();
    assert!(
        remedy.contains("medrec drive login --drive-profile home-drive"),
        "{remedy}"
    );
    assert!(h.fake.requests().is_empty(), "nothing reached Drive");
}

#[test]
fn a_records_folder_is_set_up_on_drive_with_its_inbox_and_backed_up_both_ways() {
    let h = Harness::new(true);
    h.sign_in();
    let records = h.fake.add_folder(ROOT_ID, "Medical Records");
    let loc = h.location(&records);
    let mut cfg = Config::default();
    let made = maint::init(&mut cfg, &loc, false, BUDGET).unwrap();
    assert_eq!(made.status, "created");
    assert_eq!(cfg.active.folder.as_deref(), Some(loc.display().as_str()));
    assert!(h.fake.find("Medical Records/inbox").is_some());
    // The inbox is its own store under the folder.
    let inbox = Store::open(&loc.join(folder::INBOX).unwrap(), Reads::Fresh, BUDGET).unwrap();
    let before = inbox.list_here("").unwrap().len();
    inbox.write_text("drop.txt", "a dropped file").unwrap();
    assert_eq!(inbox.list_here("").unwrap().len(), before + 1);
    assert!(h.fake.find("Medical Records/inbox/drop.txt").is_some());
    let f = Folder::open_at(&loc, Access::Write, BUDGET).unwrap();
    assert_eq!(f.household().unwrap().folder_id, made.folder_id);
    // To a directory, then from that directory into a new Drive folder.
    let disk = crate::testutil::temp_dir("drive-backup").join("copy");
    let to_disk = maint::backup(&f, &Location::Dir(disk.clone()), BUDGET).unwrap();
    assert!(to_disk.mismatched.is_empty(), "{:?}", to_disk.mismatched);
    assert!(to_disk.files > 0);
    let copy = h.fake.add_folder(ROOT_ID, "Records copy");
    let from_disk = Folder::open_at(&Location::Dir(disk), Access::Read, DEFAULT_BUDGET).unwrap();
    let to_drive = maint::backup(&from_disk, &h.location(&copy), BUDGET).unwrap();
    assert!(to_drive.mismatched.is_empty(), "{:?}", to_drive.mismatched);
    assert_eq!(to_drive.files, to_disk.files);
    assert!(h.fake.find("Records copy/household.json").is_some());
    // From Drive to Drive, one file at a time; a second backup copies
    // nothing, since every MD5 already matches.
    let other = h.fake.add_folder(ROOT_ID, "Records second copy");
    let to_other = maint::backup(&f, &h.location(&other), BUDGET).unwrap();
    assert!(to_other.mismatched.is_empty(), "{:?}", to_other.mismatched);
    assert_eq!(to_other.files, to_disk.files);
    h.fake.clear_requests();
    let again = maint::backup(&f, &h.location(&other), BUDGET).unwrap();
    assert!(again.mismatched.is_empty());
    let uploads = h
        .fake
        .requests()
        .iter()
        .filter(|l| l.contains("/upload/"))
        .count();
    assert_eq!(uploads, 0, "{:?}", h.fake.requests());
}

#[test]
fn the_views_on_drive_follow_a_write_and_then_upload_only_what_changed() {
    use crate::model::{PersonIndex, Record};
    let h = Harness::new(true);
    h.sign_in();
    let records = h.fake.add_folder(ROOT_ID, "Medical Records");
    let loc = h.location(&records);
    let mut cfg = Config::default();
    maint::init(&mut cfg, &loc, false, BUDGET).unwrap();
    let result = |n: usize, date: &str| -> Record {
        serde_json::from_value(serde_json::json!({
            "id": format!("r_{n}"), "kind": "result", "key": format!("k_{n}"), "date": date,
            "display": "Glucose", "code": {"system": "http://loinc.org", "code": "2345-7"},
            "fields": {"value": format!("{n} mg/dL")}, "provenance": []
        }))
        .unwrap()
    };
    let p = {
        // A person and their index, written through the folder: the views
        // follow as it closes.
        let f = Folder::open_at(&loc, Access::Write, BUDGET).unwrap();
        let mut hh = f.household().unwrap();
        hh.people.push(
            serde_json::from_value(serde_json::json!({
                "id": "p_drive", "name": "Robin Testperson", "folder": "Robin Testperson",
                "created_at": "2026-01-01T00:00:00Z"
            }))
            .unwrap(),
        );
        f.save_household(&mut hh).unwrap();
        let p = hh.people[0].clone();
        let mut idx = PersonIndex::new(&p.id);
        idx.records = (0..40)
            .map(|n| result(n, &format!("2026-01-{:02}", n % 28 + 1)))
            .collect();
        f.save_index(&p, &mut idx).unwrap();
        p
    };
    let st = Store::open(&loc, Reads::Fresh, BUDGET).unwrap();
    let index: crate::viewfiles::ViewsIndex =
        serde_json::from_str(&st.read_text(crate::viewfiles::INDEX).unwrap().unwrap()).unwrap();
    let mine = &index.people["p_drive"];
    assert_eq!(mine.index, st.md5(&folder::index_rel(&p)).unwrap());
    // Every view is on Drive with the MD5 views.json gives, the binary
    // items file included.
    let stored = |name: &str| {
        format!(
            "views/p_drive/{}",
            crate::viewfiles::stored_name(name, &mine.files[name])
        )
    };
    for (name, md5) in &mine.files {
        assert_eq!(st.md5(&stored(name)).unwrap().as_ref(), Some(md5), "{name}");
    }
    let items = st.read_bytes(&stored("search-items.bin")).unwrap();
    assert_eq!(u32::from_le_bytes(items[0..4].try_into().unwrap()), 40);

    // Nothing changed: no upload.
    let f = Folder::open_at(&loc, Access::Write, BUDGET).unwrap();
    h.fake.clear_requests();
    let again = crate::viewfiles::sync(&f, true).unwrap();
    assert_eq!(
        (again.people_rebuilt, again.files_written),
        (1, 0),
        "{again:?}"
    );
    assert!(
        !h.fake.requests().iter().any(|r| r.contains("upload")),
        "{:?}",
        h.fake.requests()
    );
    // One newer result: a few files, not all of them.
    let mut idx = f.index(&p).unwrap();
    idx.records.push(result(40, "2026-02-01"));
    f.save_index(&p, &mut idx).unwrap();
    let after = crate::viewfiles::sync(&f, false).unwrap();
    assert_eq!(after.people_rebuilt, 1, "{after:?}");
    assert!(
        after.files_written > 0 && after.files_written < after.files_unchanged,
        "{after:?}"
    );
    // The replaced views went: the directory holds what views.json names.
    assert!(after.files_removed > 0, "{after:?}");
    let index: crate::viewfiles::ViewsIndex =
        serde_json::from_str(&st.read_text(crate::viewfiles::INDEX).unwrap().unwrap()).unwrap();
    let mut named: Vec<String> = index.people["p_drive"]
        .files
        .iter()
        .map(|(name, md5)| format!("views/p_drive/{}", crate::viewfiles::stored_name(name, md5)))
        .collect();
    named.sort();
    let held: Vec<String> = st
        .hashes("views/p_drive")
        .unwrap()
        .into_iter()
        .map(|x| x.rel)
        .collect();
    assert_eq!(held, named);
    drop(f);
}

/// The elements of the records' raw files are counted from copies of them.
#[test]
fn the_elements_of_raw_files_on_drive_are_counted() {
    use crate::model::{PersonIndex, Record};
    let h = Harness::new(true);
    h.sign_in();
    let records = h.fake.add_folder(ROOT_ID, "Medical Records");
    let loc = h.location(&records);
    let mut cfg = Config::default();
    maint::init(&mut cfg, &loc, false, BUDGET).unwrap();
    let rel = |n: usize| format!("Robin Testperson/Records/one/Claim/c-{n}.json");
    {
        let f = Folder::open_at(&loc, Access::Write, BUDGET).unwrap();
        let mut hh = f.household().unwrap();
        hh.people.push(
            serde_json::from_value(serde_json::json!({
                "id": "p_drive", "name": "Robin Testperson", "folder": "Robin Testperson",
                "created_at": "2026-01-01T00:00:00Z"
            }))
            .unwrap(),
        );
        f.save_household(&mut hh).unwrap();
        let mut idx = PersonIndex::new("p_drive");
        idx.records = (0..3)
            .map(|n| -> Record {
                serde_json::from_value(serde_json::json!({
                    "id": format!("r_{n}"), "kind": "claim", "key": format!("k_{n}"),
                    "display": "Office visit", "fields": {}, "provenance": [{
                        "source": {"kind": "fhir", "cli": "fakeprov"}, "method": "fhir-api",
                        "filed_at": "2026-01-02T10:00:00Z", "filed_by": "medrec",
                        "raw_path": rel(n)}]
                }))
                .unwrap()
            })
            .collect();
        f.save_index(&hh.people[0], &mut idx).unwrap();
        // The last one's is not there.
        for n in 0..2 {
            let raw = format!(r#"{{"resourceType": "Claim", "id": "c-{n}", "item": [{{}}]}}"#);
            f.store.write_text(&rel(n), &raw).unwrap();
        }
    }
    let f = Folder::open_at(&loc, Access::Read, BUDGET).unwrap();
    let hh = f.household().unwrap();
    let count = crate::raw::fields(&f, &hh, None, &[]).unwrap();
    assert_eq!((count.records, count.without_raw, count.unread), (3, 0, 1));
    assert_eq!(count.types.len(), 1);
    assert_eq!(
        (count.types[0].cli.as_str(), count.types[0].files),
        ("fakeprov", 2)
    );
}

/// A raw path an edited index makes absolute is read as a file that is not
/// there: nothing is copied to it, wherever it points.
#[test]
fn a_raw_path_that_is_not_relative_is_never_copied_to() {
    use crate::model::{PersonIndex, Record};
    let h = Harness::new(true);
    h.sign_in();
    let records = h.fake.add_folder(ROOT_ID, "Medical Records");
    let loc = h.location(&records);
    let mut cfg = Config::default();
    maint::init(&mut cfg, &loc, false, BUDGET).unwrap();
    let outside = pk_cli_drive::private_temp_dir("medrec-outside").unwrap();
    let mirrored = outside.path().strip_prefix("/").unwrap().to_string_lossy();
    let rel = format!("{mirrored}/Claim/c-9.json");
    {
        let f = Folder::open_at(&loc, Access::Write, BUDGET).unwrap();
        let mut hh = f.household().unwrap();
        hh.people.push(
            serde_json::from_value(serde_json::json!({
                "id": "p_drive", "name": "Robin Testperson", "folder": "Robin Testperson",
                "created_at": "2026-01-01T00:00:00Z"
            }))
            .unwrap(),
        );
        f.save_household(&mut hh).unwrap();
        let mut idx = PersonIndex::new("p_drive");
        let r: Record = serde_json::from_value(serde_json::json!({
            "id": "r_9", "kind": "claim", "key": "k_9", "display": "Office visit", "fields": {},
            "provenance": [{
                "source": {"kind": "fhir", "cli": "fakeprov"}, "method": "fhir-api",
                "filed_at": "2026-01-02T10:00:00Z", "filed_by": "medrec",
                "raw_path": format!("/{rel}")}]
        }))
        .unwrap();
        idx.records = vec![r];
        f.save_index(&hh.people[0], &mut idx).unwrap();
        // The same folder below the records folder, so that a path cut of its
        // root names one there.
        let raw = r#"{"resourceType": "Claim", "id": "c-9", "item": [{}]}"#;
        f.store.write_text(&rel, raw).unwrap();
    }
    let f = Folder::open_at(&loc, Access::Read, BUDGET).unwrap();
    let hh = f.household().unwrap();
    let count = crate::raw::fields(&f, &hh, None, &[]).unwrap();
    assert_eq!((count.records, count.unread), (1, 1));
    assert!(!outside.path().join("Claim").exists());
}

/// A folder of raw files that cannot be copied keeps its error's kind for
/// `records fields`, and its records are unread, with its code, for `verify
/// --events`.
#[test]
fn a_raw_folder_that_cannot_be_copied_keeps_its_error() {
    use crate::model::{PersonIndex, Record};
    use pk_cli_drive::google::fake::When;
    let h = Harness::new(true);
    h.sign_in();
    let records = h.fake.add_folder(ROOT_ID, "Medical Records");
    let loc = h.location(&records);
    let mut cfg = Config::default();
    maint::init(&mut cfg, &loc, false, BUDGET).unwrap();
    let rel = |n: usize| format!("Robin Testperson/Records/one/Claim/c-{n}.json");
    {
        let f = Folder::open_at(&loc, Access::Write, BUDGET).unwrap();
        let mut hh = f.household().unwrap();
        hh.people.push(
            serde_json::from_value(serde_json::json!({
                "id": "p_drive", "name": "Robin Testperson", "folder": "Robin Testperson",
                "created_at": "2026-01-01T00:00:00Z"
            }))
            .unwrap(),
        );
        f.save_household(&mut hh).unwrap();
        let mut idx = PersonIndex::new("p_drive");
        idx.records = (0..2)
            .map(|n| -> Record {
                serde_json::from_value(serde_json::json!({
                    "id": format!("r_{n}"), "kind": "claim", "key": format!("k_{n}"),
                    "display": "Office visit", "date": "2026-01-02", "fields": {},
                    "provenance": [{
                        "source": {"kind": "fhir", "cli": "fakeprov"}, "method": "fhir-api",
                        "filed_at": "2026-01-02T10:00:00Z", "filed_by": "medrec",
                        "raw_path": rel(n)}]
                }))
                .unwrap()
            })
            .collect();
        f.save_index(&hh.people[0], &mut idx).unwrap();
        for n in 0..2 {
            let raw = format!(r#"{{"resourceType": "Claim", "id": "c-{n}", "item": [{{}}]}}"#);
            f.store.write_text(&rel(n), &raw).unwrap();
        }
    }
    let file = h.fake.find(&format!("Medical Records/{}", rel(0))).unwrap();
    h.fake
        .fail_next(&file, 403, "forbidden", 1000, When::Before);
    let f = Folder::open_at(&loc, Access::Read, BUDGET).unwrap();
    let hh = f.household().unwrap();
    let err = crate::raw::fields(&f, &hh, None, &[]).unwrap_err();
    assert_eq!(err.code(), "auth", "{err}");
    let of = crate::against::events(&f, &hh);
    assert_eq!(of.len(), 1);
    assert!(!of[0].not_compared, "{:?}", of[0]);
    assert_eq!(of[0].unread, ["r_0", "r_1"]);
    assert_eq!(
        of[0].read_errors.iter().copied().collect::<Vec<_>>(),
        ["auth"]
    );
    assert!(!of[0].clean());
}

/// A sign-in that Drive refuses for a person's index stops `verify` with its
/// own code, as nothing else would read: it is no index that does not read.
#[test]
fn an_index_drive_refuses_stops_verify() {
    use pk_cli_drive::google::fake::When;
    let h = Harness::new(true);
    h.sign_in();
    let records = h.fake.add_folder(ROOT_ID, "Medical Records");
    let loc = h.location(&records);
    let mut cfg = Config::default();
    maint::init(&mut cfg, &loc, false, BUDGET).unwrap();
    {
        let f = Folder::open_at(&loc, Access::Write, BUDGET).unwrap();
        let mut hh = f.household().unwrap();
        hh.people.push(
            serde_json::from_value(serde_json::json!({
                "id": "p_drive", "name": "Robin Testperson", "folder": "Robin Testperson",
                "created_at": "2026-01-01T00:00:00Z"
            }))
            .unwrap(),
        );
        f.save_household(&mut hh).unwrap();
        let mut idx = crate::model::PersonIndex::new("p_drive");
        f.save_index(&hh.people[0], &mut idx).unwrap();
    }
    let file = h
        .fake
        .find("Medical Records/Robin Testperson/index.json")
        .unwrap();
    h.fake
        .fail_next(&file, 403, "forbidden", 1000, When::Before);
    let f = Folder::open_at(&loc, Access::Fresh, BUDGET).unwrap();
    let err = maint::verify(&f, false).unwrap_err();
    assert_eq!(err.code(), "auth", "{err}");
}
