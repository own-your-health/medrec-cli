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
//! <DIR>/provider/     a fake provider CLI (`fakeprov`) and its stream
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

pub fn write(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// Write everything above under `out`.
pub fn write_all(out: &Path) {
    write_scans(&out.join("scans"));
    write_dicom(&out.join("dicom"));
    write_provider(&out.join("provider"), STREAM);
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
/// Its `info` is `FAKEPROV_INFO` (`unprofiled`, `no-login` or `files`, else
/// the current one, which lists `login-profiles`). Its profiles are `adult1`
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
      unprofiled|no-login|files) cat "$d/fakeprov-info-$FAKEPROV_INFO.json" ;;
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
