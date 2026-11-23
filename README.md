# medrec

A household's medical records in one plain, private Google Drive folder.
`medrec` files scans and downloaded documents, imaging studies from CDs and
download links, and the records that provider and insurer CLIs fetch. It then
answers questions over all of it: a timeline, search, results, medications,
conditions, allergies, immunizations and notes, at the terminal or, through
`medrec mcp`, in Claude.

`medrec` holds no provider credentials and makes no network calls of its
own, except to Google Drive through `rclone` and to GitHub for `self-update`.
Provider CLIs fetch records; `medrec` files them.

Part of the `piekstra-cli/1` family. It is built on
[cli-common](https://github.com/piekstra/cli-common) and
[medical-common](https://github.com/own-your-health/medical-common).

It is for anyone keeping their own or their family's records: there is no
account, no server and no one else's copy. The folder is yours, in your own
Google Drive (or a local directory), and every file in it is a plain PDF,
image, DICOM file or JSON document you can open without medrec.

> Unofficial. `medrec` is not affiliated with or endorsed by Google, the
> Weasis project, or any provider, insurer, pharmacy, lab or portal named
> here. It is not a medical device and gives no medical advice.

## Install

A release, for macOS (Apple silicon or Intel) or Linux:

```sh
T=aarch64-apple-darwin                # or x86_64-apple-darwin, x86_64-unknown-linux-gnu
curl -LO https://github.com/own-your-health/medrec-cli/releases/latest/download/medrec-$T.tar.gz
curl -LO https://github.com/own-your-health/medrec-cli/releases/latest/download/medrec-$T.tar.gz.sha256
shasum -a 256 -c medrec-$T.tar.gz.sha256
tar -xzf medrec-$T.tar.gz && install -m 0755 medrec ~/.local/bin/   # any directory on PATH
medrec self-update                    # later releases
```

Or from source (Rust 1.89 or later): `cargo install --git
https://github.com/own-your-health/medrec-cli --locked`, or `make install` from a
checkout. medrec reads no Keychain item, so an unsigned build asks for no
approval.

The tools it runs, all local (Homebrew on macOS; on Linux, your package
manager's `rclone`, `poppler-utils` and `tesseract-ocr`):

```sh
brew install rclone                   # the Drive folder
brew install poppler tesseract        # text extraction and OCR
```

Weasis (https://weasis.org) is the default DICOM viewer. Another viewer is
set with `medrec config set viewer <app or executable>`, or `none` to only
copy a study.

## Quick start

1. **A Drive remote.** `rclone config` adds one (here `gdrive`); it opens a
   browser for Google's sign-in, and rclone keeps and refreshes the token.
   A local directory works too, with no remote.
2. **A records folder and the people in it.**

   ```sh
   medrec profiles add home                        # one profile per records folder
   export MEDREC_PROFILE=home                      # or --profile home on each command
   medrec init "gdrive:Medical Records"            # creates the folder layout
   medrec people add "Alex Example" --birth-date 1990-01-01 --adult
   medrec people add "Robin Example" --birth-date 2020-02-02 --guardian Alex
   medrec doctor                                   # FileVault, rclone, free space, tools
   ```

3. **Documents you already have.** `medrec intake ~/Downloads/lab.pdf`
   files one; anything dropped in the folder's `inbox/` is filed by
   `medrec intake --inbox`. What intake cannot decide waits in
   `medrec review list`.
4. **Records from a provider.** Install its CLI ([Provider
   CLIs](#provider-clis)), do its one-time setup, and sign in as its README
   says. Then medrec runs its export and files what it returns:

   ```sh
   medrec providers list                           # the provider CLIs on PATH
   <cli> profiles add <LOGIN>                      # one per account at the provider
   <cli> auth login --profile <LOGIN> …            # a browser sign-in; the flags are the CLI's
   <cli> patients list --profile <LOGIN>           # who the sign-in can read
   medrec import --from <cli> --from-profile <LOGIN> --patient <WHO> --person Alex
   ```

   Importing again later files only what changed.
5. **Check the folder.** `medrec verify` reads every indexed file back
   against the index.

What needs you: Google's sign-in in step 1, and at each provider its
one-time registration (the CLIs that need one) and every sign-in, code and
consent. How long a provider's sign-in lasts is in its README. Everything
else runs without you.

`init` adopts an existing medrec folder, and refuses a folder that holds
other files unless given `--allow-other-files`. The folder's layout and every file format
are in [docs/data-contract.md](docs/data-contract.md).

### Profiles

A records folder is a login (its rclone remote), so it belongs to a profile,
named by the household and kept in this machine's config only. Every command
that reaches a records folder needs `--profile <NAME>`, or `MEDREC_PROFILE`;
there is no default, and a lone profile is never picked: without one the
command is exit 2 and lists the configured profiles. `profiles`, `providers
list`, `config path|show`, a machine setting's `config set|unset`, `cache
clear`, `auth status`, `info`, `completions` and `self-update` take none.

`refresh sign-in <source id>` is the one command that may run without
`--profile`, because the source id already names one choice someone made on
purpose. Only `medrec --profile <NAME> refresh add --source <id>
--provider-profile <LOGIN>`, which names both the medrec profile and the
provider's login on its command line, binds a source for refresh; nothing
infers a binding, not even when the machine has one profile. `sign-in` finds
the profile whose binding holds the source. A `--profile` (or
`MEDREC_PROFILE`) that names another is exit 2. Before it opens any sign-in
it prints the medrec profile and the provider profile it resolved, names
only. No binding, or more than one, is exit 2 and lists them. So the command
a background refresh leaves for a person in `_status/status.json` names no
profile, and profile names stay in this machine's config.

```sh
medrec profiles list                            # profile-list/v1, with each records folder
medrec profiles add <NAME>
medrec profiles remove <NAME>                   # this machine's settings only; the folder stays
medrec profiles migrate <NAME>                  # the folder this machine used before profiles
```

`profiles migrate` moves the records folder a config kept before profiles
(its top-level `folder`, `mount`, `inbox` and `me`) into the profile, adding
it when it is new. It is local: no network.

Upgrading to 0.2.0: run `medrec profiles migrate <NAME>` once. It keeps the
file as it was beside it, as `config.json.pre-profiles`. medrec before 0.2.0
reads no profiles, and a `config set` or `init` from it would save its own
settings over them, so after migrating, a downgrade starts by copying that
file back over `config.json`. `config show --json` prints the file: this
machine's settings at the top and each profile's under `profiles.<NAME>`.
With a profile named, it and `config set|unset` of a profile key also give
`profile` and that profile's `folder`, `mount`, `inbox` and `me` at the
top, where they were before 0.2.0.

## Provider CLIs

medrec signs in nowhere: each source has its own CLI, which writes the
`records-stream/v1` stream medrec imports
([medical-common](https://github.com/own-your-health/medical-common)'s
`health-records/v1` profile). `medrec providers list` probes these names;
any other CLI on that profile is added with `medrec config set providers
<cli>,<cli>`.

| CLI | Source | What it reads | Before the first sign-in |
|---|---|---|---|
| [`mychart`](https://github.com/own-your-health/mychart-cli) | Any Epic MyChart organization | Health records | A developer app at Epic ([registration](https://github.com/own-your-health/mychart-cli/blob/main/docs/registration.md)) |
| [`athena`](https://github.com/own-your-health/athena-cli) | athenahealth practices | Health records | A developer app at athenahealth ([registration](https://github.com/own-your-health/athena-cli/blob/main/docs/registration.md)) |
| [`kp`](https://github.com/own-your-health/kaiser-cli) | Kaiser Permanente, the health plan | Claims and coverage | A developer app at Kaiser ([registration](https://github.com/own-your-health/kaiser-cli/blob/main/docs/registration.md)) |
| [`modmed`](https://github.com/own-your-health/modmed-cli) | ModMed practices | Health records | A developer app at ModMed ([registration](https://github.com/own-your-health/modmed-cli/blob/main/docs/registration.md)) |
| [`icanotes`](https://github.com/own-your-health/icanotes-cli) | ICANotes practices | Health records | An API key the practice's patient portal generates ([setup](https://github.com/own-your-health/icanotes-cli/blob/main/docs/registration.md)) |
| [`uhc`](https://github.com/own-your-health/uhc-cli) | UnitedHealthcare | Coverage, claims, EOBs | None: a member site ([README](https://github.com/own-your-health/uhc-cli#readme)) |
| [`caremark`](https://github.com/own-your-health/caremark-cli) | CVS Caremark | Prescription fills, pharmacy claims | None: a member site ([README](https://github.com/own-your-health/caremark-cli#readme)) |
| [`labcorp`](https://github.com/own-your-health/labcorp-cli) | Labcorp Patient | Lab results and report PDFs | None: a member site ([README](https://github.com/own-your-health/labcorp-cli#readme)) |
| [`oneblood`](https://github.com/own-your-health/oneblood-cli) | OneBlood | A donor's blood type and readings | None: a member site ([README](https://github.com/own-your-health/oneblood-cli#readme)) |
| [`fertemr`](https://github.com/own-your-health/fertemr-cli) | Fertility clinics on the FERT EMR | Results, visits, documents, messages | None: a member site ([README](https://github.com/own-your-health/fertemr-cli#readme)) |
| [`salve`](https://github.com/own-your-health/salve-cli) | The Salve fertility app | Documents, forms, treatment records | None: a member site ([README](https://github.com/own-your-health/salve-cli#readme)) |
| [`peryourhealth`](https://github.com/own-your-health/peryourhealth-cli) | PerYourHealth (Optum) | Medical bills | None: a member site ([README](https://github.com/own-your-health/peryourhealth-cli#readme)) |
| [`flywire`](https://github.com/own-your-health/flywire-cli) | Flywire patient portals | Medical bills | None: a member site ([README](https://github.com/own-your-health/flywire-cli#readme)) |
| [`embryooptions`](https://github.com/own-your-health/embryooptions-cli) | Embryo Options | Embryo and egg storage bills | None: a member site ([README](https://github.com/own-your-health/embryooptions-cli#readme)) |

A developer app is free and registered once, by the person who uses the
CLI; each registration doc walks through the vendor's form, and the CLI's
`registration` command prints the values to enter. A member site needs only
your own sign-in; its README says what the site's terms say about automated
access. A provider whose site offers downloads instead (statements, an
export) is imported from them with `--from-files` (below).

## Use

```sh
medrec intake ~/Downloads/lab.pdf               # read, classify, file
medrec intake --inbox                           # drain the folder's inbox/
medrec intake --drive "gdrive:Old scans"        # the back catalogue, copied
medrec review list                              # what intake could not decide
medrec review file <id> --person Robin --type lab_report --date 2026-03-04
medrec documents list --person Robin
medrec documents merge <copy> --into <id>       # a document filed twice: one kept

medrec images intake /Volumes/CDROM             # a disc, folder, DICOMDIR, .zip or .iso
medrec images list
medrec images open <id>                         # a private copy, opened in Weasis

medrec providers list                           # provider CLIs on PATH
medrec import --from mychart --from-profile adult1 --patient <id> --person Robin
medrec import --from mychart --from-profile adult1 --patient <id>   # later: the FHIR id or label
medrec import --from uhc --from-files ~/Downloads/statements --patient <id>   # downloaded files

medrec timeline --person Robin --since 2026-01-01
medrec search hemoglobin
medrec results --code 718-7
medrec meds --active
medrec note add Robin "Ask about the recheck" --tag question --about <record id>

medrec verify                                   # every indexed file against the index
medrec backup --to "/Volumes/Backup/Medical Records"
```

Every command takes `--json` (one document on stdout) and `--help`. Every
list (`timeline`, the record views, `documents list`, `note list`,
`images list`, `review list`, `people list`) takes `--limit N`; it is whole
by default, and `search` stops at 50 hits. Exit codes: 0 ok, 1 a check
failed, 2 usage, 3 a provider needs a sign-in, 4 not found, 5 upstream, 6
confirmation required.

`--from-profile` names the provider's own login profile (one of `<cli>
profiles list`), which medrec passes to its export as `--profile`. It is
required for a provider that signs in, and never read from the environment,
so medrec's environment never picks a provider login. A provider that signs
in but lists no `login-profiles` capability in its `info --json` predates
profiles and is refused; one whose info says no login is needed takes no
`--from-profile`. medrec writes no provider profile name into the records
folder.

`--from-files <DIR>` imports a provider's downloaded files (statements, an
export the portal offered) instead of signing in. medrec passes the
directory to its export as `--files`, with no `--profile` and with the
provider's `<PREFIX>_PROFILE` left out of its environment, so one import
reads one source. It is for a provider whose `info --json` lists the
`records-files` capability, and it cannot be combined with
`--from-profile`. From files a provider gives no patient labels, so
`--patient` is the id `<cli> patients list --files <DIR>` lists. The same
records read through the login and from files carry the same organization
and patient id, so they file under one source and the second import of
them is unchanged.

An import files a document once. When a source serves it again with other
bytes and exactly the same text (a summary a portal generates on each
export, with a new timestamp and id), nothing is filed: the import counts it
`documents_unchanged` and notes the rendering on the document it renders.
Any change in the text files a new document. A version of a resource is
filed by medical-common's rule: one that differs only in `meta.lastUpdated`
is the same version and adds no file; another one replaces the version
filed only when it is later (`meta.lastUpdated`, or the read time of an
undated FHIR API read). An older one changes nothing: the import counts it
`records_older` and keeps that version in the raw layer beside the current
one. Copies filed before 0.2.3 fold
into the one they render with `medrec documents merge <copy> --into <id>`.
It refuses (exit 2, or 4 for an id that is not filed) unless they are that
one document by the same rule, and only then asks (`--force` to skip). It
points records, imaging studies and notes at the kept one, keeps the copy's
hash in its `renderings`, and removes the copy's file, to Drive's trash.

### Refresh

```sh
medrec --profile home refresh add --source <id> --provider-profile adult1   # bind a source to its login
medrec refresh list                             # the bindings (this machine's config)
medrec refresh                                  # import every bound source now, if its interval allows
medrec refresh --person Robin                   # one person's sources; --source <id> for one
medrec refresh --install                        # a LaunchAgent: a pass every 5 minutes (--pass-every)
medrec refresh status                           # what _status/status.json records
medrec refresh pause <id> --reason consent      # until `medrec refresh resume <id>`
medrec refresh sign-in <id>                     # sign a source's login in; clears "needs a person"
medrec refresh --uninstall
```

`refresh` imports each bound source on a schedule, on a reader's request (a
file the own-your-health app writes in `_requests/`), or now, and records
every attempt in `_status/` ([data contract](docs/data-contract.md)). A
scheduled pass lists `_requests/` once; when nothing is requested, nothing
is due and the hourly heartbeat is not due, it stops there.

| Provider | Least time between imports | Default schedule |
|---|---|---|
| A developer app (`mychart`, `athena`, `kp`, `modmed`) or a portal key (`icanotes`) | 6 hours | daily |
| A member site (`uhc`, `caremark`, `labcorp`, `oneblood`, `fertemr`, `salve`) or any other CLI | 24 hours | daily |

`refresh add --every <DURATION>` sets the schedule, never below the least
time; `--manual` imports only when asked. The interval is counted from the
source's latest import in the person's `index.json`, so an import run by
hand counts, and a request never shortens it. After a failure the source
waits 30 minutes, doubling with each failure after, up to its least time.

A scheduled pass never waits on a person. Before importing a source that
signs in, it reads `<cli> auth status --json --profile <LOGIN>` with a
30-second limit. A sign-in that is signed out or expired, or a read that
outlives the limit (a Keychain approval nobody gave), marks the source as
needing a person, with the command `medrec refresh sign-in <id>`, and
scheduled passes do not try it again until that has run. So does an export
the provider refuses for its sign-in (exit 3), as a member site does with a
session `auth status` still reads as valid. `medrec refresh` at the terminal
waits up to 10 minutes for an approval. `--scheduled` runs a pass as the
LaunchAgent does.

`refresh sign-in <id>` runs the provider's own sign-in command, built from
the `login_hint` its `info` gives (`uhc auth login --browser --profile
<NAME>`) with the source's login, organization and patient label filled
in, and `--overwrite` when its `auth login` takes it. It runs when the
sign-in is not usable, or when the provider refused the last export,
whatever `auth status` says; `needs a person` clears once a sign-in
succeeds and reads as usable.

Error messages in `_status/` are the failing command's own words, with
every profile name in them replaced: a provider profile by `<provider
profile>`, a medrec profile by `<profile>`. A line an earlier medrec wrote
with one loses it the next time its month's file is written.

One machine refreshes a folder. `--install` records this machine's random
id in `status.json`; another machine's `--install` and passes refuse while
that one's heartbeat is fresh, and `--install --take-over` moves refresh to
the machine it runs on, serving requests from one pass later. The agent is
`~/Library/LaunchAgents/io.github.own-your-health.medrec.refresh.<profile>.plist`,
in the person's own session, and logs ids, outcomes and counts to
`~/Library/Logs/medrec/refresh-<profile>.log`, never a name or a record.
`medrec refresh` exits 5 when a source failed, else 3 when one needs a
person, else 0; `--scheduled` exits 0. `refresh sign-in` exits 3 when the
source still needs a person. `refresh status --json` prints `"status": null`
for a folder refresh has never run on. A subcommand that acts on one bound
source takes its id as an argument (`refresh sign-in <id>`, `pause`,
`resume`, `remove`); `--source` picks one for a pass or binds one.
`--provider-profile` is `import --from-profile`'s login, and takes that name
too.

### Settings

`medrec config set <key> <value>`, kept in `~/.config/medrec/config.json`.
The first four are the profile's, so they take `--profile`; the rest are
this machine's:

| Key | What it sets | Default |
|---|---|---|
| `folder` | the profile's records folder, `<remote>:<path>` or a local directory | set by `init` |
| `mount` | where Drive for desktop mounts the folder, for `doctor` to compare | none |
| `inbox` | the inbox `--inbox` drains | `inbox/` in the folder |
| `me` | the person id who writes notes on this machine | none |
| `viewer` | the DICOM viewer, or `none` | Weasis |
| `ocr_languages`, `ocr_max_pages` | tesseract languages, and pages read per document | `eng`, 20 |
| `max_file_mb` | the largest file intake or import keeps, MiB | 200 |
| `import_max_mb` | the largest records stream one import reads, MiB; an import holds it until the trailer | 4096 |
| `import_timeout_secs`, `rclone_budget_secs` | how long one provider export, and all of one command's rclone calls, may take | 1800, 1800 |
| `providers` | provider CLIs to consider besides the known ones | none |

### What intake copies and what it moves

| Source | Policy | Why |
|---|---|---|
| the inbox (`--inbox`) | move | the inbox is a drop box; a filed document leaves it |
| a file under `~/Downloads` | move | a download is a transient copy |
| a file given with `--move` (the organize-scans hand-off) | move | the caller asked |
| a Drive folder (`--drive`) | copy | the back catalogue stays where it is |
| any other local file | copy | medrec does not delete what it did not receive |

`--keep` turns every move into a copy, and a symlink is always copied. A
moved file is removed only after the filed copy's MD5, as Drive reports it,
matches, and the index that points at it has been written and read back.

Intake waits until a local file has gone 2 seconds unmodified (up to 3
times), then reads it until two reads in a row agree and match the size the
file system gives, then keeps those bytes in a private copy: their
text, their hashes and the upload all come from it. A cloud folder's
online-only placeholder that serves other bytes while it downloads, or a
file still being written, is skipped with a reason rather than filed as
bytes the file does not hold. A moved source is kept, not removed, when it
no longer has the bytes filed, checked by its MD5 (a remote that lists none
is downloaded again and hashed; 0.2.10 and later).

### How a document is filed

Intake extracts text locally, with `pdftotext` first and `tesseract` for
scanned pages. It then detects the person, date, document type and
organization, each with a score and the evidence behind it. A document is
filed when all four are certain, and goes to `_review/` otherwise. Values
given on the command line win over detection. Values from another classifier
(`--classified-by organize-scans`) are checked: when medrec is certain of
something else, the document goes to `_review/` with both answers.

Duplicates are found by SHA-256 and by Drive's MD5, so a duplicate in Drive
needs no download. Re-scans of the same page are linked as near-duplicates,
by text similarity.

A bill also keeps what its statement says, where the text labels it: the
statement date, amount due, due date and account number. A paper statement
you file and the same statement a billing CLI exports later are one
statement (the same statement date, and the same account number or amount
due), so the later one points at the first (`same_statement_as`). A
payment receipt is its own type, `receipt`, filed beside bills under
`Claims/`.

### Imaging

A study's DICOM files are kept byte for byte. A study is identified by its
StudyInstanceUID and each image by its SOPInstanceUID, so a second disc of
the same study adds only the images not yet filed. `images open` copies the
study to a private cache folder (mode 0700, removed after 12 hours) and hands
the viewer that local folder. A disc's bundled viewer is never run.

## Use with Claude

`medrec mcp` serves one profile's records folder to Claude Desktop or Claude
Code on this computer, as four read-only tools over stdio. What a tool
returns goes to Anthropic as part of that Claude conversation, so add it only
where you would ask Claude about these records yourself. It is the only AI
connection: nothing in medrec calls an AI service, and no other one is
supported.

| Tool | What it answers |
|---|---|
| `medrec_summary` | The household's people. With `person`: their current medicines, conditions and allergies, immunizations, the latest result of each test with the lab's range and flag, recent visits, and bills with an amount due |
| `medrec_search` | Documents, records, imaging studies and notes holding every word of a query |
| `medrec_records` | One kind, newest first: `medications`, `conditions`, `allergies`, `immunizations`, `visits`, `procedures`, `results` (with `name`, one test over time), `tests` (the latest result of each), `reports`, `claims`, `coverage`, `bills`, `documents`, `notes` or `timeline`; `active` keeps current ones |
| `medrec_document` | A filed document's extracted text, in parts of up to 16,000 characters, with page breaks marked |

Claude Desktop: install the release's `medrec-<version>.mcpb` (open the
file, or Settings > Extensions > Install Extension) and enter the profile's
name when asked. The extension carries its own medrec binary, reads the same
`~/.config/medrec/config.json` and folder as the installed one, and looks for
`rclone` in `/opt/homebrew/bin`, `/usr/local/bin` and `/usr/bin`.

```sh
d=$(mktemp -d) && gh release download -R own-your-health/medrec-cli -p 'medrec-*.mcpb' -D "$d" && open "$d"/medrec-*.mcpb
```

Claude Code: `claude mcp add medrec -- medrec --profile <NAME> mcp`.

What the server does and does not do:

- It opens the folder to read on every call, as `timeline` and `search` do:
  no lock, no write, nothing changed in the folder. A Drive folder is read
  through the read cache, but no copy older than 15 minutes answers a call
  (`MEDREC_CACHE_MAX_STALE` overrides), and one call's Drive reads stop
  after 45 seconds with an `upstream` error.
- It needs a profile at start (`--profile` or `MEDREC_PROFILE`). Without
  one it exits 2 and names the configured ones. `medrec mcp --list-tools`
  prints the tool definitions and reads nothing.
- A list returns 25 items unless `limit` asks for up to 200, stops before
  32,000 characters of JSON, and continues with `next_cursor`, which is
  bound to the call's filters.
- `response_format: detailed` adds a record's fields as FHIR names them.
- Text from providers (titles, notes, document text) is data. The server's
  instructions tell Claude not to follow directions that appear inside it.

## Not in this version

- `costs` and `export` (PLAN phase 2), and a per-person summary page in the
  folder.
- The Huishouden bridge. People carry link fields for it, and medrec writes
  nothing to Huishouden.

## Docs

- [SECURITY.md](SECURITY.md): the threat model (Google, this Mac, a lost
  laptop, a lost rclone token or Google account).
- [PRIVACY.md](PRIVACY.md): what this public repo may and may not contain.
- [docs/data-contract.md](docs/data-contract.md): the folder layout and
  formats.
- [docs/GENERALITY.md](docs/GENERALITY.md): any household, any subset of
  sources.
- [AGENTS.md](AGENTS.md) and [CONTRIBUTING.md](CONTRIBUTING.md): working on
  medrec.

## License

MIT or Apache-2.0, at your option.
