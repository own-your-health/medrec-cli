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
<Person>/
  notes.md                    dated notes; this file is their source
  index.json                  the person's index (documents, records, studies, imports)
  text.json                   extracted text per document
  Documents/<YYYY>/<YYYY-MM-DD>_<type>_<org>_<details>_<original name>
  Claims/<YYYY>/              EOBs and bills, named the same way
  Imaging/<YYYY-MM-DD>_<modality>_<body part>_<facility>/
    S<series:03>/<instance:05>.dcm   DICOM files exactly as received
    study.json                       the study's summary (medrec-study/v1)
  Records/<source>/<ResourceType>/<id>.json   FHIR R4 resources, verbatim
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
  `id-<first 32 hex of the id's SHA-256>`, the same on every import.
- A raw resource that changed at its source is written as the new
  `<id>.json`. The previous version stays beside it as
  `<id>~<first 8 hex of its MD5>.json`.

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
   the folder's is refused, and the command is run again. Import and
   `images intake` check the `rev` before uploading, too, and save each
   study's index entry as soon as it is filed.
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
`classification` (what was detected, with scores and evidence), and
`provenance`. An imported document's `provenance.raw_path` names its
Binary's raw file, which holds the latest rendering the source served;
earlier ones stay beside it as `<id>~<md5>.json`, as for every raw
resource, and the document's own bytes are the filed file.

Document types: `visit_summary`, `lab_report`, `imaging_report`,
`immunization_record`, `discharge_summary`, `referral`, `prescription`,
`letter`, `bill`, `eob`, `insurance_card`, `other`.

**Record**, one normalized clinical fact: `id`, `kind` (`encounter`,
`result`, `report`, `condition`, `medication`, `allergy`, `immunization`,
`procedure`, `coverage`, `claim`), `key` (kind, code and date, so the same
fact from two sources is one record), `date`, `code` (`{system, code,
display}`), `display`, `status`, `fields` (kind-specific, with FHIR element
names), `documents` (linked document ids) and `provenance` (one per source).

**ImagingStudy**: `id`, `study_uid`, `path`, `date`, `modalities`,
`body_part`, `description`, `facility`, `accession`, `referring`, `series`
(`[{uid, number, modality, description, instances: {SOPInstanceUID: file}}]`),
`instances`, `size`, `report` (a document or record id), `files` (each
filed file's MD5 by its path in the study folder, for `verify`), `filed_at`,
`provenance`.

**ImportRun**, one `medrec import`: `id`, `source_dir`, `stream_sha256`,
`stream_bytes`, `cli`, `cli_version`, `organization`, `patient_id`,
`pulled_at`, `imported_at`, `counts`, `skipped`, and the added, updated,
unchanged and written counts.

**Provenance** (the `provenance/v1` fields of medical-common's
`pk-med-records`): `source` (`{kind, cli, cli_version, organization_id,
fhir_base}`), `method` (`fhir-api`, `portal`, `scan`, `file`, `media`,
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
2. One FHIR R4 resource per line. A `Binary` follows the resource that names
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
   `meta.versionId` or `lastUpdated` that differs where both imports have
   one. A portal that generates a summary on each export changes only
   markup a text reading leaves out (its timestamp and ids). Any other
   difference, one digit included, files a new document, as does a
   document with no text.
   `medrec documents merge <copy> --into <id>` folds a copy filed before
   that rule (0.2.3) into the document it renders, by the same rule.
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

The patient's birth date in the stream must match the person's in
`household.json`, unless `--allow-birth-date-mismatch`. A source's first import needs
`--person`; later imports find the person by the source's patient id or label,
as described above.
