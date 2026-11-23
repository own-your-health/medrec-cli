# Security

medrec files a household's medical records, the most sensitive data a
household has. This page says where those records are, who can read them in
each place, and what medrec does and does not do with them.

## What medrec touches

| Where | What | Who reads it |
|---|---|---|
| The records folder in Google Drive (`gdrive:<drive profile>/<folder id>`) | Every document, study, raw record, the index and extracted text, notes | The Google account, anyone it shares the folder with, and Google |
| The macOS Keychain item `piekstra.medrec` | medrec's Drive sign-ins: per Drive profile, the OAuth client (id and secret) and the token set (access and refresh) | This Mac's user, through the Keychain's approval for each binary |
| `~/.config/medrec/drive.json` | Each Drive profile's account email, the scopes Google granted, the client id and when it signed in; no secret. Owner-only (0600) | This Mac's user |
| `~/.cache/medrec/` (or `$XDG_CACHE_HOME/medrec`) | The read cache (`household.json`, `index.json`, `text.json` and files read recently) and opened copies (`open/`, removed after 12 hours) | This Mac's user; mode 0700 directories and 0600 files |
| A private temporary directory per command | A document or study being staged before upload | This Mac's user; removed when the command ends |
| `~/.config/medrec/config.json` | Each profile's records folder location and settings, and this machine's settings; no secret. Written owner-only (0600). `config.json.pre-profiles` beside it, also 0600, is the file as it was before `profiles migrate` | This Mac's user |
| A records folder or backup on a local disk (`init <dir>`, `backup --to <dir>`) | Everything the Drive folder holds | This Mac's user: medrec creates its folders 0700 and its files 0600, and `medrec doctor` warns when anything under the records folder can be read by other users |
| A study exported with `images export` | That study's DICOM files | This Mac's user; 0700 folders and 0600 files |
| `~/.cache/medrec/refresh/` | This machine's refresher id (`h_…`, random) and, per folder, the `status.json` it last wrote and when; ids, outcomes and counts, no names. The run lock is in `~/.cache/medrec/locks/`. `medrec cache clear` leaves both, so this Mac stays its folder's refresher | This Mac's user; 0700 directory, 0600 files |
| `~/Library/LaunchAgents/io.github.own-your-health.medrec.refresh.<profile>.plist` | The command `refresh --install` set up: medrec's path, the profile name, the pass interval, and the `PATH`, `XDG_CONFIG_HOME` and `XDG_CACHE_HOME` (when set) at install time; no secret | This Mac's user; launchd runs it in the user's own session, never as root |
| `~/Library/Logs/medrec/refresh-<profile>.log` | One line per scheduled pass that did something: source ids, outcomes, error codes, counts and the command a person should run; never a name or record content | This Mac's user |

medrec holds **one kind of credential**: its Google Drive sign-in, in the
Keychain, never in a file, argv or a log. Provider and insurer CLIs keep
their own logins in the macOS Keychain under their own names. medrec runs
them as bounded subprocesses and passes only a patient id, an organization
and a date.

The sign-in asks Google for `drive.file` unless given `--scope full`. Under
`drive.file` its token reaches only the files medrec's own OAuth client
created, not the rest of the Drive. The OAuth client is the household's
own, a Desktop app client in its Cloud project; Google treats such a
client's secret as not confidential, and PKCE protects each sign-in.

## What medrec never does

- It never sends a record, a document or extracted text to a third-party
  service. Text extraction and OCR run locally (`pdftotext`, `tesseract`).
  There is no telemetry, no cloud OCR and no AI service.
- It has no MCP server and is not registered in `cli-mcp`. An MCP tool returns
  records to the model that calls it, which is a third-party service.
- It never prints or logs a credential, and never takes one on argv.
- A scheduled refresh never waits on a person. A provider's `auth status` that
  hangs on a Keychain approval is stopped after 30 seconds, and the source is
  not tried again in the background until a person runs `medrec refresh
  sign-in <id>`, so the LaunchAgent never piles up dialogs. medrec's own
  Drive sign-in is held to the same rule: a scheduled pass by a binary not
  yet known to read it without an approval (a new build, an update) checks it
  in a `medrec drive status` stopped after 30 seconds. A check that hangs
  ends the pass, and no scheduled pass by that binary asks again until a
  person runs `medrec --profile <NAME> refresh` at the screen.
- It never runs a disc's bundled viewer. `images open` hands the viewer a
  local folder (Weasis through `weasis://$dicom:get -l <folder>`), and makes no
  network call with study data. Weasis's own update check is a Weasis setting;
  turn it off in Weasis's preferences.
- It never edits a filed document, a DICOM file or a raw record. A record a
  source changed is written beside the previous version, which is kept.
- It deletes a source file only when it was moved (the inbox, `~/Downloads`,
  `--move`), and only after Drive's MD5 of the filed copy matches and the index
  has been written and read back.

## Threat model

The records folder is a plain, private Drive folder without encryption
(PLAN D1 and D9). That choice puts trust in Google and in the Google account.

| If this is exposed | Its holder can read | What limits it |
|---|---|---|
| **Google** (the company, its systems, a legal request to it) | The whole folder | Google's terms and law. medrec adds no encryption. Keep the folder private and unshared. |
| **The Google account** (password, session or recovery taken over) | The whole folder, and the rest of the Drive | 2-Step Verification or a passkey on the account; review "Sharing" on the folder |
| **medrec's Drive refresh token** (read out of the Keychain) | Under `drive.file`, the files medrec's client created, read and write, the records folder included; under `drive`, the whole Drive. Until revoked, or six months unused | `medrec drive logout --revoke`, or remove the app in the Google account's third-party connections. The Keychain asks before a new binary reads it. |
| **This Mac, unlocked** (malware or someone at the keyboard) | Everything above: the sign-in, the cache, the mounted Drive if Drive for desktop is used | macOS's own protections; lock the screen |
| **A lost or stolen laptop, powered off or locked** | Nothing while FileVault is on and the account is locked | `medrec doctor` checks FileVault. On loss, revoke medrec's access in the Google account's third-party connections and sign out of the account's sessions. |
| **A backup copy** (`medrec backup --to`) | The whole folder, as of that backup | It is a second copy with the same contents. Keep it on an encrypted disk, or in a remote as private as the first. |
| **The own-your-health app's Drive token** (a browser tab signed in to it) | What the household's own Google OAuth client grants: at least the whole folder, read. The app writes only `_requests/`, and only pending requests, but Drive cannot hold a token to one folder: with a scope that can write, its holder can change or delete any file the scope reaches. | The token lives in the tab's memory and expires within the hour; the app keeps nothing secret in browser storage. Register the client with the narrowest Drive scope that can read the folder and create requests, in Testing mode with only the household as test users, and remove the app in the Google account's third-party connections to revoke it. A request can only ask for an import the refreshing Mac already has set up (data-contract.md, `_requests/`). `medrec verify` reports a filed file that changed or vanished (by MD5) and any file no index names, but it cannot tell an edited `household.json` or `index.json` from one medrec wrote. |
| **This repository** (public) | No personal data | [PRIVACY.md](PRIVACY.md) and the tests that enforce it |

`medrec cache clear` removes every copy on this Mac (the read cache and
opened copies). It keeps the run locks and the refresh state. The records
folder is not touched.

## Supply chain

- Shared crates are pinned to tags: cli-common and medical-common, both the
  family's own. No third-party FHIR, SMART, DICOM or health-data library is
  used. The records stream comes from medical-common; the DICOM header reader
  and the FHIR normalizer are this repo's own code.
- CI runs `cargo audit`, `cargo deny` and `gitleaks` on every pull request,
  on GitHub-hosted Linux runners. `make install` builds with `--locked` from
  the reviewed `Cargo.lock`.
- Release binaries are built by the release workflow from a version tag, with a SHA-256 file beside each.

## Reporting a vulnerability

Use GitHub's private vulnerability reporting on this repository ("Report a
vulnerability" under Security). Do not put a record, a screenshot or a
patient's name in any report or issue.
