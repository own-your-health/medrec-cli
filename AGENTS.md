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
| `src/store.rs` | The records folder's backend: a Google Drive folder through cli-common's Drive API store, behind the read cache, or a local directory; `drive_cli()` is medrec's Drive sign-ins (`medrec drive`, `--drive-profile`) |
| `src/drive_tests.rs` | The store, `init` and `backup` on Drive, against cli-common's fake Drive server |
| `src/folder.rs` | The folder layout, file naming, read-back writes, the lock and `rev` checks |
| `src/model.rs` | Every JSON format in the folder |
| `src/intake/` | Document intake: file types, text and OCR, detection with scores and evidence, duplicates |
| `src/review.rs` | Filing what intake held back |
| `src/import.rs`, `src/stream.rs`, `src/normalize.rs` | Provider import: the bounded subprocess, the `records-stream/v1` reader (medical-common's `pk-med-records`), FHIR to index records |
| `src/claims.rs`, `src/reindex.rs` | Claim shaping (line and claim categories, amounts, titles, visit and result links) and `medrec reindex`, which shapes filed claims again from their raw files and reads bills' and receipts' statements again |
| `src/bills.rs`, `src/bill.rs` | Bills (`medrec-bills/v1`): what a bill's or receipt's text says (`statement`: amounts, dates, account, the number to call, what was paid), each bill's claim and event, the receipts and household payments against it, what is left, the bills worth a call and the shares not billed yet; `medrec bills` and the payments in `marks.json` (`medrec-marks/v1`) (docs/data-contract.md, "Bills" and "marks.json") |
| `src/private.rs` | Private items (`medrec-private/v1`): the records, documents and events about mental health, reproductive and sexual health or substance use, by source, ICD-10 code, provider, medicine and test, the household's marks over them, `medrec private`, and whether a question names an item (docs/data-contract.md, "Private") |
| `src/marks.rs` | `marks.json` (`medrec-marks/v1`): the household's payments and private marks, kept apart from `overrides.json` (docs/data-contract.md, "marks.json") |
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

## Commands

```sh
make verify      # fmt-check, clippy -D warnings, tests, smoke: the CI gate
make install     # signed release build into ~/.cargo/bin
cargo test --test flow   # the end-to-end flow on synthetic data
```

Tests never read the Keychain and never use the network. The black-box tests
set `MEDREC_KEYCHAIN_SERVICE` to an empty service; the Drive tests sign in to
cli-common's fake Drive server with keys in memory (`store::testing`).
OCR and poppler assertions are skipped when those tools are missing.

## Process

- Pull requests only; `main` is protected. Every pull request body ends with
  the line `twapp pr marker` prints, and every commit carries
  `--trailer "$(twapp pr marker --trailer)"`.
- `cr` (as `piekstra-dev`) reviews. Merge once it approves with checks green.
- Never force-push. Never set `core.hooksPath`, add `.pii-allowed` or opt out
  of the PII guard.
