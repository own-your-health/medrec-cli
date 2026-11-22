# Data contract: the records folder

The records folder is one directory tree, normally a private Google Drive
folder reached through an rclone remote (`gdrive:Medical Records`), or a
local directory. medrec reads and writes it only through this contract.
Every file is plain: PDFs, images and DICOM as received, JSON, and Markdown.
A person can open any of it in the Drive app without medrec.

## Layout

```text
README.md                     what this folder is, and its rules (written by `init`)
household.json                people, organizations, sources
inbox/                        drops waiting for `medrec intake --inbox`
_review/                      documents intake could not file, and review.json
_status/                      what `medrec refresh` did: status.json, attempts/ (0.3.0 and later)
_requests/                    refresh requests from the app, and done/ (0.3.0 and later)
<Person>/
  notes.md                    dated notes; this file is their source
  index.json                  the person's index (documents, records, studies, imports)
  text.json                   extracted text per document
  Documents/<YYYY>/<YYYY-MM-DD>_<type>_<org>_<details>_<original name>
  Claims/<YYYY>/              EOBs, bills and receipts, named the same way
  Imaging/<YYYY-MM-DD>_<modality>_<body part>_<facility>/
    S<series:03>/<instance:05>.dcm   DICOM files exactly as received
    study.json                       the study's summary (medrec-study/v1)
  Records/<source>/<ResourceType>/<id>.json   FHIR resources (R4 or STU3), verbatim
```

- `<Person>` is the person's name when they were added (`Robin Example`). It
  never changes afterwards, even when the person is renamed; `household.json`
  maps the person's id to it.
- `<source>` is the provider CLI and organization, as `<cli>-<org slug>`
  (`mychart-example-health`).
- A file name's descriptors are lowercase ASCII words joined by `_`: the type
  is at most 24 characters, the organization and details at most 40, and the
  original name's stem at most 80. The original name is kept as the suffix so
  a filed document can be traced to what was received.
- A study folder without a date is `undated_…`. A DICOM file whose series or
  instance number is missing is named by its SOPInstanceUID.
- A raw resource's `<id>` is its FHIR id when that is at most 64 of
  `A-Z a-z 0-9 - .` and does not start with `.`. Any other id, such as one of
  Epic's unconstrained ids (up to 256 characters), is named
  `id-<first 32 hex of the id's SHA-256>`, the same on every import. A
  resource with no id is `noid-<first 32 hex of its content's SHA-256>`
  (0.2.7 and later), its content being the resource without
  `meta.lastUpdated`, so no two share a file and one stamped again keeps
  its own.
- A raw resource that changed at its source is written as the new
  `<id>.json`. The previous version stays beside it as
  `<id>~<first 8 hex of its MD5>.json`. Which version is current is the
  import's rule (Import, below). A copy refreshed in place is held as
  `<id>~refresh-<hex>.json` only while its replacement uploads, `<hex>`
  being the start of the replacement's MD5. One left by an import cut short
  is removed by the next when the current copy is that replacement.
  Otherwise it is put back, and a current copy that is not the replacement
  (an upload that did not read back) is kept beside it as an earlier
  version.

## Rules

1. **Filed bytes are never changed.** Documents, DICOM files and raw
   resources are written once and never edited. A re-scan, a second disc or
   a corrected record is a new file. An upload never replaces a file: one
   already at its name with the same bytes is taken as that upload (a run
   that stopped before writing its index left it there), and one with other
   bytes stops the command. A new study never shares a folder that exists
   in Drive but not in the index.
2. **Every write is read back.** A file is uploaded, its MD5 (as Drive
   reports it) is compared with the local one, and only then does the index
   point at it. A moved source is removed only after that.
3. **One writer at a time per machine**, through a lock file under the
   per-machine cache. Across machines, `household.json`, `index.json` and
   `review.json` carry a `rev` counter. A write whose `rev` no longer matches
   the folder's is refused, and the command is run again. An import takes
   the lock only once its provider's export has finished, so other writers
   carry on during a long export. Another writer waits up to 2 minutes for
   the lock; an import with its export in hand waits as long as an export
   may run (`import_timeout_secs`), so its export is not thrown away. It then
   refuses a folder whose `household.json` was replaced in the meantime,
   reads the household again and resolves the person from that. `images intake` checks the `rev` before
   uploading, and saves each study's index entry as soon as it is filed.
4. **A newer layout is refused.** `household.json` declares `format: 1`. A
   medrec that finds a larger number stops and says to update.
5. **Rename or move files through medrec**, so the index stays right.
   `medrec verify` reports any file that is missing, changed (by MD5, or by
   SHA-256 with `--deep`) or not in an index.

## household.json (`medrec-household/v1`)

```json
{
  "schema": "medrec-household/v1",
  "format": 1,
  "folder_id": "f_…",
  "rev": 7,
  "created_at": "2026-01-01T00:00:00Z",
  "people": [{
    "id": "p_…", "name": "Robin Example", "folder": "Robin Example",
    "birth_date": "2020-02-02", "adult": false, "guardians": ["p_…"],
    "aliases": ["Robbie Example"],
    "identifiers": [{"source": "mychart:example-health", "value": "pat-0001", "kind": "patient"}],
    "links": {"huishouden_health_person": "…", "baby_profile": "…"},
    "created_at": "…"
  }],
  "organizations": [{"id": "g_…", "name": "Example Health", "aliases": [], "fhir_base": "https://fhir.example.org/r4"}],
  "sources": [{
    "id": "s_…", "kind": "fhir", "person": "p_…", "cli": "mychart",
    "organization": "example-health", "organization_name": "Example Health",
    "fhir_base": "…", "patient_id": "pat-0001", "patient_label": "…", "last_pull": "…"
  }]
}
```

Ids are a one-letter kind (`p` person, `g` organization, `s` source, `d` document,
`r` record, `m` study, `i` import, `n` note, `q` review item, `f` folder), `_`, and 20 random base-32
characters (100 bits). An id names nothing about the person or the record. `links`
are fields only: medrec reads and writes nothing in Huishouden.

## index.json (`medrec-index/v1`)

One per person: `{schema, person, rev, documents, records, studies,
imports}`.

**Document**, a filed file (a FHIR `Binary`, with the `DocumentReference`,
`DiagnosticReport` or `Media` that names it):
`id`, `path`, `sha256` (its identity), `md5` (Drive's hash, for `verify`
without a download), `size`, `media_type`, `pages`, `original_name`, `title`,
`doc_type`, `loinc`, `date` (of service), `organization`, `filed_at`,
`origin` (`{kind: file|inbox|drive|fhir|review, name, action: copied|moved}`),
`text` (`{chars, engines, ocr_pages, note}`), `near_duplicates`
(`[{document, similarity}]`), `minhash`, `renderings` (`[{sha256, size,
fetched_at}]`: each distinct rendering later imports served of it with the
same text, and those of a copy folded in by `documents merge`, once; served
again byte for byte, it is a duplicate),
`classification` (what was detected, with scores and evidence),
`statement` and `same_statement_as` (a bill's, below), and `provenance`. An imported document's `provenance.raw_path` names its
Binary's raw file, which holds the latest rendering the source served;
earlier ones stay beside it as `<id>~<md5>.json`, as for every raw
resource, and the document's own bytes are the filed file.

Document types: `visit_summary`, `lab_report`, `imaging_report`,
`immunization_record`, `discharge_summary`, `referral`, `prescription`,
`letter`, `bill`, `receipt` (0.2.9 and later), `eob`, `insurance_card`,
`other`.

**A bill's statement** (0.2.9 and later). A `bill` carries `statement`:
`{statement_date, amount_due: {amount, currency}, due_date, account}`, each
only where its text labels it (`Statement date`, `Amount due` or `Balance
due`, `Due date` or `Please pay by`, `Account number` or `Guarantor
account #`), on the label's line or, under a column's header, on the next
line. An amount is a plain decimal with two places, negative for a credit.
The account is as printed, upper-cased, a mask kept (`****0123`). Intake,
`review file` and an import read it the same way, from the document's
text, so a statement the user files and the same statement a billing CLI
exports later are known to be one. They are one when both give the same
statement date, nothing both give differs, and both give the same account
number (a masked one matching by its last four or more digits) or the
same amount due. The one filed later keeps its own file and sets
`same_statement_as` to the first one's id. A bill filed before 0.2.9, or
saved since by an older medrec (which drops fields it does not know), gets
its statement and link again from its stored text the next time a bill is
filed for that person.

A DocumentReference names a house type with a `type.coding` in the system
`urn:pk-med:doc-type` (code `bill`, `receipt`, or any type above). With no
such coding and no LOINC code, a `type.text` of `bill` or `receipt` (any
case) is that type. Otherwise the LOINC code decides, and other text is
only a display.

**Record**, one normalized clinical fact: `id`, `kind` (`encounter`,
`result`, `report`, `condition`, `medication`, `allergy`, `immunization`,
`procedure`, `coverage`, `claim`), `key` (kind, code and date, so the same
fact from two sources is one record; a fill and a claim are keyed by their
identity instead, below), `date`, `code` (`{system, code,
display}`), `display`, `status`, `fields` (kind-specific, with FHIR element
names), `documents` (linked document ids) and `provenance` (one per source).

A `medication` comes from a MedicationRequest (`source: request`), a
MedicationStatement (`statement`) or a MedicationDispense (`dispense`, a
fill). A fill is dated when it was handed over, else when it was prepared;
it is coded by RxNorm, else NDC. Its fields are `quantity` (`{value,
unit}`), `days_supply`, `pharmacy`, `type` (first fill or refill), `ndc`,
`dosage`, and `prescriber` (0.2.8 and later): the `requester` of the
MedicationRequest its `authorizingPrescription` points at, either one
contained in the fill (`#<id>`) or one the same source serves
(`MedicationRequest/<id>`: in the same stream, or else its own raw file
under the source, filed by an earlier import). A pharmacy `claim`
(an ExplanationOfBenefit of type pharmacy) also has `days_supply`,
`refill_number`, `refills_authorized` and `brand_generic`, from its
`supportingInfo`. Each of its `items` has a `quantity`. It is named for
what was filled (its first item's product).

A fill and a claim are each one event, so their `key` names the event:
`medication-dispense|<identity>|<date>` and `claim|<identity>|<date>`. The
identity is the resource's first identifier (`system|value`). Without one,
it is `id|<fhir_base>|<cli>|<organization>|<resource id>`, since an id is
unique only on the source that issued it. Without an id either, it is
`sha256|<hash of its content>`, the resource without `meta.lastUpdated`
(0.2.7 and later), so such a resource whose content changes is a new
record, its earlier version staying beside it, and one only stamped again
is not. So two fills of a drug, or two claims of
a type, on one day stay two records, and a fill never folds into a
prescription. The same event from two sources is one record only when both
send the same identifier. Without one, it is two records, one per source.
A record a resource shares with others, whose key that resource no longer
matches, lets it go to a record of its own on its next import. That splits
the claims 0.2.5 and earlier folded under a type-and-date key. The record
left behind keeps the fields last written to it, which may be the departed
resource's, until its remaining source is imported again.

A record is the same source resource when its `fhir_base`, `cli`,
`organization_id`, resource type and id all match. Two resources from one
source that share a key (two readings of one test on one day, a condition
entered twice) are one record: the first of them on the record writes its
fields, and the other keeps only its own provenance, whatever order the
source serves them in (0.2.8 and later; before, each import's later one
wrote over the first). Both stay in the raw layer, and the record names
both. A resource with no id
is never the same resource as another; it joins a record only by key. On
that record, the provenances of its type from its source with no id belong
to the resources like it: each takes the one naming its raw file
(`noid-<digest>`), or else, for a version with other content or one filed
before 0.2.7, the first that no resource of the same import names, or adds
one. The first of those provenances on the record writes its fields,
whatever order the source serves them in, as the first of two resources
with ids that share a key does. So serving them again is unchanged, and a
correction to the first is written in place.

**ImagingStudy**: `id`, `study_uid`, `path`, `date`, `modalities`,
`body_part`, `description`, `facility`, `accession`, `referring`, `series`
(`[{uid, number, modality, description, instances: {SOPInstanceUID: file}}]`),
`instances`, `size`, `report` (a document or record id), `files` (each
filed file's MD5 by its path in the study folder, for `verify`), `filed_at`,
`provenance`.

**ImportRun**, one `medrec import`: `id`, `source_dir`, `stream_sha256`,
`stream_bytes`, `cli`, `cli_version`, `organization`, `patient_id`,
`pulled_at`, `imported_at`, `counts`, `skipped`, and the counts the import
report gives (below): `resources_written`, `resources_unchanged`,
`records_added`, `records_updated`, `records_unchanged`, `records_older`
(0.2.7 and later; 0 in a run filed before), `documents_added`,
`documents_duplicate` and `documents_unchanged`.

**Provenance** (the `provenance/v1` fields of medical-common's
`pk-med-records`): `source` (`{kind, cli, cli_version, organization_id,
fhir_base}`; an import's `kind` is `fhir` for a FHIR API read and the
stream's `method` otherwise), `method` (`fhir-api`, `portal`, `scan`, `file`, `media`,
`manual`), `source_resource` (`{type, id, version_id, last_updated}`),
`fetched_at`, `filed_at`, `raw_path` (the verbatim file), `classified_by`
(`medrec`, `organize-scans`, `hand`), `derived_from`, `filed_by` (`medrec
<version>`).

## text.json (`medrec-text/v1`)

`{schema, person, texts: {<document id>: <text>}}`. It is kept apart from the
index so listing documents never downloads every page of text.

## _review/review.json (`medrec-review/v1`)

`{schema, rev, items}`. Each item: `id`, `path` (`_review/<date>_<name>`),
`sha256`, `md5`, `size`, `media_type`, `pages`, `original_name`,
`received_at`, `origin`, `reasons` (each field that was unsure, or a
disagreement with `classified_by`), `text`, `text_info`, `minhash`,
`classification`, `classified_by`.

## study.json (`medrec-study/v1`)

The index's ImagingStudy and `person`. It carries no patient name, birth
date or patient id from the DICOM headers; those stay in the DICOM files.

## notes.md

```markdown
# Notes: Robin Example

## 2026-03-05 (n_…)
Tags: question, follow-up
About: d_…

Ask about the recheck at the next visit.
```

Notes are sorted by date. The `Tags:` and `About:` lines are optional.
`notes.md` is the notes' only store; a note edited by hand in this format is
read back as edited.

## _status/ (`medrec-status/v1`, `medrec-attempt/v1`)

What `medrec refresh` (0.3.0 and later) has done: for each source it
refreshes, the latest attempt, the latest success and the sign-in, and every
attempt. Only the one machine that refreshes writes here; every other reader
(the own-your-health app among them) only reads. `medrec refresh --install`
creates the folder. These are state, not filed bytes (Rules, above): medrec
rewrites them, and they carry no `rev`.

```text
_status/
  status.json                 each refreshed source now
  attempts/<YYYY-MM>.ndjson   every attempt that month, one per line
```

### status.json

```json
{
  "schema": "medrec-status/v1",
  "format": 1,
  "updated_at": "2026-11-22T20:05:00Z",
  "refresher": {
    "medrec_version": "0.3.0",
    "last_pass_at": "2026-11-22T20:05:00Z",
    "next_pass_at": "2026-11-22T20:10:00Z",
    "pass_every_secs": 300,
    "heartbeat_every_secs": 3600
  },
  "sources": [
    {
      "source": "s_aaaaaaaaaaaaaaaaaaaa",
      "person": "p_aaaaaaaaaaaaaaaaaaaa",
      "cli": "mychart",
      "organization": "example-health",
      "auto": true,
      "paused": null,
      "min_interval_secs": 86400,
      "next_run_at": "2026-11-23T20:04:31Z",
      "sign_in": {
        "state": "ok",
        "expires_at": "2026-12-01T00:00:00Z",
        "checked_at": "2026-11-22T20:04:02Z",
        "needs_person": null
      },
      "last_attempt": {
        "attempt": "t_aaaaaaaaaaaaaaaaaaaa",
        "started_at": "2026-11-22T20:04:02Z",
        "finished_at": "2026-11-22T20:04:31Z",
        "trigger": "request",
        "requests": ["x_aaaaaaaaaaaaaaaaaaaa"],
        "outcome": "ok",
        "exit_code": 0,
        "error": null,
        "import": "i_aaaaaaaaaaaaaaaaaaaa"
      },
      "last_success": {
        "attempt": "t_aaaaaaaaaaaaaaaaaaaa",
        "finished_at": "2026-11-22T20:04:31Z",
        "import": "i_aaaaaaaaaaaaaaaaaaaa",
        "counts": {
          "records_added": 2, "records_updated": 0, "records_unchanged": 41,
          "records_older": 0, "resources_written": 2, "resources_unchanged": 41,
          "documents_added": 0, "documents_duplicate": 0, "documents_unchanged": 3
        },
        "withheld_types": []
      }
    }
  ]
}
```

| Field | Meaning |
|---|---|
| `format` | `1`. A reader that finds a larger number stops and says to update, as for `household.json`. Fields added within a format are additive, and a reader ignores fields it does not know. |
| `updated_at` | When medrec last wrote the file. |
| `refresher.last_pass_at` | The latest pass of `medrec refresh`. A pass that runs an attempt or handles a request writes it; otherwise it is written at least every `heartbeat_every_secs`. A reader takes the refreshing machine as offline when it is older than `heartbeat_every_secs + pass_every_secs`. |
| `refresher.next_pass_at` | When the next pass is due. |
| `refresher.pass_every_secs` | How often a pass looks at `_requests/`. |
| `sources[]` | Each source set up for refresh (`medrec refresh add`). A source in `household.json` that is not here is not refreshed; its last load is its latest `ImportRun`. |
| `source`, `person`, `cli`, `organization` | The source's id in `household.json`, its person, its provider CLI and its organization. |
| `auto` | `true`: imported on its schedule. `false`: imported only when a request or a terminal `medrec refresh` asks. |
| `paused` | `null`, or `{"reason": "consent" \| "user", "note": "…", "since": "…"}`. Nothing imports a paused source, neither the schedule, a request nor a terminal `medrec refresh`, until `medrec refresh resume <source>`. A source whose provider terms wait on the household's consent is paused with `consent`. |
| `min_interval_secs` | The shortest time between two of its imports: never shorter than its CLI documents, and at least a day (`86400`) for a member site. A request does not shorten it. |
| `next_run_at` | The next scheduled import; `null` when `auto` is `false`, the source is paused, or it needs a person. |
| `sign_in.state` | From the provider's `auth status --json` (`auth-status/v1`): `ok`, `expiring` (`expires_at` within 72 hours), `expired`, `signed_out`, `needs_person`, `not_needed` (a provider with nothing to sign in to) or `unknown` (not read yet). |
| `sign_in.expires_at` | The sign-in's end, where auth status gives one; else `null`. |
| `sign_in.checked_at` | When auth status was last read. |
| `sign_in.needs_person` | Set when `state` is `needs_person`, else `null`: `{"reason": "sign_in" \| "keychain", "since": "…", "command": "medrec refresh sign-in <source>"}`. `sign_in`: the provider's sign-in ended or never happened. `keychain`: reading the provider's stored sign-in waited on an approval nobody gave, as a rebuilt provider CLI's first Keychain read does, and timed out. `command` is what to run in a terminal on the refreshing machine. Until it has run, neither the schedule nor a request tries the source again. |
| `last_attempt` | The latest attempt: an `attempts/` line without `source`, `person`, `cli`, `organization` and `counts`. While it runs, `outcome` is `running` and `finished_at` is `null`. |
| `last_success` | The latest attempt whose outcome was `ok`: its attempt, when it finished, its `ImportRun`, and the import report's counts and `withheld_types`. |

### attempts/<YYYY-MM>.ndjson (`medrec-attempt/v1`)

One JSON object per line, in the order the attempts started, in the file of
the month (UTC) the attempt started. Drive has no append, so medrec rewrites
the month's file whole after each attempt.

```json
{"schema":"medrec-attempt/v1","attempt":"t_aaaaaaaaaaaaaaaaaaaa","source":"s_aaaaaaaaaaaaaaaaaaaa","person":"p_aaaaaaaaaaaaaaaaaaaa","cli":"mychart","organization":"example-health","started_at":"2026-11-22T20:04:02Z","finished_at":"2026-11-22T20:04:31Z","trigger":"request","requests":["x_aaaaaaaaaaaaaaaaaaaa"],"outcome":"ok","exit_code":0,"error":null,"import":"i_aaaaaaaaaaaaaaaaaaaa","counts":{"records_added":2,"records_updated":0,"records_unchanged":41}}
```

| Field | Meaning |
|---|---|
| `attempt` | Its id, `t_` and 20 base32 characters. |
| `trigger` | `schedule`; `request`, with the `requests` it served; or `manual`, a terminal `medrec refresh`. |
| `requests` | The `_requests/` ids the attempt served; empty otherwise. |
| `outcome` | `ok`, `failed`, `needs_person`, `skipped_interval` (imported less than `min_interval_secs` ago) or `skipped_paused`. `running` appears only in `status.json`. |
| `exit_code` | The import's exit code (Import, below); `null` for a skip, and for `needs_person` found before the import ran. |
| `error` | `null`, or `{"code", "message", "remedy"}` from the failing command's `--json` error (the family's error shape). `message` is at most 300 characters. medrec's own codes are `timeout` (a call outlived its limit) and `needs_person`. |
| `import` | The `ImportRun` id in the person's `index.json`, when `ok`. |
| `counts` | When `ok`, the import report's counts: `records_added`, `records_updated`, `records_unchanged`, `records_older`, `resources_written`, `resources_unchanged`, `documents_added`, `documents_duplicate`, `documents_unchanged`. Additive. |

A scheduled pass records nothing for a source that is not due. A skip is
recorded only when a request or a terminal command asked for that source.
When another writer holds the folder's lock, no attempt is recorded and the
next pass tries again.

## _requests/ (`medrec-request/v1`)

Requests to refresh, from a reader that cannot run medrec itself (the
own-your-health app). Each pass of `medrec refresh` looks for them with one
listing of `_requests/`.

```text
_requests/
  <YYYYMMDDTHHMMSSZ>_<id>.json    pending
  done/<same name>                handled; kept 90 days
```

A pending request:

```json
{
  "schema": "medrec-request/v1",
  "id": "x_aaaaaaaaaaaaaaaaaaaa",
  "requested_at": "2026-11-22T20:03:12Z",
  "requested_by": {"email": "alex@example.com", "name": "Alex Testperson"},
  "scope": {"source": "s_aaaaaaaaaaaaaaaaaaaa"},
  "app": {"name": "own-your-health-app", "version": "0.1.0"}
}
```

- The writer creates each request once and never edits it. While it is
  still pending it may delete it, as a cancel. It writes nothing else in the
  folder.
- `scope` is `{"all": true}`, `{"person": "p_…"}` or `{"source": "s_…"}`.
  `all` and `person` cover the sources set up for refresh (`status.json`
  `sources`).
- `requested_by` and `app` are shown, never trusted.

How medrec handles one:

1. **A request is untrusted input.** It is handled at once as `invalid` if
   any of these holds:
   - it is larger than 4 KiB;
   - it carries another schema;
   - its id is not `x_` and 20 base32 characters, or does not match its file
     name;
   - its scope names no person or source in `household.json`;
   - it names a single source that is not set up for refresh.

   A request names what to refresh, never a command, a path, a profile or
   an argument. Unknown fields are ignored.
2. **The pending requests a pass finds are served together.** Each source
   is imported at most once, and every request that covers it gets that
   attempt. While it runs, `status.json` names them in `last_attempt.requests`.
3. **A request does not bypass a source's limits.** A source imported less
   than `min_interval_secs` ago answers `skipped_interval`. A paused source
   answers `skipped_paused`. One that needs a person answers `needs_person`
   without being tried.
4. **The outcome goes to `done/`.** Once every source of a request is done,
   medrec writes `done/<same name>`, holding the request as received plus
   `handled`, and then deletes the pending file. A pending file means the
   request is not finished. One left pending by a pass that was cut short is
   taken again by the next pass, and the interval keeps that from importing
   twice.
5. **One refresh runs at a time.** A pass that finds a refresh running ends
   at once.
6. **Old outcomes go.** medrec deletes a `done/` file 90 days after its
   `finished_at`.

```json
"handled": {
  "picked_up_at": "2026-11-22T20:04:01Z",
  "finished_at": "2026-11-22T20:04:31Z",
  "outcome": "ok",
  "sources": [
    {"source": "s_aaaaaaaaaaaaaaaaaaaa", "outcome": "ok", "attempt": "t_aaaaaaaaaaaaaaaaaaaa", "next_allowed_at": "2026-11-23T20:04:31Z"}
  ],
  "message": "1 source imported",
  "by": "medrec 0.3.0"
}
```

| Field | Meaning |
|---|---|
| `outcome` | Counts only the sources that were imported. `ok`: every one was `ok`. `partial`: some were `ok` and some `failed` or `needs_person`. `failed`: none was `ok`. `skipped`: none was imported, each being skipped. `invalid`: refused by rule 1, with `sources` empty and `message` saying why. |
| `sources[]` | Each source the request covered: its outcome (an attempt outcome), the attempt that served it (`null` for a skip found without one), and `next_allowed_at`, the earliest time it may import again. |
| `message` | One line for a person, with no record content. |
| `by` | The medrec that handled it. |

## Import input: `records-stream/v1`

`medrec import --from <cli> [--from-profile <name> | --from-files <dir>] --patient <id> [--org <org>] [--since <date>]`
runs `<cli> records export --patient <id> --ndjson [--org …] [--since …]
[--profile <name> | --files <dir>]`. A provider that signs in lists `login-profiles` in its
`info --json` `capabilities` (`cli-info/v1`); its export then needs
`--from-profile`, passed as `--profile`, and without it medrec is exit 2
listing the provider's profiles from `<cli> profiles list --json`
(`profile-list/v1`). A provider that signs in without listing it predates
login profiles and is refused (`providers list` shows it `outdated`); one
whose info has `auth.required: false` takes no profile. A provider that
also reads downloaded files lists `records-files` (medical-common DESIGN
§1.1); `--from-files <dir>` then runs its export with `--files <dir>` (the
directory made absolute) and no `--profile`, with `<PREFIX>_PROFILE`
removed from its environment, as `--from-profile` removes `<PREFIX>_FILES`.
`--from-files` for a provider without the capability, or for a path that is
not a directory, is exit 2 before the export runs. From files the
provider lists no patient labels, so `<id>` is the `PatientContext.id` of
`<cli> patients list --files <dir>`; a provider with both sources gives a
patient the same `organization` and `patient.id` on each, so both file
under one source. The provider's
profile name is never written into the records folder: a source is the CLI,
the organization and the patient, whichever login read it.
`<id>` is whatever the provider's `--patient` takes: the patient's FHIR id,
or a label the provider gave the patient. The first import of a patient
names whose records they are with `--person`. A later import without it
files under the same person when `<id>` matches a source's `patient_id`, or
otherwise its `patient_label` (the stream header's `patient.label`), from
the same CLI and `--org`. It is refused when that matches more than one
person, or when the stream's `patient.id` is not one that source recorded.
When `--org` matches no organization this patient was filed under from that
CLI, but they were filed under another (a name the provider also accepts
for its organization, such as a short alias), the person is found after the
export by the stream header's `organization.id` and `patient.id`, which a
source is filed under; nothing is written when no source or more than one
person matches. A patient never imported from that CLI is refused before
the export runs.
A provider is any CLI whose `info --json` lists the `health-records/v1`
profile. Its standard output is NDJSON:

1. A header: `{"schema": "records-stream/v1", "source": {cli, version},
   "organization": {id, name, fhir_base, vendor}, "patient": {id,
   organization, label, relationship}, "since", "pulled_at", "fhir_version",
   "method"}`.
2. One FHIR resource per line, R4, or STU3 where the header's
   `fhir_version` says so; medrec reads both shapes (0.2.8 and later: a
   status as a plain code, `assertedDate`, `Immunization.date`, `notGiven`
   and `practitioner[].actor` (the performer), `requester.agent`, a single
   `DiagnosticReport.category`), and
   an empty string as an absent value. A `Binary` follows the resource that names
   it: a `DocumentReference` (`content[].attachment.url`), a
   `DiagnosticReport` (`presentedForm[].url`) or a `Media` (`content.url`).
   medrec files the Binary as a document with that resource's date, title
   and type, preferring a DocumentReference that names it too. A
   DiagnosticReport's `LAB` category makes a `lab_report`, an imaging one
   (`RAD`, `CT`, `MR`, `US`, …) an `imaging_report`; a Media's Binary is
   `other`. The document's provenance names that resource. A Binary with
   the bytes of a document already filed, or of a rendering already noted
   on one, is a duplicate (`documents_duplicate`). One with other bytes is that document rendered
   again (`documents_unchanged`; nothing is filed, and the rendering's
   `sha256`, `size` and `fetched_at` go on the kept document's
   `renderings`) when the latest document filed from the same CLI and
   organization, through the same resource and Binary (`raw_path`), in the
   same media type, has exactly its text, whitespace aside, and no
   `meta.versionId` that differs where both imports have one (a
   `lastUpdated` alone does not make another version; 0.2.7 and later). A portal that generates a summary on each export changes only
   markup a text reading leaves out (its timestamp and ids). Any other
   difference, one digit included, files a new document, as does a
   document with no text.
   `medrec documents merge <copy> --into <id>` folds a copy filed before
   that rule (0.2.3) into the document it renders, by the same rule.

   Each resource served is filed over the version held at its raw path by
   medical-common's rule (`pk_med_records::version::decide`, DESIGN §1.4;
   0.2.7 and later). A version's content is the resource without
   `meta.lastUpdated`. Its time is its `meta.lastUpdated`, or for a FHIR API
   read (`method` `fhir-api`) the server left undated, when the source was
   read (`pulled_at`); otherwise it is unknown. A time is later only when
   it is a later instant, or when the held one is unknown.

   | Content | Later | Filing | The import |
   |---|---|---|---|
   | same | no | unchanged | changes nothing |
   | same | yes | refreshed | overwrites the raw file in place, and moves the record's provenance on (`refreshed_by`: the incoming `source_resource` and `fetched_at`, the held `filed_at`, `raw_path` and `classified_by`); no version is added |
   | other | yes | replaces | writes it as current, keeps the held one beside it, and writes its record |
   | other | no | older | keeps it beside the current one as `<id>~<md5>.json`; its record stays as filed, and the import counts it `records_older` |

   So a site read again that stamps each resource with its read time adds no
   file, and a version dated between two reads is older than the second. A
   raw file no record was filed from (a Binary, a Practitioner) has no time
   in the index, so other content replaces it; a Binary keeps the rendering
   rule above. A record is normalized again from each version filed, so a
   later medrec's reading applies on the next import. `lastUpdated`
   measures when content was current, not how complete it is. The raw
   layer keeps every version either way.
3. A trailer: `{"schema": "records-stream-end/v1", "complete", "counts":
   {<type>: n}, "skipped": [{resource_type, id, search, reason, code}],
   "finished_at", "error"}`.

medrec writes nothing unless the trailer is present, says `complete: true`,
its counts match the resources read, and the provider exits 0. The provider's
exit code maps to medrec's: 3 becomes 3 (a sign-in is needed; medrec names
the provider's own `auth login` command when its error's remedy gives one,
after an em dash, and otherwise `<cli> auth login [--org <org>]`), 4 becomes 4 (not found), 2 becomes 2 (the
provider refused the arguments, most of which are the caller's: an unknown
profile, patient or organization; its message is shown), and 5 or anything
else becomes 5 (upstream).

A skip's `code` is from an open set; medrec keeps every skip in the
ImportRun. A `forbidden` skip with a `resource_type` and neither `id` nor
`search` says the source withheld some resources of that type (a search
answered with an issue saying it left results out); the ones it served are
in the stream. `medrec import` lists those types as `withheld_types`, and
by the source's reason line as `withheld` (`[{reason, resource_types}]`);
its human output gives each reason with its types, says their records may
be incomplete, and counts only the other skips. Both fields drop control
characters and the invisible format characters that reorder or hide text
(bidi overrides, zero-width marks).

`medrec import --json` prints `import/v1`: `import` (the ImportRun's id),
`person`, `person_name`, `cli`, `cli_version`, `organization`,
`patient_id`, `source_dir`, `resources` and `counts` (by type), `skipped`,
`withheld_types`, `withheld`, `stream_bytes`, and these counts:

| Count | What it counts |
|---|---|
| `resources_written` | raw files uploaded: new, replacing, older (beside the current one) or refreshed in place |
| `resources_unchanged` | raw files left as they were |
| `records_added` | new records |
| `records_updated` | records whose content or version changed |
| `records_unchanged` | records the same as filed, a refresh included |
| `records_older` | versions not later than the one filed, which stayed (0.2.7 and later) |
| `documents_added` | documents filed |
| `documents_duplicate` | Binaries with the bytes of a document or rendering already filed |
| `documents_unchanged` | renderings of a document already filed |
| `documents_same_statement` | bills filed that are the same statement as a bill filed before (0.2.9 and later) |

A field added to `import/v1` is additive: a script reads a missing one as
absent, never as an error.

The patient's birth date in the stream must match the person's in
`household.json`, unless `--allow-birth-date-mismatch`. A source's first import needs
`--person`; later imports find the person by the source's patient id or label,
as described above.
