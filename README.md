# medrec

A household's medical records in one plain, private Google Drive folder.
`medrec` files scans and downloaded documents, imaging studies from CDs and
download links, and the records that provider and insurer CLIs fetch. It then
answers questions over all of it: a timeline, search, results, medications,
conditions, allergies, immunizations and notes, at the terminal or, through
`medrec mcp`, in Claude.

`medrec` holds no provider credentials and makes no network calls of its
own, except to Google Drive, through its own sign-in, to GitHub for
`self-update`, and to a table's publisher for `tables fetch`. Provider
CLIs fetch records; `medrec` files them.

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
checkout. medrec keeps its Google Drive sign-ins sealed under
`~/.config/medrec/vault/`, with the key in one Keychain item
(`piekstra.medrec`). macOS asks to approve each newly built binary's first
read of it, once. Make that read right after an install or an update, at the
screen, and choose "Always Allow":

```sh
medrec auth key               # reads the key once and says whether macOS asked (auth-key/v1); no profile
medrec auth key --no-dialog   # exit 0: the next read raises no dialog; exit 3: it would
```

`medrec --version` and `medrec info` say which commit a binary was built from.

The tools it runs, all local (Homebrew on macOS; on Linux, your package
manager's `poppler-utils` and `tesseract-ocr`):

```sh
brew install poppler tesseract        # text extraction and OCR
```

Weasis (https://weasis.org) is the default DICOM viewer. Another viewer is
set with `medrec config set viewer <app or executable>`, or `none` to only
copy a study.

## Quick start

1. **A Google Drive sign-in.** Register an OAuth client once: a Desktop
   app client in your own Google Cloud project, as [cli-common's Drive
   guide](https://github.com/piekstra/cli-common/blob/main/crates/pk-cli-drive/docs/google-drive.md#registering-the-oauth-client)
   describes. Then sign in, and make the records folder:

   ```sh
   medrec drive login --drive-profile home-drive --client-file ~/Downloads/client_secret.json
   medrec drive folder create "Medical Records" --drive-profile home-drive
   # prints the folder's spec: gdrive:home-drive/<folder id>
   ```

   The login opens a browser for Google's consent. The client and the
   tokens go into medrec's vault, sealed with its Keychain key, so the
   downloaded JSON can be deleted. Every client in one Cloud project shares
   its `drive.file` access, and Google ends them together: a revoke, or
   removing the app in the Google Account, signs out every client of the
   project, a web app's too (SECURITY.md). A local directory works too,
   with no sign-in.
2. **A records folder and the people in it.**

   ```sh
   medrec profiles add home                        # one profile per records folder
   export MEDREC_PROFILE=home                      # or --profile home on each command
   medrec init gdrive:home-drive/<folder id>       # creates the folder layout
   medrec people add "Alex Example" --birth-date 1990-01-01 --adult
   medrec people add "Robin Example" --birth-date 2020-02-02 --guardian Alex
   medrec doctor                                   # FileVault, the Drive sign-in, free space, tools
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

What needs you: the OAuth client and Google's consent in step 1, and at each provider its
one-time registration (the CLIs that need one) and every sign-in, code and
consent. How long a provider's sign-in lasts is in its README. Everything
else runs without you.

`init` adopts an existing medrec folder, and refuses a folder that holds
other files unless given `--allow-other-files`. A folder that holds a
`views` folder, even an empty one, is refused either way: medrec writes its
views there. The folder's layout and every file format
are in [docs/data-contract.md](docs/data-contract.md).

### Profiles

A records folder is a login (the Drive profile its spec names), so it belongs to a profile,
named by the household and kept in this machine's config only. Every command
that reaches a records folder needs `--profile <NAME>`, or `MEDREC_PROFILE`;
there is no default, and a lone profile is never picked: without one the
command is exit 2 and lists the configured profiles. `profiles`, `providers
list`, `tables` (but `tables fetch nppes`), `config path|show`, a machine
setting's `config set|unset`, `cache clear`, `auth status`, `info`,
`completions` and `self-update` take none.

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

### Google Drive

medrec reaches Drive itself, through the Drive API, with an OAuth client you
register and a sign-in per Google account. It runs no rclone.

- **Drive profiles.** A Drive profile is one Google account, named on
  `medrec drive …` by `--drive-profile` (or `MEDREC_DRIVE_PROFILE`). There is
  no default; with `--profile`, the Drive commands use the records folder's.
  `medrec drive profiles list` shows them. `medrec drive status` checks a
  sign-in (`--local` reads no Keychain), `logout [--revoke]` ends one, and
  `about` prints the account and its storage.
- **The folder is a spec**, `gdrive:<drive profile>/<folder id>`, where the
  folder id is the last part of the folder's Drive URL. A store at a path
  under it is `gdrive:<drive profile>/<folder id>/<path>`, as the inbox is.
  A folder whose Drive profile is not signed in on this machine is exit 2,
  naming `medrec drive login`.
- **Scope.** `drive login` asks for `drive.file` unless given
  `--scope full`. Under `drive.file`, medrec sees only the files a client
  of the same Cloud project created. A folder made in the Drive web app, or by rclone,
  is invisible to it, and so are files another app adds: an upload in the
  Drive web app, a file copied in through Drive for desktop, a scan another
  tool uploads. That covers the inbox and `intake --drive`. Point `inbox` at
  a local directory (the inbox as Drive for desktop mounts it) for files
  that arrive that way, or sign in with `--scope full`. `drive` is a
  restricted scope: Google shows its "unverified app" warning at consent.
- **Moving a folder off rclone.** medrec no longer reads an rclone remote
  (`remote:path` is exit 2, naming the move). Under `drive.file` the old
  folder is not visible to the new sign-in, so the records are copied once
  into a folder medrec makes:

  0. On every machine that refreshes the folder, stop its agent with the
     release it runs: `medrec --profile <NAME> refresh --uninstall`. An
     agent left running would keep importing into the old folder. Then
     wait for any medrec command still running on that profile to finish,
     a manual `import` or `intake` included: it writes too, and a copy
     taken meanwhile misses or splits what it writes.
  1. With the medrec release before this one, back the folder up to a
     local directory: `medrec backup --to ~/records-copy`.
  2. In the Drive web app, rename the old folder (for example to
     `Medical Records (moved)`). A binary before 0.6.0 that still names it
     then stops on a missing `household.json` instead of writing there.
     Do it before the next step: two folders with one name in My Drive's
     root make an rclone path such as `remote:Medical Records` name
     either of them.
  3. With this release, sign in and make a folder: `medrec drive login …`,
     then `medrec drive folder create "Medical Records"`.
  4. Point a profile at the copy, and back it up into the new folder:
     `medrec profiles add move`,
     `medrec --profile move config set folder ~/records-copy`, then
     `medrec --profile move backup --to gdrive:<drive profile>/<folder id>`.
     The backup compares every file's MD5 on both sides.
  5. Set the household's profile to the new folder (`medrec --profile
     <NAME> config set folder gdrive:<drive profile>/<folder id>`), run
     `medrec --profile <NAME> verify`, and remove the `move` profile and
     the local copy.
  6. Build the views once (`medrec --profile <NAME> views write`; see
     Views), then install
     the agent again with this release: `medrec --profile <NAME> refresh
     --install`.

  Going back to a release before 0.6.0 means setting `folder` to the
  rclone remote again; such a binary reads a `gdrive:` spec as an rclone
  path.

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
medrec review file --from held.ndjson            # many at once: one JSON object per line
medrec intake --from scans.ndjson                # the same for files, each with its own values
medrec documents list --person Robin
medrec documents merge <copy> --into <id>       # a document filed twice: one kept

medrec images intake /Volumes/CDROM             # a disc, folder, DICOMDIR, .zip or .iso
medrec images list
medrec images open <id>                         # a private copy, opened in Weasis

medrec providers list                           # provider CLIs on PATH
medrec import --from mychart --from-profile adult1 --patient <id> --person Robin
medrec import --from mychart --from-profile adult1 --patient <id>   # later: the FHIR id or label
medrec import --from uhc --from-files ~/Downloads/statements --patient <id>   # downloaded files
medrec import --from uhc --from-profile adult1 --patient <a> --person Alex --patient <b> --person Robin

medrec timeline --person Robin --since 2026-01-01
medrec events list --person Robin               # what happened: one line per visit, stay, test or fill
medrec bills list --person Robin                # each bill: its claim, what is paid and left
medrec private list --person Robin              # what stays off shared screens, and why
medrec labels list --person Robin               # what the household said of a provider
medrec search hemoglobin
medrec results --code 718-7
medrec meds --active
medrec note add Robin "Ask about the recheck" --tag question --about <record id>

medrec verify                                   # every indexed file against the index
medrec reindex                                  # claims shaped again, providers, statuses and what else served a paper read again from the raw files, and bills and receipts read again, after an upgrade
medrec backup --to "/Volumes/Backup/Medical Records"
```

A `--from` list holds one JSON object per line, with the keys the flags
name: `{"id": "<review id>", "person": "Robin", "type": "lab_report",
"date": "2026-03-04", "org": "Example Health"}` for `review file`, and
`"file"` in place of `"id"` (plus `"classified_by"`) for `intake`. `-`
reads standard input. Every line is checked before anything is filed, so a
bad type or date stops the list with nothing changed. The lines are filed in
order under one lock, a line that fails is reported while the rest are still
filed (exit 1), and the views are written once, at the end, rather than
after each filing. Each item in the output names its list `line`, and a line that failed
carries `error` and `error_code` (`not_found`, `usage`, ...).

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

`--patient` repeats for several members of one login, each with its
`--person` in the same order (or none, for members imported before). Every
member is exported first, each into an owner-only file on this machine,
and only then filed, so a website session that ends within the hour serves
them all before uploads to a Drive folder take their time. Spooled
exports live only as long as the command; one a killed command left is
removed by the next export. One member
that fails leaves the others filed; the exit code is the first failure's,
and `--json` prints one `import-batch` document with each member's import
or error; one `--patient` prints the `import` document as before.
`medrec refresh` does the same with every source a pass runs.

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
file [the own-your-health app](https://own-your-health.github.io/app/)
writes in `_requests/` when someone presses Refresh), or now, and records
every attempt in `_status/` ([data contract](docs/data-contract.md)). The
app shows `_status/` and asks for refreshes from a phone. A
scheduled pass lists `_requests/` once; when nothing is requested, nothing
is due and the hourly heartbeat is not due, it stops there.

| Provider | Least time between imports | Default schedule |
|---|---|---|
| A developer app (`mychart`, `athena`, `kp`, `modmed`) or a portal key (`icanotes`) | 6 hours | daily |
| A member site (`uhc`, `caremark`, `labcorp`, `oneblood`, `fertemr`, `salve`) or any other CLI | 24 hours | daily |

`refresh add --every <DURATION>` sets the schedule, never below the least
time; `--manual` imports only when asked. The interval is counted from the
source's latest import in the person's `index.json`, so an import run by
hand counts. After a failure the schedule waits 30 minutes, doubling with
each failure after, up to its least time.

A person asking (a request, or `medrec refresh` at the terminal) is not held
to the schedule's least time. Its own guard imports a source at most once
per 15 minutes, counted from the latest import, attempt or failure, so a
Refresh pressed soon after a load is skipped only until 15 minutes have
passed. The skip says when the next load is allowed.

A pass signs in and exports every source it runs before it files any of
them, so the members on one short website session (`uhc`, `fertemr`,
`caremark`, `oneblood`, `flywire`, `peryourhealth`: those whose
`sign_in.renews` is false) are exported back to back while it is fresh. Each export waits in an owner-only file on this machine until it
is filed.

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

medrec's own Drive sign-in is held to a stricter rule: a scheduled pass
raises no Keychain dialog for it. It reads the key first, with macOS's
dialogs turned off and nothing sent to Google. A key that macOS would ask a
person about (a new build or an update not yet allowed to read it, a locked
keychain) ends the pass with exit 3, and its line in the log says why: run
`medrec auth key` at the screen and choose "Always Allow". A vault whose key is
gone is exit 1.
The next pass looks again, without a dialog, so it goes on once the key is
allowed. A pass that cannot read the key cannot reach the folder, so it writes
nothing there: `refresher.last_pass_at` stops moving, and the folder's readers
see the refreshing machine as offline. Where the system has no such quiet read
(Linux), a scheduled pass of a folder on Drive is exit 1, since any read could
ask someone; `medrec refresh` at a terminal works there.

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
person, else 0; `--scheduled` exits 0, unless the pass could not be made
(medrec's own key, above). `refresh sign-in` exits 3 when the
source still needs a person. `refresh status --json` prints `"status": null`
for a folder refresh has never run on. A subcommand that acts on one bound
source takes its id as an argument (`refresh sign-in <id>`, `pause`,
`resume`, `remove`); `--source` picks one for a pass or binds one.
`--provider-profile` is `import --from-profile`'s login, and takes that name
too.

### Events

An event is what happened, as the household would say it: an ER trip, a
hospital stay, an office visit, a lab draw, a vaccine or a pharmacy fill. It
stays one event however many claims, portal records and papers it produced:
an ER visit billed by the hospital, the on-call doctor, the radiologist and
the lab is one event, with the hospital as its place and the household's
share summed from the plan's claim totals. medrec groups them by matching
rules on dates, providers and codes, and each grouping says how sure it is
(`sure`, `likely`, `unsure`) and what is still expected (doctors' claims
that usually follow a facility's, a bill not yet filed).

```sh
medrec events list --person Robin --since 2026-01-01   # newest first, with the share
medrec events show <event id>                           # its money, members and papers
medrec events apart <record or document id> <another>   # not the same visit
medrec events together <id in the event> <id to attach> # attach to this visit
medrec events decisions --person Robin                  # the household's decisions
medrec events undo <decision id>                        # the rules decide again
```

`show` takes an event id (`e_…`), and `undo` a decision id (`o_…`), which
`events decisions` lists. `apart` and `together` take two of a person's
record (`r_…`) or document (`d_…`) ids, never an event id. An event's id
comes from the claim or record that opened it, so it changes when that
member leaves the event; a decision names members so that it survives
every rebuild. They go to the person's
`overrides.json`, which outranks the rules. The app asks for the same
through `_requests/`, and the refreshing machine records it on its next
pass. Each person's events are also a view, `views/<person id>/events.json`,
for the app and the connector ([docs/data-contract.md](docs/data-contract.md),
"Events").

### Bills

Each bill is matched to the claim it bills and the event it belongs to,
and to the receipts that pay it. A bill the household paid with no
receipt filed can be marked paid.

```sh
medrec bills list --person Robin                              # newest first: asked, paid, left
medrec bills paid <bill document id> --on 2026-11-20 --amount 120.00
medrec bills payments --person Robin                          # the payments marked
medrec bills undo <payment id>
```

`bills list` also shows the bills and desk payments worth a call: a bill
that asks more or less than its claim says is owed, a bill with no claim
behind it, a copay paid at the desk that the claims say was not owed. A
settled bill is not among them, nor one that says it charges for storage or
is self-pay for want of a claim, since no insurer is billed for those. It
then lists the shares the claims say are owed that no bill has asked for
yet, with pharmacy fills marked, since those are usually paid at the
counter. `--limit` bounds the bills; the other two lists are shown whole.
`paid` takes a `bill` document's id (`d_…`) and `undo` a payment id
(`o_…`) that `bills payments` lists. Payments are kept in the person's
`marks.json`, apart from the event decisions in `overrides.json`.
Receipts give what was paid and
when from their text, and statements the number to call; after upgrading
to 0.9.1, `medrec reindex` reads the bills and receipts filed before. Each
person's bills are also a view, `views/<person id>/bills.json`
([docs/data-contract.md](docs/data-contract.md), "Bills").

### What is current

A condition, an allergy or a medicine often comes from several sources, and
they can differ. medrec keeps each source's word with the day it was read, and
no source's word wins:

- It is current while any source says so. Where another says otherwise, its
  line says `sources differ` and gives each word.
- A diagnosis given for a visit is not a current condition. It is in the list
  without `--active`, and a list of current conditions names each source that
  sends diagnoses and no problem list.
- What a source calls refuted is not current. A record of any kind entered in
  error is in no list, the timeline or the search, which say how many are not
  shown. It is one only while every source that has it says so, and comes back
  by itself when one no longer does. A record one source still states shows
  that one's values.
- A source's word that medrec does not keep yet never lets the others end a
  record: its line says `not every source heard`. After upgrading to 0.9.9 run
  `medrec reindex`, which reads them from the raw files.
- A prescription whose end date has passed is not current: it follows the
  current ones with that date and any fill after it, as one on hold does. A
  fill is a fact and changes nothing. An allergy is never ended by a date.

`medrec conditions`, `allergies` and `meds` show what each is and what each
source says of it, and `--active` the current ones. The person's
`standing.json` holds the same for readers (docs/data-contract.md,
"Standing").

### Private items

Records, documents, events and imaging studies about mental health,
reproductive and fertility care, sexual health or substance use, and the
medicines tied to them, are private: the app keeps them off shared screens (Home, the
Everyone calendar, a doctor sheet for someone else) and shows them on that
person's own pages, and Claude leaves them out of answers about the whole
household. medrec finds them by their codes, medicines, tests and the
practice that made them, a document by any private record that links it, a
bill or receipt by its own words, with its copies and the statements and
payments before and after it, and a study by its report. A study with no report is left out of answers
about the whole household too. The household has the last word.

```sh
medrec private list --person Robin          # each private item, its area and why
medrec private mark <record, document or event id>     # private, whatever the rules say
medrec private unmark <id>                  # not private, whatever the rules say
medrec private reset <id>                   # back to the rules
```

This is discretion, not access control: anyone who can read the folder
can read every record in it. Marks are kept in the person's `marks.json`
with the payments, and each person's private items are a view,
`views/<person id>/private.json`
([docs/data-contract.md](docs/data-contract.md), "Private").

### Provider labels

A claim may print a provider as `SAMPLE J` and nothing more. medrec puts a
name, a practice, a role and a phone beside it where it has them, and says
why: *sure* where the record carries the provider's NPI and the provider
directory (below) has it, *likely* where the directory has one person it can
be (or several, and the claim spells out the first name of one) or a paper of
the same visit prints the name. Where the household knows
better, it says so, and that wins:

```sh
medrec labels set --person Robin --printed "SAMPLE J" --name "Dr. Jordan Sample" --role radiologist
medrec labels set --person Robin --printed "SAMPLE J" --phone 555-010-0123 --item <record, document or event id>
medrec labels same --person Robin --printed "SAMPLE J" --printed "Jordan Sample MD"       # one provider
medrec labels different --person Robin --printed "SAMPLE J" --printed "SAMPLE A"          # two
medrec labels wrong --person Robin --printed "SAMPLE J"      # the label is wrong: none
medrec labels list
medrec labels undo <id>
```

- `--printed` is the name exactly as the items print it. `--item` is for that
  one item. Without it, what is said is of every item of the person's that
  prints the name.
- `set` replaces what was set before. A part left out is as a sure label has
  it, and never a likely one's.
- Nothing is edited: the record keeps its own text, beside the label. What the
  household says is kept in the person's `marks.json`, with who said it in the
  app and when, and is taken back with `undo`.
- A label, sure or the household's, in an area that makes items private makes
  the items that print the name private ("Private items", above).
- The app writes the same through `_requests/`. Each person's labels are a
  view, `views/<person id>/providers.json`
  ([docs/data-contract.md](docs/data-contract.md), "Provider labels").

### Views

`views/` at the top of the records folder holds each person's records again,
cut into small files: a summary, records by kind in shards of about 48 KB,
the timeline, the documents with their text in packs, and a search index by
word. A reader that cannot parse a whole `index.json` per request, such as
the hosted [records connector](https://github.com/own-your-health/connector)
on Cloudflare's free plan, opens only the files its answer needs. The layout
is in [docs/data-contract.md](docs/data-contract.md) ("views/").

medrec keeps them up to date itself. Every command that writes a person's
`index.json` or `text.json`, or `household.json`, rewrites the views of the
people whose files changed before it releases the folder. Each view's name
carries its MD5, and only new views are uploaded, so a refresh that adds a
few records writes a few dozen small files per person. A refresh pass, or an
import of several members, does this once at the end. `views.json` records
the MD5 of every file the views were made from, so a reader can tell when
they are stale.

On Drive, the views a pass replaces go to the trash, which keeps them for
30 days against the storage quota. One new result replaces about 30 views
(0.9 MB) for a person with about 1,600 results; SECURITY.md has more.

The first build after upgrading to 0.8.0 writes every view (100 to 340
files per person in medrec's synthetic test folders) and holds the folder
while it uploads. On
a Drive folder, run `medrec views write` once from a terminal right after
upgrading, so a scheduled pass or the app's refresh does not wait on it.

```sh
medrec --profile <NAME> views write          # bring them up to date now
medrec --profile <NAME> views write --all    # make every person's again
```

`views write --json` prints `views-write/v1`: `people`, `people_rebuilt`
(people whose views were made again), and `files_written`, `files_unchanged`
and `files_removed`.
A failure to write them leaves them stale and says so on stderr. The command
that wrote the records still succeeds, and the next write tries again.

### Code tables

A condition's code has an official title, and a portal's own words for it can
be an abbreviation. `medrec tables` keeps the published titles on this
machine, so that readers can show "Essential (primary) hypertension" where the
source gives a bare code, an abbreviation or capitals, and beside the source's
words where those are plain. The records keep the source's words, and search
and filters go by them.

```sh
medrec tables list                      # the tables, and whether each is here
medrec tables fetch                     # download them
medrec tables fetch icd10cm --file ZIP  # or keep a zip downloaded by hand
```

| Table | Publisher | Version | File |
|---|---|---|---|
| `icd10cm`, ICD-10-CM code titles | Centers for Medicare & Medicaid Services | FY2027, dated 2026-06-01 | [2027 Code Descriptions in Tabular Order](https://www.cms.gov/files/zip/2027-code-descriptions-tabular-order.zip), 2.2 MB, 98,403 rows |

- **medrec includes no table.** It downloads the publisher's whole file, and
  keeps it only if its size, SHA-256 and row count are those recorded in
  `src/tables.rs`. No code is ever looked up over the network, so no request
  says which codes the records hold.
- **No table, no new names.** Without the file, or with one that is not the
  recorded one, a pass keeps the names another machine wrote for codes still
  in use, and adds none. A code with no name shows the source's own words. A
  code the table lacks (another year's, withdrawn or invalid) has no name.
- The file is in `~/.cache/medrec/tables`. After fetching it, run `medrec
  views write` on each profile, or let the next write do it.
- The names a person's records use are in `views/` as `code-names.json`
  (docs/data-contract.md). Like any view, they are on the Drive folder.

medrec is not affiliated with or endorsed by the publisher.

### Provider directory

CMS publishes every National Provider Identifier (NPI) with its provider's
name, practice address and phone: the NPPES file, over a gigabyte, replaced
every month. medrec reads it once, on request, and keeps the rows a records
folder's providers can be found in.

```sh
medrec --profile <NAME> tables fetch nppes          # states the size, asks, downloads
medrec --profile <NAME> tables fetch nppes --force  # states the size and downloads
medrec tables list                                  # what is kept, and of which month
```

- **Only when asked.** `tables fetch` with no name leaves it out, and nothing
  runs it on a schedule. The size is stated before the download starts. With
  no terminal, or with `--json`, every run needs `--force` (exit 6 without
  it), one that finds the same file kept and downloads nothing included.
- **Nothing about the records is sent.** medrec asks `download.cms.gov`, over
  https, for the page that lists the file, the file's size, and the whole
  file. No name or number is looked up over the network.
- **What is kept**, in `~/.cache/medrec/directory` and never in the records
  folder: the rows of the providers whose NPI the records carry, the organizations of the
  states the records' own provider addresses are in, and the people of those
  states whose surname and first initial a record prints with no NPI. The zip
  and the files in it are never on disk: they are read as they arrive.
- **Checked.** The columns are found by name, every NPI by its check digit, the
  zip by its size and the checksums inside it, and a file with fewer than
  9,000,000 rows is refused. A file that fails leaves the last one in use.
  Each month's file replaces the one before, so there is no hash to record
  here: its link, month, size, SHA-256 and row count are kept with the rows.
- Records need their providers' NPI, state and city: those filed before 0.9.8
  get them from `medrec reindex`.
- **What it is for**: a name, role and phone beside a provider a record
  prints, sure or likely and with the reason, in `views/` as
  `providers.json` (docs/data-contract.md, "Provider labels"). The record's
  own text stays as it is. After fetching the directory, run `medrec views
  write`, or let the next write do it. A machine with no directory keeps the
  labels another wrote, and adds none.
- The kept rows are public ones, but which people are among them follows from
  the records. `medrec cache clear` removes them. The labels taken from them
  stay in the records folder: in `views/`, and by NPI, private providers' with
  the rest, in each person's `provider-state.json`.
- `tables list --json` has one row for each records folder it was read for,
  with `folder` (the folder's id) and `kept` (how many rows), and one row with
  neither, and null `version`, `date`, `bytes`, `rows` and `sha256`, when there
  is none. `changed` is a file that does not read as it was written, and
  `other_version` one that another version of medrec kept: neither is used.
  medrec 0.9.7 does not use what 0.9.6 kept, so fetch it again after that
  upgrade; a write says so on stderr until then.

NPPES gives what providers reported to CMS. medrec is not affiliated with or
endorsed by CMS.

### Settings

`medrec config set <key> <value>`, kept in `~/.config/medrec/config.json`.
The first four are the profile's, so they take `--profile`; the rest are
this machine's:

| Key | What it sets | Default |
|---|---|---|
| `folder` | the profile's records folder, `gdrive:<drive profile>/<folder id>` or a local directory | set by `init` |
| `mount` | where Drive for desktop mounts the folder, for `doctor` to compare | none |
| `inbox` | the inbox `--inbox` drains: a Drive spec or a local directory | `inbox/` in the folder |
| `me` | the person id who writes notes on this machine | none |
| `viewer` | the DICOM viewer, or `none` | Weasis |
| `ocr_languages`, `ocr_max_pages` | tesseract languages, and pages read per document | `eng`, 20 |
| `max_file_mb` | the largest file intake or import keeps, MiB | 200 |
| `import_max_mb` | the largest records stream one import reads, MiB; an import holds it until the trailer | 4096 |
| `import_timeout_secs`, `store_budget_secs` | how long one provider export, and all of one command's Drive requests, may take (`rclone_budget_secs` is read as `store_budget_secs`) | 1800, 1800 |
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
no longer has the bytes filed, checked by its MD5 (a file the store reports
no MD5 for is downloaded again and hashed; 0.2.10 and later).

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
| `medrec_summary` | The household's people. With `person`: their current medicines (and those on hold or ended by their date), conditions and allergies, immunizations, the latest result of each test with the lab's range and flag, recent visits, and bills with an amount due |
| `medrec_search` | Documents, records, imaging studies and notes holding every word of a query; a query naming lab tests (`blood tests`, `labs`) also finds every lab result, report and lab claim |
| `medrec_records` | One kind, newest first: `medications`, `conditions`, `allergies`, `immunizations`, `visits`, `procedures`, `results` (with `name`, one test over time), `tests` (the latest result of each), `reports`, `claims`, `coverage`, `bills`, `documents`, `notes`, `timeline` or `events` (what happened: one per visit, ER trip, stay, test or fill, with `you_owe`; `id` gives one with its money and members); `active` keeps current ones (not for `events`, whose `status` says which are still waiting), and conditions, allergies and medicines say what each source says of them ("What is current") |
| `medrec_document` | A filed document's extracted text, in parts of up to 16,000 characters, with page breaks marked |

Private items (above) are left out of an answer about the whole household,
whose warnings say how many. An answer about one person includes them, and
so does one that names the item ("sertraline", not "tablets"), each marked
`private` with its area.

Claims, events, documents and bills carry `provider_labels` beside the source's
words: who a printed provider is (Provider labels, above), how sure that is and
why, and where a phone is from. `detailed` adds them for every record that
prints one. A private item has them only in an answer about its person.

Claude Desktop: install the release's `medrec-<version>.mcpb` (open the
file, or Settings > Extensions > Install Extension) and enter the profile's
name when asked. The extension carries its own medrec binary and reads the
same `~/.config/medrec/config.json`, Drive sign-in and folder as the
installed one. Its binary is a separate copy, so its first read of a Drive
folder asks macOS once to let it use the Keychain item `piekstra.medrec`.
A dialog left unanswered for 45 seconds ends that call with `auth`, and the
next call waits on the same dialog rather than raising another. Once
answered, the server reads the Keychain again only if the key is replaced
(a reset and a new sign-in in a terminal), with the same 45-second limit.
"Always Allow" also spares the next server it starts.

```sh
d=$(mktemp -d) && gh release download -R own-your-health/medrec-cli -p 'medrec-*.mcpb' -D "$d" && open "$d"/medrec-*.mcpb
```

Claude Desktop can refuse the extension: "Extension
local.mcpb.own-your-health.medrec is blocked by a security blocklist: Could
not verify extension with allowlist service". Desktop checks a local
extension against Anthropic's allowlist service where that check is on, and
medrec's extension is not on the list. There, use a claude.ai custom
connector that reads the folder, such as
[own-your-health/connector](https://github.com/own-your-health/connector),
which Desktop lists too, or Claude Code below.

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
  laptop, a lost Drive sign-in or Google account).
- [PRIVACY.md](PRIVACY.md): what this public repo may and may not contain.
- [docs/data-contract.md](docs/data-contract.md): the folder layout and
  formats.
- [docs/GENERALITY.md](docs/GENERALITY.md): any household, any subset of
  sources.
- [AGENTS.md](AGENTS.md) and [CONTRIBUTING.md](CONTRIBUTING.md): working on
  medrec.

## License

Copyright (C) 2026 Caleb Piekstra. Licensed under the GNU Affero General Public License, version 3 only (AGPL-3.0-only); see LICENSE. Releases up to and including v0.9.0 were published under MIT OR Apache-2.0.
