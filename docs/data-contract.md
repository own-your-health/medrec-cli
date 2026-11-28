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
  marks.json                  the household's payments, private marks, provider corrections and clinician marks (0.9.1 and later)
  provider-state.json         what a provider directory gave (0.9.7 and later)
  provider-state~unread-<8 hex>.json   one that did not read, kept (0.9.7 and later)
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
- `Records/<source>/importing.json` (0.9.9 and later) is there from before an
  import of that source uploads raw files until what they give is in
  `index.json` and the source in `household.json`. One left by an import cut short has the source's next import
  read every version of its raw files again for what they gave (What a source
  gave, below), and goes when that one finishes. `{started_at}` is when the
  first of them began.

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
   SHA-256 with `--deep`) or not in an index, and (0.9.11 and later) in
   `index_unread` the ids of people whose index does not read, of whom
   nothing more is checked; a sign-in or Drive that fails stops it. With
   `--events` (0.9.11 and later)
   it also reads the raw files of the records that events are made of, as they
   are, and `people` has one entry for each person, with counts and ids alone:

   | Field | What |
   |---|---|
   | `person` | the person's id |
   | `not_compared` | `true` where their index or events could not be read, so nothing of theirs is compared and every list is empty; absent otherwise |
   | `error_code` | with `not_compared`, the code of the error that stopped it: `auth`, `upstream`, `other` and the rest of the codes medrec's errors have |
   | `events`, `records` | how many events were compared, and how many of the person's own records are in them |
   | `places_of_others` | how many events have a place by a linked person's or a paper's word, which is not compared |
   | `place_not_given` | ids of events whose place is in no raw file of their records and is no name of a source of theirs |
   | `places_differ` | ids of records two of whose raw files give places of which none is the other's |
   | `name_not_given`, `day_not_given` | ids of records whose name, or day, no raw file of theirs has |
   | `source_not_its_own` | ids of records with a raw file in the folder of another source than they say they came from |
   | `resource_not_its_own` | ids of records with a raw file of another resource type or id than they say |
   | `unread` | ids of records with a raw file that is not there, does not read, is in a folder that could not be copied, or has a path that is not relative |
   | `read_errors` | the codes of the errors that made records `unread`, where one did (a folder that could not be copied, a file the folder did not give); absent where none did |

   What is not compared: the name of a claim, which is medrec's, and a name
   that is medrec's default for the kind (such as "Visit"); the name and day of
   a record with a raw file in `unread`, since that file may be the one that
   says them; the place of an event with such a record; and as a place, a
   records connection's name or one with no word but legal ones and numbers.
   An empty list is of what was compared. Any id, `index_unread` or
   `not_compared` is exit 1, after the whole report is printed.

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
`statement` and `same_statement_as` (a bill's, below), `provenance`, and
`also_from`, `words` and `errors` (Entered in error, below), and `given` (What a
source gave, below). An imported document's `provenance.raw_path` names its
Binary's raw file, which holds the latest rendering the source served;
earlier ones stay beside it as `<id>~<md5>.json`, as for every raw
resource, and the document's own bytes are the filed file. `also_from` (0.9.9
and later) is where else the same bytes came from, a provenance for each: what
another source serves, or its own by another Binary, is not filed again, and
neither is a file of the household's, which has no `raw_path` there.

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

**What a statement adds in 0.9.1.** A bill's and a receipt's `statement`
also give `phone`, the number the paper gives to call (`Questions? Call`,
`Phone`, `Tel`), as `NNN-NNN-NNNN`; a number labeled as the patient's,
the guarantor's or a home, cell or mobile number is not it. A `receipt`
carries `statement` too (0.9.1 and later), with `amount_paid` (`Amount
paid`, `Payment amount`, `Total paid`) and `paid_on` (`Payment date`,
`Date paid`), besides any field above its text labels. A bill never
carries `amount_paid` or `paid_on`: a bill's payment lines are an earlier
payment's. A bill or receipt filed before 0.9.1 gains the fields its
stored text gives and its statement lacks, keeping those it has, the next
time a bill or receipt is filed for that person, or at `medrec reindex`.

**Fields of a paper** (0.9.12 and later). A paper of any type but `bill`,
`receipt`, `eob` and `insurance_card`, or of none, carries `fields`: what its
text says on the lines that label it, read by rules alone, and absent where
it says nothing of them.

```json
{"fields": {
  "follow_up": [{"words": "in 2 weeks with Dr. Pat Sample.", "date": "2025-03-24",
                 "dated": "relative", "with": ["Dr. Pat Sample"], "lines": [[36, 41]]}],
  "drugs": [{"drug": "Exampleprofen 200 mg tablet",
             "directions": "1 tablet by mouth twice daily", "ndc": "00000000101",
             "lines": [[77, 31], [109, 34], [144, 17]]}]}}
```

- A line is read only where it starts with its label, after any bullet or
  list number. A line of the text that says an area (Private parts, below) is
  never read, and nothing is joined across one: directions after it are no
  drug's. A label alone on its line heads its value on the next line.
- `follow_up`: one for each line that starts "Follow up" (also "Follow-up",
  "F/U"), "Next appointment" or "Next visit", or "Return" where it is "Return
  visit" or "Return appointment", is followed by a colon or a dash, or the words
  after it say when or why ("in", "on", "at", "to clinic", "if", ...). A return
  to work, school, daycare, sports or one's activities is never one, marked or
  not. `words` are the line's words after the label, as printed, up to 200
  characters.
  - `date` is the day the words tell before any condition ("if", "as needed",
    "PRN", "unless", "should"); a day after one is the condition's, so "as
    needed" or "if symptoms persist" keeps the words with no `date`. A day
    printed whole is the one told, wherever it stands; else the first told.
    `dated` says how: `printed`, a day printed whole; `relative`, a time after
    the paper's own `date`, where the words start with it or "in", "within" or
    "after" comes before it ("2 weeks", "in a month", "within 3 days"; of a
    range, "2 to 3 weeks", its earliest day; a month later than a day the
    month has not is its last day), and not where it is what something lasts
    or an age ("after 10 days of antibiotics", "6 month well-child check", "2
    days before"); `resolved`, a day printed without a year, its first
    occurrence on or after the paper's date. Without a paper `date` only a
    printed day is told.
  - `with` is each place or clinician the words name after "with", "see" or
    "at", as printed: words that start with a capital, up to a word of when or
    why or a comma, after which another may start ("with Dr. Pat Sample,
    Placeholder Counseling Center" is two). An initial ("A.") is a name's,
    never an article; a credential after a comma ("NP", "LCSW") is no name.
    "Your doctor" is none.
- `drugs`: one for each line that starts "Rx", "Medication", "Medicine",
  "Drug", "Prescribed" or "Prescription" with a colon ("Rx" with none too)
  and words after it that name one: not a number, a count or a word that there
  is none. `drug` is as printed, up to 120 characters. `directions` are the
  words after "Sig" or "Directions:", or a line that starts "Take", within
  three lines after the drug's. `rxnorm` and `ndc` are an RxNorm code ("RxNorm",
  "RXCUI") or an NDC ("NDC") printed on the drug's lines or on a line of its
  own within three lines after them, an NDC as eleven digits where its hyphens
  tell them.
- `lines` are the places of a field's lines in the paper's text, `[byte
  offset, byte length]` without the line end, as `private.json`'s parts have
  them. A field private by what it says has its lines among an open paper's
  private places (Private parts, below): `documents.json` leaves it out, and so
  does an answer that names no person.
- A drug is listed under its paper alone, "from a paper": never in a list of
  medicines, a current list, standing or the summary, and it sets no status. It
  is the person's medicine only where both have an equal RxNorm code or NDC,
  never by its name; `documents.json` and `medrec documents list` then give that
  medicine's id as the drug's `medicine`.

Intake, `review file` and an import read them as they file a paper. `medrec
reindex` reads every paper again from its stored text (`paper_fields`, the
papers whose fields changed), so one filed before 0.9.12, or saved since by an
older medrec, which drops `fields`, gets them there.

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
names), `providers`, `words`, `errors`, `fields_from`, `lists` and `listed_by`
(below), `documents`
(linked document ids) and `provenance` (one per source).

**Fields by kind.** Every name a record's `fields` can hold. A field is
present only when the source gave it, so a reader takes none for granted.
The last column is the FHIR element the value comes from.

| Kind | Field | Value | From |
|---|---|---|---|
| `encounter` | `when` | date or date and time, as written | `period.start` |
| `encounter` | `class` | text | `class` (its display, else its code) |
| `encounter` | `location` | text | first `location.location.display` |
| `encounter` | `practitioner` | text | first `participant.individual.display` |
| `encounter` | `reason` | text | first `reasonCode` |
| `encounter` | `service_provider` | text | `serviceProvider.display` |
| `encounter` | `period` | `{start, end}` | `period` |
| `result` | `when` | as written | `effectiveDateTime`, `effectivePeriod.start`, `effectiveInstant` or `issued` |
| `result` | `value` | `{value, unit, system}` (a quantity; 0.9.10 and later with `comparator`, one of `<`, `<=`, `>=` and `>`, where the source gives one); or (0.9.10 and later) a blood pressure, `{value, unit}`, whose `value` is the text `"128/76"` and not a number (its numbers are in `components`); else text, a boolean, a whole number or a date | `valueQuantity`, `valueString`, `valueCodeableConcept`, `valueBoolean`, `valueInteger`, `valueDateTime`; 0.9.10 and later `valueTime`, `valueRange` and `valueRatio` as text, and what its parts say where it has none of its own |
| `result` | `components` | a list of parts, below (0.9.10 and later) | each `component` with a code and a value |
| `result` | `reference_range` | text | first `referenceRange` (its `text`, else its low and high) |
| `result` | `interpretation` | text | first `interpretation` |
| `result` | `category` | text | first `category` |
| `result` | `categories` | a list of codes, below (0.9.10 and later) | each `category` |
| `result` | `issued` | as written | `issued` |
| `result` | `performer` | text (0.9.11 and later) | first `performer.display` |
| `result` | `members` | a list of record ids (0.9.10 and later) | `hasMember`: Panels, below |
| `result` | `about` | `a parent` or `another person` (0.9.9 and later) | `focus`: About another person, below |
| `report` | `when` | as written | `effectiveDateTime`, `effectivePeriod.start` or `issued` |
| `report` | `conclusion` | text | `conclusion` |
| `report` | `category` | text | `category` |
| `report` | `categories` | a list of codes, below (0.9.10 and later) | each `category` |
| `report` | `results` | the number of results it lists | `result` |
| `report` | `issued` | as written | `issued` |
| `report` | `performer` | text (0.9.11 and later) | first `performer.display` (STU3: `performer.actor`) |
| `report` | `members` | a list of record ids (0.9.10 and later) | `result`: Panels, below |
| `condition` | `when` | as written | `onsetDateTime`, `onsetPeriod.start`, `recordedDate` or `assertedDate` |
| `condition` | `onset` | text, below | `onset[x]` |
| `condition` | `recorded` | as written | `recordedDate` or `assertedDate` |
| `condition` | `verification_status` | text | `verificationStatus` |
| `condition` | `category` | text | first `category` |
| `condition` | `categories` | a list of codes, below | each `category` |
| `condition` | `abatement` | as written | `abatementDateTime` |
| `condition` | `abatement_text` | text, below, with no `abatement` | `abatement[x]` |
| `medication` | `when` | as written | `authoredOn`; a fill's `whenHandedOver` or `whenPrepared`; a statement's `effectiveDateTime`, `effectivePeriod.start` or `dateAsserted` |
| `medication` | `start` | as written | a request's first `dosageInstruction.timing.repeat.boundsPeriod.start`, else `dispenseRequest.validityPeriod.start`; a statement's `effectivePeriod.start` |
| `medication` | `end` | as written | a request's last `boundsPeriod.end`, when each that has a period has an end, else `dispenseRequest.validityPeriod.end`; a statement's `effectivePeriod.end` |
| `medication` | `end_of` | `dosage`, `prescription` or `taking` | which of those `end` is |
| `medication` | `dosage` | text | first `dosageInstruction.text`, or a statement's `dosage.text` |
| `medication` | `prescriber` | text | `requester.display`; on a fill, its prescription's `requester`, below |
| `medication` | `intent` | text | a request's `intent` |
| `medication` | `categories` | a list of codes, below | a request's or a statement's `category` |
| `medication` | `status_reason` | text | a request's `statusReason`, or a statement's first |
| `medication` | `reported` | `true` | a request's `reportedBoolean`, or its `reportedReference` |
| `medication` | `quantity` | `{value, unit}` | a fill's `quantity` |
| `medication` | `days_supply` | a number | a fill's `daysSupply.value` |
| `medication` | `pharmacy` | text | a fill's first `performer.actor.display`, else `location.display` |
| `medication` | `type` | text | a fill's `type` |
| `medication` | `ndc` | text | a fill's NDC, when its code is another |
| `medication` | `source` | `request`, `statement` or `dispense` | the resource type |
| `allergy` | `when` | as written | `onsetDateTime`, `recordedDate` or `assertedDate` |
| `allergy` | `onset` | text, below | `onset[x]` |
| `allergy` | `recorded` | as written | `recordedDate` or `assertedDate` |
| `allergy` | `verification_status` | text | `verificationStatus` |
| `allergy` | `criticality` | text | `criticality` |
| `allergy` | `reaction` | a list of text | each `reaction.manifestation` |
| `immunization` | `when` | as written | `occurrenceDateTime` or `date` |
| `immunization` | `lot_number` | text | `lotNumber` |
| `immunization` | `site` | text | `site` |
| `immunization` | `route` | text | `route` |
| `immunization` | `performer` | text | first `performer.actor.display` (STU3: `practitioner`) |
| `procedure` | `when` | as written | `performedDateTime` or `performedPeriod.start` |
| `procedure` | `performer` | text | first `performer.actor.display` |
| `procedure` | `location` | text | `location.display` |
| `coverage` | `when` | as written | `period.start` |
| `coverage` | `payer` | text | first `payor.display` |
| `coverage` | `period` | `{start, end}` | `period` |
| `coverage` | `relationship` | text | `relationship`'s code |
| `coverage` | `plan` | text | `class` of type `plan` |
| `coverage` | `group` | text | `class` of type `group` |
| `coverage` | `subscriber_id` | text | `subscriberId` |
| `claim` | `when` | as written | `billablePeriod.start` or `created` |
| `claim` | `provider` | text | `provider.display` |
| `claim` | `insurer` | text | `insurer.display` |
| `claim` | `facility` | text | `facility.display` |
| `claim` | `outcome` | text | `outcome` |
| `claim` | `use` | text | `use` |
| `claim` | `type` | text | `type` |
| `claim` | `category` | text, the claim's one `category` below | its counted lines |
| `claim` | `billable_period` | `{start, end}` | `billablePeriod` |
| `claim` | `benefit_period` | `{start, end}` | `benefitPeriod` |
| `claim` | `total` | `[{category, amount}]` | `total` |
| `claim` | `payment` | an amount | `payment.amount` |
| `claim` | `amounts` | `{<name>: {amount, currency}}`, the names below | `total`, its lines and `payment` |
| `claim` | `items` | a list of lines, each `{sequence, category, amounts, service, …}`, below | `item` |
| `claim` | `payer_notes` | a list of text | `processNote` no line points at |
| `claim` | `diagnoses` | `[{sequence, code: {system, code, display}}]` | `diagnosis` |
| `claim` | `benefit_balance` | `[{category, network, unit, term, financial: [{type, allowed, used}]}]` | `benefitBalance` |
| `claim` | `days_supply` | a number, or text as the payer wrote it | `supportingInfo` (`dayssupply`) |
| `claim` | `refill_number` | a number, or text as the payer wrote it | `supportingInfo` (`refillnum`) |
| `claim` | `refills_authorized` | a number, or text as the payer wrote it | `supportingInfo` (`refillsauthorized`) |
| `claim` | `brand_generic` | text | `supportingInfo` (`brandgenericindicator`) |
| `claim` | `visit` | a record id | its links, below |
| `claim` | `results` | a list of record ids | its links, below |

**When and what kind** (0.9.9 and later). `onset` is the element in whichever
form the source gives: a date as written, a period as `2019-04 – 2019-06`
(`from 2019-04`, `until 2019-06` with one end), an age as `12 years`, a range
of ages as its two, the source's own words, or `yes` where it says only that
there was one. It is text to show, and a date only where it is one.
`abatement_text` is the same of an `abatement[x]` that is no date, and
`abatement` stays a date. `categories` is the code of each `category`, in
FHIR's and US Core's own systems before a vendor's, each once; a category with
no code is its text, in the same list, so a reader looks for the codes it knows
and shows the rest as they are. A result's and a report's are those of FHIR's
observation categories and of HL7's table 0074 (0.9.10 and later). Before 0.9.9 a
condition had no `onset`,
`recorded`, `categories` or `abatement_text`, an allergy had only `when`,
`criticality` and `reaction`, and a medicine had no `start`, `end`, `end_of`,
`categories`, `status_reason` or `reported`. `medrec reindex` gives them to
records filed before.

**A reading of several parts** (0.9.10 and later) is one result. `components` is
its parts as the source orders them, `{code, value, reference_range,
interpretation}`, each as a result's own (`code` is `{system, code, display}`),
the last two absent where the part has none. A part with neither a code nor a
name is not in it. One with no value is not either: the record's `given` has its
code (What a source gave, below), which the rules of what is private read and no
reader shows. Where the Observation has a value of its own, `value` is that.
Otherwise it is what its parts say:

- the two of a blood pressure, told by their LOINC codes (8480-6 and 8462-4) and
  by nothing else, are `{value: "128/76", unit}`, where there is one of each, both
  are exact numbers and both are in the same unit;
- each other part is its name and its value, `; ` between them. Where there are
  others beside a blood pressure, `value` is a text that begins with it.

A part is not a record. A reading is judged by the names and codes of its parts
too, with a value and without, and by the texts of their values (Private, below).
`medrec reindex` (0.9.10 and later) reads each result's and report's `status`,
`display`, `code`, `date` and `fields` again from the raw file they are of: of
several sources', the one `fields_from` names, else one that an import lets write
them and whose values agree with the record's (a `comparator` aside), else none,
and those values stay as they are. A file that says it is entered in error, or that
gives another `key`, writes none of them. `lists` (Panels, below) is read from
every raw file of the record that reads as its kind, those included.
An import by a medrec before 0.9.10 drops `components` and `categories` again: a
reading that is private by its parts alone is then open until the next
`medrec reindex`, so run 0.9.10 or later on every machine that writes the folder.
Results and reports filed before 0.9.11 have no `performer`, and are not held by
it, until their source serves them again or `medrec reindex` reads it from their
raw files, with their `providers`: run `medrec reindex` once after upgrading. An
import by a medrec before 0.9.11 files them again without it, so run 0.9.11 or
later on every machine that writes the folder.

**Panels** (0.9.10 and later). `members` of a report, and of a result that is a
panel, is the ids of the results it lists, in its order: those its own resource
refers to (a report's `result`, an Observation's `hasMember`, or before R4 its
`related` of type `has-member`) on its own source's server. Of several sources'
one record it is what each of them lists, that of its values first. Nothing is a
member by its date or its name. The ids are made again on every write of the
index, from `lists` of the record (by its raw file, the raw files that one refers
to, under its source), so a result that comes later is a member then. What is
entered in error is none, no view or answer shows the id of one about somebody
else (`medrec records show` of one of those does), and `members` is absent where
none is left.

A result with no value whose `lists` is not empty is a heading, whether what it
lists is filed or not: no latest result and no test (`latest_results`,
`kind: "tests"`, `tests.json`), and where a list has it its `detail` is `panel`.
With an `interpretation` that is `panel (H)` in the lists of the command line, in
`timeline-<n>.json` and in a tool's `timeline` answer, and `panel` beside the `flag`
in a tool's other answers. `heading:
true` on it in a view's records says so. `results` of a report stays the number of
results it refers to, filed or not.

A result that a questionnaire lists (a panel or a report, Questionnaires below: by
its name and code, or a part's) is of that questionnaire, as its other answers are,
whatever it is named, to any depth. `listed_by` of its record (`{display, code}`)
keeps each, and one is never taken away. What a panel or a report listed before is
kept too (`given`, below), so nothing opens because a source lists it no more, or
lists more.

A medrec before 0.9.10 drops `lists` and `listed_by`, and `lists` of `given`, from
an index it saves, and keeps `members`. An answer that is private by its panel
alone is then open until the next `medrec reindex`, which gives back what the raw
files tell, earlier versions included: not what a raw file that is gone listed, nor an
earlier version of a resource with no id, whose raw file nothing ties to its record.
So run 0.9.10 or later on every machine that writes the folder.

**Words.** A condition's, an allergy's and a medicine's `words` (0.9.9 and
later) is what each of its sources says of it that can differ between them.
There is one for each of its provenances whose raw file is kept, in their
order: `{raw_path, status, verification, categories, abatement, end, end_of,
version}`, each but `raw_path` absent where the source gives none. A fill's and
another kind's record has them only where several of its provenances state it
or one says or said it is entered in error, and then each is its `status`.
`raw_path` is the provenance's,
and so says whose word it is and when it was read. `status` is the source's
code for a condition's or an allergy's clinical status and for a medicine's
status. `verification` is the code of the verification status, `abatement` is
`abatement` or `abatement_text`, and the others are the fields of those names.
`version` is medrec's own: which version import read into the record, as the
first 16 hex of its content's digest. A reader ignores it, and it is absent
after `medrec reindex` alone. Two resources of one source that are one record
are two words. No word is changed for another's: a reader that says whether a
condition is current reads every word. An older version served again changes no
word. `medrec reindex` reads them all again from the raw files, and the
record's `status` and `fields` from the one they are of; what a raw file that
does not read said stays. Where no raw file is said to be the one, it is, of
those that can be, the first that says all the record does, else, where each
reads, the first. A medrec before 0.9.9 drops `words` from an index it saves,
so run 0.9.9 or later on every machine that writes the folder; the next import
of each source, or `medrec reindex`, gives them back.

**Whose fields.** `fields_from` (0.9.9 and later) is the raw path of the
provenance whose resource a record's `status` and `fields` are of, where the
record has words and several provenances. It is a source's first on the record
(of those with no id, the first of its type) that does not say it is entered
in error, or with none the first; one that names another names none. A version of that resource writes
them each time it is served, so a later medrec's reading of it applies.
Another's does when it is new to the record (its `version` is another than its
word's, or with none its word says otherwise: an import cut short after its raw
files is made good by the next), and when `fields_from` names none of the
record's provenances or is absent (an earlier medrec saved the record); it then
writes them and `fields_from`. Served again as it was, it changes neither. Until 0.9.9 each
import's version wrote over the other's, and still does in a record with no
words.

**Entered in error** (0.9.9 and later). A source says a record of any kind is
entered in error by its word's `status` or `verification`, whose code is
`entered-in-error`. It says so of a paper by the `status`, or by the
`docStatus`, of what describes it: the DocumentReference, DiagnosticReport or
Media that names its Binary or that it was filed by. Where several of one
source's describe a paper there, it says so when each does, the ones earlier
imports filed included. A resource with no id is known by the Binary it names
alone and takes the place of no other, so a later one like it ends nothing an
earlier one says. A paper's word is `{raw_path, status, verification,
read}`, with `docStatus` as `verification` and `read` when it was last read. It
has them where one says or said it is entered in error, or `also_from` is
there.

- A record or a paper is entered in error while every one of its provenances
  has a word and each word says so (a record's own status is the word of its
  only provenance, or of the one that wrote its fields, where none is kept). A
  source whose word is not kept does not say so, and a file of the household's
  has none, so it stands.
- One that only some say so of stands, and differs: its `words` say so, and no
  other field of the index does. The views do: a condition's, an allergy's or a
  medicine's `differs` in `standing.json`, any other's `says` (Records, and
  `documents.json`), and `differs` on a member of an event and on a bill. Its
  values are never those
  of a source that says so: when the resource `fields_from` names does, medrec
  reads `date`, `code`, `display`, `status` and `fields` again from the raw file
  of another that does not and that reads, and names it; a version that says so
  writes nothing while another does not say so. Its `providers` are read again
  from all that do not say so, and nothing of them is kept from before.
- A version that says so stays on the record it was of, whatever else it
  leaves out, and what it leaves out stays on the record.
- It comes back by itself when a later version of the same source's no longer
  says so. `errors` keeps each time it did: `{raw_path, from, until}`, when
  medrec first read the version that says so (a paper's, its word) and the
  first after it that does not; `until` is absent while it says so. They stay
  with their resource when it moves to another record.
- A paper filed before 0.9.9 kept one source only, so another that served the
  same bytes is in no `also_from`. The next import from that source adds it,
  and files nothing again. So does `medrec reindex`, and the import that first
  reads a source saying a paper is entered in error, for every source at once:
  a raw Binary that no paper is of has the bytes. Its times, and when a word
  that was not kept was read, are those of its source's latest import, and not
  known (no `fetched_at`, no `read`) where that is none or imports of several
  patients went into one folder. One whose raw Binary is gone cannot be told,
  and until its next import the other's word alone ends the paper. Nor can a
  file of the household's taken in before 0.9.9: taking it in again notes it.
- A raw file that does not read tells nothing, and an import and `reindex` count
  it (`papers_unread`). While one of a resource that describes papers does not,
  no paper of its source's is newly entered in error.
- What is entered in error is in no list, the timeline or the search, in no
  event, and in no total, balance or "worth a call". It is no bill, receipt or
  copy, names no one, prints no labelled provider and is the report of no
  study. Each view says what it leaves out, by id (below).
- What the household decided of it (`overrides.json`, `marks.json`) is kept, is
  not applied while it is in error, and applies again when it comes back, but a
  private mark, which holds.
- A medrec before 0.9.9 drops `also_from`, `words` and `errors` from an index
  it saves. `medrec reindex` gives back what raw files tell: the sources in
  `also_from`, `words`, and `errors` for what a source says now. A time that
  ended is lost, and a file of the household's is until it is taken in again.

**What a source gave** (0.9.9 and later). A record's `given` is what its
sources gave of it, then or now, that the rules of what is private read and
that the record does not show: a list of `{raw_path, cli, display, code,
fields, npis, lists}`, each with the parts that are other than those shown. `display`
is a medication's, a result's, a report's, a procedure's or a claim's, and (0.9.10
and later) a visit's, or a condition's with no ICD-10 code, where it says an area, and `code`
a condition's, a medication's, a result's, a report's or a procedure's; `fields`
are a claim's `diagnoses`, its `payer_notes` and its `items` (of each line
`service`, `revenue`, `payer_notes` and `counted`), a result's `about`, its
`components` (0.9.10 and later: of each part its `code`, of those with no value
too, and its `value` where that is a text that says an area), and (0.9.10 and later)
a result's `value`, a report's `conclusion` and a visit's `reason`, each a text that
says one, and the fields that print a provider (Providers, below); `npis` has the NPI of the provider a field prints,
by that field; `lists` (0.9.10 and later) is what a panel or a report listed
otherwise, as its `lists` has it, each raw file's whole; `cli` is that of a source that no longer has the record.
`raw_path` is the raw file of the current version of what gave it, absent
where that is not known. A document's is `{raw_path, organization, title,
name}`: `name` is the `original_name` of a copy that was folded into it. A
condition's entry with a `display` is judged with that entry's `code` alone, and
as one with no code where it has none, whatever code the item shows now (0.9.10
and later): a name that said an area while there was no ICD-10 code still says it.

- It decides what is private and what is about another person (below), and
  nothing else: each rule and provider label reads it beside what the item
  shows (Private items, below). What an item
  shows, in every view and answer, is still of a source that stands. No view or
  answer has any of it, and a reader of `index.json` shows none of it: of it
  there is only `given: true` on an item of `private.json` and on its row of
  `medrec private list`, and, of a clinician the household opened whom only a
  source gave (0.9.10 and later), `given: true` on the mark and on its
  `clinicians_opened` row, with the mark's `people` (surnames and first
  initials) in `marks.json`.
- It is never dropped. So nothing that a source gave opens because a later
  version gives something else, a source says the item is entered in error,
  another source's values take its place, or the source takes its resource to
  another record, which then has it too. Whom another resource of the first
  record is about (`about`) does not go with it. The household's own mark opens
  it (`medrec private unmark`).
- It is of the item itself. One that is private only with another (its event,
  its report, a record or a paper tied to it) follows that as it is, but a panel or
  a report is judged by what it listed before too, and an item that prints a
  clinician by what the papers that name them, and the papers and claims of a
  day that tell of their practice, were given too (0.9.10 and later;
  `clinician`, below). A newborn's
  event that takes its place from a parent's claim (X1) is judged by the places
  that claim was given too.
- An import keeps what a version gives, an older one served late included, and
  what the record showed before another version or another source's values.
  One that follows an import of its source that was cut short keeps what the
  versions filed by that one gave too, whether it serves the resource or not.
  `medrec reindex` keeps what every version of a record's raw files gives that
  is there, and what describes a paper, earlier versions included; and, for a
  record with a raw file that is not there or does not read, what the record
  showed. `documents merge` keeps what the copy it folds in was called.
- A medrec before 0.9.9 drops it from an index it saves. `medrec reindex` gives
  back what the raw files tell: not a name a source had before of a paper
  another filed, nor what a record showed whose raw file is gone. Run 0.9.9 or
  later on every machine that writes the folder.

**About another person** (0.9.9 and later). A person's chart can hold what is
about somebody else: a screening a parent answers about themselves, or a result
its source says is of another. Of the items of a person's `index.json`, these
are:

- a `result` whose `about` says so. An import sets it from an Observation's
  `focus`, where that names a RelatedPerson, or a Patient other than its
  `subject`: `a parent` where the reference's `display` or the relationship of
  the RelatedPerson it contains says so, else `another person`. Its `key` is
  `about|` and the key it would have as the person's own, then `|` and what the
  `focus` refers to (not where only an earlier version says so), so it is never
  one record with the person's own of that code and day.
- a `result` or a `report` of a questionnaire that a grown person answers
  (Questionnaires, under Private, below), dated before the person's 11th
  birthday, or undated and first fetched before it. Not a laboratory's, whose
  `category` starts with `lab`, and none where the person's birth date is not
  known.
- a panel or a report (0.9.10 and later) that lists none but such (Panels, above),
  with what is entered in error or without it, or did: each of its raw files lists
  none but such, now or in an earlier list, and one lists some. So where several
  sources are of one, what one of them lists hides none of the person's own.
- a document with the title of one, dated so, every provenance of which names a
  source's `cli`; and one that only such records list in their `documents`, no
  provenance of which is the household's own.

An item is one by what it shows or by anything a source gave of it (`given`), so
no later version brings it back, and the household marks none. A result with an
`about` in `given` is one where that is of the resource its values are of (the
raw file of its only provenance, its `fields_from`, else that of the first
resource an import lets write it), or of no known raw file.

- No view or answer has it: no list, timeline, search, series of tests, latest
  result, current list, event, bill, total or sheet. A claim's `results` and its
  lines' `records` leave its id out. It is moved to no other person: nothing
  says whose it is. A request that names it is refused as one of an item that
  is not there.
- `people.<id>.about_another` in `views.json` has the ids of a person's. A
  reader of `index.json` leaves them out of everything. A list of that person's
  results, tests, timeline or documents may say how many, as "1 item about
  another person, not shown" or "2 items about another person, not shown", and
  nothing else: no kind, score, date or name. No shared screen, sheet or list
  of everyone's says it.
- At the Mac, `medrec records about-another` lists the ids. `medrec records show
  <ID>` shows a record by its id, with `notes` `about a parent, in <name>'s
  chart` or `about another person, in <name>'s chart`, and `medrec documents
  open <ID>` opens a paper and says the same. `medrec records raw <ID>` (0.9.10
  and later) says the same; with `--fields` or `--changes` it says `about a
  parent` or `about another person`, with no name. `medrec records fields` does
  not count them.
- An Observation that a medrec before 0.9.9 read in has its `about` once its
  source serves it again, or from `medrec reindex`, which reads the raw files.
  One that was filed as the person's own then has that key, and one that was
  filed in one record with it goes to a record of its own, and the person's has
  its own values again; where no raw file of the person's own reads, what is
  left stays about another person. A medrec
  before 0.9.9 shows all of it, so run 0.9.9 or later on every machine that
  reads the folder.

**Providers.** A record's `providers` (0.9.6 and later) is what its sources
say of the people and places its fields print, beyond their names: a list
of `{field, npi, state, city}`, with one entry for each field that has an
`npi` or a `state`.
`field` is the field that prints the provider: `practitioner`, `location`
and `service_provider` of an encounter, `prescriber` and `pharmacy` of a
medication, `performer` of an immunization, of a result and of a report
(0.9.11 and later), `performer` and `location` of a procedure, `provider` and `facility` of a claim.
(0.9.11 and later) In any field that prints a provider, a reference to a patient
or a relative (a `Patient` or `RelatedPerson`, by its reference, its `type` or a
contained resource) is none. Where the element is one of a list, the next one is
read in its place; otherwise the field is absent, or comes from its fallback
element (a MedicationRequest whose `requester` is the patient has no
`prescriber`). Claims, results and reports filed before take this with `medrec
reindex`, and so does a fill's `pharmacy`, whose `fields` reindex reads again; a
prescription keeps an earlier `prescriber` through reindex, and other records
take it when their source serves them again. `npi` is a National Provider
Identifier whose check digit holds, from the identifier of the reference
that prints the field or from the resource it points at. `state` is two
letters, from that resource's first address that gives one, and `city` (0.9.8
and later) is that address's, as the source writes it, and empty when it gives
none; it is absent where an earlier medrec read the address. A reference is followed to a resource
contained in the record's own, served in the same stream, or filed by an
earlier import of the same source, through a PractitionerRole (the person's
NPI, the place's state) and a Location's `managingOrganization`. An absolute
one is followed only under the source's own FHIR base. A name with nothing
more behind it has no entry. Where two sources state one record, each fills
what the other lacks of a provider both print, and the value there first
stands. A provider whose reference leads to something the source does not
give keeps what the record had where nothing is read in its place (but where
its sources differ: Entered in error, above). `medrec
reindex` reads them all again from the raw files, every source of a record in
turn, and so takes a correction that import, for a record two sources state,
leaves as it was; it also gives records filed before 0.9.6 theirs, and those
filed before 0.9.8 their `city`. No answer
or view shows it; `providers.json` is made with it (Provider labels, below).
A medrec before 0.9.6 drops it from an index it saves, and one before 0.9.8
drops each `city`, so run 0.9.8 or later on every machine that writes the
folder. `medrec reindex` gives back what was dropped.

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

`medrec reindex --json` counts what it did, for each person and in all
(`--dry-run`, 0.9.9 and later, counts what it would do, writes nothing and adds
`dry_run: true` and what is in "A dry run", below):
`claims` (how many there are), `reshaped`, `unchanged` and `without_raw`
(claims with no raw file), `statements` (bills and receipts whose statement
gained fields), `paper_fields` (0.9.12 and later: papers whose `fields`
changed), `providers_updated` (0.9.6 and later: records whose
`providers` changed), `statuses_updated` (0.9.9 and later: records whose
`words` or `errors` changed, or whose values another source's took the place
of, and conditions, allergies and medicines whose `status` or `fields`
changed), `given_updated` (0.9.9 and later: records whose `given` gained
something), `results_split` (0.9.9 and later: results about somebody else that
were filed as the person's own, or in one record with it), `results_updated` (0.9.10
and later: results and reports whose `fields`, `status`, `display`, `code`, `date`
or `lists` changed), `links_updated` (0.9.10 and later: records whose links to
others are made otherwise, by the rules of this medrec: a claim's, the `members` of
a panel or a report, and a result's `listed_by`), `papers_updated` (0.9.9 and later: papers whose `also_from`,
`words`, `errors` or `given` changed) and `papers_unread` (0.9.9 and later: raw files of
papers that do not read). From 0.9.9 a run makes every person's views again, even
when no record changes.

**A dry run** (0.9.9 and later) makes the records and the views in memory as a run
would, writes nothing, and counts what would change. It has no name, date, amount
or id of an item, and names no kind of questionnaire that there is one of.

- `fresh` is `true`, and is there for a dry run alone: each `index.json` is in the
  folder, at the end, with the MD5 it was read with. When another command wrote one
  meanwhile, the counts would be of neither: the dry run ends with an error and
  counts nothing.
- Each person's `views` is against the views the last pass made. `against` is that
  medrec's version, null where the folder has none (then every private item, event
  and bill is counted as added), and `stale` is whether those were made from
  other files than the folder has. A dry run that cannot read `views.json`, or a
  view it names, ends with an error and counts nothing; where a later medrec's
  schema is why, it says to upgrade.
- A run with `--person` reads only that person's records again and makes every
  person's views, so its dry run has `others`: for each of the other people, their
  `person` and their `views`. It is absent where there are none.

  | Field | Counts |
  |---|---|
  | `private.added` | by kind, items that are private and were open, or were not there; `added_by`, by the rules that make them so (`from`); `questionnaires`, the questionnaires of them, by kind, where there are several of one; `questionnaires_one_of_a_kind`, how many kinds there is one of; `clinician` (0.9.10 and later), by kind, those private by their clinician (`from: clinician`), and of them `clinician_day`, held by the day of a practice's paper alone, `clinician_linked`, held through a clinician linked to a practice, and `clinician_performer` (0.9.11 and later), the results, reports and immunizations, held by the performer they print |
  | `private.opened` | by kind, items that were private and are open |
  | `private.gone` | by kind, items that were private and are not there: what is about somebody else, and an event that is another now |
  | `private.given` | by kind, items that are private only by what a source gave |
  | `private.parts` | by kind, open items with a private part |
  | `in_error` | by kind, what is entered in error |
  | `current` | of conditions, allergies and medicines, the records that a list of current ones has: `added` and `removed`. Those of a medrec before 0.9.9 are by `status` |
  | `events` | `added`, `removed`, and `changed`, those with other money |
  | `bills`, `worth_a_call`, `not_billed` | of the lists of `bills.json`, `added`, `removed`, and `changed`, those with other amounts |
  | `about_another` | what is about somebody else: results and reports by what their source says (`said`), by the person's age (`by_age`) and by what they list alone (`listing`, 0.9.10 and later), and `documents` |
  | `papers` | `filed_before`, papers filed by a medrec that did not keep every source of one, and `source_found`, those of them another source of which the raw files tell; and (0.9.12 and later) `fields`, the follow-ups and drugs papers' texts give, by field (`follow_up`, `drugs`), `fields_private_by_paper`, those of a private paper, and `fields_private_by_content`, those of an open paper private by what they say (Private parts) |

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
  payer wrote them), `payer_notes`, `counted`, `codes` and `records`.
  `revenue`, `payer_notes` and `counted` are present only when they say
  something. `codes` (0.9.11 and later) is every coding of the line's service
  that has a system and a code, `[{system, code}]` in the payer's order, where
  they say more than `service`: more than one, or one that is not the first
  coding. `service` keeps the first, and a payer can give an RxNorm code, an
  NDC or a CVX code after a code of its own, or after a display alone. A claim filed before has it after `medrec
  reindex`.
- A line's `payer_notes` (0.8.2 and later): the texts of the
  `processNote` entries its `noteNumber` points at, in that order, as the
  payer wrote them (UHC's start with their code: `XX -- …`). The claim's
  own `payer_notes` are the `processNote` texts no line points at. They
  are the payer's, never the household's notes in `notes.md`.
- `counted: false` (0.8.2 and later) marks a line the claim's totals
  leave out. A payer can list a line it then does not count: one it
  replaced with another, or wrote off. The same amounts occur on lines
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
- A provider CLI of this project can mark a line left out itself, with an
  adjudication whose category is `not-counted` in its own
  `https://github.com/own-your-health/<cli>/fhir/adjudication` code system
  (uhc-cli 0.3.6 and later). When any line of a claim carries that mark,
  the marks decide which lines are `counted: false`, and the totals are
  not compared (0.9.2 and later).
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
  claim's dates of service (each line's `serviced`, else the claim's date
  for a line with none, or for a claim with no lines), leaving out encounters
  whose `status` is `planned` or `cancelled`, or that are entered in error (as
  above: every source that has it says so). Of several, it is
  the one that shares a word of four or more letters between its
  `service_provider`, `location` or `practitioner` and the claim's
  `provider` or `facility`. When that leaves no single
  encounter, there is no `visit`.
- `results`: the ids of the `result` and `report` records dated on a `lab`
  line's date of service, leaving out those `cancelled` or entered in error.
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
both. Where two sources state one record, whose fields it has is "Whose
fields", above. A resource with no id is never the same resource as another; it
joins a record only by key. On that record, the provenances of its type from
its source with no id belong to the resources like it: each takes the one
naming its raw file (`noid-<digest>`), or else, for a version with other
content or one filed before 0.2.7, the first that no resource of the same
import names, or adds
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
`pulled_at`, `from_files` (whether it was of downloaded files; 0.9.10 and later,
absent in a run filed before), `imported_at`, `counts`, `skipped`, and the counts
the import report gives (below): `resources_written`, `resources_unchanged`,
`records_added`, `records_updated`, `records_unchanged`, `records_older`
(0.2.7 and later; 0 in a run filed before), `records_not_served` (0.9.10 and
later; absent where it cannot be told), `documents_added`,
`documents_duplicate` and `documents_unchanged`.

**Provenance** (the `provenance/v1` fields of medical-common's
`pk-med-records`): `source` (`{kind, cli, cli_version, organization_id,
fhir_base}`; an import's `kind` is `fhir` for a FHIR API read and the
stream's `method` otherwise), `method` (`fhir-api`, `portal`, `scan`, `file`, `media`,
`manual`), `source_resource` (`{type, id, version_id, last_updated}`),
`fetched_at`, `filed_at`, `raw_path` (the verbatim file), `classified_by`
(`medrec`, `organize-scans`, `hand`), `derived_from` (`{document_id, rule_id}`, on a fact a rule read out of a
document), `filed_by` (`medrec <version>`).

**Which source a record or document came from** (0.9.6 and later). A
provenance's `cli`, `organization_id` and `fhir_base` do not name a source:
sources in `household.json` can have all three alike and differ in `person`,
in `patient_id`, or in both. What tells them apart is each one's `person` and
`patient_id`:

1. The patient. Of the index's `imports` with the provenance's `cli` and
   `organization_id` (its `organization`), *filed* is the `patient_id`s of those
   whose `imported_at` is the provenance's `filed_at`, and *read* the
   `patient_id`s of those whose `pulled_at` is its `fetched_at`. One in filed:
   the patient is that. Several in filed: the only one of them that is in read
   too. None in filed: the only one in read. Otherwise it is not known.
2. Its sources: those of the index's **own person** with the provenance's
   `cli`, `organization_id` (a source's `organization`) and `fhir_base` (every
   trailing `/` aside; any, where the provenance has none), and with that
   `patient_id`, when the patient is known. A provenance with no `cli` or no
   `organization_id` has none.
3. Its name: the `organization_name` they all have, trimmed; a blank one is
   none. Where they differ, or one has none, there is **no name**: never the
   first or the last of them.
4. A record's label, what a reader shows as its source: that name, else the
   `cli`. No other person's source and no entry of `organizations` names it. A
   document's row shows its own `organization`.

Two came from one source when their `cli`, `organization_id`, `fhir_base` and
patient are the same, the patient being step 1's, else the `patient_id` of the
only source step 2 leaves.

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
  no longer holds is skipped, and one naming a member entered in error while it
  is (0.9.9 and later): it is kept, and applies again when the member comes
  back. The same two members have at most one
  decision: the other kind replaces it, and the same kind again changes
  nothing.
- `by` is who asked, as shown (a request's `requested_by`), and `request`
  the request it came from; both are `null` from a terminal. Neither is
  trusted.
- medrec writes it (`medrec events apart|together|undo`, and event
  requests in `_requests/`, below). Readers read it only to judge the
  freshness of the views made from it (views, below).
- A field a later medrec adds is kept when an earlier one, from 0.9.1 on,
  rewrites the file. medrec 0.9.0 keeps only `schema`, `person` and
  `events`, so nothing else the household marks is kept here
  (`marks.json`, below).

## marks.json (`medrec-marks/v1`, 0.9.1 and later)

What the household marked on a person's records that the rules do not
decide: the bills they marked paid (`bills.json`, below), the items they
marked private or not (`private.json`, below), what they said of the
providers the items print (`providers.json`, below), and the clinicians they
said are of no private area (`private.json`, below). It is a file of its own
so that a medrec before 0.9.1, which rewrites `overrides.json` with only the
fields it knows, never drops a mark. It is absent until the first mark.

```json
{"schema": "medrec-marks/v1", "person": "p_…",
 "payments": [
   {"id": "o_…", "document": "d_…", "on": "2026-11-20",
    "amount": {"amount": "120.00", "currency": "USD"},
    "at": "2026-11-24T12:00:00Z", "by": "Alex Testperson", "request": "x_…"}
 ],
 "private": [
   {"id": "o_…", "item": "r_…", "private": true,
    "at": "2026-11-24T12:00:00Z", "by": "Alex Testperson", "request": "x_…"}
 ],
 "providers": [
   {"id": "o_…", "printed": ["J SAMPLE"], "set": {"name": "Dr. Sample", "phone": "555-010-0123"},
    "at": "2026-11-24T12:00:00Z", "by": "Alex Testperson", "request": "x_…"}
 ],
 "clinicians": [
   {"id": "o_…", "printed": "J SAMPLE", "people": [{"surname": "SAMPLE", "initial": "J"}],
    "item": "r_…", "at": "2026-11-24T12:00:00Z"}
 ]}
```

- A payment is the household saying a bill (`document`, a `bill` of this
  person's) was paid `amount` on `on`, as a receipt never filed would. A
  bill may have several. The same bill, day and amount again changes
  nothing.
- `payments_undone` (`[{request, at}]`, 0.9.7 and later, absent when empty) is
  the requests whose payments were taken back, for 90 days. A request in it
  that is served again records no payment; a new request does.
- A private mark is the household saying an item (`item`, one of this
  person's record, document or event ids) is private (`true`) or not
  (`false`), whatever the rules say. An item has at most one mark: another
  replaces it, the same again changes nothing, and `medrec private reset`
  drops it. `private` is absent until the first such mark.
- A provider correction (0.9.7 and later) is what the household said of
  `printed`, names exactly as the person's items print them, and one of:
  - `set`: what to call the one name, or each of several: `name`, `practice`,
    `role` and `phone`, at least one;
  - `same`: that two names or more are one provider (`true`) or are not
    (`false`);
  - `wrong`: `true`, that the label a name has is wrong.

  `item` (a record, document or event id, with one name and `set` or
  `wrong`) holds it to that item. A correction of the same names and item
  replaces one of the same sort (`set` and `wrong` are one sort, `same` the
  other), and the same again changes only `at`. `medrec labels undo` drops one.
  `providers` is absent until the first. It is in the order things were said:
  `at` is when, for a request the store's time for its file (the folder's clock,
  not the asking device's) and never after medrec first tried it, and from a
  terminal after all that is there.
- `providers_undone` (`[{printed, item, kind, at}]`, absent when empty) is what
  was taken back, until the same is said of again, and for 90 days at most. A
  request asked before then and served after does not say it again. An entry of
  a `kind` medrec does not know is kept as it is.
- A mark on an event, or a correction held to one, also lists the event's own
  `members` (record and document ids) when it was made. An event keeps its id
  only while its anchor member stays (Events, below); when the id changes, the
  mark follows its members to the event that holds any of them.
- `by` and `request` are as in `overrides.json`: who asked and the request
  it came from, `null` from a terminal, neither trusted.
- medrec writes it (`medrec bills paid|undo`, `medrec private
  mark|unmark|reset` (with `--clinician` too), `medrec labels set|same|different|wrong|undo`, and paid,
  unpaid, private and provider requests in `_requests/`, below). Readers read it only
  to judge the freshness of the views made from it (views, below).
- A clinician mark (0.9.10 and later) is the household saying that a
  clinician is of no private area: no item of this person's is private by the
  `clinician` rule for them (`private.json`, below), whatever ties them.
  `printed` is the clinician as the record (`item`) shows them. Where only a
  source gave the record that clinician, `printed` is absent and `given` is
  `true`, so that no name a source gave is shown. `people` is the surnames
  (upper case) and first initials that name them: those that tie
  the record by a paper or a link, and where a day holds it, the first way the
  name reads (the last word the surname, or the words before a comma). A name
  is matched to them as the rule matches it, a surname cut short included. It
  is said at the Mac alone, of a record that the rule holds for a clinician it
  prints (`medrec private unmark <id> --clinician`, one mark for each such
  clinician), and `medrec private reset <id> --clinician` drops the marks of
  every clinician the record prints. No request says it. `clinicians`
  is absent until the first. Other rules still hold such an item, and a private
  mark on it is kept.
- A file that does not read as this person's `medrec-marks/v1` is left
  out: `bills.json` and `private.json` say `marks_unreadable`, `medrec
  bills list` and `medrec private list` say so,
  and medrec writes no mark over it until it is fixed or removed.
- A field a later medrec adds is kept when an earlier one rewrites the
  file.

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
          "documents_same_statement": 0,
          "papers_unread": 0
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
| `sign_in.needs_person` | `null` unless the source cannot be refreshed without a person: `{"reason": "sign_in" \| "keychain", "since": "…", "command": "medrec refresh sign-in <source>"}`. `sign_in`: the state is `signed_out` or `expired`, or the import itself answered that the provider needs a sign-in (exit 3). `keychain`: a scheduled pass's auth status read timed out waiting on an approval nobody gave, as a rebuilt provider CLI's first Keychain read does; the state is then `unknown`. medrec's own Drive sign-in never appears here: a scheduled pass raises no dialog for its key (0.9.7 and later), and one that cannot read it without an approval cannot reach the folder either, so it writes nothing, and `refresher.last_pass_at` goes stale. `command` is what to run in a terminal on the refreshing machine. Until it has run, neither the schedule nor a request tries the source again. An `expiring` sign-in is still refreshed. |
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
| `error` | `null`, or `{"code", "message", "remedy"[, "provider_message"]}` from the failing command's `--json` error (the family's error shape). `message` is at most 300 characters. When the provider's export answered that its sign-in ended (exit 3), `code` is `needs_person`, `message` and `remedy` are medrec's own (the remedy runs `medrec refresh sign-in <source>`), and the provider's own words, which can name the provider's own sign-in command, are in `provider_message` (0.7.0 and later; before 0.9.9 after `<cli> needs a sign-in`, which was there where the provider gave no text). `provider_message` is present only then, always (an empty string when the provider gave no text), and is at most 300 characters too. Show `remedy` as the one thing to do, and `provider_message` at most as detail. A profile name in `message`, `remedy` or `provider_message` is replaced, a provider profile's by `<provider profile>` and a medrec profile's by `<profile>`, so the folder holds none. medrec's own codes are `timeout` (a call outlived its limit), `interrupted` (the pass running it stopped before it finished) and `needs_person`. |
| `import` | The `ImportRun` id in the person's `index.json`, when `ok`. |
| `counts` | When `ok`, the import report's counts: `records_added`, `records_updated`, `records_unchanged`, `records_older`, `resources_written`, `resources_unchanged`, `documents_added`, `documents_duplicate`, `documents_unchanged`, `documents_same_statement`, `papers_unread` (0.9.9 and later) and `records_not_served` (0.9.10 and later; absent where it cannot be told). A count the import report gains later is added here too. |

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
file as found; `sha256` is `null` for one refused for its size, which medrec
does not read past the limit) and `handled`. A `done/` file is at most 8 KiB.

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
| `outcome` | From the sources' outcomes. `ok`: at least one `ok`, none `failed` or `needs_person`. `partial`: at least one `ok`, and at least one `failed` or `needs_person`. `needs_person`: none `ok` or `failed`, at least one `needs_person`. `failed`: none `ok`, at least one `failed`. `skipped`: every source skipped. `invalid`: refused by rule 1, or because it would open something the rules hold private (`reason`), with `sources` empty and `message` saying why. |
| `sources[]` | Each source the request covered: its outcome (an attempt outcome), the attempt that served it (`null` for a skip found without one), and `next_allowed_at`, the earliest time a request may import it again (rule 3). |
| `message` | One line for a person, with no record content. |
| `reason` | `kept_private` when the request was refused because it would open something the rules hold private (Private requests, below; 0.9.9 and later). Absent otherwise. A reader goes by this, never by `message`. |
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
  different record (`r_…`) or document (`d_…`) ids. A request with more
  than one of `scope`, `event`, `paid`, `unpaid`, `private` and `provider` (below), or
  with none, is `invalid` (rule 1), as is one whose `person` is not in
  `household.json`.
- The next pass writes the decision to the person's `overrides.json`, and
  the views follow before it releases the lock, once for all of a pass's
  decisions. It imports nothing, so no guard or interval applies. While
  another writer holds the folder, the request stays pending, with no
  `done/` file, for the next pass.
- Its `done/` file keeps `event` where a refresh's keeps `scope`.
  `handled.outcome` is `ok` once recorded (a decision already recorded is
  `ok` too), `invalid` when a member is not one of that person's records or
  documents, or when it would open something the rules hold private (Private
  requests, below), and `failed` when the folder could not be read or written.
  `sources` is empty.
- A `failed` decision (an event, paid, unpaid or private request) is answered
  once and not tried again: its `done/` file says `failed`, and the person
  asks again. Its `handled.message` names the step that failed in medrec's own
  words ("it failed at reading the person's marks.json") and repeats nothing
  from the folder (0.9.7 and later). The kind of error is only in the
  refreshing machine's own output (`medrec refresh` `requests[]` `cause`).

### Paid requests (0.9.1 and later)

The app's "Mark paid" is a request with `paid` in place of `scope`:

```json
{"schema": "medrec-request/v1", "id": "x_…", "requested_at": "…",
 "requested_by": {…}, "app": {…},
 "paid": {"person": "p_…", "document": "d_…", "on": "2026-11-20", "amount": "120.00"}}
```

- `amount` is a decimal string in US dollars, above zero with at most two
  places, or `{"amount", "currency"}` with a three-letter currency code.
  `on` is a `YYYY-MM-DD` day. A request with more than one of `scope`,
  `event`, `paid`, `unpaid`, `private` and `provider`, or whose `person` is not in
  `household.json`, is `invalid` (rule 1).
- It is applied as an event request is: by the next pass, to the person's
  `marks.json` `payments`, pending while another writer holds the
  folder. Its `done/` file keeps `paid`, with `amount` as two places.
  `handled.outcome` is `invalid` when the document is not one of that
  person's, or not a `bill`, and `skipped` when it is served again after the
  payment it recorded was taken back (0.9.7 and later).

### Unpaid requests (0.9.7 and later)

The app's undo of "Mark paid" is a request with `unpaid` in place of `scope`:

```json
{"schema": "medrec-request/v1", "id": "x_…", "requested_at": "…",
 "requested_by": {…}, "app": {…},
 "unpaid": {"person": "p_…", "payment": "o_…"}}
```

- `payment` is the id of a payment the household marked, as `bills.json`
  `paid[].id` gives it. A request whose `payment` is not an `o_…` id, or whose
  `person` is not in `household.json`, is `invalid` (rule 1).
- It is applied as an event request is: by the next pass, to the person's
  `marks.json` `payments`, pending while another writer holds the folder and
  for nothing else: one that could not be read or written is `failed` at once
  (Event requests, above). Its `done/` file keeps `unpaid`. `handled.outcome`
  is `ok` when the payment is not, or no longer, one of that person's: a
  request can be served twice, and two people can undo the same one.

### Private requests (0.9.1 and later)

The app's "Keep private" and its undo are a request with `private` in place of
`scope`:

```json
{"schema": "medrec-request/v1", "id": "x_…", "requested_at": "…",
 "requested_by": {…}, "app": {…},
 "private": {"person": "p_…", "item": "r_…", "private": true}}
```

- `item` is one of the person's record, document or event ids (`r_…`,
  `d_…`, `e_…`). `private` is `true`, or `null` to drop the household's mark
  and leave the item to the rules, or `false` (next). A request with more
  than one of `scope`, `event`, `paid`, `unpaid`, `private` and `provider`, or whose
  `person` is not in `household.json`, is `invalid` (rule 1).
- It is applied as an event request is: by the next pass, to the person's
  `marks.json` `private`, pending while another writer holds the folder.
  Its `done/` file keeps `private`. `handled.outcome` is `invalid` when the
  item is not one of that person's.
- **A request opens nothing the rules hold private** (0.9.9 and later).
  Whoever can write to `_requests/` can make one, so what is private otherwise
  than by the household's own mark (by a rule, a provider label, or what a
  source gave) is opened at the Mac alone, by `medrec private unmark`, `events
  apart` and `together`, and `labels`.
  - `false` records no mark. It is refused where the item is private without
    the household's mark, or is open with a private part (Private parts, below),
    and does what `null` does where it is not. An item the household marked not
    private at the Mac stays so.
  - An event request, and a provider request of any kind, is refused when
    something would not be private after it that is now, or not be left out as a
    private part, otherwise than by the household's own mark. Both are judged without the household's `true` marks,
    so that no two requests open together what neither does alone. Of an event
    that is private, each event that would have a private member of it must be
    private.
  - Refused, `handled.outcome` is `invalid`, `reason` is `kept_private`, and
    `message` is "something in it is kept off shared screens, and that can be
    changed at the Mac": everyone's outcomes show together, so it does not say
    which item. While the person's provider labels are not known
    (`labels_unreadable`), one that has to be judged cannot be: its outcome is
    `failed`, and a provider request is tried again first, as any it could not
    record.
  - Marks, decisions and corrections recorded before stay as they are.

### Provider requests (0.9.7 and later)

What the household says of a provider in the app is a request with `provider`
in place of `scope`:

```json
{"schema": "medrec-request/v1", "id": "x_…", "requested_at": "…",
 "requested_by": {…}, "app": {…},
 "provider": {"person": "p_…", "printed": ["J SAMPLE"],
   "set": {"name": "Dr. Sample"}, "item": "r_…"}}
```

- `provider` is `{person, printed, …}` with exactly one of `set`, `same` (`true`
  or `false`) and `wrong` (`true`), and `item` where it is about one item, as
  in `marks.json` (above); or `{person, undo: "o_…"}`. A correction has at most
  10 names of 200 characters, `same` at least 2, and one with an `item` exactly
  1. A part of `set` has at most 120 characters, none of them a control
  character; `phone` has 7 to 20 digits, and else only spaces, `()+-.` and `x`
  or `ext`. Anything else, another key included, is `invalid` (rule 1).
- It is applied as an event request is: by the next pass, to the person's
  `marks.json` `providers`, pending while another writer holds the folder.
  `handled.outcome` is `invalid` when the person's items do not print a name
  it gives, when the item is not theirs or does not print it, and when it would
  open something the rules hold private (Private requests, above). The same said
  again is `ok`, and so is an undo of what is not there: a request can be
  served twice, and two people can ask for the same.
- What it says is as of when its file reached the folder, by the store's time
  for the file; `requested_at` counts only where the store gives none.
  `handled.outcome` is `skipped` when
  something said of the same names and item after that stands, or was taken
  back after that; what it says of some of the same names only is recorded
  before what was said later.
- Everyone's outcomes show together, so its `done/` file keeps only
  `provider: {person, kind}` (`set`, `same`, `different`, `wrong` or `undo`),
  and `handled.message` repeats no name or text: "recorded it as o_…".
- As that file does not say what was asked, a request that could not be written
  (the folder does not answer, the person's marks do not read) stays pending,
  and each pass tries it again for 24 hours from the first that could not; then
  it is `failed`, and what it said is gone from the folder. The person's later
  provider requests wait behind it. The app may show what it asked while it is
  pending. The pass says which wait and why on its own machine (`medrec refresh`
  `requests[]`: `retrying` with a `cause`, the step and the kind of error, or
  `waiting`).

## views/ (`medrec-views/v2`, 0.8.0 and later)

The views hold each person's records again, cut into small files in the
order a reader lists them. A reader that cannot parse a whole `index.json`
per request opens only the few kilobytes its answer needs. The hosted
connector is one: it runs on Cloudflare's Workers Free plan, with 10 ms of
CPU per request, and parsing a 2.5 MB index alone takes about 6 ms.

Every view is derived from `household.json` and the person's `index.json`
and `text.json`. `events.json` also reads the person's `overrides.json`, and
the `index.json` and `text.json` of the people linked to them (Events,
below); `bills.json` and `private.json` read the same and the person's
`marks.json`.
`notes.md` has no view. It is small, the app may change
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
views/<person id>/search-parts.json      0.9.9 and later
views/<person id>/events.json            0.9.0 and later
views/<person id>/bills.json             0.9.1 and later
views/<person id>/private.json           0.9.1 and later
views/<person id>/standing.json          0.9.9 and later
views/<person id>/code-names.json        0.9.4 and later, when there are any
views/<person id>/providers.json         0.9.7 and later, when there are any
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

### Code names (`code-names.json`, `medrec-code-names/v1`, 0.9.4 and later)

The ICD-10-CM titles of the codes the person's conditions and claim
diagnoses use, from the published table on the machine that wrote the views
(README, "Code tables"). **It only labels items a reader already shows.**
By itself it lists a person's conditions, private ones included, so a
reader never lists, searches or serves it on its own. A person with no
named code has no `code-names.json`. A pass on a machine without the table
keeps the names the last pass wrote for codes still in use, splits them by
what is private now, and adds none.

```json
{"schema": "medrec-code-names/v1", "person": "p_…",
 "table": {"id": "icd10cm", "version": "FY2027", "date": "2026-06-01"},
 "codes": {"icd10cm|J30.9": {"name": "…"}},
 "private_codes": {"icd10cm|F32.1": {"name": "…"}}}
```

- **The key** is `icd10cm|` and the code in upper case, with a dot after its
  third character when it has more: `J309`, `j30.9` and `J30.9` are all
  `icd10cm|J30.9`. Only the system `http://hl7.org/fhir/sid/icd-10-cm` maps
  to it. WHO's ICD-10, ICD-10-PCS and ICD-9 do not. A code the table lacks (another year's, withdrawn or
  invalid) has no entry. A heading code has one.
- **`codes`** has the codes at least one item that is not private uses.
  **`private_codes`** has those only private items use, private as
  `private.json` says, the household's marks included. A claim is private
  by its principal diagnosis: another of its diagnoses in a private area
  counts as private too, unless the household marked the claim not
  private. When the person's `marks.json` does not read, every code is in
  `private_codes`.
- **A reader looks up a name only for an item it is showing.** In an answer
  or a page about one person it reads `codes`, then `private_codes`. In one
  about the whole household it reads `codes` only, and gives no name to an
  item that is private. `medrec mcp` does this with `plain_name` on
  `kind: "conditions"` rows and on the summary's condition lines (Readers,
  below).
- **A name never replaces the source's words.** A reader leads with the
  source's words when they are plain, and gives the name second (in Details,
  or as `plain_name` beside `name`). The name leads only where the source
  gives no words, a bare code, or wording in capitals or billing
  abbreviations. One word in capitals is an abbreviation when it has 5
  letters or fewer, and the name leads with the source's beside it; a longer
  one is the source's own word, which leads, and may be shown in sentence
  case. Filters, search, sorting, cursors and the check of whether a
  question names a private item use the source's words alone.
- **Stale** as `private.json` is. Missing or stale, the source's words stand,
  with no warning. A reader need not check which table made it
  (`code_table` in `views.json`, below).

### Provider labels (`providers.json`, `medrec-providers/v1`, 0.9.7 and later)

A name, practice, role and phone for a provider the person's items print,
with how sure medrec is and why. **A label changes nothing from a source.**
It stands beside the source's own text, which every reader still has. It
labels items a reader already shows: there is no list of providers, and no
reader searches, sorts or filters by a label. A person with no labelled
provider, of whom the household said nothing, has no `providers.json`.

```json
{"schema": "medrec-providers/v1", "person": "p_…",
 "directory": {"id": "nppes", "version": "2026-11", "date": "2026-11-08"},
 "labels": {"v_…": {"name": "Jordan Sample, MD", "practice": "Sample Health Partners",
   "role": "radiologist", "phone": "555-010-0123",
   "phone_from": {"from": "paper", "document": "d_…"}, "confidence": "likely",
   "evidence": [{"from": "only_match", "state": "FL"}, {"from": "paper", "document": "d_…"}]}},
 "by_name": {"J SAMPLE": "v_…"},
 "by_item": {"r_…": {"J SAMPLE": "v_…"}},
 "claims": {"r_…": "v_…"},
 "overrides": [{"id": "o_…", "kind": "set", "printed": ["R PLACEHOLDER"], "item": "r_…",
   "by": "Alex Testperson", "at": "2026-11-24T12:00:00Z"}],
 "private": {"labels": {…}, "by_name": {…}, "by_item": {…}, "claims": {…}, "overrides": […]}}
```

- **Looking one up.** An item is a record, a document or an event, and a
  printed name is the exact text it prints, not folded or trimmed: a record's
  `facility`, `location`, `performer`, `pharmacy`, `practitioner`,
  `prescriber`, `provider` and `service_provider`, a document's
  `organization`, an event's `place` and `clinicians`. For an item that is
  not private the label's id is `by_item[item]?.[printed] ??
  by_name[printed]`, in `labels`. For a private one it is the same two looked
  up in `private`, and the label is in `private.labels`, else in `labels`.
  No id, no label. An event member's `provider` is looked up by the member's
  `record`. A member of another person's (`person`) is looked up in that
  person's file. A bill or a receipt is looked up by its paper, and a
  `not_billed` row by its claim.
- **`by_name`** has a printed name only when every item that prints it has a
  label. **`by_item`** has the items whose label is not the one `by_name`
  gives. **`claims`** has the label of each labelled claim's `provider`, for a
  reader that shows a claim without its fields; it says nothing the other two
  do not. Such a reader tells which of the label's texts a claim prints, for
  that text's own `first_name`, by the claim's title, which ends with ` · ` and
  the provider or is the provider alone, its spaces trimmed. It shows all of
  them where the title says none.
- **`confidence`**: `household` (the household said so), `sure`, or `likely`. A
  reader treats one it does not know as likely. A likely label is marked as
  likely, with its evidence, and never leads the source's words.
- **`evidence`**, why:

  | `from` | |
  |---|---|
  | `household` | The household said so: `id` (a correction's, in `overrides`), `by` and `at`. |
  | `npi` | The record carries the provider's National Provider Identifier, and the directory has it. Every sure label is this one. |
  | `only_match` | The directory has one person of the printed surname and first initial, in the state the record's source gives (`state`; or, when it gives none, in any state the directory was read for), whose role gives the service, and no given name the text spells out is another. A person whose role medrec has no words for counts as one it can be, and is never the label. |
  | `first_name` | (0.9.8 and later) The directory has several such people, and the text (`printed`) spells out the first name of exactly one of them, who is in `state` and, where the directory gives one, `city`. It narrows only when every other is ruled out, and nothing else the record says is against the one: the state and city of its address, the service, a middle name or initial, a degree. An address with no `city` field in the record's `providers` (one an earlier medrec read) counts as against it. (The directory gives a person no practice.) An initial, a nickname, a shorter form or another spelling of a name narrows nothing, and one of the others whose first, middle or other first name begins as the printed one does, or is its beginning, is not ruled out. Like any likely label it decides nothing: no amount, no private item and no two names that are one provider come of it. A label shared by several texts has one of these for each text it came from; a reader shows a text its own, and all of them for a text that has none (one said to be the same provider's). |
  | `paper` | A paper of the same event with a practice's own heading (a bill, receipt, visit or discharge summary, prescription or immunization record, and not a laboratory's, pharmacy's or imaging center's) prints the name, and every such paper that does is of one practice. `document` is the paper. |

  A reader has no words for one it does not know, and still shows the label.
- **`name`, `practice`, `role`, `phone`** are each absent when nothing gives
  one. `role` is medrec's own plain words for the directory's code.
  `phone_from` says where the phone is from: `directory` (as of its `date`,
  which is an earlier one than the file's when a newer directory no longer has
  the provider), `paper` (the `document`, a bill or receipt that gives a number
  to call) or `household` (the `id` of what they said). A phone is shown with
  where it came from, and never as verified.
- **What the household says** (`marks.json` `providers`) wins over every match.
  What is said of an item wins over what is said of a name, and the latest over
  an earlier one.
  - `set`: the label is the household's. A part they left out is the sure
    label's, and never a likely one's: a guess is not made theirs. With only a
    phone the label stays what it is, with that phone.
  - `wrong`: there is no label, whatever one is found.
  - `same`: the names share one label, the one the household set, else a sure
    one, as the household's, with the phone they gave any of the names. With
    only a likely one it stays likely.
  - `different`: the names share no label. One label both would have, and the
    household did not set, is neither's.
  - One said of an item is outside what `same` and `different` say of its name.
  - Two names that stand for one provider in a list or a count do so by a sure
    label or the household's, never by a likely one.
  - `overrides` lists what they said, for readers to show it and to undo it, in
    the order they said it, `item` the event's id now when it followed the
    event's members. It is in `private` unless items that are not private print
    every name it gives, and when its `item` or what it sets is private.
    `applied` is `false` while its `item` is entered in error (0.9.9 and
    later), and absent otherwise.
- **The id** comes from the label's confidence, name (the printed one when it
  has none), practice, role and phone, with the folder's random `label_salt`
  (`views.json`), never from the NPI. One provider's sure label has one id in
  every person's file. No NPI is in the file.
- **Private, per person.** `private` holds what only private items say
  (`private.json`): their labels, and the evidence and phone a private paper
  gives. A label whose name, practice or role is in a private area is only
  there, whatever item prints it. A sure one, or the household's, makes the
  item that prints it private (`provider`, Private, below), unless the
  household marked it not private, and then the item has no label. A sure one
  the household said is wrong does not, nor one they set a role over when that
  role, and the name and practice the label then has, are in no private area;
  that they said two names are not one provider changes nothing about what is
  private. Where names are one provider (`same`), a private area of one is of
  all of them. A likely one makes nothing private, and an item that is not
  private does without it. When the person's `marks.json` does not read,
  everything is in `private`. A reader uses `private` only for a private item
  it is showing, in an answer or on a page about that person, and never in one
  about the whole household.
- **Machines** (README, "Provider directory"). What a directory gave is kept
  in `provider-state.json` (below), so what is labelled and what is private do
  not depend on which machine wrote last. Every machine that writes the folder
  needs 0.9.8 or later, as for `providers` in an index: one before it drops
  `first_names` from that file and every `city` from an index it saves, and no
  label comes of a first name until `medrec reindex`, then `medrec views write`
  on a machine with the directory. One with no directory, or an older
  one than the last, keeps what is there for the NPIs and names still printed,
  and splits it by what is private now; the older one adds providers whose NPI
  nothing is known of. A newer one replaces what it has, and takes away only
  a match it no longer makes. `directory` is the newest one read, and absent
  when none gave any label.
- **Entered in error** (0.9.9 and later): an item that is prints nothing here.
  It has no label and is evidence of none. What it makes private, by a sure
  label or the household's, it makes private still.
- **Stale** as `private.json` is. Missing or stale, there is no label, with
  no warning. Nothing of it is in `events.json`, `bills.json` or
  `documents.json`.

**`provider-state.json`** (`medrec-provider-state/v1`), in the person's folder
rather than in `views/`, is medrec's own: the label of each NPI the person's
records carry or carried (`by_npi`), shown or not, and those of the printed
names only one provider can be (`matched`, by record and text) or a first name
told apart (`first_names`, 0.9.8 and later, which an earlier medrec drops when it
writes the file), with the newest directory's `directory`. It is not split: it holds private providers' names
and roles with the rest. medrec judges what is private by it, in its own
answers too. No other reader reads it or serves it: one compares its MD5
(When they are written, below). Nothing else in the folder
says it again, so it stays when the views are removed, and an earlier medrec
leaves it alone. `people.<id>.provider_state` in `views.json` is its MD5. A
write is read back. When another machine wrote the file since it was read, the
pass leaves it, ends with an error and publishes no views.
When it does not read, or is gone and the views were made with one (a pass
then writes one that says `lost`), what labels make private is not known:
`private.json` and medrec's own answers say `labels_unreadable` (Private,
below), until a pass has a directory that was asked of every NPI the records
carry (`medrec tables fetch nppes`, then `medrec views write`). Until then
medrec writes nothing over a file that does not read, and then keeps its bytes
beside the new one as `provider-state~unread-<8 hex>.json`, which no pass
removes and no reader reads. It never writes over a later
`medrec-provider-state/`.

### When they are written

- **After every write.** A command that writes `household.json`, an
  `index.json` or a `text.json` brings the views up to date before it
  releases the folder lock. A refresh pass, and an import of several
  members, does it once, after the last filing. `medrec views write` does
  it on demand, and `--all` makes every person's again. `medrec reindex` (0.9.9
  and later; not with `--dry-run`) makes every person's again too, whether or not
  it writes any of them.
- **Only what changed.** A person keeps their views when their `index.json`,
  `text.json`, `overrides.json` and `marks.json` and their linked people's
  `index.json` and `text.json` have the MD5s `views.json` recorded, under the same `household.json`,
  medrec version and `build`, the code table on this machine, when there is one, is the
  one they were made with (`code_table`), CDC's vaccine tables on it, when
  there are both, are those they were made with (`vaccine_tables`), and the
  provider directory on it, when there is one, is not newer (`directory`).
  Otherwise their views are made
  again, and only views the folder does not already hold under their
  stored names are uploaded. An unchanged view keeps its name and is not
  written again. A person whose views lack one every person has
  (`summary.json`, `tests.json`, `documents.json`, `events.json`,
  `bills.json`, `private.json`, `standing.json`, `search-parts.json`) gets them
  made again: a medrec that did not record its build, or did not know it, wrote
  them before that view existed.
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
- Each of these makes only some of the person's views stale, and a reader
  that reads none of those views ignores it:

  | Differs from `views.json` | Stale |
  |---|---|
  | `overrides.json`'s MD5 from `overrides` | `events.json`, `bills.json`, `private.json`, `search-parts.json`, `code-names.json`, `providers.json` |
  | a linked person's `index.json` or `text.json` MD5 from `linked` | `events.json`, `bills.json`, `private.json`, `search-parts.json`, `code-names.json`, `providers.json` |
  | `marks.json`'s MD5 from `marks` (0.9.1 and later) | `bills.json`, `private.json`, `search-parts.json`, `code-names.json`, `providers.json` |
  | `provider-state.json`'s MD5 from `provider_state` (0.9.7 and later) | `private.json`, `search-parts.json`, `code-names.json`, `providers.json` |

  No other view reads those files: `summary.json` and `documents.json`
  say nothing of payments or privacy, and a reader applies `bills.json`
  and `private.json` to them as it answers (Readers, below). A pass writes
  `provider-state.json` before the views, so one cut short between them
  leaves these four stale. A reader compares its MD5 and does not read it,
  so it cannot judge from the files what a label makes private: while the
  person's `private.json` is missing or stale, one whose own rules judge the
  files counts as having none for a person who has a `provider-state.json`,
  or whose `provider_state` in `views.json` names one that is gone
  (`private-not-prepared`, Readers, below).
- `events.json`'s dates are those of the zone of the machine that wrote
  it (Events, below). That zone is no input `views.json` records: a
  change of it makes nothing stale, and the dates change with the next
  pass that makes the person's events again.
- A view that is missing, or whose MD5 is not the one `files` gives,
  belongs to a pass still arriving.

### views.json

```json
{"schema":"medrec-views/v2","medrec":"0.9.1","household":"<md5>",
 "people":{"p_…":{"index":"<md5>","text":"<md5>","overrides":null,"marks":null,"linked":{"p_…":{"index":"<md5>","text":"<md5>"}},
                  "row":{…},"records":{…},"timeline":{…},"search":{…},
                  "files":{"summary.json":"<md5>","records-result-0.json":"<md5>",…}}}}
```

| Field | What |
|---|---|
| `medrec` | The version that wrote them. Another version makes every view again. |
| `reader` | The oldest medrec version whose contract a reader must follow to read these views (0.9.11 and later; below): `MAJOR.MINOR.PATCH`, compared as numbers. Absent from views an earlier medrec wrote, where `medrec` stands in its place. |
| `build` | The commit that medrec was built from, where it is known (0.9.10 and later); absent where it is not. Views recorded with another commit, or with one where this medrec knows none (or the reverse), are all made again, as another version's are. Builds of one commit, or with none known, are not told apart, uncommitted changes included. |
| `household` | The MD5 (hex) of `household.json` as the views were made from it. |
| `label_salt` | What the ids of provider labels are salted with: random, made with the folder's first label and kept (0.9.7 and later). Absent before that. A reader does not use it. |
| `people.<id>.index`, `.text` | The MD5s of the person's `index.json` and `text.json` as read; `null` when the file is absent. |
| `people.<id>.overrides` | The MD5 of the person's `overrides.json` as read; `null` when there is none (0.9.0 and later). |
| `people.<id>.marks` | The MD5 of the person's `marks.json` as read; `null` when there is none (0.9.1 and later). |
| `people.<id>.linked` | For each person linked to this one (Events, below), the MD5s of their `index.json` and `text.json` as read, as `{"index", "text"}`, each `null` when the file is absent; `{}` when no one is linked (0.9.0 and later). |
| `people.<id>.code_table` | The code table the person's names were made with (`icd10cm FY2027`); absent when the pass that made the views had none, whatever names it carried over (0.9.4 and later). A pass on a machine with another table makes the person's views again, which fills in the names; one on a machine with none leaves them. |
| `people.<id>.vaccine_tables` | CDC's vaccine tables the person's vaccines from claims were read with, `<id> <SHA-256>` each (`cvx …, cpt-cvx …`); absent when the pass that made the views had not both, since one alone names no vaccine by a CPT code (0.9.11 and later). A pass on a machine with both, and other ones, makes the person's views again; one on a machine without both leaves them, and when it makes them for another reason lists only the vaccines its tables name. |
| `people.<id>.directory` | Which provider directory the pass that made the views had: the last day it covers and when it was fetched, so that a newer one sorts after (0.9.7 and later). Absent when it had none. A pass on a machine with a newer one makes the person's views again; one with an older one or none leaves them. |
| `people.<id>.provider_state` | The MD5 of the person's `provider-state.json`, absent when there is none (0.9.7 and later). Another one makes four of the person's views stale (above). |
| `people.<id>.in_error` | `standing.json`'s `in_error`, as it is there: the ids of the person's records entered in error, by kind, and of their papers, `{"condition": ["r_…"], "document": ["d_…"]}`; absent when there is none (0.9.9 and later). |
| `people.<id>.about_another` | The ids of what is in the person's chart and about somebody else (About another person, above), records and documents, in order: `["d_…", "r_…"]`; absent when there is none (0.9.9 and later). |
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
only in a release whose notes name the connector version that reads it.

A field can change what a reader must leave out or hold back, or what it must
say of an item it shows, without changing the layout: `heading` in
`private.json` (0.9.10), with which a reader that ignores it shows what it must
not, or leaves out what it need not; and `from_claim` (0.9.11), of a fill in
`standing.json` and of a vaccine in `records-immunization-<n>.json` and
`summary.json`, with which a reader that ignores it shows a payer's claim as
the pharmacy's fill or as a record of the vaccine.
`reader` (0.9.11 and later) is the oldest medrec version whose contract a
reader must follow to read the views: medrec raises it, in the release that
adds such a field, to that release's version, and its notes say so. It is
`0.9.11` now (`0.9.10` in 0.9.10's views). It is `MAJOR.MINOR.PATCH`, three decimal integers, compared as
numbers part by part and never as text: `0.9.9` is older than `0.9.10`. Where
`reader` is absent, a reader takes `medrec` (the version that wrote the views) in
its place. A reader that follows an older version's contract than that, or that
cannot parse the value, reads nothing of the views and says, as for a newer
layout, that it must be updated. Nor does it fall back to the files themselves
by its own rules, as it does for views that are stale (Readers, below): those
rules do not know the field, so it answers nothing until it is updated. medrec has
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
to its first entry's `source`, with only `fhir_base`, `cli`, from 0.9.4
`organization_id` (`null` when the source has none) and from 0.9.6 `label`:
the source's label (Provenance, above), or `null`. Each is absent in a view an
older medrec wrote. A reader shows `label`, as `medrec mcp` does. Without it, a
view holds no `imports` to find the patient by: the sources step 2 gives
with the patient unknown name it, by steps 3 and 4.

A result that is a heading (Panels, above; 0.9.10 and later) has `heading: true`,
and another has no `heading`.

`records-immunization-<n>.json` also holds the vaccines claims tell of (0.9.11
and later). Each is listed as an immunization with its claim's `id`, `date` the
line's day (`serviced`, else the claim's), `display` the line's service text,
else the claim's, `status` `completed`, `provenance` the claim's, and `fields`
`{from_claim: true, cvx: [codes]}`; `code` is the CVX code where there is one.
It is no record, and what is private of the claim is private of it, by the id;
the claim's entry in `private.json`'s `parts` is the claim's alone, and a
reader applies none of it (its `display`, its `lines`) to such an immunization.
A counted line of a claim that is not cancelled (as a reversal is), planned or
entered in error, nor denied with nothing of it paid (Standing, `filled`),
names a vaccine by its CVX code, any of its `codes` (else `service`) that is
CVX's; or, where it has none, with both of CDC's tables on the machine that
writes the views (README, "Code tables"), its CPT code's CVX codes by
`cpt-cvx` that `cvx` lists as vaccines. A line's own CVX code that `cvx` marks
no vaccine (`nonvaccine`) is none, one it does not list is one, and 998 and 999
never are. A line `private.json`'s `parts` leaves out of an open claim tells of
none in the views, since a reader could not leave it out where it leaves the
line out; `medrec mcp`'s answers that name the person, which leave nothing out
of a claim, list it, and those that name no person do not. One is listed for
each vaccine and day that no immunization of the person's has, by its CVX code,
or by its kind of vaccine where that immunization has no CVX code, and once for
two claims of it; an immunization entered in error, or not given (`not-done`, a
refusal), has none. A reader shows it as "from a claim", as `medrec mcp` does
(its `detail`). Which tables made the views is `vaccine_tables` in
`views.json`.

A record whose sources differ on whether it is entered in error, or that is, has
`says` (0.9.9 and later), unless it is a condition, an allergy or a medicine
other than a fill, whose words `standing.json` has: what each of its provenances
says, in their order, as `{source, read, status, verification,
unheard, household}`. `source` is the source's label, or `null`; `read` the day
it was last read saying so. `unheard` is `true` for one whose word is not kept,
and `household` for the household's own file. Each but `source` is absent when
it has no value.

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

`entries` holds every result of the series, newest first, but those entered in
error (0.9.9 and later) and headings (Panels, above; 0.9.10 and later). `shard` and
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
`immunizations`, `latest_results`, `recent_visits`, `bills` and `standing`
(0.9.9 and later). Each list but `standing` is `{items, more}`, at most 25
items; `standing` is uncut. It is computed as of no day, so no
bill is `past_due`; a reader works `past_due` out for the day it answers.
It is made without the household's payments: a reader leaves out the
bills `bills.json` has `settled` as it answers (0.9.1 and later). Each
medicine, condition, allergy and visit carries its record's `id`, a
vaccine its last dose's and a latest result its own (0.9.1 and later), so
a reader finds them in `private.json`. A vaccine a claim tells of (Records,
above; 0.9.11 and later) is a line of its own with `from_claim: true`, one dose
and the claim's `id`, so that a private claim's dose never adds to a line shown;
`medrec mcp`'s summary says "from a claim" of it in place of its doses. Where its
line has no name of its own it has its claim's, as the claim is shown without
its private parts, never with them. A condition whose code can have a
name carries `name_key`, its key in `code-names.json` (0.9.4 and later).

`medications`, `conditions` and `allergies` are 0.9.8's lists, by each record's
own `status`, for a reader of those. `standing` (0.9.9 and later) is what the
summary's lists are made of, since what is current changes with the day and
with what an answer may show: `medications`, `conditions` and `allergies`, each
every record that `standing.json` has as `current` or `on_hold`, uncut, in its
`order`, as the same lines. A reader makes the lists by the rules of "As a
reader lists them", cuts each, and shows a line by the first of its `records`.
Immunizations, results, visits and bills leave out what is entered in error.

### documents.json and text-<n>.bin

`documents.json` is `index.json`'s `documents` without `given`, each with these
fields added:

- `text_at`: where its text is, `[pack, byte offset, byte length,
  characters]`. Absent when the document has no text.
- `linked`: the names (`display`) of the first 20 records that list it in
  their `documents`, in `index.json` order, but those entered in error (0.9.9
  and later). Absent when none do.
- `linked_ids`: the ids of those records, in the same order (0.9.9 and later).
  A name whose record is private is of a private item, which a reader leaves
  out where it leaves that out, unless the document is private too.
- `says`: of a paper that a source says is entered in error, what each of its
  provenances says, as a record's (Records, above; 0.9.9 and later). Absent of
  any other.
- `fields` (0.9.12 and later): `index.json`'s without any field whose lines
  are among the paper's private places (Private parts, below), so that a reader
  of an older contract shows none of them; absent where none is left. Each drug
  has `medicine`, the id of the person's medicine of an equal code, where there
  is one (Fields of a paper, above).

`text-<n>.bin` holds the documents' texts, UTF-8, one after another in
document order. A pack closes before a text would take it past 1,000,000
bytes; a text is never split.

### Search

The search items are what `medrec_search` looks through, notes aside, as
the same text:

- **Documents:** the title, then the organization and document type, then
  the document's text.
- **Records:** the name, then the record's fields flattened: each text and
  number in them, but no id (0.9.9 and later: a text that is one lowercase
  letter, `_` and 20 of `a` to `z` and `2` to `7`). So no snippet has the id of
  an item that is not to be shown, and none is found by another's id. A claim's
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
- **`search-parts.json`** (0.9.9 and later): what a search finds of each open
  item with a private part (Private parts, below), by item number:
  `{"12": {"cuts": [[offset, length], …], "words": {"word": [occurrences,
  offset], "gone": [0]}, "lab": false}}`, and `{}` when there are none. The text
  that is left is the item's without its `cuts` (bytes of its text, in order),
  after the part's `display` and a line break where the first cut starts at 0.
  `words` is each word that is there another number of times, or first
  elsewhere, than the postings say, with the byte offset of its first occurrence
  in the text that is left; a word of the `display` that the postings do not
  have is there too. Any other word is there as often, its first occurrence
  moved by what is cut before it and by the `display`. `lab: false` is an item
  no lab query finds without its parts. A reader that leaves private items out,
  and whose `private.json` of the person is current while this view is missing,
  finds no item that `parts` names, and counts each it would have found in
  `private-left-out`.

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
  {"id": "e_…", "kind": "er", "start": "2024-10-08", "end": "2024-10-08",
   "title": "ER visit", "what": "Emergency room visit", "what_from": "claims",
   "place": "Sample Valley Hospital", "place_from": "claims",
   "clinicians": ["T PLACEHOLDER"],
   "confidence": "sure", "status": "complete",
   "money": {"currency": "USD", "billed": "2630.00", "allowed": "1005.00",
             "discount": "1625.00", "plan_paid": "844.00", "you_owe": "161.00",
             "share": {"copay": "100.00", "coinsurance": "61.00"}},
   "members": [
     {"record": "r_…", "kind": "claim", "title": "…", "date": "2024-10-08",
      "role": "facility", "rule": "C1", "confidence": "sure",
      "provider": "Sample Valley Hospital",
      "billed": "1800.00", "allowed": "700.00", "plan_paid": "600.00", "you_owe": "100.00"},
     {"document": "d_…", "kind": "bill", "title": "…", "date": "2024-11-19",
      "role": "paper", "rule": "D3s", "confidence": "sure", "matches": "r_…"}
   ],
   "expected": [{"what": "doctor_claims", "why": "…", "since": "YYYY-MM-DD"}],
   "differences": [{"field": "clinician", "values": [{"from": "portal", "value": "…"}, {"from": "claims", "value": "…"}], "rule": "N3"}],
   "links": [{"event": "e_…", "person": "p_…", "relation": "newborn"}],
   "could_join": ["e_…"]}
 ],
 "pending": [{"document": "d_…", "could_join": ["e_…", "e_…"], "rule": "D4"}],
 "aliases": [{"clinician": "…", "organization": "…", "rule": "N5", "document": "d_…", "confidence": "likely"}],
 "undated": ["r_…"],
 "overrides": 1,
 "events_in_error": [],
 "in_error": {"r_…": {"event": "e_…", "not_applied": [{"id": "o_…", "kind": "together", "members": ["r_…", "r_…"], "at": "…"}]}}}
```

| Field | What |
|---|---|
| `id` | `e_` and 20 base32 characters of the SHA-256 of the person's id and the event's anchor member: the claim that opened it (a facility's, a stay's first hospital claim, a day's first), else its encounter, record or paper. It holds while that member stays the anchor. |
| `kind` | `er`, `inpatient`, `outpatient`, `office`, `service`, `tests`, `dental`, `vaccine`, `fill`, `series`, `stay_no_facility` (a stay's doctors' claims, no facility claim yet), `er_no_facility` or `portal` (a portal visit with no claim). From 0.9.1 a portal visit with no claim whose encounter class is inpatient is `inpatient`, and one whose class is emergency is `er`: an `inpatient` or `er` event can have no claim member, and then expects `claim`, not `doctor_claims`. From 0.9.3 a doctor's claim for vaccines and giving them, and nothing else, is a `vaccine` event (a `service` event before), and a line that only bills for giving a vaccine (CPT 90460–90461, 90471–90474, HCPCS G0008–G0010) names nothing in its `what`. A reader shows an unknown kind by its `title`. |
| `start`, `end` | Local calendar dates, `YYYY-MM-DD`; equal for a one-day event. A time with an offset is on its own date. From 0.9.1 a UTC time is on its date in the zone of the machine that writes the views, when that is one of the US dates it can be (UTC−4 to UTC−10), else on the earlier of them; and a portal visit runs to its period's end, up to 90 days. |
| `title` | Plain words for the kind; the reader adds the place. |
| `what`, `what_from` | The portal's reason, unless it only names the kind of visit in billing words ("Return appointment"). Then, for a visit (an office, portal or ER visit; 0.9.3 and later), its visit code's plain name, the portal's encounter code before the claims' lines (below). Else that reason, or a portal visit's CPT descriptor, else the claims' services. A name written in capitals is put in title case, its acronyms kept (0.9.3 and later). `portal` or `claims`. A fill's or a vaccine's is its claims' names, else (0.9.1 and later) the fills' or doses' own names from the portal, as "A", "A and B" or "A, B and N more". Absent when nothing says. |
| `place`, `place_from` | The portal's organization: the encounter's service provider, else its location, else (0.9.3 and later) its source's name (from 0.9.6 the person's own source's, by "Which source", above, and none where its encounters' sources have different names, one with no name or a connection's included). Else the facility claim's provider, else the first claim's, else an N5 alias; `portal`, `claims` or `alias`, or, after the facility claim's and before the first claim's, `linked`: the facility claim's provider of the linked parent's stay it was `born_during` (X1; a reader shows it as "A place that is linked", under Private, below, says). A clinician's name is never a place. Nor (0.9.3 and later) is a records connection's: a name ending "(all practices)", or a portal platform's own name such as athenahealth or MyChart; no alias is learned to one. |
| `clinicians` | As the portal or the claims name them. |
| `confidence` | The lowest of its members': `sure`, `likely` or `unsure`. |
| `status` | `unsure` when the confidence is; else `waiting` when something is `expected`; else `complete`. |
| `money` | The claim members' claim-level amounts, summed, as decimal strings: `billed`, `allowed`, `plan_paid`, `you_owe`, `discount` (`billed` − `allowed`) and `share`, the reasons for `you_owe` that the claims give (`deductible`, `copay`, `coinsurance`, `not_covered`). An amount some claim lacks is left out and named in `partial`. A claim with no `you_owe` of its own counts its share's parts added up, and `you_owe_added` is then `true`. A claim whose `allowed` is below zero counts `plan_paid` + `you_owe` as its allowed, and one with neither leaves `allowed` out. Absent with no claim. |
| `members` | Every record and document in the event, each with `record` or `document`, its `kind` (a record kind, or a document type), its `title` (at most 120 characters) and `date`, its `role`, the `rule` that placed it and that placement's `confidence`. A claim adds `provider` and its amounts. A bill joined by its amount due (D3s) adds `matches`, the claim whose share it bills. `person` marks another person's paper (X2). `differs` is `true` of one whose sources differ on whether it is entered in error (0.9.9 and later), and absent otherwise. `role` is `facility`, `hospital`, `ancillary`, `office`, `dental`, `fill`, `vaccine`, `portal`, `contact`, `satellite` or `paper`; a reader ignores one it does not know. |
| `expected` | What has not arrived: `doctor_claims`, `facility_claim`, `claim` or `bill`, with `why` in words and `since`, the day it was first expected. `doctor_claims` is expected of an `er` or `inpatient` event that has a facility claim and no other; `claim`, of a portal visit with no claim, whatever its `kind`. A fill or a vaccine expects no bill. |
| `differences` | Where the sources disagree (a clinician's name, a date), each source's value, and the rule. |
| `links` | Related events of a linked person: `newborn` (from a parent's stay to the child's care in it) and `born_during` (back). A reader ignores a relation it does not know. |
| `could_join` | For an `unsure` event, the events it may belong to. |
| `pending` | Papers that fit two or more events, merged into none. |
| `aliases` | Clinicians learned to belong to a practice, with the paper that says so. |
| `undated` | Claims with no date, in no event. |
| `overrides` | How many of the household's decisions applied. |
| `events[].in_error` | The ids of the person's own items entered in error that the event would have (0.9.9 and later): a list, where the document's own `in_error` is an object by id. Absent when there are none. |
| `events[].not_applied` | The household's decisions naming one of those, each with the `id`, `kind`, `members` and `at` it has in `overrides.json`, which alone says who asked for it (0.9.9 and later). Absent when there are none. |
| `events_in_error` | The events that are none without what is entered in error, as they would be, newest first (0.9.9 and later). A reader lists none, counts them ("N entered in error, not shown"), and shows one only when asked for it by its id, as entered in error. Absent when there are none. |
| `in_error` | Every item of the person's entered in error that an event would have or a decision names, by id (0.9.9 and later): `event`, the event that has the rest of it, else the one in `events_in_error`, absent when it would be in none; and `not_applied`, the decisions naming it, absent when there are none. Absent when there are none. |
| `overrides_unreadable` | `true` when the person's `overrides.json` does not read as `medrec-overrides/v1`: the events are the rules' alone, and medrec writes no decision over the file until it is fixed or removed. Absent otherwise. |

Plain visit names (0.9.3 and later), bundled with medrec and never fetched,
from CPT evaluation-and-management codes:

| Codes | Name |
|---|---|
| 99202–99205 | New-patient visit |
| 99211–99215 | Follow-up visit |
| 99381, 99391 | Well-baby checkup |
| 99382–99384, 99392–99394 | Well-child checkup |
| 99385–99387, 99395–99397 | Preventive checkup |
| 99460–99463 | Newborn care |
| 99281–99285 | Emergency room visit |

`money` for an office visit whose one claim gave no `allowed` and no
`you_owe`, only its deductible:

```json
{"currency": "USD", "billed": "110.00", "plan_paid": "45.00", "you_owe": "20.00",
 "you_owe_added": true, "share": {"deductible": "20.00"}, "partial": ["allowed"]}
```

- **Every claim is in exactly one event**, but those entered in error. A portal
  visit, a result or a paper joins at most one.
- **Entered in error** (0.9.9 and later). The events are made of what is not:
  no claim, record or paper entered in error is in one, adds to its money,
  names it, is expected of it or joins others to it. Its event's id changes
  only when it was the anchor. A linked person's are left out too, and are
  named in that person's own file alone.
- **A portal visit's records.** Records from the same portal source join a
  portal visit on its day, a result or report also the day before (E4),
  and from 0.9.1 on any day of the visit's period, as a result during a
  stay does. With no claim, a day's vaccine doses from one source are one
  `vaccine` event (0.9.1 and later; one event per dose before).
- **Linked people.** A person's events read the records of their guardians
  and children, when the child has a birth date: a newborn's care during a
  parent's birth stay links to it (X1). A paper filed under a child and dated
  before the child was born, other than an EOB or an insurance card, joins the
  parent's visit on its day, or one that ended the day before (X2): the one it
  names, else the only one.
- **Order.** Newest first by `start`, then by `end`, then by `id`, so a
  cursor over the list holds between calls.
- **Size.** One file per person, about 1 KB per event: a person with 200
  claims takes about 150 KB. A reader parses it whole; it stays one file up
  to 1 MB, and a person past that would take a later schema that cuts it
  the way the timeline is cut.
- **No `private` field.** A private event is listed in the person's
  `private.json` (Private, below), with the records and documents, so that
  a household's mark changes no `events.json`. A reader before 0.9.1, or
  of views without `private.json`, has not judged any item: its absence
  never means "not sensitive".

### Bills (`bills.json`, `medrec-bills/v1`, 0.9.1 and later)

Each of a person's bills with the claim it bills, what was paid against
it and what is left; the bills and desk payments worth a call; and the
shares the claims say are owed that nothing has asked for or paid yet.
Every person has a `bills.json`, with an empty `bills` and empty lists
when they have no bills. It is made from the person's documents, their
`events.json` and the `payments` in their `marks.json`.

```json
{"schema": "medrec-bills/v1", "person": "p_…",
 "bills": {
   "d_…": {"copies": ["d_…"], "organization": "Sample Imaging House",
    "title": "Statement from Sample Imaging House", "date": "2024-10-08", "currency": "USD",
    "amount_due": "90.00", "due_date": "2024-10-29", "phone": "555-010-0155",
    "event": "e_…", "rule": "D2", "claim": "r_…", "claim_you_owe": "72.00",
    "paid": [{"from": "receipt", "document": "d_…", "on": "2024-10-21", "amount": "50.00", "currency": "USD"},
             {"from": "household", "id": "o_…", "on": "2024-11-04", "amount": "25.00", "currency": "USD"}],
    "paid_total": "75.00", "left": "15.00", "settled": false}
 },
 "copy_of": {"d_…": "d_…"},
 "receipts": [
   {"document": "d_…", "organization": "Sample Imaging House", "on": "2024-10-21",
    "amount": "50.00", "currency": "USD", "bill": "d_…"}
 ],
 "worth_a_call": [
   {"reason": "asks_more", "bill": "d_…", "claim": "r_…", "event": "e_…",
    "asked": "90.00", "owed": "72.00", "currency": "USD"}
 ],
 "not_billed": [
   {"event": "e_…", "start": "2024-05-21", "provider": "Sample Corner Pharmacy",
    "claims": ["r_…"], "you_owe": "7.50", "currency": "USD", "fill": true}
 ],
 "in_error": {
   "d_…": {"as": "bill", "not_applied": [{"from": "household", "id": "o_…", "on": "2024-11-04", "amount": "25.00", "currency": "USD"}]},
   "d_…": {"as": "copy", "bill": "d_…"},
   "r_…": {"as": "claim"}
 }}
```

- **`bills`**, by document id, newest first by `date` (the statement date,
  else the document's), then by id: every `bill` that is not
  `same_statement_as` another. A reader joins it to `documents.json` by
  id. `copies` lists the later copies of the same statement, and
  `copy_of` maps each copy to the bill it copies.
  `amount_due`, `due_date` and `phone` are the statement's, each absent
  when it gives none.
- **Its event and claim.** `event` is the event holding the bill or one of
  its copies, and `rule` the rule that put it there. `claim` is the claim
  it bills: the one D3s matched by amount, else the event's one claim from
  the bill's organization, else the event's only claim. `claim_you_owe` is
  that claim's share. Each is absent when there is none.
- **`paid`**, oldest first: receipts that pay it (`from: "receipt"`), and
  the household's payments on it or a copy (`from: "household"`, by its
  `o_…` id). A receipt pays the bill from its organization, or with its
  account number where both give one, whose date is 0 to 120 days before
  the receipt's `paid_on`: the one whose `amount_due` equals the amount
  paid, else the only one. `paid_total` sums those in the bill's currency;
  one in another currency is listed with `counted: false`.
  `left` is `amount_due` less `paid_total`, never below zero, and
  `settled` is `true` once it is zero; both need an `amount_due`, and
  `settled` is `false` without one.
- **`self_pay`** (0.9.6 and later): what a bill with no `claim` charges for
  that no insurer is billed for, by its first page that has any text.
  `storage` is a fee to store samples or tissue; `stated` is a
  bill that says it is self-pay. Absent otherwise. A reader asks for no
  claim, and treats a value it does not know as `stated`.
- **`superseded_by`**: the newest later bill from the same biller (the same
  account number where both give one, else the same organization). Its
  balance includes this one's, so a reader asks for that one, not both.
- **`receipts`**, newest first: every receipt with an `amount_paid`. `bill`
  is the bill it pays. One that pays no bill, paid on a day of an event
  whose place or a claim's provider is its organization (the only such
  event), is a desk payment: `event` names that visit.
- **`worth_a_call`**, the bills' order, then desk payments. A `settled` bill
  is never in it, for any reason (0.9.6 and later):
  - `asks_more`: the bill asks more than its claim's share.
  - `asks_less`: it asks less, above zero.
  - `no_claim`: it asks above zero and its event has no claim, or it is in
    no event and follows no earlier bill from the same biller. Insurance
    may not have been billed. A `self_pay` bill is not one (0.9.6 and later).
  - `desk_copay`: a desk payment above what that visit's claims from its
    organization (else all its claims) say is owed; `bill` is the receipt.

  `asked` is the bill's `amount_due` or the amount paid at the desk, and
  `owed` the claims' share.
- **`not_billed`**, the events' order, then by provider: each event's
  claims with a share above zero that no bill and no desk payment has met,
  summed per provider. A bill meets its `claim` and its event's claims from
  its organization; a desk payment, the claims it was compared with. `fill`
  is `true` for a pharmacy fill, usually paid at the counter, and absent
  otherwise.
- **Entered in error** (0.9.9 and later). The bills are made of the papers,
  events and claims that are not. A bill is all the papers that are one
  statement, so one whose first paper is entered in error is its next copy's,
  with its payments, and it is out only when every paper of it is.
  - `bills.<id>.in_error` lists the papers of it that are, and `differs` is
    `true` when the sources of its first paper differ on it; its amounts are
    those of a source that does not say so, counted once. Each is absent
    otherwise.
  - The top-level `in_error` has each paper and claim that is, by id, and what
    it is (`as`): a `bill` none of whose papers stands, by its first, with
    `not_applied`, the payments the household marked on it and the receipts
    that paid it, as a bill's `paid` has them (absent when there are none); a
    `copy`, of the `bill` it names, which stands or not; a `receipt`; or a
    `claim`. A reader takes an `as` it does not know as one to count. Absent
    when there are none.
  - Those payments are in no total and settle nothing. They pay no other bill:
    a receipt that paid one entered in error has `bill_in_error` and no `bill`
    or `event`, unless another bill is its amount to the cent. One that could be
    of it or of another bill, and is neither's amount, pays neither. A receipt
    that is entered in error itself pays nothing.
  - The bill is in no `paid_total`, `left`, `worth_a_call` or `not_billed`, and
    no bill is `superseded_by` it. Its claim is as if nothing had asked for it.
  - A page of the person's bills counts those it may show: "1 bill entered in
    error, not shown", "2 bills entered in error, not shown". Where the
    household marked a payment on one, the line says so instead, since money may
    be owed back: "1 bill entered in error, with a payment you marked", "2 bills
    entered in error, 1 with a payment you marked", "2 bills entered in error,
    each with a payment you marked".
- **`marks_unreadable`**: `true` when the person's `marks.json` does not
  read (above), so no household payment is counted. Absent otherwise.
- Amounts are decimal strings with two places. A reader treats a field it
  does not know as unknown.

### Standing (`standing.json`, `medrec-standing/v1`, 0.9.9 and later)

What is current among the person's conditions, allergies and medicines, by
what each of their sources says of it (`words`, Records, above). No source's
word wins over another's. Every person has one.

```json
{"schema": "medrec-standing/v1", "person": "p_…",
 "sources": ["Example Health", "Sample Clinic East"],
 "order": {"condition": ["r_1…"], "allergy": ["r_2…"], "medication": ["r_3…", "r_4…"]},
 "records": {
   "r_1…": {"kind": "condition", "is": "current", "line": "http://example.org/codes|C-1",
            "differs": true,
            "words": [{"source": 0, "read": "2024-05-02", "is": "current", "status": "active"},
                      {"source": 1, "read": "2024-05-03", "is": "ended", "status": "resolved"}]},
   "r_2…": {"kind": "allergy", "is": "current", "line": "~sample nut",
            "words": [{"source": 1, "read": "2024-05-03", "is": "ended", "status": "inactive"}],
            "unheard": [0]},
   "r_3…": {"kind": "medication", "is": "current", "line": "http://example.org/codes|M-1",
            "until": "2024-03-31", "until_of": "prescription",
            "filled": [{"record": "r_5…", "date": "2024-04-02"}],
            "words": [{"source": 0, "read": "2024-05-02", "is": "current", "status": "active",
                       "end": "2024-03-31", "end_of": "prescription"}]},
   "r_4…": {"kind": "medication", "is": "current", "line": "http://example.org/codes|M-2",
            "differs_after": "2024-03-31",
            "words": [{"source": 0, "read": "2024-05-02", "is": "current", "status": "active",
                       "end": "2024-03-31", "end_of": "prescription"},
                      {"source": 1, "read": "2024-05-03", "is": "current", "status": "active"}]}},
 "in_error": {"condition": ["r_6…"], "result": ["r_7…"]},
 "no_problem_list": [{"source": 1, "records": ["r_8…"]}]}
```

- **`sources`**: the labels of the sources ("Which source", step 4), each at its
  place; two can have one label. It is null where there is none: of a
  provenance with no `cli`, and of a word that the record does not say whose it
  is. A reader says "a source with no name", and names a source only by a word
  or an `unheard` of a record it shows.
- **`order`**: of each kind, its records by full date, newest first, then by
  name (`display`), then as `index.json` holds them.
- **`records`**: every condition, allergy and medicine by id, but a fill
  (`source: dispense`), which is a fact and has no standing. A reader leaves
  out a `kind` it does not know.
- **`words`**: what each source says, in the order of the record's provenances:
  `source`; `read`, the day it was last read saying so (`fetched_at`'s, else
  `filed_at`'s); `is`, below; and `status`, `verification`, `abatement`, `end`
  and `end_of` as the record's word has them. The source that wrote a record's
  fields ("Whose fields") and keeps no word, or is its only one, has the
  record's own: its `status`, and its fields' `verification_status`,
  `categories` (else `category`, as the one), `abatement` (else
  `abatement_text`), `end` and `end_of`. A record that keeps no word and has
  several sources, or none, has its own as its one word, of no source and with
  no `read`.
- **`unheard`**: the record's other sources, whose words it does not keep: an
  earlier medrec saved it, and neither an import of theirs nor `medrec reindex`
  has read it since.
- **`is`**, of a word and of the record. Codes compare lowercased, with spaces
  and `_` as `-`.

  | `is` | A word is, whose | In |
  |---|---|---|
  | `current` | is none of those below, `unknown`, `provisional`, `differential` and `unconfirmed` too | the current list, with its word |
  | `on_hold` | medicine's status is `on-hold` | a group of its own, "On hold" |
  | `draft` | medicine's status is `draft` | the list of all |
  | `ended` | status is `completed`, `stopped`, `inactive`, `resolved`, `remission` or `cancelled`, or a medicine's `not-taken` or `refuted`, or which has no status and is a condition with an abatement | the list of all |
  | `refuted` | status or verification is `refuted`, of a condition or an allergy | the list of all |
  | `visit` | categories have `encounter-diagnosis` and not `problem-list-item`, of a condition that would be `current` | the list of all, with its category |
  | `error` | status or verification is `entered-in-error` | none, unless asked for by its id |

  A record is the first of these, in this order, that any of its words is. So
  it is current while any source says so, and one entered in error counts for
  nothing beside another word. One with an `unheard` source is current: that
  source may say so. A reader takes an `is` it does not know as `current`, and
  shows the word.
- **`line`**: its code with its system, or `~` and its name with case and
  accents folded. Conditions, or allergies, with one line are one line of a
  list. Medicines are not: each prescription is its own, and a reader may put
  those with one line under one heading.
- **`until`** and **`until_of`**, of a current medicine whose every current
  word has an end, with no source unheard: the last day of the last to end, and
  what of it ends (`end_of`). A month or a year ends with its last day, and an
  end that is no date is none. After that day it is not current but in a group of its own, "Ended by its date",
  with the day and what ended. An allergy or a condition never stops being
  current by a date.
- **`filled`**: the fills of the medicine after `until`, newest first, of one
  day the greater record id first: those whose `status` is `completed` or
  `unknown`, or that have none. Of a medicine ended by its date a reader shows
  the first it may, a fact beside it that changes nothing. A pharmacy claim is
  one too, with `from_claim: true` (0.9.11 and later; absent otherwise), and a
  reader says "from a claim" wherever it shows it. Its `record` is the claim, and
  what is private of the claim decides whether it is shown. It is a fill of each
  medicine that an RxNorm code or an NDC of a line its totals count (any of the
  line's `codes`, else its `service`'s) is equal to
  (the medicine's own, or the `ndc` of a fill of it; an NDC as eleven digits, five,
  four and two, where it has hyphens), else of each whose folded name is the
  drug's in its `display`. One that is cancelled (as a reversal is) or entered in
  error is none, and neither is one the plan denied of which the household paid
  nothing: each line its totals count has an adjudication of category
  `denialreason` or `rejectreason` (by code or name), and the member's share of
  the claim and of each such line is nothing or not given. Neither is one where a
  fill of that medicine and day is filed that the pharmacy gave (`completed`,
  `unknown` or no status, as above): one it cancelled, declined, stopped or is
  still making is none, and the claim's is the fill. A claim makes no
  medicine, and says nothing of what is current.
- **`differs`**: its words differ: some say another `is` than the rest (`visit`
  and `error` say nothing here), or give another `verification`.
  **`differs_after`**: they differ after this day, when a word that says it is
  current ends and another has not.
- **`in_error`**: the person's records entered in error (index.json, "Entered
  in error"), by kind, those with no standing too (a fill, a result, a
  vaccine), and their papers as `document`. `views.json` has the same object,
  for a reader that opens no other view.
- **`no_problem_list`**: each source with a diagnosis given for a visit and no
  other condition, heard or not, and those diagnoses. A condition that is
  `error`, and a word of no source, are neither.

**As a reader lists them**, of the records it may show, on the day it answers:

1. A medicine that is `current`, with `until` before that day, is ended by its
   date. Every other record is where its `is` says.
2. The current list has one line for each `line` of the conditions or the
   allergies, and one for each medicine. A line is current while any of its
   records is. Its name is that of the first of those in `order`. The other
   groups have one for each record.
3. A line differs when one of its current records does (`differs`, or
   `differs_after` before that day), or when a word on the line is current and
   a source of which none is has one that is ended or refuted. A source's own
   earlier episode that ended does not differ from its current one.
4. A line says what each source says, with its name and `read`: its `status`,
   and its `verification` unless that is `confirmed`; then what those records
   say that are not current and have an ended or refuted word of a source of
   which no word on the line is current; and names the sources not heard.
5. What is in `in_error`, of any kind, is out of every list, and of the
   timeline and the search, and the list says how many: "1 entered in error,
   not shown" (a page of bills as Bills, above, has it). The views' other files
   hold it as they hold the rest, and one opened by its id says that it is
   entered in error, what each source says (`says`), and what the household
   decided of it, which is kept and not applied.
6. A list of conditions names each source in `no_problem_list` with a diagnosis
   there that the answer may show: "No problem list from <source>; diagnoses
   from visits are listed under All".
7. A record the answer may not show (`private.json`) is left out before the
   rest is worked out: its word, its fill and its sources are not shown, it
   makes no line current or differ, and rules 5 and 6 do not count it.

A reader without a current `standing.json` of a person (views of 0.9.8 or
before), and with no rules of its own to make it from `index.json`, lists their
conditions, allergies and medicines by `status`, as 0.9.8 did, and says nothing
of sources that differ.

`tests/fixtures/standing/` in medrec's repository has a person's `index.json`,
the `standing.json` it gives, and `lists.json`, the lists a reader makes of it.
Each case has why, the kind, the day, the records the answer may not show
(`hidden`), and `lists`, a shape of the fixture's alone: `current`, `on_hold`
and `ended_by_date` are lines, each with `records` (those of it that are there,
the first its name's), `against` (those that rule 4 adds),
`differs`, `filled` (the fill shown) and `unheard`; `in_error` is how many, and
`without_problem_list` the places in `sources` of those that rule 6 names.

### Private (`private.json`, `medrec-private/v1`, 0.9.1 and later)

The person's records, documents, events and imaging studies about mental
health, reproductive and fertility care, sexual health or substance use, and the
medicines tied to them. **It is discretion, not access control.** A reader
keeps them off shared screens (the household's Home, its Everyone calendar,
a doctor sheet for someone else) and out of answers about the whole
household. It shows them on that person's own pages, and in answers about
that person or that name the item. Anyone who can read the folder can read
every one of them. Every person has a `private.json`, with empty maps when
nothing is private.

```json
{"schema": "medrec-private/v1", "person": "p_…",
 "records": {"r_…": {"area": "substance_use", "from": "code"}},
 "documents": {"d_…": {"area": "substance_use", "from": "provider"}},
 "events": {"e_…": {"area": "substance_use", "from": "members"}},
 "studies": {"m_…": {"area": "substance_use", "from": "report"}},
 "unreported_studies": ["m_…"]}
```

- **`area`**: `mental_health`, `reproductive`, `sexual_health`,
  `substance_use`, or `household` for an item the household marked private
  that no rule gives an area. A reader treats an area it does not know as
  private all the same.
- **`from`**, why it is private:

  | `from` | The item |
  |---|---|
  | `source` | came from a portal whose records are all of one area: a behavioral health or a fertility clinic's record system |
  | `service` | is a claim each line of which that its totals count is of a service in an area (the first line's), or one with no such line whose own name says it; or a procedure that is one (0.9.9 and later; Services, below) |
  | `code` | is a condition whose ICD-10 code is in an area, or a claim whose principal diagnosis (the lowest `sequence`) is: F10–F19 and Z71.4–Z71.5 substance use, and (0.9.9 and later) Z71.6, Z72.0, Z77.22, Z87.891 and O99.33, and (0.9.10 and later) Z81.1–Z81.4, O99.31 and O99.32; the other F codes mental health, and (0.9.10 and later) Z13.3, R45.851, T14.91, Z91.5, Z86.5, the rest of Z81, and O99.34; Z30–Z32, Z33.2, O00–O08, N46 and N96–N98 reproductive, and (0.9.10 and later) Z79.3 and Z97.5; A50–A64, B20, Z21, Z11.3–Z11.4, Z20.2, Z20.6 and Z72.5 sexual health, and (0.9.10 and later) Z29.81. Each with the codes under it. Pregnancy itself is none |
  | `provider` | was made by a practice or clinician whose name or credential says its area: a claim's provider or facility, a visit's place, location or clinician, a medicine's prescriber, a result's or (0.9.11 and later) a report's performer, a document's organization; or is a record that prints a provider with a sure label (Provider labels, above) whose name, practice or role does, or a record, document or event that prints one the household calls so (0.9.7 and later) |
  | `clinician` | is an item (0.9.10 and later) that prints a clinician, by surname and first initial: a claim's `provider`, a visit's `practitioner`, a medicine's `prescriber`, or a procedure's, result's or report's `performer`, and (0.9.11 and later) an immunization's. It is private where a paper of the person names that clinician: in its `organization`, where that says an area as `provider` has it, or in its `title`, where that or the organization does. A claim is private too where it prints a clinician and is of the day of a `bill`, `receipt`, `discharge_summary`, `visit_summary`, `imaging_report` or `lab_report` from a practice whose organization says an area, and is not a pharmacy's (with "pharmacy" in its name) nor a records connection's name (`learned: day`): every such claim of that day, however the events are grouped. A claim of that day that prints a place is not. And an item is private where it prints a clinician linked to such a practice (`learned: linked`): by two days or more of such papers of one area on each of which the clinician has a claim, or by one such day on which they are the only clinician with a claim: a claim of the day prints them as shown or as a source gave it, and no claim of it, each version of which prints a clinician, is of another. A day alone holds no claim of another day. A surname cut to 8 letters or more is the one it begins. A name with no initial, no degree and no comma can be a place's: a claim's provider or a performer that is one is looked for in an organization alone, and is linked by no day. A result's and a report's `performer` is read from 0.9.11, when medrec first keeps one, and an immunization's from 0.9.11 too. The papers and the items are each read as shown and as a source gave them, then or now, those entered in error too, and the links are made again on each run, so a later claim of another clinician on a day can end a link by that day but not one by two days. An item private only by what a source gave of any of them, and none shows, has `given`; what is entered in error does not by itself make it so. Only the person's own papers and items are read, never another person's. A likely match: it holds the item back, and names, joins, sets a status and settles nothing. The household opens one item at the Mac (`medrec private unmark <id>`), or every item that a clinician makes private (`medrec private unmark <id> --clinician`, marks.json's `clinicians`, above) |
  | `medicine` | is a medicine of an area by its name, its brand (0.9.10 and later; Brands, below) or code, or (0.9.9 and later) a pharmacy claim its `display` names as one: antidepressants, antipsychotics, anxiolytics, stimulants and lithium; fertility drugs and contraceptives; HIV medicines and those for erectile dysfunction; medicines for substance use and smoking |
  | `test` | is a result or report of a test of an area: sexually transmitted infections, pregnancy and fertility, drug screens; or (0.9.9 and later) of a questionnaire of one, or of tobacco use (Questionnaires, below); by its own name and code or those of any of its parts, or (0.9.10 and later) by a text that is its value or the value of a part, or a report's `conclusion`, as a line of a paper's text is (Private parts, below): by the words of a service, a medicine, a test or a questionnaire; or (0.9.10 and later) by its own name or a part's, as a visit's (`words`). A name that is "Gender identity" and no more is none |
  | `members` | is an event with a private claim or portal visit, or whose own members are all private, or (0.9.10 and later) a panel or a report all of what one source lists of which (Panels, above), now or before, is private, with what is entered in error or without it |
  | `event` | is a member of a private event |
  | `words` | is a bill or receipt whose first page that has any text says an area, by a service, a medicine or a test, or any document whose title does (0.9.6 and later), or whose title is a questionnaire's, or with an `original_name` that says either (0.9.9 and later; `_`, `.` and `-` read as spaces); or (0.9.10 and later) a condition with no ICD-10 code whose name, or its code's, is of a condition of an area, or a visit whose name or `reason` is, or says one as a paper's title does |
  | `record` | is a document that a private record links (`documents` in `records`), whatever else links it (0.9.5 and later) |
  | `report` | is an imaging study whose `report`, a document or record, is private (0.9.5 and later) |
  | `paper` | is a document tied to a private one, either way: its copy (`same_statement_as`, `near_duplicates`), the bill that supersedes it or one it supersedes (`superseded_by` in `bills.json`), or the receipt that pays it or the bill it pays (0.9.6 and later) |
  | `household` | was marked private by the household; its `area` is the one the rules give it, else `household` |

- **`given`** (0.9.9 and later): `true` where the item is private only by what a
  source gave, then or now, that no item shows (What a source gave, above): of
  the item, or of the one it is private with. Absent otherwise. A reader shows
  the area and no more of it. A row of `medrec private list` has it too (`yes`
  in its table's `given`).
- **`learned`** (0.9.10 and later): of an item private by its clinician, what
  ties them to the area where no paper names them: `day` (the item is of the day
  of a practice's paper, and no more) or `linked` (the clinician is linked to
  such a practice). Absent where a paper names them, and on every other item. A
  row of `medrec private list` has it, and its table says it in `from`. The
  list's `clinicians_opened` (absent when there are none) has the household's
  clinician marks: `id`, `clinician` (the mark's `printed`: the clinician as the
  record shows them, `null` where only a source gave them, or where the record
  shows them no more or is entered in error), `given` (`true` where `clinician`
  is `null`; absent otherwise), `item`, `at`, `person` and
  `person_name`.
- **`heading`** (0.9.10 and later): `true` on a private record that is a heading
  (Panels, above): it has no entry in `tests.json`. Absent otherwise.
- **What a source takes back still tells** (0.9.9 and later). Nothing opens
  because a record or a paper is entered in error, the person's or one of a
  person their events read (X1, X2): each rule is judged with what is and with
  what is not, events as they would be with it included, so an event in
  `events_in_error` can be here. A study with no report, with what is
  entered in error or without it, is in `unreported_studies`; so one whose only
  report is, which is in `studies` too when that report was private.
- **Brands** (0.9.10 and later). Wherever a text is judged by a medicine's words,
  its brands are among them: a bill's page, the lines of a paper, a service, a payer note, a
  text value and a conclusion. A brand that is a person's name too, or a word
  ("Camila", "Plan B"), is one only as a medicine's name or code, or in a pharmacy
  claim's `display`, and in no other text.
- **Questionnaires** (0.9.9 and later). A result or a report is a
  questionnaire's by the LOINC code it has, else by the words of its name or of
  its code's, and (0.9.10 and later) a result by the same of any of its parts, for
  what is private and for what is about another person, and a document by those of its title. So is a question of one, and
  its score.

  | Questionnaire | Area | Who answers it of themselves |
  |---|---|---|
  | postnatal depression | `mental_health` | a grown person |
  | depression, anxiety | `mental_health` | a grown person; a child, where the words say it is one for children, or only "anxiety" |
  | suicide risk and self-harm | `mental_health` | anyone |
  | other mental health: trauma, mood, attention, obsessive-compulsive and eating disorders, psychosis, a child's symptoms and difficulties | `mental_health` | anyone |
  | alcohol use, drug use | `substance_use` | a grown person |
  | tobacco use: smoking status, amount, smoke around the person, nicotine and its tests, whatever the answer | `substance_use` | anyone |

  A child's growth, hearing and development are none, nor is a screening for
  autism, or a laboratory's alcohol or drug test (a drug screen is a `test`).
- **Services** (0.9.9 and later). A service is in an area by its CPT or HCPCS
  number, else by the words of its name: those of a paper's title, of a medicine,
  of a test or of a questionnaire. A claim's line is too by its revenue code, by
  that code's name, and by the words of what its payer notes of it. The numbers,
  in ranges, are in medrec's `src/services.rs`; a code whose `system` is of
  medicines, results, findings or diagnoses (NDC, RxNorm, LOINC, SNOMED, ICD-10)
  is none. A claim's category, its amounts and its dates say nothing.
- **Private parts** (0.9.9 and later). A claim of several services, a paper and an
  event stay open when only a part of them is private. `parts` has what is
  private in each open item, by its id, and how the item is shown without it.
  Where a reader leaves private items out (a shared screen, an answer that names
  no person), it leaves these out of open ones, so no title, search, count or
  event is made of them. Amounts stay, whatever they are for.

  Every `private.json` of 0.9.9 and later has `parts`, `{}` when there are none.
  One without it is of a medrec that judged none, as it judged by none of the
  other rules marked 0.9.9, and is current for those it has: it never says that
  nothing in an open item is private. A reader with rules of its own judges the
  parts from the files. One without shows what that medrec's rules show, until
  the next medrec that writes makes the views again. The places are those of the
  `index.json` and `text.json` the view was made from, so a reader applies them
  to no other, and shows nothing of a list or a text they are not places of.

  ```json
  {"parts": {
    "r_…": {"display": "Office visit · Example Pediatrics", "date": "2024-05-06",
            "lines": [1], "says": "1 service not shown"},
    "d_…": {"text": [[38, 26]], "says": "1 line not shown"},
    "e_…": {"what": "Office visit", "place": null, "says": "1 service not shown"}}}
  ```

  - **A claim** has `lines`, `diagnoses` and `payer_notes`: the places, from 0,
    of what is private in its `items`, its `diagnoses` and its `payer_notes`,
    each absent when there is none. A line is by its service (above), a diagnosis
    by its ICD-10 code (`code`, above), and a payer's note by its words; and one
    in whose place any version of any of its sources gave one that is. `display`
    is its name by the lines that are left, `category` theirs where that is
    another (null where none is left), and `date` its date.
  - **A paper** (before 0.9.10 only a bill, a receipt or an explanation of
    benefits) has `text`: the byte offset and length in its text (`text.json`) of
    each line that says an area, by words or by a service's number, without its
    line end (a line feed or a form feed). Words that go over from one line to
    the next are in both. From 0.9.12 it has the lines of each of its `fields`
    (Fields of a paper, above) that is private by what it says, too: a drug whose
    name or directions name a medicine of an area or its brand (Private's
    `medicine`), or that is the person's medicine by an equal code where any
    medicine of that code is private; a follow-up whose words name a place of an
    area (by the words of Private's `provider`), or that names, in `with` or as
    any run of words that start with a capital wherever they stand, a clinician
    the `clinician` rule ties to one (by a paper that names them or a link to a
    practice's, not by a day) or a name the household's provider word, of every
    item that prints it, gives an area (`medrec providers`); and a field that
    prints an ICD-10 code of an area. These are read from the paper's text as
    the views are written, never from its `fields` in `index.json`, which a
    medrec older than 0.9.12 drops: what is held back is the same before a
    `medrec reindex` as after it. `documents.json` has no field any of whose
    `lines` are among these places; `index.json` keeps it.
  - **An event** has what of `kind`, `title`, `what`, `what_from`, `place`,
    `place_from`, `clinicians`, `money` and `differences` its members that are
    not private do not say as `events.json` has it: as they say it, null where
    they say nothing; and `expected` too where they make it another `kind`. A
    paper that is private tells no alias there. Its `says` counts the private
    lines of its open claims, absent when they have none. In its `members`, a
    claim with a part is named by the part's `display`.
  - `says` is how much is left out, for the item's own row: "1 service not
    shown", "2 diagnoses and 1 payer note not shown", "3 lines not shown".
  - An item that is private has no part, and neither has a claim or a paper the
    household marked not private: `medrec private unmark` shows all of it. An
    event has one whether or not the household marked it so, for its mark opens
    the event and not what is private in it.
  - A private part is no reason to leave its item out, and is counted in no
    `private-left-out`. `medrec private list --json` has the items with one as
    `parts`, `[]` when there are none, newest first: `id`, `kind` (`record`,
    `document` or `event`), `not_shown` (its `says`), `date`, `title`, `person`
    and `person_name`. Its `items` are the private ones, which have `area` and
    `from` in place of `not_shown`. `--limit` is of each of the two.
- An event's own members are those without `person`. A paper an event shows
  from another person (X2) is judged as that person's, and so is an event of
  theirs it links (X1): where theirs is private, or what is private of theirs is
  not known, a reader leaves it out of what it shows of this person too (0.9.9
  and later).
- **A place that is linked** (0.9.9 and later). An event's `place` whose
  `place_from` is `linked` is another person's item's word. A reader shows it, on
  the person's own pages too, only where every one of the event's `born_during`
  links is to an event in that person's `clear_events`, and what is private of
  theirs is known. Nothing takes its place. `clear_events` has the ids of the
  person's events that have `links`, are not private and have no private member
  of their own; absent when there are none.
- **A private item in an open event** (0.9.9 and later). An event stays open
  with a private result or paper in it. Where a reader leaves the item out, it
  leaves it out of the event's `members`, with its name, date and id, and may
  say how many it left out.
- **The household has the last word** (`marks.json` `private`, above). A
  mark `true` makes an item private, with the area the rules give it, else
  `household`. A mark `false` takes it out, even as a member of a private
  event or a paper tied to a private one. Marks on records and documents apply before events are judged,
  marks on events after. An event's own mark comes first; else the last
  mark on an event id the person no longer has that shares a member with
  it (`marks.json`, above). A mark naming a record or document the person
  no longer has, or an event none of whose members remain, is skipped.
- **`studies`** (0.9.5 and later) has the studies whose report makes them
  private. **`unreported_studies`** has the ids of those with no `report`, or
  one the person does not have. No report judges them, so a reader leaves
  them out of answers about the whole household as it does private items,
  counts them in the same `private-left-out` count, and shows them in an
  answer about their person, with no `private` mark. Naming one includes it
  as it does any item. Absent when there are none. Views from 0.9.4 and
  earlier have neither map: a reader judges a study from its `report`, or
  treats it as unreported.
- **`marks_unreadable`**: `true` when the person's `marks.json` does not
  read, so no household mark applies. Absent otherwise. Any of the
  person's items may then be one the household marked private, so a
  reader holds all of them back from shared screens and household-wide
  answers until it reads.
- **`labels_unreadable`** (0.9.7 and later): `true` when what a provider label
  makes private is not known (`provider-state.json`, above). Absent otherwise.
  `marks_unreadable` is `true` with it, so a reader that does not know it
  holds everything back too. It ends when a pass has a provider directory that
  was asked of every NPI the person's records carry.

## Readers

Anything that only reads the folder follows the files above and writes
nothing but `_requests/` (the app, by that section's rules). A reader that
answers from `views/` (the hosted connector) answers as `medrec mcp` does,
and falls back to the files themselves when the views are stale, but never
when `reader` stops it (views.json, above). Two
readers ship with medrec:

- **The commands** (`timeline`, `search`, `results`, `meds`, `documents
  list` and the rest) read `household.json`, each person's `index.json`,
  `text.json` and `notes.md`. Of a folder on Drive, the `--json` answer of one
  that may read this machine's copies has `reads` (0.9.11 and later): `from`
  (`drive`, `cache` for this machine's copies, or `both`), how many reads each
  answered (`drive`, `cache`), how many copies were past the time one is taken
  as it is or of no known age (`stale`), how many were read because Drive could
  not be reached (`unreachable`), and `oldest_seconds`, the age of the oldest,
  absent where none was read or one's is not known. `--fresh` (`--no-cache`,
  `MEDREC_NO_CACHE=1`) reads Drive alone, and keeps nothing it reads: the copies
  stay as they were, so a later read without it can answer from one older than
  what it showed. A command that always reads Drive (one that changes the
  folder, `backup`, `doctor`, `reindex --dry-run`, `verify`, with or without
  `--events`) has none, nor has one that reads a local folder.
- **`medrec mcp`**, for Claude Desktop and Claude Code, reads the same files
  through the same views, per call, with no lock. It writes nothing,
  `_requests/` included. Its answers are computed, not stored:
  - what is current among conditions, allergies and medicines is what
    `standing.json` makes of it (0.9.9 and later; "As a reader lists them"):
    - an item, or a summary's line, has `standing`, what it is on the day of
      the answer, and `says`, what each source says, and no `status`, which is
      one source's. `standing` is `current`, `on hold`, `ended by its date
      <until> (<until_of>)` with `, filled <date>` for the fill shown
      (`, filled <date> (from a claim)` where a claim tells of it, 0.9.11 and later), `draft`,
      `ended`, `refuted` or `diagnosis from a visit`; then `, sources differ`
      where they do; then `, not every source heard` where one is `unheard`.
      `says` is what each word says, each once, joined by `; `: the words of
      the line's records, then of those of other sources that say otherwise,
      each in `order` and a record's as they stand, then `<source>: not heard`
      for each not heard. A word is the source, a colon, its `status` (or `no
      status`), its `verification` unless that is `confirmed`, `ended
      <abatement>` and `ends <end> (<end_of>)`, joined by commas, then ` (read
      <day>)`. A fill has none of its own and keeps its `status`; its
      sources can differ on it as on any other item (below).
    - `medrec_records` with `active` has a row for each current line, then for
      each medicine on hold, then for each ended by its date; in each group
      newest first, then by title. A line's row is the first of its records
      that the question fits (`name`, `since`, `until`): they choose lines, and
      change nothing a line says. In an answer about the whole household a
      private record counts on a line only where the answer lists it, so where
      the question fits it and names it. Without `active`, each record is a
      row, with its own words.
    - what a page of an `active` list leaves out sits by its date in a group: a
      private record in the group it would be in alone (one that would be in
      none is not left out), and one entered in error with the current ones.
    - a `no-problem-list` warning says which sources send no problem list, by
      rule 6: with `active`, on each page. An answer about the whole
      household names the person first.
    - an `entered-in-error` warning says how many are not shown, of any kind,
      page by page as `private-left-out` does, in `medrec_records` (`timeline`
      too) and `medrec_search`. It counts none that is left out as private.
      `tests` is made of the results that are not, and counts none.
      `medrec_document` names none. A page of `bills` has Bills' own line, of
      the bills on it, and `events` counts those in `events_in_error`; an event
      on it says how many of its own are, of those the answer may show, in its
      `detail` ("1 entered in error, not shown"), and has `in_error` and
      `not_applied` in its `fields` when those are in the answer, each absent
      when nothing is left of it (0.9.10 and later; 0.9.9 could give an empty
      list). So with an event asked for by its id in a call that names no person
      (0.9.10 and later; 0.9.9 named them all). One that is private itself, asked
      for by its id or named, has all of its own (one named, 0.9.10 and later;
      0.9.9 left out what was held back).
    - any other item its sources differ on has `standing` `sources differ` and
      `says`, as `standing.json`'s words are said; the household's own file is
      `the household's own file`. It keeps its `status`.
    - `medrec_document` of a paper entered in error says so first in its `hint`:
      "Entered in error (<says>): it is in no list, event, bill or total." and,
      where the household decided something of it, "Kept and not applied: 1
      decision about its event, 2 payments on it, 1 correction of a provider it
      names." One its sources differ on: "Its sources differ (<says>)." An
      event asked for by the id it has in `events_in_error` has `standing`
      `entered in error`.
    - a person's summary has the current lists, each line once, with
      `medications_on_hold` and `medications_ended_by_date` after
      `medications` when they are not empty. A list's `note` has what its
      warnings would say: of sources with no problem list, then of those
      entered in error, each a sentence, joined by `. `. So has the note of
      `immunizations`, `latest_results`, `recent_visits` and `bills`.
    - the commands give the same: `conditions`, `allergies` and `meds` have
      `standing` and `says`, and the lists, the timeline and the search have
      `notes`, with what they say of those not shown. So do `documents list`,
      `events list`, `bills list` and `private list`, and `documents open` with
      what it says of the paper. What was decided of an item entered in error
      has `applied: false`, absent otherwise, where `events decisions`, `bills
      payments` and `labels list` list it, and where `events apart`, `events
      together` and `bills paid` record it.
  - the latest result of each test is one per code (with its system), or per
    name with case and accents folded when uncoded: the newest that is no heading
    (Panels, above). A test's
    series is every `result` record whose code is the one asked for, or
    whose name holds the words asked for. A series is private when any of its
    results is (0.9.3 and later; see the private items below). In `kind: "tests"`
    and `tests.json` headings are in none, and make none private.
  - a bill is a `bill` document with a `statement`, not `same_statement_as`
    another. The newest statement per organization and account is shown
    when its `amount_due` is over zero and `bills.json` does not have it
    settled (0.9.1 and later), and is `past_due` when its `due_date` is
    before today and it is not settled. `medrec_records` with `kind:
    "bills"` adds `past due` to such a bill's `detail`, `paid` to a
    settled bill's, and `<amount> left` to one partly paid. Each is worked
    out as the call is answered, from `bills.json` and the day.
  - a document's text is its `text.json` entry. Pages are the form feeds
    between `pdftotext` pages, shown as `--- page N ---`.
  - `medrec_records` with `kind: "events"` lists a person's events as
    `events.json` holds them (0.9.0 and later): `source` is the place,
    `detail` what it was, who, a grouping that is not sure and what is
    still expected, and `you_owe` the share with its currency. `id` gives
    one event, with its money, members and evidence in `fields`.
  - an answer about one person says how much of their chart is about somebody
    else (About another person, above; 0.9.9 and later): an `about-another`
    warning with that line, on the first page of `results`, `tests`, `timeline`
    and `documents`. No other answer has it, and `medrec_document` finds none.
  - in an answer about the whole household (`medrec_document` too), an open item
    tells of no private one (0.9.9 and later). An event's `fields.members` has
    none of its private members, and `fields.private_members` is how many it
    leaves out, absent when none. One that is private itself, asked for by its
    id or named, has all of its own. A held-back person's has none of them. Its
    `links` and `could_join`, and the `fields` of any record, have no id of an
    item held back: a list keeps the rest, and a key with nothing left is
    absent. `medrec_document`'s `records` has no private record's name, unless
    the document is private. In an answer about one person, another person's
    private paper in an event (`private_members`) and their private events in
    its `links` are left out too. While what is private of theirs is not known,
    all of theirs are, and a page with an event that has any says so of each
    such person, in a `private-marks-unreadable` or a
    `private-labels-unreadable` warning: "Which of <name>'s items are private
    is not known (their marks.json does not read): the events on this page
    leave out <name>'s documents and events. Ask about <name> for them".
  - an answer that names no person shows an open item with a private part
    without it (0.9.9 and later; Private parts, above): its title, `detail` and
    `fields`, what a question finds it by, and where it sorts, are those of what
    is left. `not_shown` is its `says`, on the rows of claims and events, in
    `timeline` and in `medrec_search`, after `detail` or `snippet`; a document's
    row has none. `medrec_search` counts a hit that only a private part has the
    words of in `private-left-out`. `medrec_document` gives a text with
    `(1 line not shown)` or `(2 lines not shown)` in place of the private lines
    that follow one another, and the hint "1 line not shown: pass <name> as person
    for all of it.", with its person's name; `text_chars` and the pages are those of what it gives. With
    `person` (a usage error where the document is not that person's) it gives all
    of it, with its records and labels as an answer about them has them.
    Its `labeled` lines (0.9.12 and later; before, `amount_due` and `due_date`)
    are a line each, at most 20 and then one that says how many more the text
    has: "Amount due: $90.00, by 2024-10-29" of a bill's statement,
    "Follow-up: <words> (<date>, <how>)" of each follow-up, `how` being "as
    printed", "from the paper's date" or "printed with no year", and "Drug, from
    a paper: <drug>: <directions> (RxNorm …; NDC …; the person's medicine <id>)"
    of each drug (Fields of a paper, above), leaving out, without `person`, each
    whose lines are private places, and a private medicine's id.
  - private items (`private.json`, 0.9.1 and later) are left out of an
    answer about the whole household. Each page says how many it left out
    in a `private-left-out` warning (0.9.2 and later; 0.9.1 gives the whole
    answer's count on every page). A page counts the items left out that
    sort after the previous page's last item and up to its own last, or to
    the end on the last page, so the pages add up to the whole count and
    a reader that reads only what a page needs can count it. A series of
    tests (`kind: "tests"`) is private when any of its results is, not
    only its latest (0.9.3 and later): it is left out whole, counted once,
    and marked with the first private result's area where it is included.
    An answer about one person (`person`)
    includes them, and so does one that names the item: `medrec_records`
    with a `name`, or `medrec_search` with a query, each of whose words
    starts a word of the item's title (for an event, its title, detail or
    place), one of them a word of its own rather than a form, unit or kind
    of record ("sertraline", not "tablets"); an event or a document asked
    for by `id`. Each private item an answer includes carries `private`,
    its area. A person whose `marks.json` does not read has all their
    items held back from answers about the whole household, with a
    `private-marks-unreadable` warning naming them. So has one whose provider
    labels do not read (`labels_unreadable`, 0.9.7 and later), with a
    `private-labels-unreadable` warning. The commands are the household's own
    terminal and leave nothing private out.
  - `kind: "conditions"` rows and the summary's condition lines carry
    `plain_name`, the code's name in the person's current
    `code-names.json` (0.9.4 and later). `title` and `name` stay the
    source's. `medrec mcp` reads it from the views, unlike the rest of its
    answer, so a reader of the views gives the same names. In an answer
    about the whole household it names no private item.
  - `provider_labels` (0.9.7 and later) is the labels of what an item prints,
    from the person's current `providers.json`, beside the source's own words
    and never in them:
    `[{field, record?, printed?, name?, practice?, role?, phone?, phone_from?,
    confidence, evidence}]`. `field` is where it is printed: a record's field in
    the order `provider`, `practitioner`, `performer`, `prescriber`,
    `service_provider`, `facility`, `location`, `pharmacy`; a document's
    `organization`; an event's `place`, then its `clinicians`, then its
    `members`. Two that print the same and have one label are one, the first.
    - `claims`, `events`, `documents` and `bills` have it, and the claims and
      documents of `timeline`, and `medrec_document`. A claim has its
      `provider`'s, without `printed`, which only its title can say. Any other
      record has none until `response_format` is `detailed`: then each record
      (not in `timeline`) has every labelled field's. An event in full
      (`detailed`, or by `id`) has its own members' `provider`s too, `record`
      naming the member.
    - In an answer about the whole household (no `person`, so `medrec_document`
      too), an item that is private, or held back, has none, even one the
      question names, and an event's private member gives none. `private` is
      read only in an answer about the person, for an item that is private.
    - Evidence, and a phone's `phone_from`, that names a paper held back from
      such an answer is left out, and so is one naming something the household
      said that is in `private.overrides`; a phone goes with its `phone_from`. A
      likely label left with no evidence is left out. A confidence medrec does
      not know is `likely`, and evidence it does not know is its `from` alone.
    - They are added before a page is cut, so they count in its size. An item
      that would be too large for a page with all of its own has the first that
      fit, and a `provider-labels-left-out` warning says how many items on the
      page that is: a record is never lost to its labels. No tool
      finds, sorts or filters by one, and `medrec_search` and `medrec_summary`
      have none.
  - a reader that answers from the views without a current `private.json`
    for a person cannot tell which of their items are private: theirs is
    stale or absent (views from 0.9.0 or earlier), or the reader falls
    back to the files and has no rules of its own, or rules that cannot
    judge the person's provider labels (a `provider-state.json` is there,
    or `views.json` names one). It holds back all of
    that person's items from answers about the whole household, with a
    `private-not-prepared` warning naming them, and includes them
    unmarked, with the same warning, in an answer about that person.
    `medrec mcp` judges from the files and is never without it.
  - a reader without a current `bills.json` for a person shows their bills
    as 0.9.0 did, due and past due by date, with a `bills-not-prepared`
    warning that payments may be missing.

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
   `DiagnosticReport.category`, and from 0.9.11 `DiagnosticReport.performer[].actor`), and
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
   file, and a version dated between two reads is older than the second. A raw
   file no record was filed from (a Binary, a Practitioner) has no time in the
   index, so other content replaces it; a Binary keeps the rendering rule
   above, and a DocumentReference or a Media stays current over a version
   older than its own `meta.lastUpdated` (0.9.9 and later). A record is
   normalized again from each version filed, so a later medrec's reading
   applies on the next import. `lastUpdated` measures when content was
   current, not how complete it is. The raw layer keeps every version either
   way.
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
| `records_not_served` | records this source gave before that this export does not have (0.9.10 and later), which are kept; `null` where that cannot be told |
| `documents_added` | documents filed |
| `documents_duplicate` | Binaries with the bytes of a document or rendering already filed |
| `documents_unchanged` | renderings of a document already filed |
| `documents_same_statement` | bills filed that are the same statement as a bill filed before (0.2.9 and later) |
| `papers_unread` | raw files that describe papers, or are the bytes of one, and do not read (0.9.9 and later): the source's, where the stream says that something describing a paper is entered in error or carries an older version of it, and those of the person's other sources, where a paper is newly entered in error; otherwise 0 |

`records_not_served` is of an export through a sign-in of all the source has: it is
`null` with `--since`, where the export's header has `since`, with `--from-files`,
with neither `--from-profile` nor `--from-files` for a provider that reads files
(`records-files`), where the source's folder has imports of another patient, and
where a skip names neither a type nor a search. A record is not counted while any of its resources
from this source is in the export, or may be among those it left out (its type was
withheld or cut short, it is itself among the skips, or it is a `MedicationRequest`
and a `MedicationDispense` is), nor when it is about another person. Nor is it
where the provenance of one of them is not from a sign-in's run (its `fetched_at`
is that run's `pulled_at`): files can have what the source gives no sign-in. A
sign-in that gives the same version again, no later, leaves the provenance as it
was. A run says which it was (`from_files`); with neither `--from-profile` nor
`--from-files`, a provider that reads files (`records-files`) may have read some,
and the run is of files. One filed before 0.9.10 does not say, and is taken for a
sign-in's where the provider reads no files, else for neither. A record that is
counted stays in the count of each later import that can tell it. `not_served`
(0.9.10 and later) has the count of each kind, `{<kind>: n}`, and is `null` when
the count is.

`updated` (0.9.10 and later) is a list of the records updated, `{id, kind,
changed}`: `changed` has the names of what is other than it was, a member of the
record (`status`, `code`, `display`, `date`, `words`, `errors`, `providers`,
`lists`, `documents`) or a field as `fields.<name>`, and no value. It is empty for a
version that differs in nothing medrec reads, or only in what no reader shows
(`given`, `listed_by`). Several resources of one record make one entry. A
record about another person has none, and neither does one that this same import
first added, though `records_updated` counts a later resource that joins it.

A field added to `import/v1` is additive: a script reads a missing one as
absent, never as an error.

The patient's birth date in the stream must match the person's in
`household.json`, unless `--allow-birth-date-mismatch`. A source's first import needs
`--person`; later imports find the person by the source's patient id or label,
as described above.
