# Data contract: the records folder

The records folder is one directory tree, normally a private Google Drive
folder reached through medrec's own Drive sign-in
(`gdrive:<drive profile>/<folder id>`), or a local directory. Under the
`drive.file` scope, medrec sees only the files in it that a client of the
household's Cloud project created (README, "Google Drive"). medrec reads and
writes it only through this contract.
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
views/                        each person's records cut into small files, for readers (0.8.0 and later)
<Person>/
  notes.md                    dated notes; this file is their source
  index.json                  the person's index (documents, records, studies, imports)
  text.json                   extracted text per document
  overrides.json              the household's decisions on events (0.9.0 and later)
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
`r` record, `m` study, `i` import, `n` note, `q` review item, `f` folder, `t` refresh
attempt, `x` refresh request, `h` refreshing machine), `_`, and 20 random base-32
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
`supportingInfo`. Each of its `items` has a `quantity`.

A `claim` (an ExplanationOfBenefit) is shaped for costs (0.5.0 and later).
An earlier import's claims take this shape when their source serves them
again, or all at once with `medrec reindex`, which shapes every claim again
from the raw file filed for it, reading nothing but the folder. A claim
with no `amounts` and no `category` was filed by an earlier medrec. Every
machine that imports into one folder must run 0.5.0 or later: an earlier
medrec files the claims it imports again in the earlier shape. The line
fields `counted`, `payer_notes` and `revenue` and the claim's
`payer_notes` come with 0.8.2: run `medrec reindex` once after upgrading
to it, and run 0.8.2 or later on every machine that imports into the
folder, since an earlier medrec files claims again without them. A claim
filed by an earlier medrec has no line marked `counted: false` even where
its totals leave one out.

- `display`: what it paid for and who. That is the service names of the
  lines its totals count (`A`, `A and B`, `A, B and 2 more`), a pharmacy
  claim's drug (RxNorm, else NDC), or failing those its category's label
  (`Lab work`), then ` · ` and its `provider`. The FHIR claim type stays in `type`
  (`professional`, `institutional`, `pharmacy`, `oral`).
- `category`: the one category of the lines its totals count, `mixed` when
  they have several, `pharmacy` for a pharmacy claim.
- `amounts`: the claim's, by the names below. A name its `total` gives
  comes from there: the totals are the claim's figures, whatever its lines
  say. Otherwise it is the sum of the lines the totals count that give it,
  and `plan_paid` failing both is its `payment`.
- `benefit_period` (`{start, end}`): `benefitPeriod` as the payer wrote
  it, the plan year the claim counts toward.
- `billable_period`, `provider`, `insurer`, `facility`, `outcome`, `use`,
  `total`, `payment` and `benefit_balance` (deductible and out-of-pocket
  used and allowed) as the resource gives them.
- `diagnoses`: `[{sequence, code}]` from `diagnosis`, `code` a
  `{system, code, display}` with ICD-10 preferred; an entry without a code
  is left out.
- `items`, one per line: `sequence`, `category`, `amounts`, `service`
  (`{system, code, display}`), `revenue` (a facility line's revenue code,
  the same shape; 0.8.2 and later), `serviced` (a date or `{start, end}`),
  `quantity`, `net`, `adjudication` (`[{category, amount, reason}]` as the
  payer wrote them), `payer_notes`, `counted` and `records`. `revenue`,
  `payer_notes` and `counted` are present only when they say something.
- A line's `payer_notes` (0.8.2 and later): the texts of the
  `processNote` entries its `noteNumber` points at, in that order, as the
  payer wrote them (UHC's start with their code: `XX -- …`). The claim's
  own `payer_notes` are the `processNote` texts no line points at. They
  are the payer's, never the household's notes in `notes.md`.
- `counted: false` (0.8.2 and later) marks a line the claim's totals
  leave out. A payer can list a line it then does not count: UHC lists
  the billed code it replaced, at the member's whole cost, beside its
  replacement, and lines it wrote off. The same amounts occur on lines
  that are counted, so medrec tells them apart by the totals alone. It
  compares the lines' sums of `billed`, `plan_paid` and `you_owe` with
  the totals, for those the totals and some line give. When at least two
  are compared and a sum differs, the lines left out are the one smallest
  set of at most 3 lines, never all of them, whose removal makes every
  compared sum agree. When no such set exists, or two do, or fewer than
  two amounts can be compared, every line stays counted, and the claim's
  totals and its lines simply disagree. A reader that sums or shows line
  amounts reads them with `counted`: it sums only counted lines, and shows
  a line left out as not owed. `allowed`, `discount`,
  `not_covered` and the cost-share parts are not compared, since payers
  break them down differently on a line and on the claim.
- links, below: `visit`, `results` and each line's `records`.

A field that is missing is unknown, never zero.

A line's category is the first row whose rule holds for its codes. Ranges
are inclusive; descriptors come from the source.

| `category` | Rule |
|---|---|
| `pharmacy` | a pharmacy claim, or a line coded NDC or RxNorm |
| `visit` | CPT 99202–99499 |
| `lab` | CPT 80047–89398 and 36415–36416; CPT PLA and MAAA codes (`0001U`, `0002M`) |
| `imaging` | CPT 70010–79999 |
| `vaccine` | CPT 90460–90756 |
| `medicine` | other CPT 90281–99199 and 99500–99607 |
| `procedure` | CPT 10004–69990 |
| `anesthesia` | CPT 00100–01999 |
| `drug` | HCPCS J-codes |
| `supply` | HCPCS A-, E- and L-codes |
| `dental` | CDT D-codes |
| `facility` | a line with a revenue code and no CPT, HCPCS or CDT code |
| `other` | anything else, CPT Category II and III codes and other HCPCS letters included |

Amounts are `{amount, currency}`, `amount` a decimal string. `you_owe` is
only ever the payer's own figure for what the member owes; medrec never
fills it in. A reader that needs the member's share when it is missing
adds `deductible`, `copay`, `coinsurance` and `not_covered`, of those
given, and never `discount`. medrec's own readers do (`search`, `records`
and the MCP tools), labelling the sum "your share" and the payer's
figure "you owe". A line's come
from its `adjudication` entries, a claim's from its `total` entries. Each
entry's category maps to a name by its code, and only failing that by the
payer's words. Of one name's entries, those of the best rank count, summed,
so a payer that sends both `benefit` and `paidtoprovider` is not counted
twice. Two currencies under one name leave it out.

| Name | Codes (CARIN Blue Button, HL7), rank 0 unless noted | Words, rank 2 |
|---|---|---|
| `billed` | `submitted` | submitted, billed |
| `allowed` | `eligible` | allowed, eligible |
| `plan_paid` | `benefit`; `paidtoprovider` and `paidtopatient` at rank 1 | plan paid, paid by plan, plan pays |
| `deductible` | `deductible` | deductible |
| `copay` | `copay` | copay |
| `coinsurance` | `coinsurance` | coinsurance |
| `not_covered` | `noncovered` | not covered, non covered |
| `discount` | `discount` | discount |
| `you_owe` | `memberliability` | member liability, patient responsibility, you owe, your share |

A claim's links are guesses by date and name, recomputed on every write of
the person's index, so a visit imported after its claim is linked then:

- `visit`: the record id of the one `encounter` dated on one of the
  claim's dates of service, leaving out encounters whose `status` is
  `planned`, `cancelled` or `entered-in-error` (each line's `serviced`, else the claim's date
  for a line with none, or for a claim with no lines). Of several, it is
  the one that shares a word of four or more letters between its
  `service_provider`, `location` or `practitioner` and the claim's
  `provider` or `facility`. When that leaves no single
  encounter, there is no `visit`.
- `results`: the ids of the `result` and `report` records dated on a `lab`
  line's date of service, leaving out those `cancelled` or
  `entered-in-error`.
- a line's `records`: the claim's `visit` for a `visit` line, its date's
  results and reports for a `lab` line.

An import compares a claim without its links, so re-importing one whose
links have changed counts it unchanged.

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

## overrides.json (`medrec-overrides/v1`, 0.9.0 and later)

The household's decisions on a person's events (`events.json`, below),
which outrank the matching rules. It is absent until the first decision.

```json
{"schema": "medrec-overrides/v1", "person": "p_…",
 "events": [
   {"id": "o_…", "kind": "apart", "members": ["r_…", "d_…"],
    "at": "2026-11-24T12:00:00Z", "by": "Alex Testperson", "request": "x_…"}
 ]}
```

- `kind` is `apart` ("Not the same visit") or `together` ("Attach to this
  visit"). `members` names two of the person's records or documents by
  id, never an event id, so a decision survives every rebuild.
- `apart`: the two are never in one event. The one that is not the event's
  anchor leaves it. A claim or an encounter becomes an event of its own,
  and a paper or another record joins none.
- `together`: the second joins the event holding the first.
- Decisions apply in order, after every rule. One naming a member the index
  no longer holds is skipped. The same two members have at most one
  decision: the other kind replaces it, and the same kind again changes
  nothing.
- `by` is who asked, as shown (a request's `requested_by`), and `request`
  the request it came from; both are `null` from a terminal. Neither is
  trusted.
- medrec writes it (`medrec events apart|together|undo`, and event requests
  in `_requests/`, below). Readers read it only to judge `events.json`'s
  freshness.

## _status/ (`medrec-status/v1`, `medrec-attempt/v1`)

What `medrec refresh` (0.3.0 and later) has done: for each source it
refreshes, the latest attempt, the latest success and the sign-in, and every
attempt. One machine refreshes a folder (`refresher.machine`, below), and only
it writes here; every other reader, the own-your-health app among them, only
reads. `medrec refresh --install` creates the folder. These are state, not
filed bytes (Rules, above): medrec rewrites them, under refresh's own lock
rather than the folder's, and they carry no `rev`.

```text
_status/
  status.json                 each refreshed source now
  attempts/<YYYY-MM>.ndjson   every attempt that month, one per line
```

A reader treats an enum value it does not know as unknown, not as an error,
and a field or count that is missing as unknown, not as zero or false.

### status.json

```json
{
  "schema": "medrec-status/v1",
  "format": 1,
  "updated_at": "2026-11-22T20:05:00Z",
  "refresher": {
    "machine": "h_aaaaaaaaaaaaaaaaaaaa",
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
      "failures": 0,
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
          "documents_added": 0, "documents_duplicate": 0, "documents_unchanged": 3,
          "documents_same_statement": 0
        },
        "withheld_types": []
      }
    }
  ]
}
```

| Field | Meaning |
|---|---|
| `format` | `1`. A reader that finds a larger number stops and says to update, as for `household.json`. Within a format, fields and enum values are only added. |
| `updated_at` | When medrec last wrote the file. |
| `refresher.machine` | The machine that refreshes this folder: a random id it keeps in its own cache, `h_` and 20 base32 characters. A pass on another machine, and `medrec refresh --install` there, refuse while this one's `last_pass_at` is within the offline window (next row), and say to run `medrec refresh --install --take-over` on the new machine. Once that machine has written its own id, the old one's passes refuse in turn. Every write to `status.json` or `attempts/` first reads `status.json` again, and a pass whose machine is no longer the one named there stops without writing. `--take-over` writes its id, waits one `pass_every_secs`, and serves requests only once its id is still in place. |
| `refresher.last_pass_at` | The latest pass of `medrec refresh`. A pass that runs an attempt or handles a request writes it; otherwise it is written at least every `heartbeat_every_secs`. A reader takes the refreshing machine as offline when it is older than `heartbeat_every_secs + pass_every_secs`. |
| `refresher.next_pass_at` | When the next pass is due. |
| `refresher.pass_every_secs` | How often a pass looks at `_requests/`. |
| `sources[]` | Each source set up for refresh (`medrec refresh add`). A source in `household.json` that is not here is not refreshed; its last load is its latest `ImportRun`. |
| `source`, `person`, `cli`, `organization` | The source's id in `household.json`, its person, its provider CLI and its organization. |
| `auto` | `true`: imported on its schedule. `false`: imported only when a request or a terminal `medrec refresh` asks. |
| `paused` | `null`, or `{"reason": "consent" \| "user", "note": "…", "since": "…"}`. Nothing imports a paused source, neither the schedule, a request nor a terminal `medrec refresh`, until `medrec refresh resume <source>`. A source whose provider terms wait on the household's consent is paused with `consent`. |
| `min_interval_secs` | The shortest time between two of its scheduled imports: never shorter than its CLI documents, and at least a day (`86400`) for a member site. It is measured from the source's latest `ImportRun` in the person's `index.json` (its `imported_at`), which the import writes under the folder's lock and `rev` check, so an import is counted even when the pass that ran it stopped before updating this file. A person's request is held to its own guard instead (`_requests/`, rule 3). |
| `failures` | `failed` attempts in a row since the last `ok`. After one, the schedule waits before it tries the source again: 30 minutes after the first failure, doubling with each one after, never longer than `min_interval_secs`, measured from the failed attempt's `started_at`. So a provider that keeps failing is contacted no more often than its interval allows. A person's request waits only its guard from the failed attempt (`_requests/`, rule 3). |
| `next_run_at` | The next scheduled import, after any wait that follows a failure (`failures`); `null` when `auto` is `false`, the source is paused, or it needs a person. |
| `sign_in.state` | medrec's reading of the provider's `auth status --json` (`auth-status/v1`). `not_needed`: the provider signs in to nothing (`required: false`). `signed_out`: `authenticated` is false. `expired`: `session_valid` is false, or `expires_at` has passed. `expiring`: `expires_at` is within 72 hours. `ok`: signed in otherwise. `unknown`: not read yet, or the read failed or timed out. |
| `sign_in.expires_at` | The sign-in's end, where auth status gives one; else `null`. |
| `sign_in.checked_at` | When auth status was last read. |
| `sign_in.renews` | `true`: the provider CLI keeps its sign-in going unattended (a refresh token, a portal key). `false`: a member-site session that ends within hours and nothing renews, so the source loads only soon after someone signs in on the refreshing machine, and a `sign_in` `needs_person` is its normal state between sign-ins. Missing: unknown. It comes from the provider's auth status when that gives `renews` (0.7.0 and later read it; none does yet), else from medrec's table: `false` for `uhc`, `fertemr`, `caremark`, `oneblood`, `flywire` and `peryourhealth`; `true` for `mychart`, `athena`, `kp`, `modmed` and `icanotes`. |
| `sign_in.lasts_secs` | About how long a fresh sign-in lasts, where measured: from the provider's auth status when it gives `lasts_secs`, else medrec's table (`uhc` 2400). Missing: unknown. |
| `sign_in.needs_person` | `null` unless the source cannot be refreshed without a person: `{"reason": "sign_in" \| "keychain", "since": "…", "command": "medrec refresh sign-in <source>"}`. `sign_in`: the state is `signed_out` or `expired`, or the import itself answered that the provider needs a sign-in (exit 3). `keychain`: a scheduled pass's auth status read timed out waiting on an approval nobody gave, as a rebuilt provider CLI's first Keychain read does; the state is then `unknown`. medrec's own Drive sign-in never appears here: a pass that cannot read it without an approval cannot reach the folder either, so it writes nothing, and `refresher.last_pass_at` goes stale. `command` is what to run in a terminal on the refreshing machine. Until it has run, neither the schedule nor a request tries the source again. An `expiring` sign-in is still refreshed. |
| `last_attempt` | The latest attempt: an `attempts/` line without `source`, `person`, `cli`, `organization` and `counts`. While it runs, `outcome` is `running` and `finished_at` is `null`. A pass that finds `running` left by an earlier pass (no refresh of the folder runs on this machine) records that attempt as `failed` with the error code `interrupted`, counts it in `failures`, and waits from its `started_at`, before it considers the source due. |
| `last_success` | The latest attempt whose outcome was `ok`: its attempt, when it finished, its `ImportRun`, and the import report's counts and `withheld_types`. |

### attempts/<YYYY-MM>.ndjson (`medrec-attempt/v1`)

One JSON object per line, in the order the attempts started, in the file of
the month (UTC) the attempt started.

```json
{"schema":"medrec-attempt/v1","attempt":"t_aaaaaaaaaaaaaaaaaaaa","source":"s_aaaaaaaaaaaaaaaaaaaa","person":"p_aaaaaaaaaaaaaaaaaaaa","cli":"mychart","organization":"example-health","started_at":"2026-11-22T20:04:02Z","finished_at":"2026-11-22T20:04:31Z","trigger":"request","requests":["x_aaaaaaaaaaaaaaaaaaaa"],"outcome":"ok","exit_code":0,"error":null,"import":"i_aaaaaaaaaaaaaaaaaaaa","counts":{"records_added":2,"records_updated":0,"records_unchanged":41}}
```

| Field | Meaning |
|---|---|
| `attempt` | Its id, `t_` and 20 base32 characters. |
| `trigger` | `schedule`; `request`, with the `requests` it served; or `manual`, a terminal `medrec refresh`. |
| `requests` | The `_requests/` ids the attempt served; empty otherwise. |
| `outcome` | `ok`, `failed`, `needs_person`, `skipped_interval` (for the schedule: imported less than `min_interval_secs` ago, or still waiting after a failure, see `failures`; for a person: within the request guard, `_requests/` rule 3) or `skipped_paused`. `running` appears only in `status.json`. |
| `exit_code` | The import's exit code (Import, below); `null` for a skip, and for a `needs_person` found before the import ran. |
| `error` | `null`, or `{"code", "message", "remedy"[, "provider_message"]}` from the failing command's `--json` error (the family's error shape). `message` is at most 300 characters. When the provider's export answered that its sign-in ended (exit 3), `code` is `needs_person`, `message` and `remedy` are medrec's own (the remedy runs `medrec refresh sign-in <source>`), and the provider's own message, which names the provider's own sign-in command, is in `provider_message` (0.7.0 and later). `provider_message` is present only then, always (an empty string when the provider gave no text), and is at most 300 characters too. Show `remedy` as the one thing to do, and `provider_message` at most as detail. A profile name in `message`, `remedy` or `provider_message` is replaced, a provider profile's by `<provider profile>` and a medrec profile's by `<profile>`, so the folder holds none. medrec's own codes are `timeout` (a call outlived its limit), `interrupted` (the pass running it stopped before it finished) and `needs_person`. |
| `import` | The `ImportRun` id in the person's `index.json`, when `ok`. |
| `counts` | When `ok`, the import report's counts: `records_added`, `records_updated`, `records_unchanged`, `records_older`, `resources_written`, `resources_unchanged`, `documents_added`, `documents_duplicate`, `documents_unchanged`, `documents_same_statement`. A count the import report gains later is added here too. |

A scheduled pass records nothing for a source that is not due. A skip is
recorded only when a request or a terminal command asked for that source.
When another writer holds the folder's lock, no attempt is recorded and the
next pass tries again.

Drive has no append, so medrec rewrites the month's file whole after each
attempt, and never loses a line doing it:
- It reads the file first. A failed read of a file that exists stops the
  write; it is never taken as empty.
- The new file holds every line the old one did, then the new line. One
  that would hold fewer lines is not written.
- The upload is read back by MD5 (Rule 2).

The month files are kept: a month is a few kilobytes.

## _requests/ (`medrec-request/v1`)

Requests to refresh, from a reader that cannot run medrec itself (the
own-your-health app). Each pass of `medrec refresh` looks for them with one
listing of `_requests/`.

```text
_requests/
  <YYYYMMDDTHHMMSSZ>_<id>.json    pending
  done/<same name>                handled; kept 90 days, or while its pending file stays
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
  folder (SECURITY.md has what Drive's permissions can and cannot hold it
  to).
- `scope` is `{"all": true}`, `{"person": "p_…"}` or `{"source": "s_…"}`.
  `all` and `person` cover the sources set up for refresh (`status.json`
  `sources`).
- `requested_by` and `app` are shown, never trusted.

How medrec handles one:

1. **A request is untrusted input.** Anyone who can write to the folder can
   write one, so it can ask only for an import that the refreshing machine
   already has set up. It is handled at once as `invalid` if any of these
   holds:
   - it is larger than 4 KiB, or is not a JSON object;
   - it carries another schema;
   - its id is not `x_` and 20 base32 characters, or does not match its file
     name;
   - its scope names no person or source in `household.json`;
   - it names a single source that is not set up for refresh.

   A request names what to refresh, never a command, a path, a profile or
   an argument. Unknown fields are ignored. A file in `_requests/` whose
   name is not `<YYYYMMDDTHHMMSSZ>_<id>.json` is handled as `invalid` too,
   under `done/invalid_<first 16 hex of its name's SHA-256>.json`. Folders
   other than `done/` are left alone.
2. **The pending requests a pass finds are served together.** Each source
   is imported at most once, and every request that covers it gets that
   attempt. While it runs, `status.json` names them in `last_attempt.requests`.
3. **A request has its own guard, not the schedule's floor** (0.7.0 and
   later; before, a request waited `min_interval_secs` too). A person
   asking imports a source at most once per 15 minutes: one imported,
   attempted or failed less than 15 minutes ago answers `skipped_interval`,
   with `next_allowed_at` 15 minutes after the latest of those. A
   terminal `medrec refresh` is held to the same guard. A paused source
   answers `skipped_paused`. One that needs a person answers
   `needs_person` without being tried.
4. **The outcome goes to `done/`.** Once every source of a request is done,
   medrec writes `done/<same name>`, and then deletes the pending file. A
   pending file means the request is not finished, unless its `done/` file
   exists: a request written by another Google account than the one medrec
   runs as may be a file medrec cannot remove. medrec then leaves it, never
   serves it again, keeps its `done/` file for as long as it stays, and
   tries to remove it on later passes (at least hourly); the app may delete
   it once it has read `done/`. A request left pending
   by a pass that was cut short is answered by the next pass. A source
   imported since the request's `requested_at` answers from that
   `ImportRun`: `ok`, with `attempt` the attempt that recorded it (`null`
   when none did) and `next_allowed_at` 15 minutes after its
   `imported_at`. The guard keeps it from importing twice.
5. **One refresh runs at a time** on the refreshing machine. A pass that
   finds a refresh running ends at once.
6. **Old outcomes go.** medrec deletes a `done/` file 90 days after its
   `finished_at`, unless its request file is still pending (rule 4).

A `done/` file is written by medrec, not copied. It holds the request's
`schema`, `id`, `requested_at`, `scope`, `requested_by` and `app`,
re-serialised, with each string in `requested_by` and `app` cut to 200
characters, then `handled`. Nothing else of the request is kept. For an
`invalid` request it holds only `schema`, `id` (from the file name, when the
name has one; else `null`), `received` (`{"bytes": n, "sha256": "…"}` of the
file as found) and `handled`. A `done/` file is at most 8 KiB.

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
| `outcome` | From the sources' outcomes. `ok`: at least one `ok`, none `failed` or `needs_person`. `partial`: at least one `ok`, and at least one `failed` or `needs_person`. `needs_person`: none `ok` or `failed`, at least one `needs_person`. `failed`: none `ok`, at least one `failed`. `skipped`: every source skipped. `invalid`: refused by rule 1, with `sources` empty and `message` saying why. |
| `sources[]` | Each source the request covered: its outcome (an attempt outcome), the attempt that served it (`null` for a skip found without one), and `next_allowed_at`, the earliest time a request may import it again (rule 3). |
| `message` | One line for a person, with no record content. |
| `by` | The medrec that handled it. |

### Event requests (0.9.0 and later)

The app's "Not the same visit" and "Attach to this visit" are requests too,
with `event` in place of `scope`:

```json
{"schema": "medrec-request/v1", "id": "x_…", "requested_at": "…",
 "requested_by": {…}, "app": {…},
 "event": {"person": "p_…", "apart": ["r_…", "d_…"]}}
```

- `event` holds `person` and exactly one of `apart` or `together`, each two
  different record (`r_…`) or document (`d_…`) ids. A request with both
  `scope` and `event`, or with neither, is `invalid` (rule 1), as is one
  whose `person` is not in `household.json`.
- The next pass writes the decision to the person's `overrides.json`, and
  the views follow before it releases the lock, once for all of a pass's
  decisions. It imports nothing, so no guard or interval applies. While
  another writer holds the folder, the request stays pending, with no
  `done/` file, for the next pass.
- Its `done/` file keeps `event` where a refresh's keeps `scope`.
  `handled.outcome` is `ok` once recorded (a decision already recorded is
  `ok` too), `invalid` when a member is not one of that person's records or
  documents, and `failed` when the folder could not be written. `sources`
  is empty.

## views/ (`medrec-views/v2`, 0.8.0 and later)

The views hold each person's records again, cut into small files in the
order a reader lists them. A reader that cannot parse a whole `index.json`
per request opens only the few kilobytes its answer needs. The hosted
connector is one: it runs on Cloudflare's Workers Free plan, with 10 ms of
CPU per request, and parsing a 2.5 MB index alone takes about 6 ms.

Every view is derived from `household.json` and the person's `index.json`
and `text.json`. `events.json` also reads the person's `overrides.json`, and
the `index.json` and `text.json` of the people linked to them (Events,
below). `notes.md` has no view. It is small, the app may change
it, and readers read it as it is. Nothing in `views/` is a source: deleting
it loses nothing, and `medrec views write` makes it again.

```text
views/views.json                         what was written, and from which files
views/<person id>/summary.json
views/<person id>/records-<kind>-<n>.json
views/<person id>/records-<kind>-undated.json
views/<person id>/names-<kind>.json
views/<person id>/tests.json
views/<person id>/timeline-<n>.json
views/<person id>/timeline-undated.json
views/<person id>/documents.json
views/<person id>/text-<n>.bin
views/<person id>/search-<hex>.json
views/<person id>/search-1-<hex>.json
views/<person id>/search-items.bin
views/<person id>/search-hay-<n>.bin
views/<person id>/search-labs-<n>.json
views/<person id>/events.json            0.9.0 and later
```

A person's directory is named by their id in `household.json`, not their
name. Each view is stored under its name with the first 12 hex of its MD5
before the extension: `records-result-7.json` is
`records-result-7.0a1b2c3d4e5f.json` in the folder. A file's bytes never
change under its name, so a reader holding an older `views.json` reads that
pass's views, or finds them gone, and never another pass's bytes.
`views.json` is the one file with a fixed name. This document names views
without the MD5. JSON files are compact UTF-8 without a byte-order mark. Binary
integers are little-endian unsigned 32-bit. Strings compare as UTF-8 bytes.
"Newest first" sorts by the date string, descending, with undated items
last.

### When they are written

- **After every write.** A command that writes `household.json`, an
  `index.json` or a `text.json` brings the views up to date before it
  releases the folder lock. A refresh pass, and an import of several
  members, does it once, after the last filing. `medrec views write` does
  it on demand, and `--all` makes every person's again.
- **Only what changed.** A person keeps their views when their `index.json`,
  `text.json` and `overrides.json` and their linked people's `index.json`
  and `text.json` have the MD5s `views.json` recorded, under the same `household.json` and
  medrec version. Otherwise their views are made
  again, and only views the folder does not already hold under their
  stored names are uploaded. An unchanged view keeps its name and is not
  written again.
- **`views.json` last.** It records the MD5 of every view, so a reader
  checks each view it reads and never mixes two passes, whatever order an
  uploader sends them in. Views that it no longer names, a person's who
  left `household.json` among them, are deleted after it; a left person's
  directory may stay, empty, since readers list only the directories of
  the people `household.json` names. On Drive, a deleted view goes to the
  account's trash, like every file medrec removes there, until the trash
  is emptied or 30 days pass. medrec deletes
  only files of a stored view's shape (`<name>.<12 hex>.json` or `.bin`) in
  the directory of a person the household or the last `views.json` names,
  and nothing else under `views/`.
- **`views` is medrec's.** No person folder is named `views` (in any case):
  a person named that way gets `Views (person)`. A folder whose person
  folder is already `views` gets no views, and `init` refuses a folder
  that already holds one, even empty, with or without `--allow-other-files`.
- **Each file whole.** On a local directory a file is written to a
  temporary name and renamed. On Drive it is one upload.
- **A failure leaves them stale**, never wrong. medrec says so on stderr,
  the command's result stands, and the next write tries again.

A reader decides freshness by MD5, as Drive reports it (`md5Checksum`):

- A person whose `index.json` or `text.json` MD5 differs from `views.json`
  has stale views.
- A different `household.json` MD5 makes every person's views stale.
- A person whose `overrides.json` MD5 differs from `overrides`, or one of
  whose linked people's `index.json` or `text.json` MD5 differs from
  `linked`, has a stale `events.json` only. A reader that does not read `events.json`
  ignores both.
- A view that is missing, or whose MD5 is not the one `files` gives,
  belongs to a pass still arriving.

### views.json

```json
{"schema":"medrec-views/v2","medrec":"0.9.0","household":"<md5>",
 "people":{"p_…":{"index":"<md5>","text":"<md5>","overrides":null,"linked":{"p_…":{"index":"<md5>","text":"<md5>"}},
                  "row":{…},"records":{…},"timeline":{…},"search":{…},
                  "files":{"summary.json":"<md5>","records-result-0.json":"<md5>",…}}}}
```

| Field | What |
|---|---|
| `medrec` | The version that wrote them. Another version makes every view again. |
| `household` | The MD5 (hex) of `household.json` as the views were made from it. |
| `people.<id>.index`, `.text` | The MD5s of the person's `index.json` and `text.json` as read; `null` when the file is absent. |
| `people.<id>.overrides` | The MD5 of the person's `overrides.json` as read; `null` when there is none (0.9.0 and later). |
| `people.<id>.linked` | For each person linked to this one (Events, below), the MD5s of their `index.json` and `text.json` as read, as `{"index", "text"}`, each `null` when the file is absent; `{}` when no one is linked (0.9.0 and later). |
| `row` | `documents`, `records`, `studies`, and `newest` and `last_import` when there are any: the person's line in `medrec_summary`'s people list, without `notes`. |
| `records` | For each record kind, `{"shards": [[newest day, oldest day, count], …], "undated": count}`: its dated shards by number, and how many records its undated file holds. |
| `timeline` | The timeline's shards, the same way. |
| `search` | `items` (how many search items) and `labs` (how many `search-labs-<n>.json`). |
| `files` | Every view in `views/<person id>/`, by name (without the MD5), with its MD5 (hex), from which its stored name follows. A search shard exists only for a character some word starts with, so one missing from `files` means no word matches. |

`schema` names the layout. A change that a reader of the current layout
would misread takes the next number (`medrec-views/v3` after this one). A
field readers may ignore does not, such as a claim line's `counted` and
`payer_notes` (0.8.2): a reader that lists records or sums a claim's
totals reads them as before, and one that sums or shows line amounts
reads them with `counted` (Claims, above). medrec moves to a new number
only in a release whose notes name the connector version that reads it. medrec has
written v2 from its first views. v1, the same layout with plain names and
one search text file, was only ever written by the connector's own `npm run
views`. medrec removes a v1 `views.json`'s views by the names in its
`files`.

### Shards: from the oldest end

Records and timeline events are cut into shards from the oldest end.
`-0.json` holds the oldest days, and the highest-numbered shard is the
newest, the only one still filling. A shard closes once it holds 48,000
bytes or more, never inside a day (the date's first 10 characters).
Undated items go in the kind's `-undated.json`, outside the numbered run;
a date range leaves them out. Inside a shard items keep the reader's
order, newest first, and a reader reads shards from the highest number
down, choosing them by their spans.

New records are nearly always the newest, so a pass that adds a few
changes the newest shard and starts the next one at most. Backfilled
history (a new source's first import) moves more.

### Records

`records-<kind>-<n>.json` is a JSON array of one kind's records (`result`,
`medication`, …), in the records tool's order: by day, newest first, then
by name (`display` clipped to 200 characters with `…`), then as `index.json`
holds them. Each keeps its `index.json` fields `id`, `kind`, `date`,
`code`, `display`, `status` and `fields` when present. `provenance` is cut
to its first entry's `source`, with only `fhir_base` and `cli`.

`names-<kind>.json` lists every distinct name the kind's records carry, as
`[code, display, code display, [shard numbers]]`. `code` and `code display`
are `code.code` and `code.display`, or `null`. Shard `-1` is the undated
file. A reader filtering by name opens only the shards a matching name is
in.

`tests.json` lists each test series (the `result` records of one code with
its system, or of one folded name when uncoded), in the order of its newest
result:

```json
[{"names":[[code, display, code display], …],
  "entries":[[date, name number, shard, place in shard, rank], …]}, …]
```

`entries` holds every result of the series, newest first. `shard` and
`place` locate it in `records-result-<shard>.json` (`-1`: the undated file).
`rank` is its place among all the person's results, newest first, then by
name.

### Timeline

`timeline-<n>.json` holds the person's documents, records and studies as
events, `{date, kind, id, title, detail, source}`, each key absent when it
has no value. They are in the records tool's timeline order: newest first
by full date, then by title clipped to 200 characters, then by kind and
full title, then documents, records and studies in their `index.json`
order. Notes are not in it; a reader adds them from `notes.md`.

### summary.json

`medrec_summary`'s summary of the person without `person` and `as_of`:
`counts`, `sources`, `medications`, `conditions`, `allergies`,
`immunizations`, `latest_results`, `recent_visits` and `bills`. Each list
is `{items, more}`, at most 25 items. It is computed as of no day, so no
bill is `past_due`; a reader works `past_due` out for the day it answers.

### documents.json and text-<n>.bin

`documents.json` is `index.json`'s `documents`, each with two fields added:

- `text_at`: where its text is, `[pack, byte offset, byte length,
  characters]`. Absent when the document has no text.
- `linked`: the names (`display`) of the first 20 records that list it in
  their `documents`, in `index.json` order. Absent when none do.

`text-<n>.bin` holds the documents' texts, UTF-8, one after another in
document order. A pack closes before a text would take it past 1,000,000
bytes; a text is never split.

### Search

The search items are what `medrec_search` looks through, notes aside, as
the same text:

- **Documents:** the title, then the organization and document type, then
  the document's text.
- **Records:** the name, then the record's fields flattened. A claim's
  fields include its lines' `payer_notes`, so the payer's words are
  searched; the household's `notes.md` is the notes set aside.
- **Studies:** the description, then the modalities, body part, facility
  and series descriptions.

**Item numbers** run in the reverse of medrec's order for hits of equal
score (newest first, undated last, then documents, records and studies in
their `index.json` order). Undated items take the lowest numbers, then
dated ones from the oldest; within one date a later `index.json` entry has
the lower number. Hits rank by score, then by item number, highest first.
A date range is one run of numbers, and a new item, nearly always the
newest, takes a new number.

- **`search-<hex>.json`:** postings by word. Each item's text is folded as
  medrec folds it (lowercase, Latin accents removed, every other character
  a word break). For each distinct word: `[item, occurrences, byte offset of
  its first occurrence in the item's text]`, repeated, by item number, as
  `{"cholesterol":[12,3,40,57,1,0,…],…}`. `<hex>` is the lowercase hex of
  the UTF-8 bytes of the word's first two characters, or of the whole word
  when it has one.
- **`search-1-<hex>.json`:** the same postings keyed by the first character
  only, for one-character terms such as the "d" in "vitamin d".
- **`search-items.bin`:** `n`, the number of items; then `n + 1` offsets
  of each item's text, counted over the `search-hay-<n>.bin` packs in
  order; then `n + 1` offsets of each item's details in the rest of the
  file; then each item's details, `[date or null, kind, id, title]` as
  JSON, one after another.
- **`search-hay-<n>.bin`:** the items' texts, UTF-8, in item order, with no
  separators, 512 items to a pack: pack `n` holds items `512n` to
  `512n + 511`, and there are ⌈items / 512⌉ packs. Item `i` starts at
  `text[i] − text[512 · ⌊i / 512⌋]` in pack `⌊i / 512⌋`. A reader fetches
  only the byte ranges its snippets need. A changed item rewrites only its
  pack, and a new newest one only the last.
- **`search-labs-<n>.json`:** for the lab queries (Readers, below), every
  `result` and `report` record and every claim with a `lab` line, as
  `[item, date, kind, id, title, detail line]`, by item number, 256 to a
  file.

### Events (`events.json`, `medrec-events/v1`, 0.9.0 and later)

What happened, as the household would say it: each visit, ER trip,
hospital stay, test, vaccine or fill is one event, however many claims,
portal records and papers it produced. Every person has an `events.json`,
with no events when nothing groups, so a missing one means views still
being prepared. It is derived on every views write by the matching rules
(medrec's `src/events.rs`; the rule names are those of the design's
matching rules), then the household's decisions (`overrides.json`).

```json
{"schema": "medrec-events/v1", "person": "p_…",
 "events": [
  {"id": "e_…", "kind": "er", "start": "2026-10-07", "end": "2026-10-08",
   "title": "ER visit", "what": "Emergency room visit", "what_from": "claims",
   "place": "Example General Hospital", "place_from": "claims",
   "clinicians": ["J SAMPLE"],
   "confidence": "sure", "status": "complete",
   "money": {"currency": "USD", "billed": "10400.00", "allowed": "1445.00",
             "discount": "8955.00", "plan_paid": "1185.00", "you_owe": "260.00",
             "share": {"copay": "200.00", "coinsurance": "60.00"}},
   "members": [
     {"record": "r_…", "kind": "claim", "title": "…", "date": "2026-10-07",
      "role": "facility", "rule": "C1", "confidence": "sure",
      "provider": "Example General Hospital",
      "billed": "9000.00", "allowed": "1100.00", "plan_paid": "900.00", "you_owe": "200.00"},
     {"document": "d_…", "kind": "bill", "title": "…", "date": "2026-10-20",
      "role": "paper", "rule": "D3s", "confidence": "sure"}
   ],
   "expected": [{"what": "doctor_claims", "why": "…", "since": "2026-10-08"}],
   "differences": [{"field": "clinician", "values": [{"from": "portal", "value": "…"}, {"from": "claims", "value": "…"}], "rule": "N3"}],
   "links": [{"event": "e_…", "person": "p_…", "relation": "newborn"}],
   "could_join": ["e_…"]}
 ],
 "pending": [{"document": "d_…", "could_join": ["e_…", "e_…"], "rule": "D4"}],
 "aliases": [{"clinician": "…", "organization": "…", "rule": "N5", "document": "d_…", "confidence": "likely"}],
 "undated": ["r_…"],
 "overrides": 1}
```

| Field | What |
|---|---|
| `id` | `e_` and 20 base32 characters of the SHA-256 of the person's id and the event's anchor member: the claim that opened it (a facility's, a stay's first hospital claim, a day's first), else its encounter, record or paper. It holds while that member stays the anchor. |
| `kind` | `er`, `inpatient`, `outpatient`, `office`, `service`, `tests`, `dental`, `vaccine`, `fill`, `series`, `stay_no_facility` (a stay's doctors' claims, no facility claim yet), `er_no_facility` or `portal` (a portal visit with no claim). A reader shows an unknown kind by its `title`. |
| `start`, `end` | Local calendar dates, `YYYY-MM-DD`; equal for a one-day event. |
| `title` | Plain words for the kind; the reader adds the place. |
| `what`, `what_from` | The portal's reason, else the claims' services in plain words, else the papers'; `portal`, `claims` or `papers`. Absent when nothing says. |
| `place`, `place_from` | The portal's organization, else the facility claim's provider, else the first claim's, else an N5 alias; `portal`, `claims` or `alias`. A clinician's name is never a place. |
| `clinicians` | As the portal or the claims name them. |
| `confidence` | The lowest of its members': `sure`, `likely` or `unsure`. |
| `status` | `unsure` when the confidence is; else `waiting` when something is `expected`; else `complete`. |
| `money` | The claim members' claim-level amounts, summed, as decimal strings: `billed`, `allowed`, `plan_paid`, `you_owe`, `discount` (`billed` − `allowed`) and `share`, the reasons for `you_owe` that the claims give (`deductible`, `copay`, `coinsurance`, `not_covered`). An amount some claim lacks is left out and named in `partial`. A claim with no `you_owe` of its own counts its share's parts added up, and `you_owe_added` is then `true`. A claim whose `allowed` is below zero counts `plan_paid` + `you_owe` as its allowed, and one with neither leaves `allowed` out. Absent with no claim. |
| `members` | Every record and document in the event, each with `record` or `document`, its `kind` (a record kind, or a document type), its `title` (at most 120 characters) and `date`, its `role`, the `rule` that placed it and that placement's `confidence`. A claim adds `provider` and its amounts. A bill joined by its amount due (D3s) adds `matches`, the claim whose share it bills. `person` marks another person's paper (X2). `role` is `facility`, `hospital`, `ancillary`, `office`, `dental`, `fill`, `portal`, `contact`, `satellite` or `paper`; a reader ignores one it does not know. |
| `expected` | What has not arrived: `doctor_claims`, `facility_claim`, `claim` or `bill`, with `why` in words and `since`, the day it was first expected. A fill or a vaccine expects no bill. |
| `differences` | Where the sources disagree (a clinician's name, a date), each source's value, and the rule. |
| `links` | Related events of a linked person: `newborn` (from a parent's stay to the child's care in it) and `born_during` (back). A reader ignores a relation it does not know. |
| `could_join` | For an `unsure` event, the events it may belong to. |
| `pending` | Papers that fit two or more events, merged into none. |
| `aliases` | Clinicians learned to belong to a practice, with the paper that says so. |
| `undated` | Claims with no date, in no event. |
| `overrides` | How many of the household's decisions applied. |
| `overrides_unreadable` | `true` when the person's `overrides.json` does not read as `medrec-overrides/v1`: the events are the rules' alone, and medrec writes no decision over the file until it is fixed or removed. Absent otherwise. |

`money` for an office visit whose one claim gave no `allowed` and no
`you_owe`, only its deductible:

```json
{"currency": "USD", "billed": "300.00", "plan_paid": "180.00", "you_owe": "40.00",
 "you_owe_added": true, "share": {"deductible": "40.00"}, "partial": ["allowed"]}
```

- **Every claim is in exactly one event.** A portal visit, a result or a
  paper joins at most one.
- **Linked people.** A person's events read the records of their guardians
  and children, when the child has a birth date: a newborn's care during a
  parent's birth stay links to it (X1), and a child's paper filed under a
  parent joins the child's visit that day (X2).
- **Order.** Newest first by `start`, then by `end`, then by `id`, so a
  cursor over the list holds between calls.
- **Size.** One file per person, about 1 KB per event: a person with 200
  claims takes about 150 KB. A reader parses it whole; it stays one file up
  to 1 MB, and a person past that would take a later schema that cuts it
  the way the timeline is cut.
- **Planned, not yet written: `private`.** A later release (0.9.1) marks
  an event about mental health, reproductive and fertility care, sexual
  health or substance use with `private: true`. No release writes it yet,
  so until one does its absence means "not judged", never "not
  sensitive". It will be discretion, not access control: a reader leaves
  such an event off shared screens and household-wide answers, and shows
  it on that person's own pages, or when a question is about that person
  or names the item. Anyone who can read the folder can read it.

## Readers

Anything that only reads the folder follows the files above and writes
nothing but `_requests/` (the app, by that section's rules). A reader that
answers from `views/` (the hosted connector) answers as `medrec mcp` does,
and falls back to the files themselves when the views are stale. Two
readers ship with medrec:

- **The commands** (`timeline`, `search`, `results`, `meds`, `documents
  list` and the rest) read `household.json`, each person's `index.json`,
  `text.json` and `notes.md`.
- **`medrec mcp`**, for Claude Desktop and Claude Code, reads the same files
  through the same views, per call, with no lock. It writes nothing,
  `_requests/` included. Its answers are computed, not stored:
  - a person's summary takes medicines, conditions and allergies whose
    `status` is not ended (`completed`, `stopped`, `inactive`, `resolved`,
    `remission`, `entered-in-error`, `cancelled`, `refuted`). It keeps one
    per code (with its system), or per name with case and accents folded
    when uncoded: the newest.
  - the latest result of each test is grouped the same way. A test's
    series is every `result` record whose code is the one asked for, or
    whose name holds the words asked for.
  - a bill is a `bill` document with a `statement`, not `same_statement_as`
    another. The newest statement per organization and account is shown
    when its `amount_due` is over zero, and is `past_due` when its
    `due_date` is before today. Whether a bill was paid is not recorded.
  - a document's text is its `text.json` entry. Pages are the form feeds
    between `pdftotext` pages, shown as `--- page N ---`.
  - `medrec_records` with `kind: "events"` lists a person's events as
    `events.json` holds them (0.9.0 and later): `source` is the place,
    `detail` what it was, who, a grouping that is not sure and what is
    still expected, and `you_owe` the share with its currency. `id` gives
    one event, with its money, members and evidence in `fields`.

Both search the same way: every item whose text holds all the query's
words, as word prefixes, with case, accents and punctuation ignored. A
query that names lab tests and nothing else (`blood test`, `blood tests`,
`bloodwork`, `blood work`, `labs`, `lab work`, `lab test`, `lab tests`,
`lab results`) also finds every `result` and `report` record and every
claim with a `lab` line, after the items whose text holds the words: lab
results never say "blood test".

## Import input: `records-stream/v1`

`medrec import --from <cli> [--from-profile <name> | --from-files <dir>] --patient <id> [--person <name>] [--patient <id> [--person <name>] …] [--org <org>] [--since <date>]`
runs `<cli> records export --patient <id> --ndjson [--org …] [--since …]
[--profile <name> | --files <dir>]` for each `--patient`. With several
(0.7.0 and later; not with `--from-files`, which is one member's files),
`--person` is given once per `--patient` in the same order, or not at all.
Every export runs first, each into an owner-only file on this machine, and
the imports are filed after, so one short website session serves them all.
A member that fails leaves the others filed, the exit code is the first
failure's, and `--json` prints one `import-batch/v1` document,
`{ok, items: [{patient, import | error}]}`, where `import` is the
`import/v1` document one `--patient` prints and `error` the family's error
shape. A provider that signs in lists `login-profiles` in its
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
