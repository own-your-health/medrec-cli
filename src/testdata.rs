//! Synthetic data for tests and dogfooding (`cargo run --example synth --
//! <DIR>`). Everything here is invented: the people are `Testperson`s, the
//! organizations `Example …`, every DICOM UID is under the synthetic root
//! `2.25.0`, and the records stream is `tests/fixtures/stream/`.
//!
//! ```text
//! <DIR>/scans/        PDFs: two lab reports (one a re-scan), an exact copy,
//!                     a visit summary, an EOB, an ambiguous page, and a PNG
//!                     "scan" (rendered with pdftoppm when it is installed)
//! <DIR>/dicom/cd1/    an ultrasound study (DICOMDIR + 3 images)
//! <DIR>/dicom/cd2/    the same study with a second series
//! <DIR>/dicom/ct.zip  a CT study for the adult, zipped (needs bsdtar)
//! <DIR>/provider/     a fake provider CLI (`fakeprov`) and its streams: the
//!                     one it prints, and four alike in all but the person
//!                     and the patient (`--patient north`, `south`, `east`,
//!                     `west`)
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::dicom::synthetic::SyntheticStudy;
use crate::pdfgen::text_pdf;

/// The synthetic records stream, as a provider CLI writes it.
pub const STREAM: &str = include_str!("../tests/fixtures/stream/synthetic.ndjson");
/// A stream whose records refer to practitioners, organizations and places
/// in each way a source does.
pub const PROVIDERS_STREAM: &str = include_str!("../tests/fixtures/stream/providers.ndjson");
/// One patient's stream, of several with one CLI, organization and FHIR base
/// ([`shared_key_streams`]).
const SHARED_KEY_STREAM: &str = include_str!("../tests/fixtures/stream/shared-key.ndjson");
/// The fake provider's `info --json`.
pub const PROVIDER_INFO: &str = include_str!("../tests/fixtures/stream/fakeprov-info.json");
/// The same from before login profiles: it signs in, but lists no
/// `login-profiles`.
pub const PROVIDER_INFO_UNPROFILED: &str =
    include_str!("../tests/fixtures/stream/fakeprov-info-unprofiled.json");
/// The same that also reads downloaded files (`records-files`).
pub const PROVIDER_INFO_FILES: &str =
    include_str!("../tests/fixtures/stream/fakeprov-info-files.json");
/// The same for a provider that needs no login.
pub const PROVIDER_INFO_NO_LOGIN: &str =
    include_str!("../tests/fixtures/stream/fakeprov-info-no-login.json");

/// The same of one that reads files and signs in nowhere.
pub const PROVIDER_INFO_FILES_ONLY: &str =
    include_str!("../tests/fixtures/stream/fakeprov-info-files-only.json");
/// The fake provider's `profiles list --json`: `adult1` and `adult2`.
pub const PROVIDER_PROFILES: &str = include_str!("../tests/fixtures/stream/fakeprov-profiles.json");
/// A synthetic clinical summary with `{{RENDERING}}` (its timestamp and
/// document id) and `{{FINDING}}` (a line of its text) to fill in.
pub const SUMMARY_XML: &str = include_str!("../tests/fixtures/stream/summary.xml");

/// [`STREAM`] with `doc-0001`'s Binary served as [`SUMMARY_XML`], rendered
/// at `rendering` and saying `finding`, and the DocumentReference at
/// `meta.versionId` `version`: a portal's summary, generated anew on each
/// export, as two exports of the same document serve it.
pub fn stream_with_summary(rendering: &str, finding: &str, version: &str) -> String {
    use base64::Engine;
    let xml = SUMMARY_XML
        .replace("{{RENDERING}}", rendering)
        .replace("{{FINDING}}", finding);
    let data = base64::engine::general_purpose::STANDARD.encode(xml);
    let mut out = String::new();
    for line in STREAM.lines() {
        let mut v: serde_json::Value = serde_json::from_str(line).unwrap();
        match (v["resourceType"].as_str(), v["id"].as_str()) {
            (Some("Binary"), Some("bin-0001")) => {
                v["contentType"] = "application/xml".into();
                v["data"] = data.clone().into();
            }
            (Some("DocumentReference"), Some("doc-0001")) => {
                v["meta"]["versionId"] = version.into();
                v["content"][0]["attachment"]["contentType"] = "application/xml".into();
            }
            _ => {}
        }
        out.push_str(&serde_json::to_string(&v).unwrap());
        out.push('\n');
    }
    out
}

/// Sources alike in CLI, organization and FHIR base, as `(patient, person,
/// name, stream)`: each of two people as each of two patients.
pub fn shared_key_streams() -> Vec<(&'static str, &'static str, &'static str, String)> {
    let people = [("Alex", "1990-01-01"), ("Robin", "2020-02-02")];
    [
        ("north", 0, "Sample Clinic North", "09"),
        ("east", 1, "Sample Clinic East", "11"),
        ("south", 0, "Sample Clinic South", "13"),
        ("west", 1, "Sample Clinic West", "15"),
    ]
    .map(|(patient, person, name, hour)| {
        let (given, born) = people[person];
        let stream = SHARED_KEY_STREAM
            .replace("{{PATIENT}}", patient)
            .replace("{{NAME}}", name)
            .replace("{{LABEL}}", &format!("{given} Testperson, {patient}"))
            .replace("{{GIVEN}}", given)
            .replace("{{BORN}}", born)
            .replace("{{VISIT}}", &format!("Check at {patient}"))
            .replace("{{HOUR}}", hour)
            .replace("{{PULLED}}", &format!("2024-06-19T{hour}:00:00Z"));
        (patient, given, name, stream)
    })
    .into()
}

/// Write [`shared_key_streams`] where `fakeprov` in `provider` finds them.
pub fn write_shared_key(provider: &Path) {
    for (patient, _, _, stream) in shared_key_streams() {
        write(
            &provider.join(format!("synthetic-{patient}.ndjson")),
            stream.as_bytes(),
        );
    }
}

/// An invented CDSi supporting data archive in the shape of CDC's
/// (tests/fixtures/schedule): its XML files under "Version 0.1 - 508/XML/"
/// and its release notes, a document whose first date is the table's.
pub fn cdsi_archive() -> Vec<u8> {
    const AT: &str = "Version 0.1 - 508";
    let fixture = |name: &str| -> &'static str {
        match name {
            "ScheduleSupportingData.xml" => {
                include_str!("../tests/fixtures/schedule/ScheduleSupportingData.xml")
            }
            "AntigenSupportingData- Examplitis.xml" => {
                include_str!("../tests/fixtures/schedule/AntigenSupportingData- Examplitis.xml")
            }
            "AntigenSupportingData- Samplepox A.xml" => {
                include_str!("../tests/fixtures/schedule/AntigenSupportingData- Samplepox A.xml")
            }
            "AntigenSupportingData- Samplepox B.xml" => {
                include_str!("../tests/fixtures/schedule/AntigenSupportingData- Samplepox B.xml")
            }
            "AntigenSupportingData- Samplepox C.xml" => {
                include_str!("../tests/fixtures/schedule/AntigenSupportingData- Samplepox C.xml")
            }
            _ => include_str!("../tests/fixtures/schedule/document.xml"),
        }
    };
    let notes = zip_of(&[(
        "word/document.xml",
        fixture("document.xml").as_bytes(),
        (2026, 9, 8),
    )]);
    let mut files: Vec<(String, Vec<u8>)> = [
        "ScheduleSupportingData.xml",
        "AntigenSupportingData- Examplitis.xml",
        "AntigenSupportingData- Samplepox A.xml",
        "AntigenSupportingData- Samplepox B.xml",
        "AntigenSupportingData- Samplepox C.xml",
    ]
    .iter()
    .map(|n| (format!("{AT}/XML/{n}"), fixture(n).as_bytes().to_vec()))
    .collect();
    files.push((format!("{AT}/Release Notes.docx"), notes));
    let named: Vec<Zipped> = files
        .iter()
        .map(|(n, b)| (n.as_str(), b.as_slice(), (2026, 9, 1)))
        .collect();
    zip_of(&named)
}

/// A file of a zip: its name, its bytes and its day (year, month, day).
pub type Zipped<'a> = (&'a str, &'a [u8], (u16, u16, u16));

/// A zip of deflated files, each stamped with its day: what medrec's zip
/// reader reads, with no CRC.
pub fn zip_of(files: &[Zipped]) -> Vec<u8> {
    use std::io::Write;
    let mut z = Vec::new();
    let mut central = Vec::new();
    for (name, data, (y, m, d)) in files {
        let mut enc = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::best());
        enc.write_all(data).unwrap();
        let packed = enc.finish().unwrap();
        let day = ((y - 1980) << 9) | (m << 5) | d;
        let n = name.len() as u16;
        let at = z.len() as u32;
        z.extend(b"PK\x03\x04\x14\0\0\0\x08\0\0\0");
        z.extend(day.to_le_bytes());
        z.extend([0u8; 4]);
        z.extend((packed.len() as u32).to_le_bytes());
        z.extend((data.len() as u32).to_le_bytes());
        z.extend(n.to_le_bytes());
        z.extend([0u8; 2]);
        z.extend(name.as_bytes());
        z.extend(&packed);
        central.extend(b"PK\x01\x02\x14\0\x14\0\0\0\x08\0\0\0");
        central.extend(day.to_le_bytes());
        central.extend([0u8; 4]);
        central.extend((packed.len() as u32).to_le_bytes());
        central.extend((data.len() as u32).to_le_bytes());
        central.extend(n.to_le_bytes());
        central.extend([0u8; 12]);
        central.extend(at.to_le_bytes());
        central.extend(name.as_bytes());
    }
    let (at, len) = (z.len() as u32, central.len() as u32);
    z.extend(central);
    let count = (files.len() as u16).to_le_bytes();
    z.extend(b"PK\x05\x06\0\0\0\0");
    z.extend(count);
    z.extend(count);
    z.extend(len.to_le_bytes());
    z.extend(at.to_le_bytes());
    z.extend([0u8; 2]);
    z
}

pub fn write(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// Write everything above under `out`.
pub fn write_all(out: &Path) {
    write_scans(&out.join("scans"));
    write_dicom(&out.join("dicom"));
    write_provider(&out.join("provider"), STREAM);
    write_shared_key(&out.join("provider"));
}

pub fn write_scans(scans: &Path) {
    let lab = text_pdf(&[
        "Example Health Laboratory",
        "Laboratory report",
        "Patient: Robin Testperson",
        "DOB: 02/02/2020",
        "Collected: 03/04/2026",
        "Hemoglobin 12.5 g/dL (11.5 - 15.5)",
        "White blood cells 7.1 K/uL",
        "Platelets 250 K/uL",
        "Reviewed by the Example Health laboratory team",
    ]);
    write(&scans.join("lab_robin.pdf"), &lab);
    write(&scans.join("lab_robin_copy.pdf"), &lab);
    write(
        &scans.join("lab_robin_rescan.pdf"),
        &text_pdf(&[
            "Example Health Laboratory",
            "Laboratory report",
            "Patient: Robin Testperson",
            "DOB: 02/02/2020",
            "Collected: 03/04/2026",
            "Hemoglobin 12.5 g/dL (11.5 - 15.5)",
            "White blood cells 7.1 K/uL",
            "Platelets 250 K/uL",
            "Reviewed by the Example Health laboratory team.",
            "Page 1 of 1",
        ]),
    );
    write(
        &scans.join("visit_alex.pdf"),
        &text_pdf(&[
            "Example Clinic",
            "After visit summary",
            "Alex Testperson",
            "Date of birth: 1990-01-01",
            "Visit date: February 10, 2026",
            "Reason for visit: annual physical",
            "Follow up in one year.",
        ]),
    );
    write(
        &scans.join("eob_alex.pdf"),
        &text_pdf(&[
            "Example Health Plan",
            "Explanation of Benefits",
            "Member: Alex Testperson",
            "Date of service: 02/10/2026",
            "Amount billed $150.00  Plan paid $110.00  You owe $40.00",
            "This is not a bill.",
        ]),
    );
    write(
        &scans.join("ambiguous.pdf"),
        &text_pdf(&[
            "Example Clinic",
            "Family consent form",
            "Alex Testperson",
            "Robin Testperson",
            "Please sign and return.",
        ]),
    );
    // A raster "scan" for the OCR path.
    if Command::new("pdftoppm").arg("-v").output().is_ok() {
        let pdf = scans.join("visit_alex.pdf");
        let stem = scans.join("tmp-scan");
        let ok = Command::new("pdftoppm")
            .args(["-png", "-r", "150", "-singlefile"])
            .arg(&pdf)
            .arg(&stem)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            std::fs::rename(
                stem.with_extension("png"),
                scans.join("scan_visit_alex.png"),
            )
            .unwrap();
        }
    }
}

/// Two copies of an ultrasound study (the second adds a series) and a
/// zipped CT.
pub fn write_dicom(dicom: &Path) {
    // Robin's (2020-02-02), like the household the flow tests set up.
    let us = SyntheticStudy::new("US")
        .seed(101)
        .series(1)
        .instances(3)
        .patient_birth_date("20200202");
    us.write_to(dicom.join("cd1")).unwrap();
    SyntheticStudy::new("US")
        .seed(101)
        .patient_birth_date("20200202")
        .series(2)
        .instances(3)
        .write_to(dicom.join("cd2"))
        .unwrap();
    let ct_dir = dicom.join("ct-src");
    SyntheticStudy::new("CT")
        .seed(202)
        .series(1)
        .instances(4)
        .study_date("20260210")
        .body_part("CHEST")
        .patient_name("Testperson^Alex")
        .patient_birth_date("19900101")
        .write_to(&ct_dir)
        .unwrap();
    let tar = if cfg!(target_os = "macos") {
        "/usr/bin/tar"
    } else {
        "bsdtar"
    };
    let zipped = Command::new(tar)
        .arg("-a")
        .arg("-cf")
        .arg(dicom.join("ct.zip"))
        .arg("-C")
        .arg(&ct_dir)
        .arg(".")
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if zipped {
        std::fs::remove_dir_all(&ct_dir).unwrap();
    }
}

/// A fake `health-records/v1` provider, `fakeprov`, that prints `stream`
/// for `records export`. Returns the directory to put on `PATH`.
///
/// Its `info` is `FAKEPROV_INFO` (`unprofiled`, `no-login`, `files` or
/// `files-only`, else the current one, which lists `login-profiles`). Its profiles are `adult1`
/// and `adult2`; an export that signs in without one of them is exit 2, as a
/// provider's is, and `--files <DIR>` (with `files`) needs a directory and
/// no `--profile`. It writes the export's arguments to `export-args`, and
/// the `FAKEPROV_PROFILE` and `FAKEPROV_FILES` it saw to `export-env`. With
/// `FAKEPROV_HOLD=<DIR>` its export writes `<DIR>/exporting` and waits, up to
/// 30 seconds, for `<DIR>/go` before it prints the stream. An export for
/// `--patient <P>` prints `synthetic-<P>.ndjson` when the directory has one.
/// With `FAKEPROV_WATCH=<DIR>` each export appends the checksum of
/// `<DIR>/*/index.json` to `export-log`; with `FAKEPROV_EXIT_FOR=<P>` the
/// export for `--patient <P>` answers that its session expired (exit 3).
pub fn write_provider(provider: &Path, stream: &str) -> PathBuf {
    std::fs::create_dir_all(provider).unwrap();
    std::fs::write(provider.join("synthetic.ndjson"), stream).unwrap();
    std::fs::write(provider.join("fakeprov-info.json"), PROVIDER_INFO).unwrap();
    std::fs::write(
        provider.join("fakeprov-info-unprofiled.json"),
        PROVIDER_INFO_UNPROFILED,
    )
    .unwrap();
    std::fs::write(
        provider.join("fakeprov-info-no-login.json"),
        PROVIDER_INFO_NO_LOGIN,
    )
    .unwrap();
    std::fs::write(
        provider.join("fakeprov-info-files.json"),
        PROVIDER_INFO_FILES,
    )
    .unwrap();
    std::fs::write(
        provider.join("fakeprov-info-files-only.json"),
        PROVIDER_INFO_FILES_ONLY,
    )
    .unwrap();
    std::fs::write(provider.join("fakeprov-profiles.json"), PROVIDER_PROFILES).unwrap();
    let script = provider.join("fakeprov");
    write(
        &script,
        format!(
            r#"#!/bin/sh
# A fake health-records/v1 provider for medrec's tests.
d='{0}'
case "$1" in
  info) case "$FAKEPROV_INFO" in
      unprofiled|no-login|files|files-only) cat "$d/fakeprov-info-$FAKEPROV_INFO.json" ;;
      *) cat "$d/fakeprov-info.json" ;;
    esac ;;
  profiles) cat "$d/fakeprov-profiles.json" ;;
  auth)
    case " $* " in
      *" --help "*) printf '%s\n' 'Usage: fakeprov auth login [OPTIONS]' '      --browser' '      --overwrite' '          Replace a stored sign-in'; exit 0 ;;
    esac
    printf '%s\n' "$*" >> "$d/auth-calls"
    case "$2" in
      status)
        mode="${{FAKEPROV_AUTH:-ok}}"
        [ -f "$d/auth-state" ] && mode=$(cat "$d/auth-state")
        case "$mode" in
          hang) sleep 600 ;;
          expired) printf '%s\n' '{{"schema":"auth-status/v1","required":true,"authenticated":true,"session_valid":false}}' ;;
          signed-out) printf '%s\n' '{{"schema":"auth-status/v1","required":true,"authenticated":false}}' ;;
          short) printf '%s\n' '{{"schema":"auth-status/v1","required":true,"authenticated":true,"session_valid":true,"renews":false,"lasts_secs":1800}}' ;;
          *) printf '%s\n' '{{"schema":"auth-status/v1","required":true,"authenticated":true,"session_valid":true,"expires_at":"2099-01-01T00:00:00Z"}}' ;;
        esac ;;
      login)
        [ -n "$FAKEPROV_LOGIN_FAIL" ] && {{ echo "fakeprov: the sign-in was canceled" >&2; exit 3; }}
        printf 'ok' > "$d/auth-state"; echo "fakeprov: signed in" ;;
      *) echo "fakeprov: unknown auth command $2" >&2; exit 2 ;;
    esac ;;
  records)
    printf '%s\n' "$*" > "$d/export-args"
    printf 'profile=%s files=%s\n' "${{FAKEPROV_PROFILE:-}}" "${{FAKEPROV_FILES:-}}" > "$d/export-env"
    profile=''; files=''; patient=''; prev=''
    for a in "$@"; do
      [ "$prev" = --profile ] && profile="$a"
      [ "$prev" = --files ] && files="$a"
      [ "$prev" = --patient ] && patient="$a"
      prev="$a"
    done
    if [ -n "$files" ]; then
      [ "$FAKEPROV_INFO" = files ] || {{ echo "fakeprov: unexpected argument --files" >&2; exit 2; }}
      [ -z "$profile" ] || {{ echo "fakeprov: --files and --profile are exclusive" >&2; exit 2; }}
      [ -d "$files" ] || {{ echo "fakeprov: no directory $files" >&2; exit 2; }}
    elif [ -z "$FAKEPROV_INFO" ] || [ "$FAKEPROV_INFO" = files ]; then
      case "$profile" in
        adult1|adult2) ;;
        *) echo "fakeprov: this command needs a profile; configured: adult1, adult2" >&2; exit 2 ;;
      esac
    fi
    if [ -n "$FAKEPROV_EXIT_FOR" ] && [ "$patient" = "$FAKEPROV_EXIT_FOR" ]; then
      echo "error: the session for profile $profile has expired; sign in again" >&2; exit 3
    fi
    if [ -n "$FAKEPROV_WATCH" ]; then
      cat "$FAKEPROV_WATCH"/*/index.json | cksum >> "$d/export-log"
    fi
    if [ -n "$FAKEPROV_HOLD" ]; then
      : > "$FAKEPROV_HOLD/exporting"; n=0
      while [ ! -e "$FAKEPROV_HOLD/go" ] && [ $n -lt 300 ]; do sleep 0.1; n=$((n + 1)); done
    fi
    if [ -n "$patient" ] && [ -f "$d/synthetic-$patient.ndjson" ]; then
      cat "$d/synthetic-$patient.ndjson"
    else
      cat "$d/synthetic.ndjson"
    fi
    [ -n "$FAKEPROV_STDERR" ] && printf '%s\n' "$FAKEPROV_STDERR" >&2
    exit "${{FAKEPROV_EXIT:-0}}" ;;
  *) echo "fakeprov: unknown command $1" >&2; exit 2 ;;
esac
"#,
            provider.display()
        )
        .as_bytes(),
    );
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    provider.to_path_buf()
}

/// `PATH` for a test's run of medrec: `path` after a folder whose `open` and
/// `xdg-open` launch nothing and only add their arguments to its `launched`
/// file, so that a viewer a test reaches never opens on the machine. The
/// tests also set `MEDREC_VIEWER=none`, which launches nothing at all.
pub fn no_viewer_path(path: &str) -> String {
    format!("{}:{path}", no_viewer_dir().display())
}

/// The folder of that `open` and `xdg-open`, one for the test process: made
/// new, under a name no one can foresee, for this user alone, and its files
/// made new in it.
pub fn no_viewer_dir() -> PathBuf {
    static DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        use std::io::Write;
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        let name = format!("medrec-no-viewer-{}", crate::ids::new("t"));
        let dir = std::env::temp_dir().join(name);
        std::fs::DirBuilder::new().mode(0o700).create(&dir).unwrap();
        let script = "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$(dirname \"$0\")/launched\"\n";
        for name in ["open", "xdg-open"] {
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o700)
                .open(dir.join(name))
                .unwrap();
            f.write_all(script.as_bytes()).unwrap();
        }
        dir
    })
    .clone()
}
