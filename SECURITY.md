# Security

medrec files a household's medical records, the most sensitive data a
household has. This page says where those records are, who can read them in
each place, and what medrec does and does not do with them.

## What medrec touches

| Where | What | Who reads it |
|---|---|---|
| The records folder in Google Drive (`gdrive:<drive profile>/<folder id>`) | Every document, study, raw record, the index and extracted text, notes | The Google account, anyone it shares the folder with, and Google |
| The macOS Keychain item `piekstra.medrec` | A random 256-bit key, the vault's: nothing else | This Mac's user, through the Keychain's approval for each binary |
| `~/.config/medrec/vault/` | medrec's Drive sign-ins, per Drive profile the OAuth client (id and secret) and the token set (access and refresh), each in its own file sealed with the Keychain item's key (XChaCha20-Poly1305). A file is named by a hash, so no profile name is on disk in the clear | This Mac's user: an owner-only (0700) directory of 0600 files, unreadable without the key. A copy taken elsewhere (a backup, a dotfiles sync) holds the sign-ins sealed; it opens only with this Mac's Keychain item |
| `~/.config/medrec/drive.json` | Each Drive profile's account email, the scopes Google granted, the client id and when it signed in; no secret. Owner-only (0600) | This Mac's user |
| `~/.cache/medrec/` (or `$XDG_CACHE_HOME/medrec`) | The read cache (`household.json`, `index.json`, `text.json` and files read recently) and opened copies (`open/`, removed after 12 hours) | This Mac's user; mode 0700 directories and 0600 files |
| A private temporary directory per command | A document or study being staged before upload; in a backup from Drive to Drive, the one file being copied; a provider's records stream spooled between its export and its filing (`medrec-spool-*`), for as long as a `refresh` pass or a several-member `import` runs; for `reindex`, and for `records fields` and `verify --events` of a folder on Drive, copies of the raw-record folders of the records they read; for `records raw` of a folder on Drive, each raw file, while it is read | This Mac's user (directory `0700`, spool file `0600`); removed when the command ends (a backup's, once each file is uploaded), and a spool whose command was killed is removed by the next export |
| `~/.config/medrec/config.json` | Each profile's records folder location and settings, and this machine's settings; no secret. Written owner-only (0600). `config.json.pre-profiles` beside it, also 0600, is the file as it was before `profiles migrate` | This Mac's user |
| A records folder or backup on a local disk (`init <dir>`, `backup --to <dir>`) | Everything the Drive folder holds | This Mac's user: medrec creates its folders 0700 and its files 0600, and `medrec doctor` warns when anything under the records folder can be read by other users |
| A study exported with `images export` | That study's DICOM files | This Mac's user; 0700 folders and 0600 files |
| `~/.cache/medrec/refresh/` | This machine's refresher id (`h_…`, random) and, per folder, the `status.json` it last wrote and when; ids, outcomes and counts, no names. The run lock is in `~/.cache/medrec/locks/`. `medrec cache clear` leaves both, so this Mac stays its folder's refresher | This Mac's user; 0700 directory, 0600 files |
| `~/.cache/medrec/tables/` | Published code tables (`medrec tables fetch`): their publishers' files as downloaded, and no record. | This Mac's user; 0700 directory, 0600 files |
| `~/.cache/medrec/directory/` | With `tables fetch nppes`, rows of CMS's public provider file, chosen by the NPIs, states and provider names in a records folder's records: who is among them says something of the records. `medrec cache clear` removes it | This Mac's user; 0700 directory, 0600 files |
| `~/Library/LaunchAgents/io.github.own-your-health.medrec.refresh.<profile>.plist` | The command `refresh --install` set up: medrec's path, the profile name, the pass interval, and the `PATH`, `XDG_CONFIG_HOME` and `XDG_CACHE_HOME` (when set) at install time; no secret | This Mac's user; launchd runs it in the user's own session, never as root |
| `~/Library/Logs/medrec/refresh-<profile>.log` | One line per scheduled pass that did something: source ids, outcomes, error codes, counts and the command a person should run; never a name or record content | This Mac's user |
| A Claude Desktop or Claude Code conversation that uses `medrec mcp` | What a tool returned for a question: names, results, medicines, document text | Anthropic, under the user's Claude account and its retention settings |

medrec holds **one kind of credential**: its Google Drive sign-in, sealed in
its vault with a key only the Keychain holds, never in the clear in a file,
argv or a log. One Keychain item serves every sign-in, so a new build asks
for one approval, however many Drive profiles there are. A vault whose key
is gone (the Keychain item deleted) cannot be read and is not replaced
while sealed files remain: remove each Drive profile (`medrec drive
profiles remove <NAME> --force`, which deletes its files without the key),
then sign in again with the client's JSON (`drive login --client-file`). Provider and insurer CLIs keep
their own logins in the macOS Keychain under their own names. medrec runs
them as bounded subprocesses and passes only a patient id, an organization
and a date.

The sign-in asks Google for `drive.file` unless given `--scope full`. Under
`drive.file` its token reaches only the files a client of the household's
Cloud project created, not the rest of the Drive: every OAuth client in one
Cloud project shares that access. The OAuth client is the household's
own, a Desktop app client in its Cloud project; Google treats such a
client's secret as not confidential, and PKCE protects each sign-in.

## What medrec never does

- It never sends a record, a document or extracted text to a third-party
  service on its own. Text extraction and OCR run locally (`pdftotext`,
  `tesseract`). There is no telemetry, no cloud OCR and no AI service.
- `medrec mcp` answers only the Claude client the user adds on this Mac
  (Claude Desktop or Claude Code). What a tool returns goes to Anthropic in
  that conversation; no other AI service is connected. Its four tools read
  and never write: no lock is taken and nothing in the folder changes. A
  folder on Google Drive is read through medrec's own Drive sign-in; it holds
  no other credential.
- It never prints or logs a credential, and never takes one on argv.
- A scheduled refresh never waits on a person. A provider's `auth status` that
  hangs on a Keychain approval is stopped after 30 seconds, and the source is
  not tried again in the background until a person runs `medrec refresh
  sign-in <id>`, so the LaunchAgent never piles up dialogs. For medrec's own
  Drive sign-in a scheduled pass raises none: it reads the key first with
  macOS's dialogs turned off, sending nothing to Google, and a key that a
  person would have to allow (a new build or an update, a locked keychain)
  ends the pass, naming `medrec auth key` to run at the screen. One process reads the
  key once: every later sign-in in that run uses it, so a person's "Allow"
  covers the whole run, and "Always Allow" covers that binary from then
  on.
- It never runs a disc's bundled viewer. `images open` hands the viewer a
  local folder (Weasis through `weasis://$dicom:get -l <folder>`), and makes no
  network call with study data. Weasis's own update check is a Weasis setting;
  turn it off in Weasis's preferences.
- `medrec tables fetch` asks a table's publisher (www.cms.gov) for one fixed
  file over https, whole, and keeps it only if its SHA-256 is the recorded
  one. `medrec tables fetch nppes` asks download.cms.gov for the page that
  lists its provider file, and for that file, whole. No other command contacts
  either, and no request carries a code, a name, an NPI or anything else from
  the records. The titles a person's codes have and the labels of their
  providers are in `views/` (`code-names.json`, `providers.json`), inside the
  records folder. Each person's `provider-state.json` there has their
  providers' labels by NPI, those of private ones with the rest. What the
  household says of a provider (`medrec labels`, and provider requests) is in
  the person's `marks.json`; a handled request's `done/` file and message
  repeat no name.
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
| **medrec's Drive sign-in** (the vault files under `~/.config/medrec/vault/` and the key in the Keychain item) | Under `drive.file`, the files the household's Cloud project's clients created, read and write, the records folder included; under `drive`, the whole Drive. Until revoked, or six months unused | The vault files alone open nothing: they are sealed with the Keychain item's key, and the Keychain asks before a new binary reads it. `medrec drive logout --revoke`, or removing the app in the Google account's third-party connections, revokes the grant for every client of the project, so it signs the own-your-health app out too. |
| **This Mac, unlocked** (malware or someone at the keyboard) | Everything above: the sign-in, the cache, the mounted Drive if Drive for desktop is used | macOS's own protections; lock the screen |
| **A lost or stolen laptop, powered off or locked** | Nothing while FileVault is on and the account is locked | `medrec doctor` checks FileVault. On loss, revoke medrec's access in the Google account's third-party connections and sign out of the account's sessions. |
| **A backup copy** (`medrec backup --to`) | The whole folder, as of that backup | It is a second copy with the same contents. Keep it on an encrypted disk, or in a remote as private as the first. |
| **The own-your-health app's Drive token** (a browser tab signed in to it) | What the household's own Google OAuth client grants: at least the whole folder, read. The app writes only `_requests/`, and only pending requests, but Drive cannot hold a token to one folder: with a scope that can write, its holder can change or delete any file the scope reaches. | The token lives in the tab's memory and expires within the hour; the app keeps nothing secret in browser storage. Register the client with the narrowest Drive scope that can read the folder and create requests, in Testing mode with only the household as test users, and remove the app in the Google account's third-party connections to revoke it (which revokes medrec's sign-in too: Google keeps one grant per project). A request can only ask for an import the refreshing Mac already has set up (data-contract.md, `_requests/`). `medrec verify` reports a filed file that changed or vanished (by MD5) and any file no index names, but it cannot tell an edited `household.json` or `index.json` from one medrec wrote. A path an index gives is read only where it is relative and made of plain names: one with a root, `.` or `..` is read as a file that is not there, and nothing is copied to it. |
| **The user's Claude account** (with `medrec mcp` added) | What the tools returned in its conversations | The account's own security and Anthropic's retention settings. Remove the extension (or `claude mcp remove medrec`) to stop; past conversations keep what they were given until deleted. |
| **This repository** (public) | No personal data | [PRIVACY.md](PRIVACY.md) and the tests that enforce it |

`views/` in the records folder (0.8.0 and later) holds the same records as
each person's `index.json` and `text.json`, cut into small files for
readers. It is inside the folder, so everything above covers it. On Drive,
the views a pass replaces go to the account's trash, as every file medrec
removes there does. They stay there until the trash is emptied or Drive
removes them after 30 days, and they count against the storage quota. One
new result for a person with about 1,600 replaces about 30 views, 0.9 MB,
in medrec's large synthetic folder. `medrec views write --all`, or a large
backfill, can send megabytes at once. Emptying the trash removes them for
good; the views themselves are made again from `index.json` and `text.json`.

`medrec cache clear` removes every copy on this Mac (the read cache and
opened copies, and the provider directory's rows). It keeps the run locks, the
refresh state and the code tables, which are published files and no record. The records folder is not
touched.

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
