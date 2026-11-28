# AGENTS.md: working in this repo

This is the canonical agent guide. `CLAUDE.md` points here.

## What this is

`medrec-cli` (binary `medrec`) is a household medical-records orchestrator.
It files scans, documents, imaging studies and provider records into one
plain records folder ([docs/data-contract.md](docs/data-contract.md)) and
answers questions over them. It conforms to `piekstra-cli/1`
(`~/Dev/cli-common/DESIGN.md`) with `info.auth.required = false`: it holds no
provider credentials. Its one credential is its Google Drive sign-in, which
only a folder on Drive needs; `auth status --profile <NAME>` reports it.
Provider CLIs (the README's provider table) fetch records; medrec files
them.

## Invariant 1: never read a real record

Medical data is the most sensitive data a household has, and an agent
session is a third-party service.

- Never read, print or summarize a real record, document, study or patient
  name: not from the user's records folder, not from a provider CLI, not
  from Drive. Do not run medrec against the user's real folder to look at
  output.
- Develop and dogfood on synthetic data only:
  `cargo run --example synth -- "$TMPDIR/medrec-dog"` writes scans, DICOM
  discs and a fake provider (`fakeprov`, with login profiles `adult1` and
  `adult2`). Point a config at it with `XDG_CONFIG_HOME` and
  `XDG_CACHE_HOME` in that directory, and give it a profile of its own
  (`medrec profiles add dog`, then `MEDREC_PROFILE=dog`).
- A check against real data reports counts, statuses and exit codes only,
  or is left to the user in their own terminal.
- `medrec mcp` (src/mcp.rs) is the one route by which records reach a
  model: the user's own Claude client, by the user's decision of
  2026-11-22. Its tools stay read-only (`ToolDef::read`, `Access::Read`).
  Nothing in medrec calls an AI service. Develop and test it on synthetic
  data (tests/mcp.rs); never call it against the real folder from an agent
  session.

## Invariant 2: this repo is public

Nothing personal in code, fixtures, docs, commits, pull requests or issues
([PRIVACY.md](PRIVACY.md)). Fixtures are invented, not scrubbed
(`tests/fixtures/README.md`). `tests/no_personal_data.rs` fails on
personal-data shapes in tracked files. Never weaken it or `.gitignore`.

## Invariant 3: build it ourselves

No FHIR, SMART, DICOM or health-data libraries or SDKs, no aggregators, no
other people's MyChart or Kaiser integrations, and no cloud OCR or AI.
General-purpose crates are fine (clap, serde, reqwest, rustls, sha2, md-5,
quick-xml, lopdf), and so are local tools (poppler, tesseract, bsdtar).

## Layout

| File | What it owns |
|---|---|
| `src/main.rs` | The clap tree, output and exit codes, and the `profiles` commands |
| `src/config.rs` | This machine's settings, and the profiles that hold each records folder's (cli-common's `pk_cli_config::profile`) |
| `src/store.rs` | The records folder's backend: a Google Drive folder through cli-common's Drive API store, behind the read cache, or a local directory, and where a command's reads came from (`reads`, `--fresh`); `drive_cli()` is medrec's Drive sign-ins (`medrec drive`, `--drive-profile`) |
| `src/drive_tests.rs` | The store, `init` and `backup` on Drive, against cli-common's fake Drive server |
| `src/folder.rs` | The folder layout, file naming, read-back writes, the lock and `rev` checks |
| `src/model.rs` | Every JSON format in the folder |
| `src/intake/` | Document intake: file types, text and OCR, detection with scores and evidence, duplicates |
| `src/review.rs` | Filing what intake held back |
| `src/import.rs`, `src/stream.rs`, `src/normalize.rs` | Provider import: the bounded subprocess, the `records-stream/v1` reader (medical-common's `pk-med-records`), FHIR to index records |
| `src/against.rs` | `medrec verify --events`: what events and their records say, against the raw files as they are (not through what shaped them), in counts and ids |
| `src/raw.rs` | `medrec records raw` and `records fields`: the raw files of records, the names of their elements with counts (no values but the codes of what a claim's amounts are), and what changed between the versions kept of each |
| `src/served.rs` | What an import says of the records its source gave before: how many it does not give now, by kind, and the names of what changed in each it updated |
| `src/changes.rs` | What a dry run of the reindex counts that the views would say otherwise, for each person (docs/data-contract.md, "A dry run") |
| `src/claims.rs`, `src/reindex.rs` | Claim shaping (line and claim categories, amounts, titles, visit and result links) and `medrec reindex`, which shapes filed claims again, reads every record's providers, every condition's, allergy's and medicine's status and words, and every result's and report's fields, and what each lists, again from their raw files, links the records again, puts results about somebody else that were filed as the person's own in records of their own, and reads bills' and receipts' statements again |
| `src/providers.rs` | Who a record's provider fields name, as its source identifies them: the NPI and the address (state and city) behind each reference (docs/data-contract.md, "Providers") |
| `src/bills.rs`, `src/bill.rs` | Bills (`medrec-bills/v1`): what a bill's or receipt's text says (`statement`: amounts, dates, account, the number to call, what was paid), each bill's claim and event, the receipts and household payments against it, what is left, what no insurer is billed for, the bills worth a call and the shares not billed yet; `medrec bills` and the payments in `marks.json` (`medrec-marks/v1`) (docs/data-contract.md, "Bills" and "marks.json") |
| `src/plain.rs` | Plain names, bundled and never fetched: visit names from CPT codes, billing descriptors in capitals put in title case, and the names of records connections, which are never a place (docs/data-contract.md, "Events") |
| `src/tables.rs` | Published code tables kept on this machine, none bundled: `medrec tables list` and `fetch`, the recorded address, SHA-256, size and row count of each, the zip reader, CDC's text tables and a changed one kept on `--accept`, and ICD-10-CM titles by code (README, "Code tables") |
| `src/nppes.rs` | The provider directory: CMS's monthly NPPES file found, streamed, checked and read for the rows one folder's records need, kept on this machine (README, "Provider directory") |
| `src/code_names.rs` | `code-names.json` (`medrec-code-names/v1`): the titles of the codes a person's conditions and claims use, those only private items use apart, and which an answer may show (docs/data-contract.md, "Code names") |
| `src/labels.rs`, `src/roles.rs` | `providers.json` (`medrec-providers/v1`): a name, practice, role and phone for a provider an item prints, sure by the record's NPI or likely by the directory or a paper of the event, private apart; `provider-state.json` in the person's folder, what the directory gave, by which every machine labels and judges alike; and the role words, medrec's own, by taxonomy code (docs/data-contract.md, "Provider labels") |
| `src/provider_labels.rs` | What an answer shows of `providers.json`: the labels of what an item prints, without what an answer about the whole household may not say (docs/data-contract.md, `provider_labels`) |
| `src/corrections.rs` | What the household says of a provider its items print (a name, practice, role or phone; that two names are one provider or are not; that a label is wrong): checked, written and undone in `marks.json` for `medrec labels` and provider requests alike, and applied in `src/labels.rs` (docs/data-contract.md, "marks.json" and "Provider requests") |
| `src/sources.rs` | Which of a person's sources a record or document came from, and the name and place it gives: only the person's own, told apart by patient, and none where several differ (docs/data-contract.md, "Which source") |
| `src/fills.rs` | The fills that pharmacy claims tell of, by RxNorm, NDC or name, for `standing.json`'s `filled` |
| `src/vaccines.rs` | The vaccines claims tell of, by a line's CVX code or CDC's CPT table, listed as immunizations of their claim's id; the schedules kept on this machine |
| `src/schedule.rs` | A person's vaccine schedule by CDC's CDSi supporting data: the archive read, each antigen's routine series on the path where every dose is given at its earliest recommended age (CDC's skips of a forecast), the dates by CDC's rules, the doses on file under their groups by CVX code, `medrec schedule`, `schedule.json` and the records tool's `schedule` (docs/data-contract.md, "Schedule") |
| `src/xml.rs` | A small reader of XML elements and text, for CDC's schedule files |
| `src/standing.rs` | `standing.json` (`medrec-standing/v1`): what is current among conditions, allergies and medicines by every source's word, and the lists a reader makes of it (docs/data-contract.md, "Standing") |
| `src/given.rs` | What a source gave of an item that the item does not show, which the private rules read beside it (docs/data-contract.md, "What a source gave") |
| `src/about.rs` | What a person's chart holds about somebody else, which no list, view or answer has (docs/data-contract.md, "About another person") |
| `src/panels.rs` | What a panel or a report lists, as its source says it: `lists`, the `members` made of it on each write of the index, headings, what it listed before, and the questionnaires that list a result (`listed_by`) (docs/data-contract.md, "Panels") |
| `src/questionnaires.rs` | Questionnaires about mental health and substance use, by our own names for them and by code: their area, and whether a child answers them of themself |
| `src/services.rs` | Which private area a service is of (a claim's line, a procedure, a line of a paper's text), by its number, its revenue code and its words |
| `src/parts.rs` | Private parts of open items (docs/data-contract.md, "Private parts"): a claim's lines, diagnoses and payer notes, the lines of a paper's text, what an event says by them, and how each is shown without them |
| `src/clinicians.rs` | The clinicians a person's papers name with a private area, the days of a private practice's papers and the clinicians linked to one, and the items that print a clinician by surname and first initial; the household's clinician marks (docs/data-contract.md, "Private", `clinician`) |
| `src/private.rs` | Private items (`medrec-private/v1`): the records, documents, events and imaging studies about mental health, reproductive and sexual health or substance use, by source, ICD-10 code, provider, medicine and test, an item by the clinician it prints (`src/clinicians.rs`), a visit, a condition with no ICD-10 code, a report's conclusion and a result's text value by their own words, a document by the records that link it, a bill or receipt by its words and the papers tied to it, and a study by its report, the household's marks over them, `medrec private`, and whether a question names an item (docs/data-contract.md, "Private") |
| `src/marks.rs` | `marks.json` (`medrec-marks/v1`): the household's payments, private marks, provider corrections and clinician marks, kept apart from `overrides.json` (docs/data-contract.md, "marks.json") |
| `src/events.rs` | Events (`medrec-events/v1`): claims, portal records and papers grouped into one event per visit, stay, test or fill (rules C0-C6, E1-E5, D2-D6 with D3s, X1-X2, N3, N5), the household's `overrides.json` decisions, and what `medrec events` and `medrec_records` kind `events` list (docs/data-contract.md, "Events") |
| `src/mcp.rs`, `src/summary.rs` | `medrec mcp`, the read-only MCP tools (pk-cli-mcp), and the per-person summary behind `medrec_summary` |
| `src/refresh/` | `medrec refresh`: the pass, the `_status/` and `_requests/` shapes (`contract.rs`, docs/data-contract.md), request checks, provider sign-in reads, intervals, the LaunchAgent and this machine's refresh state |
| `src/dicom.rs`, `src/imaging.rs`, `src/view.rs` | DICOM headers (medical-common's `pk-med-dicom`, re-exported), study intake and export, private copies and viewers |
| `src/views.rs`, `src/notes.rs`, `src/people.rs` | Timeline, search, record views, notes, people. The views writer reuses its per-person timeline events and search items (`person_events`, `order_events`, `searchables`) |
| `src/viewfiles.rs` | `views/` (`medrec-views/v2`): each person's view files under content-addressed names, `events.json` among them, `medrec views write`, and the sync `Folder`'s drop and `viewfiles::hold` run after writes (docs/data-contract.md, "views/") |
| `src/maint.rs` | `init`, `doctor`, `verify`, `backup`, `cache clear` |
| `src/testdata.rs` | The synthetic data generator (tests and `examples/synth.rs`) |

The stream reader and the `provenance/v1` types are medical-common's
`pk-med-records` (`src/stream.rs` and `src/model.rs` re-export them).
DICOM headers, DICOMDIR and the synthetic study writer are its
`pk-med-dicom` (`src/dicom.rs` re-exports it; feature `test-util` is on
because `src/testdata.rs` is built in normal builds). Every `pk-med-*`
crate is on one medical-common tag, and every `pk-cli-*` and `pk-med-*`
crate must resolve to one cli-common tag, or two copies of `pk-cli-core`
are built.

## Rules of the code

- A filed document, DICOM file or raw resource is never modified. Changes
  are new files, and an upload never replaces a file (`store::Existing::Keep`;
  on Drive, `put_tree` with `Existing::Keep` refuses the whole copy before
  uploading anything). Only `backup` replaces, and only in its target.
- On a local folder, medrec's folders are 0700 and its files 0600
  (`store::private_dirs`, `store::private_copy`).
- Every write is read back by MD5 before the index points at it. A moved
  source is removed only after that.
- A command that rewrites `household.json`, an `index.json` or
  `review.json` reads it fresh (`Reads::Fresh`) and holds the `FolderLock`.
  An import opens the folder `Access::Fresh` (fresh reads, no lock) for the
  provider's export, then opens it to write (`Folder::open_to_write_within`,
  waiting up to `import_timeout_secs` for the lock), checks the `folder_id`
  is the one it started with, and reads the household again before it
  resolves the person and writes anything.
- Arguments are validated before any Drive request, Keychain read or
  subprocess, so `--help` and bad input never touch the folder.
- One JSON document per invocation. A command that has printed its result
  and still fails (doctor, verify, backup, a partial intake) exits 1
  directly.
- Mutations that discard something (`review drop`) need `--force` without a
  terminal, and exit 6 otherwise.
- A view that gains a field a reader must not ignore (one that changes what
  it leaves out or holds back, or what it must say of an item it shows, as
  "from a claim") raises `viewfiles::READER` to the release's
  version in the same release, and the notes say so; a change a reader of
  the layout would misread takes the next `schema` instead.

## Commands

```sh
make verify      # fmt-check, clippy -D warnings, tests, smoke: the CI gate
make install     # signed release build into ~/.cargo/bin
cargo test --test flow   # the end-to-end flow on synthetic data
```

Tests never read the Keychain, never use the network and never open a viewer.
The black-box tests set `MEDREC_KEYCHAIN_SERVICE` to an empty service and
`MEDREC_VIEWER` to `none`, and run with `testdata::no_viewer_path`: the first
`open` and `xdg-open` on their `PATH` launch nothing. A test of a viewer checks
the command it would run (`Viewer::command`), never runs it. Every harness that
runs the binary sets both; the Drive tests sign in to
cli-common's fake Drive server with keys in memory (`store::testing`).
OCR and poppler assertions are skipped when those tools are missing.

## Process

- Pull requests only; `main` is protected. Every pull request body ends with
  the line `twapp pr marker` prints, and every commit carries
  `--trailer "$(twapp pr marker --trailer)"`.
- `cr` (as `piekstra-dev`) reviews. Merge once it approves with checks green.
- Never force-push. Never set `core.hooksPath`, add `.pii-allowed` or opt out
  of the PII guard.
