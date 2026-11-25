//! medrec: a household's medical records in a plain Drive folder
//! (piekstra-cli/1). The command tree and output live here; the mechanisms
//! live in the library.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::PathBuf;

use clap::{Args, CommandFactory, Parser, Subcommand};
use clap_complete::Shell;
use pk_cli_auth::{AuthStatus, KeyArgs};
use pk_cli_config::profile::ProfileName;
use pk_cli_config::ConfigStore;
use pk_cli_core::info::{AuthInfo, CliInfo};
use pk_cli_core::{confirm, output, CliError, CommonArgs};
use pk_cli_drive::google::{DriveCommand, StatusArgs};
use pk_cli_selfupdate::{SelfUpdateArgs, Updater};
use serde::Serialize;
use serde_json::{json, Value};

use medrec::config::{Config, FolderSettings, KEYS};
use medrec::folder::{Access, Folder};
use medrec::intake::{self, Given};
use medrec::store::{Location, BIN};
use medrec::views::Range;
use medrec::{imaging, import, maint, notes, nppes, people, review, store, tables, view, views};

const REPO: &str = "own-your-health/medrec-cli";

/// A household's medical records in a plain Drive folder: people, intake of
/// scans and files with local OCR, imaging studies, provider and insurer
/// imports, timelines, search and notes.
#[derive(Parser, Debug)]
#[command(name = BIN, version, long_version = pk_cli_core::version!(), about, long_about = None)]
struct Cli {
    #[command(flatten)]
    common: CommonArgs,
    /// The profile, one per records folder (env MEDREC_PROFILE). Required
    /// by every command that reaches a records folder; there is no default.
    #[arg(long, global = true, value_name = "NAME", value_parser = profile_name_text)]
    profile: Option<String>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// The profiles on this machine, one per records folder.
    #[command(subcommand)]
    Profiles(ProfilesCmd),
    /// Set up a records folder (or adopt one) as the profile's.
    Init(InitArgs),
    /// The people whose records these are.
    #[command(subcommand)]
    People(PeopleCmd),
    /// File documents: scans, PDFs and images, from files, the inbox or a
    /// Drive folder.
    Intake(IntakeArgs),
    /// What intake could not decide.
    #[command(subcommand)]
    Review(ReviewCmd),
    /// Import records from a provider or insurer CLI (records-stream/v1).
    Import(ImportArgs),
    /// Provider CLIs medrec can import from.
    #[command(subcommand)]
    Providers(ProvidersCmd),
    /// Published code tables kept on this machine: the plain names of
    /// diagnosis codes.
    #[command(subcommand)]
    Tables(TablesCmd),
    /// Import each source set up for it: on a schedule (a LaunchAgent), on
    /// a reader's request in `_requests/`, or now.
    Refresh(RefreshArgs),
    /// Everything dated, newest first: visits, results, documents, studies,
    /// notes.
    Timeline(TimelineArgs),
    /// Find documents, records, studies and notes by their words.
    Search(SearchArgs),
    /// Lab results and reports.
    Results(RecordArgs),
    /// Medications.
    Meds(RecordArgs),
    /// Conditions and problems.
    Conditions(RecordArgs),
    /// Allergies and intolerances.
    Allergies(RecordArgs),
    /// Immunizations.
    Immunizations(RecordArgs),
    /// One record by its id, and the ids of what is in a chart and about
    /// somebody else.
    #[command(subcommand)]
    Records(RecordsCmd),
    /// Filed documents.
    #[command(subcommand)]
    Documents(DocumentsCmd),
    /// What happened: each visit, ER trip, stay, test or fill, with the
    /// claims, encounters, results and papers it produced.
    #[command(subcommand)]
    Events(EventsCmd),
    /// Bills: what each asks, the claim it bills, what was paid and what is
    /// left; those worth a call; and shares no bill has asked for yet.
    #[command(subcommand)]
    Bills(BillsCmd),
    /// Private items: mental health, reproductive and sexual health and
    /// substance use, kept off shared screens and household-wide answers.
    /// Discretion, not access control.
    #[command(subcommand)]
    Private(PrivateCmd),
    /// What the household says of the providers its items print: a name, a
    /// practice, a role or a phone, that two printed names are one provider
    /// or are not, or that a label is wrong. It edits no record.
    #[command(subcommand)]
    Labels(LabelsCmd),
    /// Dated notes, kept in each person's notes.md.
    #[command(subcommand)]
    Note(NoteCmd),
    /// Imaging studies (DICOM).
    #[command(subcommand)]
    Images(ImagesCmd),
    /// Check this machine and the records folder.
    Doctor,
    /// Check every indexed file is in the folder with the hash the index
    /// recorded.
    Verify(VerifyArgs),
    /// Shape every claim again, and read every record's providers and every
    /// condition's, allergy's and medicine's fields and what each source says
    /// of it again, from the filed raw resources, put each result about
    /// somebody else that was filed as the person's own in a record of its
    /// own, and read bills' and receipts' statements again, as this medrec's
    /// import does: once after an upgrade that changes them.
    Reindex(ReindexArgs),
    /// The views: each person's records cut into small files under views/,
    /// for readers that cannot parse a whole index at once. medrec brings
    /// them up to date after every write.
    #[command(subcommand)]
    Views(ViewsCmd),
    /// Copy the whole records folder to another Google Drive folder or a
    /// directory, then compare every file. A file the backup already holds
    /// with other bytes is replaced; files only the backup holds are kept.
    Backup(BackupArgs),
    /// Google Drive sign-ins: one Drive profile per Google account, through
    /// medrec's own OAuth client, tokens in the Keychain.
    Drive(DriveArgs),
    /// Non-secret settings.
    #[command(subcommand)]
    Config(ConfigCmd),
    /// The records folder's Google Drive sign-in (auth-status/v1); none for
    /// a folder on a local disk.
    #[command(subcommand)]
    Auth(AuthCmd),
    /// Update to the latest release from GitHub.
    SelfUpdate(SelfUpdateArgs),
    /// Print a shell completion script.
    Completions { shell: Shell },
    /// Machine-readable capability discovery (cli-info/v1).
    Info,
    /// Serve the records folder to Claude Desktop and Claude Code as
    /// read-only MCP tools over stdio.
    Mcp(pk_cli_mcp::McpArgs),
    /// This machine's copies of records-folder files: the read cache and
    /// opened documents and studies
    #[command(subcommand)]
    Cache(CacheCmd),
}

#[derive(Subcommand, Debug)]
enum ProfilesCmd {
    /// The configured profiles (profile-list/v1).
    List,
    /// Add a profile; `medrec --profile <NAME> init <LOCATION>` then gives
    /// it its records folder.
    Add {
        #[arg(value_parser = profile_name)]
        name: ProfileName,
    },
    /// Remove a profile from this machine. Its records folder and the files
    /// in it are left as they are.
    Remove {
        #[arg(value_parser = profile_name)]
        name: ProfileName,
        /// Do not ask.
        #[arg(long)]
        force: bool,
    },
    /// Move the records folder this machine used before profiles into a
    /// profile, adding it when it is new. Local only.
    Migrate {
        #[arg(value_parser = profile_name)]
        name: ProfileName,
    },
}

/// With no subcommand, a pass: every bound source, or one `--person`'s or
/// `--source`'s. A subcommand that acts on one bound source takes its id
/// as an argument (`medrec refresh sign-in <id>`); `--source` picks or binds
/// one.
///
/// Exit codes of a pass: 0 when nothing failed, 5 when a source failed, 3
/// when none failed and one needs a person (`refresh sign-in <id>`);
/// `--scheduled` exits 0. `refresh status --json` prints `"status": null`
/// for a folder refresh has never run on.
#[derive(Args, Debug)]
#[command(args_conflicts_with_subcommands = true)]
struct RefreshArgs {
    #[command(subcommand)]
    cmd: Option<RefreshCmd>,
    /// Only this source (its `s_` id).
    #[arg(long, value_name = "ID", conflicts_with = "person")]
    source: Option<String>,
    /// Only this person's sources (name or id).
    #[arg(long)]
    person: Option<String>,
    /// Run as the LaunchAgent does, with nobody at the terminal: only what
    /// is due or requested, and a sign-in that waits on a person is not
    /// waited for.
    #[arg(long, conflicts_with_all = ["source", "person"])]
    scheduled: bool,
    /// Install the LaunchAgent that runs a scheduled pass every
    /// `--pass-every` (5 minutes unless given), as this profile.
    #[arg(long, conflicts_with_all = ["uninstall", "scheduled", "source", "person"])]
    install: bool,
    /// How often the LaunchAgent looks for due sources and requests: `90s`,
    /// `5m`. Not how often a source is imported (`refresh add --every`).
    #[arg(long, requires = "install", value_name = "DURATION", value_parser = duration)]
    pass_every: Option<u64>,
    /// Move refresh of this folder to this machine from the one that does
    /// it now.
    #[arg(long, requires = "install")]
    take_over: bool,
    /// Remove this profile's LaunchAgent.
    #[arg(long, conflicts_with_all = ["scheduled", "source", "person"])]
    uninstall: bool,
}

#[derive(Subcommand, Debug)]
enum RefreshCmd {
    /// Set a source up for refresh, bound to the provider login it imports
    /// through. Needs `--profile` on the command line.
    Add {
        /// The source (its `s_` id, from `medrec people show`).
        #[arg(long, value_name = "ID")]
        source: String,
        /// The provider CLI's own profile (its login) to import through, as
        /// `import --from-profile`. Required for a provider that signs in;
        /// refused for one that needs no login.
        #[arg(long, alias = "from-profile", value_name = "NAME", value_parser = profile_name_text)]
        provider_profile: Option<String>,
        /// The patient at the provider, when the source does not say.
        #[arg(long)]
        patient: Option<String>,
        /// The provider organization, when the source does not say.
        #[arg(long)]
        org: Option<String>,
        /// Import only when asked (a request or `medrec refresh`), never on
        /// the schedule.
        #[arg(long)]
        manual: bool,
        /// How often it is imported on the schedule (default daily; never
        /// below the provider's minimum). Not `--install --pass-every`.
        #[arg(long, value_name = "DURATION", value_parser = duration)]
        every: Option<u64>,
    },
    /// Stop refreshing a source. Its records stay.
    Remove { source: String },
    /// The sources set up for refresh in this profile (refresh-list/v1).
    List,
    /// What refresh last recorded for each source (`_status/status.json`).
    Status,
    /// Stop a source's imports, scheduled and requested, until `resume`.
    Pause {
        source: String,
        #[arg(long, value_enum)]
        reason: PauseReason,
        /// Why, for the household (no record content).
        #[arg(long)]
        note: Option<String>,
    },
    /// Let a paused source be imported again.
    Resume { source: String },
    /// Sign in a source's provider login, with a person at the terminal,
    /// and clear "needs a person". Takes a source id: the profile is the
    /// one that set the source up (see README, Profiles).
    SignIn { source: String },
}

#[derive(clap::ValueEnum, Clone, Copy, Debug)]
enum PauseReason {
    /// Waiting on a consent or a terms decision.
    Consent,
    /// The household paused it.
    User,
}

fn duration(s: &str) -> Result<u64, String> {
    medrec::refresh::time::parse_duration(s)
}

#[derive(Args, Debug)]
struct InitArgs {
    /// `gdrive:<drive profile>/<folder id>` (canonical; `medrec drive folder
    /// create` makes one) or a local directory.
    location: String,
    /// Set up medrec in a folder that already holds other files, except a
    /// `views` folder, where medrec writes its views.
    #[arg(long)]
    allow_other_files: bool,
}

#[derive(Subcommand, Debug)]
enum TablesCmd {
    /// The tables medrec knows, and whether each is on this machine.
    List,
    /// Download whole tables from their publishers (all the code tables, or
    /// those named), checked against the SHA-256 medrec records. The provider
    /// directory, `nppes`, is fetched only when it is named: one file of over
    /// a gigabyte, whose size is stated first, read as it arrives for the
    /// providers the --profile folder's records need.
    Fetch {
        /// Tables by id (default: the code tables).
        ids: Vec<String>,
        /// Keep this downloaded zip as the one code table named.
        #[arg(long, value_name = "ZIP", requires = "ids")]
        file: Option<std::path::PathBuf>,
        /// Fetch `nppes` without asking. Every run of it with no terminal,
        /// or with --json, needs this.
        #[arg(long, requires = "ids")]
        force: bool,
    },
}

#[derive(Subcommand, Debug)]
enum PeopleCmd {
    /// Add a person.
    Add(PersonAddArgs),
    /// List the people.
    List {
        #[command(flatten)]
        list: ListArgs,
    },
    /// One person, with counts of what is filed for them.
    Show { person: String },
    /// Change a person (their folder keeps its name).
    Edit(PersonEditArgs),
}

#[derive(Args, Debug)]
struct PersonAddArgs {
    /// Their name as it appears on records.
    name: String,
    /// YYYY-MM-DD.
    #[arg(long)]
    birth_date: Option<String>,
    /// An adult (default: from the birth date).
    #[arg(long, conflicts_with = "child")]
    adult: bool,
    /// A child.
    #[arg(long)]
    child: bool,
    /// Who looks after a child's records (a person, repeatable).
    #[arg(long = "guardian")]
    guardians: Vec<String>,
    /// Another name records may use (repeatable).
    #[arg(long = "alias")]
    aliases: Vec<String>,
    /// Their Huishouden Health person id (a link only; nothing is read).
    #[arg(long)]
    huishouden_health_person: Option<String>,
    /// Their Huishouden Baby profile id (a link only).
    #[arg(long)]
    baby_profile: Option<String>,
}

#[derive(Args, Debug)]
struct PersonEditArgs {
    person: String,
    #[arg(long)]
    name: Option<String>,
    /// YYYY-MM-DD.
    #[arg(long)]
    birth_date: Option<String>,
    #[arg(long, conflicts_with = "child")]
    adult: bool,
    #[arg(long)]
    child: bool,
    #[arg(long = "add-guardian")]
    add_guardians: Vec<String>,
    #[arg(long = "remove-guardian")]
    remove_guardians: Vec<String>,
    #[arg(long = "add-alias")]
    add_aliases: Vec<String>,
    #[arg(long = "remove-alias")]
    remove_aliases: Vec<String>,
    /// `source=value`, e.g. `uhc:member=...` (repeatable).
    #[arg(long = "add-identifier")]
    add_identifiers: Vec<String>,
    /// A source, or `source=value` (repeatable).
    #[arg(long = "remove-identifier")]
    remove_identifiers: Vec<String>,
    #[arg(long)]
    huishouden_health_person: Option<String>,
    #[arg(long)]
    baby_profile: Option<String>,
    /// Remove both Huishouden links.
    #[arg(long)]
    clear_links: bool,
}

#[derive(Args, Debug, Clone)]
struct GivenArgs {
    /// Whose document it is.
    #[arg(long)]
    person: Option<String>,
    /// Document type (`medrec intake --help` lists them).
    #[arg(long = "type", value_name = "TYPE")]
    doc_type: Option<String>,
    /// Date of service, YYYY-MM-DD.
    #[arg(long)]
    date: Option<String>,
    /// The organization it is from.
    #[arg(long)]
    org: Option<String>,
}

#[derive(Args, Debug)]
#[command(
    after_help = "Document types: visit_summary, lab_report, imaging_report, immunization_record, \
discharge_summary, referral, prescription, letter, bill, receipt, eob, insurance_card, other.\n\n\
Files from the inbox and ~/Downloads are moved (removed once filed and read back); \
other files and Drive folders are copied. --move moves named files too; --keep copies everything."
)]
struct IntakeArgs {
    /// Files to file.
    files: Vec<PathBuf>,
    /// File the files a list names, each with its own values: one JSON
    /// object per line (`{"file", "person", "type", "date", "org",
    /// "classified_by"}`), or `-` for standard input. Every line is checked
    /// first; the views are written once, at the end.
    #[arg(
        long,
        value_name = "FILE|-",
        conflicts_with_all = ["files", "inbox", "drive", "person", "doc_type", "date", "org"]
    )]
    from: Option<String>,
    /// Drain the inbox (config `inbox`, default `inbox/` in the records
    /// folder).
    #[arg(long)]
    inbox: bool,
    /// File everything in a Drive folder or directory (copied, not moved).
    #[arg(long, value_name = "gdrive:PROFILE/FOLDER-ID|DIR")]
    drive: Option<String>,
    #[command(flatten)]
    given: GivenArgs,
    /// Who classified the given values (`organize-scans`): medrec checks
    /// them against its own reading and holds a disagreement for review.
    #[arg(long)]
    classified_by: Option<String>,
    /// Move named files (remove each once filed and read back).
    #[arg(long = "move", conflicts_with = "keep")]
    move_files: bool,
    /// Copy everything, the inbox included.
    #[arg(long)]
    keep: bool,
    /// Show what would happen; change nothing.
    #[arg(long)]
    dry_run: bool,
}

#[derive(Subcommand, Debug)]
enum ReviewCmd {
    /// What waits for review.
    List {
        #[command(flatten)]
        list: ListArgs,
    },
    /// One item: what medrec read and why it is held.
    Show {
        id: String,
        /// All of the extracted text.
        #[arg(long)]
        text: bool,
    },
    /// File an item, with the values it was unsure of (or be asked).
    File {
        #[arg(required_unless_present = "from")]
        id: Option<String>,
        #[command(flatten)]
        given: GivenArgs,
        /// File the items a list names, each with its own values: one JSON
        /// object per line (`{"id", "person", "type", "date", "org"}`), or
        /// `-` for standard input. Every line is checked first; the views
        /// are written once, at the end.
        #[arg(
            long,
            value_name = "FILE|-",
            conflicts_with_all = ["id", "person", "doc_type", "date", "org"]
        )]
        from: Option<String>,
    },
    /// Discard an item and its file.
    Drop {
        id: String,
        /// Do not ask.
        #[arg(long)]
        force: bool,
    },
}

#[derive(Args, Debug)]
struct ImportArgs {
    /// The provider CLI (`mychart`, `kp`, `uhc`, ...).
    #[arg(long)]
    from: String,
    /// Which of the provider's logins to import through: one of
    /// `<cli> profiles list`. Required for a provider that signs in, unless
    /// `--from-files` is given; never taken from the environment.
    #[arg(long, value_name = "NAME", value_parser = profile_name)]
    from_profile: Option<ProfileName>,
    /// Import the provider's downloaded files in DIR instead of signing in:
    /// passed as its export's `--files`, for a provider that lists
    /// `records-files`.
    #[arg(long, value_name = "DIR", conflicts_with = "from_profile")]
    from_files: Option<PathBuf>,
    /// The patient (or member) id at that provider. Repeat it for several
    /// members of one login: each is exported in turn and only then filed,
    /// so a short website session serves them all.
    #[arg(long, required = true)]
    patient: Vec<String>,
    /// Whose records they are (remembered for the next import): one per
    /// `--patient`, in the same order, or none.
    #[arg(long)]
    person: Vec<String>,
    /// The provider's organization, for a CLI that serves several.
    #[arg(long)]
    org: Option<String>,
    /// Only what changed since, YYYY-MM-DD (passed to the provider).
    #[arg(long)]
    since: Option<String>,
    /// Import even when the source's birth date differs from the person's.
    #[arg(long)]
    allow_birth_date_mismatch: bool,
}

#[derive(Subcommand, Debug)]
enum ProvidersCmd {
    /// Each known or configured provider CLI and whether it can export.
    List,
}

#[derive(Args, Debug, Clone)]
struct RangeArgs {
    /// From this date, YYYY-MM-DD.
    #[arg(long, value_parser = iso_date)]
    since: Option<String>,
    /// Up to this date, YYYY-MM-DD.
    #[arg(long, value_parser = iso_date)]
    until: Option<String>,
}

/// A `YYYY-MM-DD` flag, checked as clap parses it: a bad date is a usage
/// error before anything is read.
fn iso_date(s: &str) -> Result<String, String> {
    pk_cli_core::dates::parse_iso(s)
        .map(|_| s.to_string())
        .map_err(|e| e.to_string())
}

impl RangeArgs {
    fn range(&self) -> Range {
        Range {
            since: self.since.clone(),
            until: self.until.clone(),
        }
    }
}

/// `--limit N` (DESIGN.md §1.3): every list command takes it. Unset, a
/// list is whole; `search` alone defaults to 50 hits.
#[derive(Args, Debug, Clone)]
struct ListArgs {
    /// At most this many items (1 or more; default: all).
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u64).range(1..))]
    limit: Option<u64>,
}

impl ListArgs {
    fn take<'a, T>(&self, rows: &'a [T]) -> &'a [T] {
        match self.limit {
            Some(n) => &rows[..rows.len().min(n as usize)],
            None => rows,
        }
    }
}

#[derive(Args, Debug)]
struct TimelineArgs {
    #[arg(long)]
    person: Option<String>,
    #[command(flatten)]
    range: RangeArgs,
    /// Only these kinds (document, study, note, encounter, result, report,
    /// condition, medication, allergy, immunization, procedure, coverage,
    /// claim); repeatable.
    #[arg(long = "kind", value_parser = clap::builder::PossibleValuesParser::new(views::TIMELINE_KINDS))]
    kinds: Vec<String>,
    #[command(flatten)]
    list: ListArgs,
}

#[derive(Args, Debug)]
struct SearchArgs {
    /// Words to find (all must match, each as a word prefix).
    #[arg(required = true)]
    query: Vec<String>,
    #[arg(long)]
    person: Option<String>,
    #[command(flatten)]
    range: RangeArgs,
    /// At most this many hits (1 or more).
    #[arg(long, value_name = "N", default_value_t = 50, value_parser = clap::value_parser!(u64).range(1..))]
    limit: u64,
}

#[derive(Args, Debug)]
struct RecordArgs {
    #[arg(long)]
    person: Option<String>,
    #[command(flatten)]
    range: RangeArgs,
    /// Only current ones. Conditions, allergies and medicines are current
    /// while any of their sources says so; medicines on hold and those whose
    /// end date has passed follow.
    #[arg(long)]
    active: bool,
    #[command(flatten)]
    list: ListArgs,
    /// One test or item over time: its code (LOINC, ICD-10, RxNorm, CVX)
    /// or words of its name.
    #[arg(long, value_name = "CODE|NAME")]
    code: Option<String>,
}

#[derive(Subcommand, Debug)]
enum EventsCmd {
    /// Events, newest first.
    List {
        #[arg(long)]
        person: Option<String>,
        #[command(flatten)]
        range: RangeArgs,
        #[command(flatten)]
        list: ListArgs,
    },
    /// One event: its money, members, papers and what is still expected.
    Show { id: String },
    /// "Not the same visit": two members never in one event. The one that
    /// is not the event's anchor leaves it.
    Apart {
        /// A record or document id.
        a: String,
        /// Another of the same person's.
        b: String,
    },
    /// "Attach to this visit": the second member joins the event holding
    /// the first.
    Together { a: String, b: String },
    /// The household's decisions, which outrank the rules.
    Decisions {
        #[arg(long)]
        person: Option<String>,
    },
    /// Drop a decision; the rules decide again.
    Undo { id: String },
}

#[derive(Subcommand, Debug)]
enum BillsCmd {
    /// Bills, newest first, with what is paid and left; then those worth
    /// a call and the shares not billed yet. `--limit` bounds the bills;
    /// the other two lists are given whole.
    List {
        #[arg(long)]
        person: Option<String>,
        #[command(flatten)]
        list: ListArgs,
    },
    /// Mark a bill paid, as on a receipt that was never filed.
    Paid {
        /// The bill's document id (d_…).
        document: String,
        /// The day it was paid (YYYY-MM-DD).
        #[arg(long)]
        on: String,
        /// What was paid, in dollars (`120.00`).
        #[arg(long)]
        amount: String,
    },
    /// The payments the household marked.
    Payments {
        #[arg(long)]
        person: Option<String>,
    },
    /// Drop a marked payment.
    Undo { id: String },
}

#[derive(Subcommand, Debug)]
enum PrivateCmd {
    /// A person's private records, documents, events and imaging studies, with
    /// why, and the open items with a private part. `--limit` is of each.
    List {
        #[arg(long)]
        person: Option<String>,
        #[command(flatten)]
        list: ListArgs,
    },
    /// Mark a record, document or event private, whatever the rules say.
    Mark { id: String },
    /// Mark a record, document or event not private, whatever the rules say: of
    /// a claim or a paper with a private part, all of it.
    Unmark { id: String },
    /// Drop the household's mark on an item: the rules decide again.
    Reset { id: String },
}

#[derive(Subcommand, Debug)]
enum LabelsCmd {
    /// What the household said, newest first.
    List {
        #[arg(long)]
        person: Option<String>,
        #[command(flatten)]
        list: ListArgs,
    },
    /// Say who a printed name is: in place of what was set before, with what
    /// is left out as medrec has it.
    Set {
        #[command(flatten)]
        of: LabelOf,
        /// What to call them.
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        practice: Option<String>,
        /// What they do, in plain words ("radiologist").
        #[arg(long)]
        role: Option<String>,
        #[arg(long)]
        phone: Option<String>,
    },
    /// Say that printed names are one provider.
    Same {
        #[command(flatten)]
        of: LabelsOf,
    },
    /// Say that printed names are not one provider.
    Different {
        #[command(flatten)]
        of: LabelsOf,
    },
    /// Say that the label a printed name has is wrong: it has none.
    Wrong {
        #[command(flatten)]
        of: LabelOf,
    },
    /// Take back what the household said.
    Undo {
        /// Its id (o_…), from `labels list`.
        id: String,
    },
}

#[derive(Args, Debug)]
struct LabelOf {
    #[arg(long)]
    person: String,
    /// The name, exactly as the items print it.
    #[arg(long, required = true)]
    printed: Vec<String>,
    /// Only where this record, document or event prints it.
    #[arg(long)]
    item: Option<String>,
}

#[derive(Args, Debug)]
struct LabelsOf {
    #[arg(long)]
    person: String,
    /// A name, exactly as the items print it; two or more.
    #[arg(long, required = true)]
    printed: Vec<String>,
}

#[derive(Subcommand, Debug)]
enum RecordsCmd {
    /// One record, also one that no list shows.
    Show { id: String },
    /// The ids of what is in a person's chart and about somebody else: a
    /// screening a parent answers about themselves, a result its source says
    /// is another's. Lists only count these.
    AboutAnother {
        #[arg(long)]
        person: Option<String>,
        #[command(flatten)]
        list: ListArgs,
    },
}

#[derive(Subcommand, Debug)]
enum DocumentsCmd {
    /// Filed documents, newest first.
    List {
        #[arg(long)]
        person: Option<String>,
        #[arg(long = "type", value_name = "TYPE")]
        doc_type: Option<String>,
        #[command(flatten)]
        range: RangeArgs,
        #[command(flatten)]
        list: ListArgs,
    },
    /// Open a document from a private local copy.
    Open {
        id: String,
        /// Make the copy and print its path; open nothing.
        #[arg(long)]
        no_launch: bool,
    },
    /// Fold a document filed twice into the one it renders again: the same
    /// source document and Binary, exactly the same text. Its file is
    /// removed (on Drive, to the trash) and its hash kept on the other's
    /// `renderings`.
    Merge {
        /// The copy to fold in.
        duplicate: String,
        /// The document to keep.
        #[arg(long, value_name = "ID")]
        into: String,
        /// Merge without asking.
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand, Debug)]
enum NoteCmd {
    /// Add a note (`-` reads the text from stdin).
    Add {
        person: String,
        text: String,
        /// YYYY-MM-DD (default today).
        #[arg(long)]
        date: Option<String>,
        /// A tag (repeatable, or comma-separated).
        #[arg(long = "tag")]
        tags: Vec<String>,
        /// A document, record or study the note is about (repeatable).
        #[arg(long = "about")]
        about: Vec<String>,
    },
    /// Notes, newest first.
    List {
        #[arg(long)]
        person: Option<String>,
        #[arg(long)]
        tag: Option<String>,
        #[command(flatten)]
        range: RangeArgs,
        #[command(flatten)]
        list: ListArgs,
    },
}

#[derive(Subcommand, Debug)]
enum ImagesCmd {
    /// File the studies on a disc, in a folder, a DICOMDIR, a .zip or an
    /// .iso; a study already filed gains only its new images.
    Intake {
        path: PathBuf,
        #[arg(long)]
        person: Option<String>,
        /// File even when the study's birth date differs from the person's.
        #[arg(long)]
        allow_birth_date_mismatch: bool,
        #[arg(long)]
        dry_run: bool,
    },
    /// Filed studies, newest first.
    List {
        #[arg(long)]
        person: Option<String>,
        #[command(flatten)]
        list: ListArgs,
    },
    /// One study: its series and report.
    Show { study: String },
    /// Open a study in the viewer from a private local copy.
    Open {
        study: String,
        /// Make the copy and print its path; open nothing.
        #[arg(long)]
        no_launch: bool,
    },
    /// Copy a study's files to a new or empty folder.
    Export {
        study: String,
        #[arg(short = 'o', long = "out", value_name = "DIR")]
        out: PathBuf,
    },
}

#[derive(Subcommand, Debug)]
enum ViewsCmd {
    /// Bring views/ up to date now: each person whose files changed since
    /// the views were written. Only views not yet stored under their name
    /// are written, and the views they replace are removed (on Drive, to
    /// the trash).
    Write(ViewsWriteArgs),
}

#[derive(Args, Debug)]
struct ViewsWriteArgs {
    /// Make every person's views again, changed or not.
    #[arg(long)]
    all: bool,
}

#[derive(Args, Debug)]
struct ReindexArgs {
    /// Only this person's records (name or id).
    #[arg(long)]
    person: Option<String>,
    /// Count what would change, in the records and in the views, and write
    /// nothing.
    #[arg(long)]
    dry_run: bool,
}

#[derive(Args, Debug)]
struct VerifyArgs {
    /// Download every document and compare its SHA-256 too.
    #[arg(long)]
    deep: bool,
}

#[derive(Args, Debug)]
struct BackupArgs {
    /// `gdrive:<drive profile>/<folder id>` or a local directory.
    #[arg(long)]
    to: String,
}

#[derive(Args, Debug)]
struct DriveArgs {
    /// The Drive profile, one per Google account (env MEDREC_DRIVE_PROFILE).
    /// Default: the one the `--profile` records folder names.
    #[arg(long = "drive-profile", global = true, value_name = "NAME")]
    drive_profile: Option<String>,
    #[command(subcommand)]
    command: DriveCommand,
}

#[derive(Subcommand, Debug)]
enum ConfigCmd {
    /// Print the config file path.
    Path,
    /// Print the settings.
    Show,
    /// Set a setting.
    Set { key: String, value: String },
    /// Remove a setting.
    Unset { key: String },
}

#[derive(Subcommand, Debug)]
enum AuthCmd {
    /// The records folder's Drive sign-in, as `medrec drive status` reports
    /// it; none is needed for a folder on a local disk.
    Status,
    /// Read the vault's key from the Keychain once, past every cache, and say
    /// what macOS did (auth-key/v1): the first call after an install. It
    /// needs no profile: one key seals every Drive sign-in.
    Key(KeyArgs),
}

#[derive(Subcommand, Debug)]
enum CacheCmd {
    /// Remove every copy medrec keeps on this machine (the records folder
    /// itself is untouched), the provider directory's rows included. The
    /// next command reads from Drive again. The run locks, the refresh state
    /// and the code tables stay.
    Clear,
    /// The background refresh a stale cached read starts.
    #[command(hide = true)]
    Sync {
        #[arg(long, value_name = "REL", conflicts_with = "revalidate_listing")]
        revalidate_file: Option<String>,
        #[arg(long, value_name = "REL")]
        revalidate_listing: Option<String>,
        #[arg(long, hide = true)]
        remote_spec: Option<String>,
    },
}

fn main() {
    let cli = Cli::parse();
    let json = cli.common.json;
    if let Err(e) = run(&cli) {
        std::process::exit(output::fail(&e, json));
    }
}

fn to_value<T: Serialize>(v: &T) -> Value {
    serde_json::to_value(v).expect("medrec's DTOs serialize to JSON")
}

fn items<T: Serialize>(rows: &[T]) -> Value {
    json!({ "items": to_value(&rows) })
}

/// A list, with what it says of those it does not show.
fn noted(mut list: Value, notes: &[String]) -> Value {
    if !notes.is_empty() {
        list["notes"] = to_value(&notes);
    }
    list
}

fn table(v: &Value, columns: &[&str]) {
    output::table(&output::table_view(&output::rows_of(v, "items"), columns));
}

fn note(cli: &Cli, msg: &str) {
    if !cli.common.quiet && !cli.common.json {
        eprintln!("{msg}");
    }
}

/// This machine's settings, with the profile the command names
/// (`--profile`, else `MEDREC_PROFILE`) selected: every command that reaches
/// a records folder loads through this.
fn load(store: &ConfigStore, cli: &Cli) -> Result<Config, CliError> {
    let mut cfg = Config::load(store)?;
    cfg.select(cli.profile.as_deref())?;
    Ok(cfg)
}

/// The Drive profile the `--profile` records folder is in; `None` for a
/// folder on a local disk.
fn folder_drive_profile(store: &ConfigStore, cli: &Cli) -> Result<Option<String>, CliError> {
    Ok(match load(store, cli)?.folder_location()? {
        Location::Drive(spec) => Some(spec.profile().to_string()),
        Location::Dir(_) => None,
    })
}

/// `--profile`, checked as clap parses it and kept as text for
/// [`Config::select`].
fn profile_name_text(s: &str) -> Result<String, String> {
    profile_name(s).map(|n| n.to_string())
}

/// A profile name, checked as clap parses it.
fn profile_name(s: &str) -> Result<ProfileName, String> {
    ProfileName::parse(s).map_err(|e| e.to_string())
}

/// The published tables. A code table needs no records folder, profile or
/// sign-in; the provider directory is read for the --profile folder.
fn tables_cmd(cli: &Cli, cmd: &TablesCmd, store: &ConfigStore) -> Result<(), CliError> {
    let json = cli.common.json;
    match cmd {
        TablesCmd::List => {
            output::emit(json, "table-list", items(&tables::list()), |v| {
                table(v, &["id", "version", "date", "state", "publisher"])
            });
            Ok(())
        }
        TablesCmd::Fetch { ids, file, force } => {
            let directory = ids.iter().any(|i| i == nppes::ID);
            let wanted: Vec<&tables::Table> = if ids.is_empty() {
                tables::TABLES.to_vec()
            } else {
                ids.iter()
                    .filter(|i| *i != nppes::ID)
                    .map(|i| tables::by_id(i))
                    .collect::<Result<_, _>>()?
            };
            if file.is_some() && (directory || wanted.len() != 1) {
                return Err(CliError::Usage(
                    "--file is one code table's zip: name that one table".into(),
                ));
            }
            if *force && !directory {
                return Err(CliError::Usage(
                    "--force is for nppes: name it, or leave --force out".into(),
                ));
            }
            let no_cache =
                || CliError::Other("cannot resolve a cache directory (is $HOME set?)".into());
            let dir = tables::dir().ok_or_else(no_cache)?;
            let kept_in = nppes::dir().ok_or_else(no_cache)?;
            // Refused before anything is fetched or a sign-in is read.
            let cfg = if directory {
                let cfg = load(store, cli)?;
                pk_cli_core::confirm::require_confirmable(
                    *force,
                    cli.common.interactive(),
                    "nppes is one download of over a gigabyte",
                )?;
                Some(cfg)
            } else {
                None
            };
            let mut rows = Vec::new();
            for t in wanted {
                note(cli, &format!("{} {}", t.id, t.version));
                rows.push(to_value(&tables::fetch(t, &dir, file.as_deref())?));
            }
            if let Some(cfg) = cfg {
                rows.push(to_value(&fetch_directory(cli, &cfg, &kept_in, *force)?));
            }
            output::emit(json, "table-fetch", items(&rows), |v| {
                table(v, &["id", "version", "status", "bytes"])
            });
            Ok(())
        }
    }
}

/// `tables fetch nppes`: the size is stated before the download, and the
/// download is the only thing sent: CMS is asked for a page and a file.
fn fetch_directory(
    cli: &Cli,
    cfg: &Config,
    dir: &std::path::Path,
    force: bool,
) -> Result<nppes::Fetched, CliError> {
    let f = Folder::open(cfg, Access::Read)?;
    let h = f.household()?;
    let indexes = h
        .people
        .iter()
        .map(|p| f.index(p))
        .collect::<Result<Vec<_>, _>>()?;
    let wanted = nppes::Wanted::of(&indexes);
    if wanted.npis.is_empty() && wanted.states.is_empty() {
        return Err(CliError::NotFound(
            "the records give no provider's NPI or state, so there is nothing to keep: import \
             again, or run `medrec reindex` once after an upgrade from before 0.9.6"
                .into(),
        ));
    }
    let offer = nppes::offer(&nppes::CMS)?;
    let path = nppes::path(dir, &h.folder_id);
    if let Ok(have) = nppes::Directory::load(&path) {
        if have.about.is_current(&offer, &wanted) {
            return Ok(have.about.fetched("unchanged"));
        }
    }
    let size = format!(
        "CMS's NPPES file for {} is {} bytes ({:.1} GB). It is read as it arrives and not kept whole.",
        offer.month,
        offer.bytes,
        offer.bytes as f64 / 1e9
    );
    if !cli.common.quiet {
        eprintln!("{size}");
    }
    pk_cli_core::confirm::confirm(force, "Download it?")?;
    let mut tenth = 0;
    nppes::fetch(&nppes::CMS, &offer, &wanted, &path, &mut |n| {
        let now = n * 10 / offer.bytes;
        if now > tenth {
            tenth = now;
            note(cli, &format!("{}%", now * 10));
        }
    })
}

fn run(cli: &Cli) -> Result<(), CliError> {
    let json = cli.common.json;
    if cli.common.quiet || json {
        medrec::viewfiles::quiet();
    }
    let store = ConfigStore::new(BIN);
    match &cli.command {
        Command::Profiles(cmd) => profiles_cmd(cli, cmd, &store),
        Command::Init(a) => {
            let mut cfg = load(&store, cli)?;
            let loc = Location::parse(&a.location)?;
            let budget = cfg.store_budget();
            let r = maint::init(&mut cfg, &loc, a.allow_other_files, budget)?;
            cfg.keep_active()?;
            store.save(&cfg)?;
            output::emit(json, "init", to_value(&r), |v| {
                println!(
                    "{} records folder {}",
                    v["status"].as_str().unwrap_or(""),
                    v["folder"].as_str().unwrap_or("")
                );
                if r.people == 0 {
                    println!("Next: medrec people add \"<name>\" --birth-date YYYY-MM-DD");
                }
            });
            Ok(())
        }
        Command::People(cmd) => people_cmd(cli, cmd, &store),
        Command::Intake(a) => intake_cmd(cli, a, &store),
        Command::Review(cmd) => review_cmd(cli, cmd, &store),
        Command::Import(a) => import_cmd(cli, a, &store),
        Command::Refresh(a) => refresh_cmd(cli, a, &store),
        Command::Providers(ProvidersCmd::List) => {
            let cfg = Config::load(&store)?;
            let rows = import::providers(&cfg);
            let v = items(&rows);
            output::emit(json, "provider-list", v, |v| {
                table(v, &["cli", "status", "version", "note"])
            });
            Ok(())
        }
        Command::Tables(cmd) => tables_cmd(cli, cmd, &store),
        Command::Timeline(a) => {
            let (f, h) = read(&store, cli)?;
            let (rows, notes) =
                views::timeline(&f, &h, a.person.as_deref(), &a.range.range(), &a.kinds)?;
            let out = noted(items(a.list.take(&rows)), &notes);
            output::emit(json, "timeline", out, |v| {
                table(
                    v,
                    &["date", "person_name", "kind", "title", "detail", "source"],
                );
                notes.iter().for_each(|n| note(cli, n));
            });
            Ok(())
        }
        Command::Search(a) => {
            let (f, h) = read(&store, cli)?;
            let q = a.query.join(" ");
            let (rows, notes) = views::search(
                &f,
                &h,
                a.person.as_deref(),
                &q,
                &a.range.range(),
                a.limit as usize,
            )?;
            output::emit(json, "search", noted(items(&rows), &notes), |v| {
                table(
                    v,
                    &["date", "person_name", "kind", "id", "title", "snippet"],
                );
                notes.iter().for_each(|n| note(cli, n));
            });
            Ok(())
        }
        Command::Results(a) => records_cmd(cli, &store, a, &["result", "report"], "result"),
        Command::Meds(a) => records_cmd(cli, &store, a, &["medication"], "medication"),
        Command::Conditions(a) => records_cmd(cli, &store, a, &["condition"], "condition"),
        Command::Allergies(a) => records_cmd(cli, &store, a, &["allergy"], "allergy"),
        Command::Immunizations(a) => records_cmd(cli, &store, a, &["immunization"], "immunization"),
        Command::Records(cmd) => one_record_cmd(cli, cmd, &store),
        Command::Documents(cmd) => documents_cmd(cli, cmd, &store),
        Command::Events(cmd) => events_cmd(cli, cmd, &store),
        Command::Bills(cmd) => bills_cmd(cli, cmd, &store),
        Command::Private(cmd) => private_cmd(cli, cmd, &store),
        Command::Labels(cmd) => labels_cmd(cli, cmd, &store),
        Command::Note(cmd) => note_cmd(cli, cmd, &store),
        Command::Images(cmd) => images_cmd(cli, cmd, &store),
        Command::Doctor => {
            let cfg = load(&store, cli)?;
            let checks = maint::doctor(&cfg);
            let failed = checks.iter().filter(|c| c.status == "fail").count();
            output::emit(
                json,
                "doctor",
                json!({ "ok": failed == 0, "checks": to_value(&checks) }),
                |v| {
                    output::table(&output::table_view(
                        &output::rows_of(v, "checks"),
                        &["check", "status", "detail"],
                    ))
                },
            );
            if failed > 0 {
                std::process::exit(1);
            }
            Ok(())
        }
        Command::Reindex(a) => {
            let cfg = load(&store, cli)?;
            // A dry run reads what a run would, and takes no lock.
            let access = if a.dry_run {
                Access::Fresh
            } else {
                Access::Write
            };
            let f = Folder::open(&cfg, access)?;
            let h = f.household()?;
            let r = medrec::reindex::claims(&f, &h, a.person.as_deref(), a.dry_run)?;
            output::emit(json, "reindex", to_value(&r), |_| {
                if r.dry_run {
                    println!("nothing was written; a run would do this:");
                }
                println!(
                    "{} claims: {} shaped again, {} unchanged, {} with no raw file; {} bills and receipts read again; {} records gained or changed what is said of their providers; {} records and {} papers what their sources say of them; {} records keep more of what their sources gave",
                    r.claims, r.reshaped, r.unchanged, r.without_raw, r.statements, r.providers_updated, r.statuses_updated, r.papers_updated, r.given_updated
                );
                if let Some(n) = papers_unread(r.papers_unread) {
                    println!("{n}");
                }
                if r.fresh == Some(false) {
                    println!("an index was written elsewhere meanwhile: run this again");
                }
                for (p, views) in r.people.iter().filter_map(|p| Some((p, p.views.as_ref()?))) {
                    let against = match (&views.against, views.stale) {
                        (None, _) => "there are no views yet".to_string(),
                        (Some(m), false) => format!("against medrec {m}'s"),
                        (Some(m), true) => format!("against medrec {m}'s, of older files"),
                    };
                    println!("{}: in the views ({against})", p.person);
                    for line in views.lines() {
                        println!("  {line}");
                    }
                }
            });
            Ok(())
        }
        Command::Views(ViewsCmd::Write(a)) => {
            let cfg = load(&store, cli)?;
            let f = Folder::open(&cfg, Access::Write)?;
            let r = medrec::viewfiles::sync(&f, a.all)?;
            output::emit(json, "views-write", to_value(&r), |_| {
                println!(
                    "views: {} people, {} made again; {} files written, {} unchanged, {} removed",
                    r.people, r.people_rebuilt, r.files_written, r.files_unchanged, r.files_removed
                );
            });
            Ok(())
        }
        Command::Verify(a) => {
            let cfg = load(&store, cli)?;
            let f = Folder::open(&cfg, Access::Read)?;
            let r = maint::verify(&f, a.deep)?;
            let clean = r.clean();
            output::emit(json, "verify", to_value(&r), |v| {
                println!(
                    "checked {} documents, {} study files, {} raw records, {} review items: {} ok",
                    r.documents, r.study_files, r.raw_records, r.review_items, r.ok
                );
                for (label, key) in [
                    ("missing", "missing"),
                    ("changed", "changed"),
                    ("not indexed", "unindexed"),
                ] {
                    for p in v[key].as_array().into_iter().flatten() {
                        println!("{label}: {}", p.as_str().unwrap_or(""));
                    }
                }
            });
            if !clean {
                std::process::exit(1);
            }
            Ok(())
        }
        Command::Backup(a) => {
            let cfg = load(&store, cli)?;
            let f = Folder::open(&cfg, Access::Read)?;
            let r = maint::backup(&f, &Location::parse(&a.to)?, cfg.store_budget())?;
            let clean = r.mismatched.is_empty();
            output::emit(json, "backup", to_value(&r), |_| {
                println!(
                    "copied {} files ({} bytes) to {}: {} verified",
                    r.files, r.bytes, r.to, r.verified
                );
                for m in &r.mismatched {
                    println!("mismatched: {m}");
                }
            });
            if !clean {
                std::process::exit(1);
            }
            Ok(())
        }
        Command::Config(cmd) => config_cmd(cli, cmd, &store),
        Command::Auth(AuthCmd::Key(args)) => {
            let code = store::shared_vault()?.check_key(args)?.emit(json);
            if code != 0 {
                std::process::exit(code);
            }
            Ok(())
        }
        Command::Auth(AuthCmd::Status)
            if cli.profile.is_none()
                && std::env::var(medrec::config::PROFILE_ENV).map_or(true, |v| v.is_empty()) =>
        {
            // No records folder named: none is needed until one is.
            AuthStatus::not_required().emit(json);
            if !json {
                eprintln!(
                    "name a records profile (`--profile <NAME>`) for its folder's Drive sign-in; \
                     `medrec drive profiles list` lists the Drive profiles"
                );
            }
            Ok(())
        }
        Command::Auth(AuthCmd::Status) => match folder_drive_profile(&store, cli)? {
            Some(profile) => store::drive_cli()?.run(
                &DriveCommand::Status(StatusArgs::default()),
                Some(&profile),
                json,
                cli.common.interactive(),
            ),
            None => {
                AuthStatus::not_required().emit(json);
                Ok(())
            }
        },
        Command::Drive(a) => {
            // --drive-profile, else MEDREC_DRIVE_PROFILE (read by the Drive
            // commands), else the records folder's.
            let env_set = std::env::var("MEDREC_DRIVE_PROFILE").is_ok_and(|v| !v.is_empty());
            let profile = match &a.drive_profile {
                Some(p) => Some(p.clone()),
                None if env_set || cli.profile.is_none() => None,
                None => folder_drive_profile(&store, cli)?,
            };
            store::drive_cli()?.run(
                &a.command,
                profile.as_deref(),
                json,
                cli.common.interactive(),
            )
        }
        Command::SelfUpdate(args) => Updater {
            repo: REPO.into(),
            binary: BIN.into(),
            target: env!("BUILD_TARGET").into(),
            current: env!("CARGO_PKG_VERSION").into(),
        }
        .run(args, json, cli.common.quiet),
        Command::Completions { shell } => {
            clap_complete::generate(*shell, &mut Cli::command(), BIN, &mut std::io::stdout());
            Ok(())
        }
        Command::Mcp(args) => {
            let tools = if args.list_tools {
                medrec::mcp::MedrecTools::listing_only()
            } else {
                // The profile and its folder are checked before serving, so a
                // client set up without them fails at start, naming them.
                let cfg = load(&store, cli)?;
                cfg.folder_location()?;
                medrec::mcp::MedrecTools::limit_staleness();
                medrec::mcp::MedrecTools::new(cfg)
            };
            pk_cli_mcp::run(args, tools)
        }
        Command::Info => {
            let info = CliInfo::new(
                BIN,
                env!("CARGO_PKG_VERSION"),
                &format!("https://github.com/{REPO}"),
                AuthInfo {
                    // Not required: a records folder on a local disk needs
                    // none, and `info` names no folder. One on Google Drive
                    // is reached through a Drive profile's sign-in;
                    // `auth status --profile <NAME>` reports it.
                    required: false,
                    method: "browser-session".into(),
                    login_hint: Some(format!(
                        "{} login {} <NAME>",
                        store::DRIVE_COMMAND,
                        store::DRIVE_PROFILE_FLAG
                    )),
                },
                // Its records-folder commands take --profile.
                &[pk_med_records::LOGIN_PROFILES],
            )
            .with_build(pk_cli_core::build_info!());
            output::json(&to_value(&info));
            Ok(())
        }
        Command::Cache(CacheCmd::Clear) => {
            let r = maint::clear_local_copies()?;
            output::emit(json, "cache-clear", to_value(&r), |v| {
                println!(
                    "removed {} files ({} bytes) from {}",
                    v["files"],
                    v["bytes"],
                    v["path"].as_str().unwrap_or("")
                )
            });
            Ok(())
        }
        Command::Cache(CacheCmd::Sync {
            revalidate_file,
            revalidate_listing,
            remote_spec,
        }) => {
            let cfg = match remote_spec {
                Some(_) => Config::load(&store)?,
                None => load(&store, cli)?,
            };
            let spec = match (remote_spec, cfg.folder_location()) {
                (Some(s), _) => s.clone(),
                (None, Ok(Location::Drive(s))) => s.to_string(),
                (None, Err(e)) => return Err(e),
                _ => {
                    return Err(CliError::Usage(
                        "the records folder is not on Google Drive, so it has no read cache".into(),
                    ))
                }
            };
            if let Some((listing, rel)) = revalidate_file
                .as_ref()
                .map(|r| (false, r))
                .or(revalidate_listing.as_ref().map(|r| (true, r)))
            {
                let outcome = store::revalidate(&spec, listing, rel, cfg.store_budget())?;
                output::emit(
                    json,
                    "cache-sync",
                    json!({ "rel": rel, "outcome": outcome }),
                    |_| {},
                );
                return Ok(());
            }
            Err(CliError::Usage(
                "nothing to do: pass --revalidate-file or --revalidate-listing".into(),
            ))
        }
    }
}

fn read(store: &ConfigStore, cli: &Cli) -> Result<(Folder, medrec::model::Household), CliError> {
    let cfg = load(store, cli)?;
    let f = Folder::open(&cfg, Access::Read)?;
    let h = f.household()?;
    Ok((f, h))
}

/// The person whose index holds a record or document.
fn owner_of<'a>(
    f: &Folder,
    h: &'a medrec::model::Household,
    id: &str,
) -> Result<&'a medrec::model::Person, CliError> {
    for p in &h.people {
        let idx = f.index(p)?;
        if idx.records.iter().any(|r| r.id == id) || idx.documents.iter().any(|d| d.id == id) {
            return Ok(p);
        }
    }
    Err(CliError::NotFound(format!(
        "no record or document {}",
        medrec::printable(id)
    )))
}

fn bills_cmd(cli: &Cli, cmd: &BillsCmd, store: &ConfigStore) -> Result<(), CliError> {
    let json = cli.common.json;
    match cmd {
        BillsCmd::List { person, list } => {
            let (f, h) = read(store, cli)?;
            let (mut items, mut calls, mut not_billed) = (Vec::new(), Vec::new(), Vec::new());
            let mut unreadable: Vec<String> = Vec::new();
            let mut notes: Vec<String> = Vec::new();
            let selected = medrec::people::selected(&h, person.as_deref())?;
            for p in &selected {
                let v = medrec::bills::of_person(&f, &h, p)?;
                if v["marks_unreadable"] == true {
                    unreadable.push(p.id.clone());
                    note(
                        cli,
                        &format!(
                            "{}'s marks.json does not read: the payments marked are left out",
                            medrec::printable(&p.name)
                        ),
                    );
                }
                notes.extend(medrec::bills::in_error(&v, &|_| true).map(
                    |n| match selected.len() {
                        1 => n,
                        _ => format!("{}: {n}", medrec::printable(&p.name)),
                    },
                ));
                for (id, b) in v["bills"].as_object().into_iter().flatten() {
                    let mut row = b.clone();
                    row["document"] = id.clone().into();
                    row["person"] = p.id.clone().into();
                    row["person_name"] = p.name.clone().into();
                    items.push(row);
                }
                for (key, into) in [
                    ("worth_a_call", &mut calls),
                    ("not_billed", &mut not_billed),
                ] {
                    for mut row in v[key].as_array().cloned().unwrap_or_default() {
                        row["person"] = p.id.clone().into();
                        row["person_name"] = p.name.clone().into();
                        into.push(row);
                    }
                }
            }
            let date = |v: &serde_json::Value, k: &str| v[k].as_str().unwrap_or("").to_string();
            items.sort_by(|a, b| {
                date(b, "date")
                    .cmp(&date(a, "date"))
                    .then(date(a, "document").cmp(&date(b, "document")))
            });
            not_billed.sort_by_key(|v| std::cmp::Reverse(date(v, "start")));
            let shown = list.take(&items);
            let doc = serde_json::json!({ "items": shown, "worth_a_call": calls, "not_billed": not_billed });
            let mut doc = noted(doc, &notes);
            if !unreadable.is_empty() {
                doc["marks_unreadable"] = unreadable.into();
            }
            output::emit(json, "bill-list", doc, |v| {
                table(
                    &serde_json::json!({ "items": v["items"] }),
                    &[
                        "date",
                        "person_name",
                        "organization",
                        "amount_due",
                        "paid_total",
                        "left",
                        "settled",
                        "document",
                    ],
                );
                if !calls.is_empty() {
                    println!();
                    table(
                        &serde_json::json!({ "items": v["worth_a_call"] }),
                        &["reason", "person_name", "asked", "owed", "bill", "claim"],
                    );
                }
                if !not_billed.is_empty() {
                    println!();
                    table(
                        &serde_json::json!({ "items": v["not_billed"] }),
                        &[
                            "start",
                            "person_name",
                            "provider",
                            "you_owe",
                            "event",
                            "fill",
                        ],
                    );
                }
                notes.iter().for_each(|n| note(cli, n));
            });
        }
        BillsCmd::Paid {
            document,
            on,
            amount,
        } => {
            if !medrec::ids::is_valid("d", document) {
                return Err(CliError::Usage(format!(
                    "`{}` is not a document id (d_…)",
                    medrec::printable(document)
                )));
            }
            let amount = medrec::model::Amount {
                amount: amount.trim_start_matches('$').to_string(),
                currency: "USD".into(),
            };
            let cfg = load(store, cli)?;
            let f = Folder::open(&cfg, Access::Write)?;
            let h = f.household()?;
            let p = owner_of(&f, &h, document)?;
            use medrec::bills::Marked;
            let (pm, new) =
                match medrec::bills::mark_paid(&f, p, document, on, &amount, None, None)? {
                    Marked::Recorded(pm) => (pm, true),
                    Marked::Already(pm) => (pm, false),
                    Marked::TakenBack => {
                        return Err(CliError::Other("the payment was taken back".into()))
                    }
                };
            let mut v = to_value(&pm);
            let bills = medrec::bills::of_person(&f, &h, p)?;
            if medrec::bills::not_applied(&bills).contains(pm.id.as_str()) {
                v["applied"] = false.into();
                note(
                    cli,
                    "The bill is entered in error: the payment is kept and not applied.",
                );
            }
            v["person"] = p.id.clone().into();
            v["status"] = if new { "recorded" } else { "unchanged" }.into();
            output::emit(json, "bill-payment", v, |v| {
                println!(
                    "{} {} paid on {} ({})",
                    v["status"].as_str().unwrap_or(""),
                    document,
                    on,
                    v["id"].as_str().unwrap_or("")
                )
            });
        }
        BillsCmd::Payments { person } => {
            let (f, h) = read(store, cli)?;
            let mut items = Vec::new();
            for p in medrec::people::selected(&h, person.as_deref())? {
                let bills = medrec::bills::of_person(&f, &h, p)?;
                let kept = medrec::bills::not_applied(&bills);
                for pm in f.marks(p)?.payments {
                    let mut v = to_value(&pm);
                    if kept.contains(pm.id.as_str()) {
                        v["applied"] = false.into();
                    }
                    v["person"] = p.id.clone().into();
                    v["person_name"] = p.name.clone().into();
                    items.push(v);
                }
            }
            output::emit(
                json,
                "bill-payments",
                serde_json::json!({ "items": items }),
                |v| {
                    // The amount as one column of the table only.
                    let mut v = v.clone();
                    for i in v["items"].as_array_mut().into_iter().flatten() {
                        i["amount"] = format!(
                            "{} {}",
                            i["amount"]["amount"].as_str().unwrap_or(""),
                            i["amount"]["currency"].as_str().unwrap_or("")
                        )
                        .into();
                    }
                    table(
                        &v,
                        &["id", "person_name", "document", "on", "amount", "at", "by"],
                    )
                },
            );
        }
        BillsCmd::Undo { id } => {
            if !medrec::ids::is_valid("o", id) {
                return Err(CliError::Usage(format!(
                    "`{}` is not a payment id (o_…)",
                    medrec::printable(id)
                )));
            }
            let cfg = load(store, cli)?;
            let f = Folder::open(&cfg, Access::Write)?;
            let h = f.household()?;
            let mut found = None;
            for p in &h.people {
                if f.marks(p)?.payments.iter().any(|pm| pm.id == *id) {
                    found = Some(p);
                    break;
                }
            }
            let p = found.ok_or_else(|| CliError::NotFound(format!("no payment {id}")))?;
            let pm = medrec::bills::undo(&f, p, id)?;
            output::emit(json, "bill-payment-undone", to_value(&pm), |_| {
                println!("undone {}", pm.id)
            });
        }
    }
    Ok(())
}

fn private_cmd(cli: &Cli, cmd: &PrivateCmd, store: &ConfigStore) -> Result<(), CliError> {
    let json = cli.common.json;
    let (id, private) = match cmd {
        PrivateCmd::List { person, list } => {
            let (f, h) = read(store, cli)?;
            let mut items = Vec::new();
            let mut parts = Vec::new();
            let mut unreadable: Vec<String> = Vec::new();
            let mut labels: Vec<String> = Vec::new();
            let mut in_error = 0;
            for p in medrec::people::selected(&h, person.as_deref())? {
                let idx = f.index(p)?;
                let gone: BTreeSet<&str> = medrec::standing::errors(&idx).collect();
                let (v, events) = medrec::private::with_events(&f, &h, p, &idx)?;
                if v.get("labels_unreadable").is_some() {
                    labels.push(p.id.clone());
                } else if v.get("marks_unreadable").is_some() {
                    unreadable.push(p.id.clone());
                }
                let event_of = |id: &str| {
                    events.events["events"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .find(|e| e["id"] == id)
                        .map(|e| {
                            (
                                e["start"].as_str().map(str::to_string),
                                e["title"].as_str().unwrap_or("").to_string(),
                            )
                        })
                };
                let named = |kind: &str, id: &str| {
                    match kind {
                        "record" => idx
                            .records
                            .iter()
                            .find(|r| r.id == id)
                            .map(|r| (r.date.clone(), r.display.clone())),
                        "document" => idx.documents.iter().find(|d| d.id == id).map(|d| {
                            (
                                d.date.clone(),
                                d.title
                                    .clone()
                                    .or(d.original_name.clone())
                                    .unwrap_or_default(),
                            )
                        }),
                        "study" => idx.studies.iter().find(|s| s.id == id).map(|s| {
                            (
                                s.date.clone(),
                                s.description
                                    .clone()
                                    .unwrap_or_else(|| "Imaging study".into()),
                            )
                        }),
                        _ => event_of(id),
                    }
                    .unwrap_or_default()
                };
                for (id, part) in v["parts"].as_object().into_iter().flatten() {
                    let kind = match id.get(..2) {
                        Some("r_") => "record",
                        Some("d_") => "document",
                        _ => "event",
                    };
                    let known = kind != "event" || event_of(id).is_some();
                    let Some(says) = part["says"].as_str().filter(|_| known) else {
                        continue;
                    };
                    if gone.contains(id.as_str()) {
                        continue;
                    }
                    let (date, title) = named(kind, id);
                    parts.push(serde_json::json!({
                        "id": id, "kind": kind, "not_shown": says, "date": date,
                        "title": title, "person": p.id, "person_name": p.name,
                    }));
                }
                for (kind, singular) in [
                    ("events", "event"),
                    ("records", "record"),
                    ("documents", "document"),
                    ("studies", "study"),
                ] {
                    for (id, why) in v[kind].as_object().into_iter().flatten() {
                        if gone.contains(id.as_str()) {
                            in_error += 1;
                            continue;
                        }
                        if kind == "events" && event_of(id).is_none() {
                            continue;
                        }
                        let (date, title) = named(singular, id);
                        let mut row = serde_json::json!({
                            "id": id, "kind": singular, "area": why["area"],
                            "from": why["from"], "date": date, "title": title,
                            "person": p.id, "person_name": p.name,
                        });
                        if why["given"] == true {
                            row["given"] = true.into();
                        }
                        items.push(row);
                    }
                }
            }
            let day = |v: &serde_json::Value| {
                v["date"]
                    .as_str()
                    .unwrap_or("")
                    .get(..10)
                    .map(str::to_string)
            };
            for rows in [&mut items, &mut parts] {
                rows.sort_by(|a: &serde_json::Value, b| {
                    day(b)
                        .cmp(&day(a))
                        .then(a["id"].as_str().cmp(&b["id"].as_str()))
                });
            }
            let shown = list.take(&items);
            let mut doc = serde_json::json!({ "items": shown, "parts": list.take(&parts) });
            if in_error > 0 {
                let n = medrec::standing::in_error(in_error);
                note(cli, &n);
                doc = noted(doc, &[n]);
            }
            if !unreadable.is_empty() {
                for p in h.people.iter().filter(|p| unreadable.contains(&p.id)) {
                    note(
                        cli,
                        &format!(
                            "{}'s marks.json does not read: the household's marks are left out",
                            medrec::printable(&p.name)
                        ),
                    );
                }
                doc["marks_unreadable"] = unreadable.into();
            }
            if !labels.is_empty() {
                for p in h.people.iter().filter(|p| labels.contains(&p.id)) {
                    note(
                        cli,
                        &format!(
                            "{}'s provider-state.json does not read: what provider labels make private is left out until `medrec tables fetch nppes`, then `medrec views write`, on one machine, or until this medrec is updated when a newer one wrote it",
                            medrec::printable(&p.name)
                        ),
                    );
                }
                doc["labels_unreadable"] = labels.into();
            }
            output::emit(json, "private-list", doc, |v| {
                let mut v = v.clone();
                for i in v["items"].as_array_mut().into_iter().flatten() {
                    if i["given"] == true {
                        i["given"] = "yes".into();
                    }
                }
                table(
                    &v,
                    &[
                        "date",
                        "person_name",
                        "kind",
                        "area",
                        "from",
                        "given",
                        "title",
                        "id",
                    ],
                );
                if v.get("parts").is_some() {
                    println!();
                    table(
                        &serde_json::json!({"items": v["parts"]}),
                        &["date", "person_name", "kind", "not_shown", "title", "id"],
                    );
                }
            });
            return Ok(());
        }
        PrivateCmd::Mark { id } => (id, Some(true)),
        PrivateCmd::Unmark { id } => (id, Some(false)),
        PrivateCmd::Reset { id } => (id, None),
    };
    if !["r", "d", "e"].iter().any(|k| medrec::ids::is_valid(k, id)) {
        return Err(CliError::Usage(format!(
            "`{}` is not a record, document or event id (r_…, d_…, e_…)",
            medrec::printable(id)
        )));
    }
    let cfg = load(store, cli)?;
    let f = Folder::open(&cfg, Access::Write)?;
    let h = f.household()?;
    let p = medrec::private::holder(&f, &h, id)?
        .ok_or_else(|| CliError::NotFound(format!("no record, document or event {id}")))?;
    let (m, changed) = medrec::private::mark(&f, &h, p, id, private, None, None)?;
    let v = serde_json::json!({
        "item": id, "person": p.id, "private": private,
        "mark": m.map(|m| to_value(&m)),
        "status": if changed { "recorded" } else { "unchanged" },
    });
    output::emit(json, "private-mark", v, |v| {
        let what = match private {
            Some(true) => "private",
            Some(false) => "not private",
            None => "back to the rules",
        };
        println!("{} {id} {what}", v["status"].as_str().unwrap_or(""))
    });
    Ok(())
}

fn labels_cmd(cli: &Cli, cmd: &LabelsCmd, store: &ConfigStore) -> Result<(), CliError> {
    use medrec::corrections::{Ask, Set, What};
    let json = cli.common.json;
    let say = |printed: &[String], what: What, item: &Option<String>| Ask::Say {
        printed: printed.to_vec(),
        what,
        item: item.clone(),
    };
    let (person, ask) = match cmd {
        LabelsCmd::List { person, list } => {
            let (f, h) = read(store, cli)?;
            let mut items = Vec::new();
            for p in medrec::people::selected(&h, person.as_deref())? {
                let idx = f.index(p)?;
                let gone: BTreeSet<&str> = medrec::standing::errors(&idx).collect();
                for c in f.marks(p)?.providers {
                    let Some(kind) = c.kind() else { continue };
                    let mut v = to_value(&c);
                    if c.item.as_deref().is_some_and(|i| gone.contains(i)) {
                        v["applied"] = false.into();
                    }
                    v["kind"] = to_value(&kind);
                    v["person"] = p.id.clone().into();
                    v["person_name"] = p.name.clone().into();
                    items.push(v);
                }
            }
            items.sort_by(|a, b| b["at"].as_str().cmp(&a["at"].as_str()));
            let shown = list.take(&items);
            output::emit(
                json,
                "label-list",
                serde_json::json!({ "items": shown }),
                |v| {
                    let mut v = v.clone();
                    for i in v["items"].as_array_mut().into_iter().flatten() {
                        let printed: Vec<&str> = i["printed"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|p| p.as_str())
                            .collect();
                        i["printed"] = printed.join(" / ").into();
                        for part in ["name", "practice", "role", "phone"] {
                            i[part] = i["set"][part].clone();
                        }
                    }
                    table(
                        &v,
                        &[
                            "id",
                            "person_name",
                            "kind",
                            "printed",
                            "name",
                            "practice",
                            "role",
                            "phone",
                            "item",
                            "at",
                        ],
                    )
                },
            );
            return Ok(());
        }
        LabelsCmd::Set {
            of,
            name,
            practice,
            role,
            phone,
        } => {
            let set = What::Set(Set {
                name: name.clone(),
                practice: practice.clone(),
                role: role.clone(),
                phone: phone.clone(),
                ..Set::default()
            });
            (Some(&of.person), say(&of.printed, set, &of.item))
        }
        LabelsCmd::Same { of } => (Some(&of.person), say(&of.printed, What::Same(true), &None)),
        LabelsCmd::Different { of } => {
            (Some(&of.person), say(&of.printed, What::Same(false), &None))
        }
        LabelsCmd::Wrong { of } => (Some(&of.person), say(&of.printed, What::Wrong, &of.item)),
        LabelsCmd::Undo { id } => (None, Ask::Undo(id.clone())),
    };
    let ask = ask.checked().map_err(|why| CliError::Usage(why.into()))?;
    let cfg = load(store, cli)?;
    let f = Folder::open(&cfg, Access::Write)?;
    let h = f.household()?;
    let p = match (person, &ask) {
        (Some(q), _) => medrec::people::resolve(&h, q)?,
        (None, Ask::Undo(id)) => {
            let mut found = None;
            for p in &h.people {
                if f.marks(p)?.providers.iter().any(|c| c.id == *id) {
                    found = Some(p);
                }
            }
            found
                .ok_or_else(|| CliError::NotFound(format!("the household said nothing as {id}")))?
        }
        (None, _) => unreachable!("only an undo names no person"),
    };
    use medrec::corrections::Done;
    let (status, said) = match medrec::corrections::decide(&f, &h, p, &ask, None)? {
        Done::Recorded(c) => ("recorded", Some(c)),
        Done::Already(c) => ("unchanged", Some(c)),
        Done::Undone => ("recorded", None),
        Done::NothingToUndo | Done::SaidLater => ("unchanged", None),
    };
    let v = serde_json::json!({
        "person": p.id, "kind": ask.word(), "said": said.map(|c| to_value(&c)),
        "status": status,
    });
    output::emit(json, "label-said", v, |v| {
        println!(
            "{} {} {}",
            v["status"].as_str().unwrap_or(""),
            v["kind"].as_str().unwrap_or(""),
            v["said"]["id"].as_str().unwrap_or("")
        )
    });
    Ok(())
}

fn events_cmd(cli: &Cli, cmd: &EventsCmd, store: &ConfigStore) -> Result<(), CliError> {
    let json = cli.common.json;
    match cmd {
        EventsCmd::List {
            person,
            range,
            list,
        } => {
            let (f, h) = read(store, cli)?;
            let rows = medrec::events::list(&f, &h, person.as_deref(), &range.range())?;
            let (lost, rows): (Vec<_>, Vec<_>) = rows
                .into_iter()
                .partition(|e| e["entered_in_error"] == true);
            let notes: Vec<String> = (!lost.is_empty())
                .then(|| medrec::standing::in_error(lost.len()))
                .into_iter()
                .collect();
            let out = noted(serde_json::json!({ "items": list.take(&rows) }), &notes);
            output::emit(json, "event-list", out, |v| {
                // The share as a column of the table only; the JSON
                // keeps it in `money`.
                let mut v = v.clone();
                for e in v["items"].as_array_mut().into_iter().flatten() {
                    e["you_owe"] = e["money"]["you_owe"].clone();
                }
                table(
                    &v,
                    &[
                        "start",
                        "end",
                        "person_name",
                        "title",
                        "place",
                        "you_owe",
                        "confidence",
                        "status",
                        "id",
                    ],
                );
                notes.iter().for_each(|n| note(cli, n));
            });
        }
        EventsCmd::Show { id } => {
            if !medrec::ids::is_valid("e", id) {
                return Err(CliError::Usage(format!(
                    "`{}` is not an event id (e_…)",
                    medrec::printable(id)
                )));
            }
            let (f, h) = read(store, cli)?;
            let rows = medrec::events::list(&f, &h, None, &Range::default())?;
            let e = rows
                .into_iter()
                .find(|e| e["id"] == id.as_str())
                .ok_or_else(|| CliError::NotFound(format!("no event {id}")))?;
            output::emit_one(json, "event", e);
        }
        EventsCmd::Apart { a, b } | EventsCmd::Together { a, b } => {
            let kind = if matches!(cmd, EventsCmd::Apart { .. }) {
                "apart"
            } else {
                "together"
            };
            if a == b {
                return Err(CliError::Usage("name two different members".into()));
            }
            let cfg = load(store, cli)?;
            let f = Folder::open(&cfg, Access::Write)?;
            let h = f.household()?;
            let p = owner_of(&f, &h, a)?;
            let (d, new) = medrec::events::decide(&f, &h, p, kind, a, b, None, None)?;
            let mut v = to_value(&d);
            let idx = f.index(p)?;
            if medrec::standing::errors(&idx).any(|id| id == a || id == b) {
                v["applied"] = false.into();
                note(
                    cli,
                    "One of them is entered in error: the decision is kept and not applied.",
                );
            }
            v["person"] = p.id.clone().into();
            v["status"] = if new { "recorded" } else { "unchanged" }.into();
            output::emit(json, "event-decision", v, |v| {
                println!(
                    "{} {kind}: {} and {} ({})",
                    v["status"].as_str().unwrap_or(""),
                    a,
                    b,
                    v["id"].as_str().unwrap_or("")
                )
            });
        }
        EventsCmd::Decisions { person } => {
            let (f, h) = read(store, cli)?;
            let mut items = Vec::new();
            for p in medrec::people::selected(&h, person.as_deref())? {
                let idx = f.index(p)?;
                let gone: BTreeSet<&str> = medrec::standing::errors(&idx).collect();
                for d in f.overrides(p)?.events {
                    let mut v = to_value(&d);
                    if d.members.iter().any(|m| gone.contains(m.as_str())) {
                        v["applied"] = false.into();
                    }
                    v["person"] = p.id.clone().into();
                    v["person_name"] = p.name.clone().into();
                    v["a"] = d.members[0].clone().into();
                    v["b"] = d.members[1].clone().into();
                    items.push(v);
                }
            }
            output::emit(
                json,
                "event-decisions",
                serde_json::json!({ "items": items }),
                |v| table(v, &["id", "person_name", "kind", "a", "b", "at", "by"]),
            );
        }
        EventsCmd::Undo { id } => {
            if !medrec::ids::is_valid("o", id) {
                return Err(CliError::Usage(format!(
                    "`{}` is not a decision id (o_…)",
                    medrec::printable(id)
                )));
            }
            let cfg = load(store, cli)?;
            let f = Folder::open(&cfg, Access::Write)?;
            let h = f.household()?;
            let mut found = None;
            for p in &h.people {
                if f.overrides(p)?.events.iter().any(|d| d.id == *id) {
                    found = Some(p);
                    break;
                }
            }
            let p = found.ok_or_else(|| CliError::NotFound(format!("no decision {id}")))?;
            let d = medrec::events::undo(&f, p, id)?;
            output::emit(json, "event-decision-undone", to_value(&d), |_| {
                println!("undone {}", d.id)
            });
        }
    }
    Ok(())
}

fn people_cmd(cli: &Cli, cmd: &PeopleCmd, store: &ConfigStore) -> Result<(), CliError> {
    let json = cli.common.json;
    match cmd {
        PeopleCmd::Add(a) => {
            let cfg = load(store, cli)?;
            let f = Folder::open(&cfg, Access::Write)?;
            let p = people::add(
                &f,
                people::NewPerson {
                    name: a.name.clone(),
                    birth_date: a.birth_date.clone(),
                    adult: if a.adult {
                        Some(true)
                    } else if a.child {
                        Some(false)
                    } else {
                        None
                    },
                    guardians: a.guardians.clone(),
                    aliases: a.aliases.clone(),
                    huishouden_health_person: a.huishouden_health_person.clone(),
                    baby_profile: a.baby_profile.clone(),
                },
            )?;
            output::emit(json, "person", to_value(&p), |_| {
                println!("added {} ({}), folder `{}`", p.name, p.id, p.folder)
            });
            Ok(())
        }
        PeopleCmd::List { list } => {
            let (_, h) = read(store, cli)?;
            output::emit(json, "person-list", items(list.take(&h.people)), |v| {
                table(v, &["id", "name", "birth_date", "adult", "folder"])
            });
            Ok(())
        }
        PeopleCmd::Show { person } => {
            let cfg = load(store, cli)?;
            let f = Folder::open(&cfg, Access::Read)?;
            let d = people::show(&f, person)?;
            output::emit_one(json, "person", to_value(&d));
            Ok(())
        }
        PeopleCmd::Edit(a) => {
            let cfg = load(store, cli)?;
            let f = Folder::open(&cfg, Access::Write)?;
            let p = people::edit(
                &f,
                &a.person,
                people::Edit {
                    name: a.name.clone(),
                    birth_date: a.birth_date.clone(),
                    adult: if a.adult {
                        Some(true)
                    } else if a.child {
                        Some(false)
                    } else {
                        None
                    },
                    add_guardians: a.add_guardians.clone(),
                    remove_guardians: a.remove_guardians.clone(),
                    add_aliases: a.add_aliases.clone(),
                    remove_aliases: a.remove_aliases.clone(),
                    add_identifiers: a.add_identifiers.clone(),
                    remove_identifiers: a.remove_identifiers.clone(),
                    huishouden_health_person: a.huishouden_health_person.clone(),
                    baby_profile: a.baby_profile.clone(),
                    clear_links: a.clear_links,
                },
            )?;
            output::emit_one(json, "person", to_value(&p));
            Ok(())
        }
    }
}

fn given(g: &GivenArgs, classified_by: Option<String>) -> Given {
    Given {
        person: g.person.clone(),
        doc_type: g.doc_type.clone(),
        date: g.date.clone(),
        organization: g.org.clone(),
        classified_by,
    }
}

fn intake_cmd(cli: &Cli, a: &IntakeArgs, store: &ConfigStore) -> Result<(), CliError> {
    let json = cli.common.json;
    if let Some(from) = &a.from {
        return intake_list(cli, a, from, store);
    }
    if a.files.is_empty() && !a.inbox && a.drive.is_none() {
        return Err(CliError::Usage(
            "name files, or pass --inbox, --drive <folder> or --from <list>".into(),
        ));
    }
    // Arguments first: a bad --type or --date never costs a folder read.
    let given = given(&a.given, a.classified_by.clone());
    intake::validate_given(&given)?;
    let cfg = load(store, cli)?;
    let inbox = if a.inbox {
        Some(cfg.inbox_location()?)
    } else {
        None
    };
    let f = Folder::open(
        &cfg,
        if a.dry_run {
            Access::Read
        } else {
            Access::Write
        },
    )?;
    let req = intake::Request {
        files: a.files.clone(),
        inbox,
        drive: a.drive.as_deref().map(Location::parse).transpose()?,
        given,
        keep: a.keep,
        move_files: a.move_files,
        dry_run: a.dry_run,
        interactive: cli.common.interactive(),
    };
    let (report, ok) = intake::run(&f, &cfg, &req)?;
    print_intake(json, &report);
    // `exit` runs no destructors: the folder closes first, so what was
    // filed before a failure reaches the views.
    drop(f);
    if !ok {
        std::process::exit(1);
    }
    Ok(())
}

/// `intake --from`: every line checked, then each filed in order in one
/// folder session, so the views are written once.
fn intake_list(cli: &Cli, a: &IntakeArgs, from: &str, store: &ConfigStore) -> Result<(), CliError> {
    let lines: Vec<(usize, medrec::batch::IntakeLine)> = medrec::batch::read(from)?;
    let classified_by = a.classified_by.as_deref();
    let given: Vec<(usize, intake::Given)> = lines
        .iter()
        .map(|(n, l)| (*n, l.given(classified_by)))
        .collect();
    medrec::batch::validate(&given)?;
    let cfg = load(store, cli)?;
    let f = Folder::open(
        &cfg,
        if a.dry_run {
            Access::Read
        } else {
            Access::Write
        },
    )?;
    let mut report = intake::Report {
        dry_run: a.dry_run,
        items: Vec::new(),
        counts: BTreeMap::new(),
    };
    let mut all_ok = true;
    let total = lines.len();
    for (k, ((_, line), (n, given))) in lines.iter().zip(given).enumerate() {
        note(
            cli,
            &format!("filing {} of {total} (list line {n}) ...", k + 1),
        );
        let req = intake::Request {
            files: vec![line.file.clone()],
            inbox: None,
            drive: None,
            given,
            keep: a.keep,
            move_files: a.move_files,
            dry_run: a.dry_run,
            interactive: false,
        };
        let (items, ok) = match intake::run(&f, &cfg, &req) {
            Ok((r, ok)) => (r.items, ok),
            Err(e) => (
                vec![intake::Outcome::failed(
                    &line.file.display().to_string(),
                    &e,
                )],
                false,
            ),
        };
        all_ok &= ok;
        for mut o in items {
            o.line = Some(n);
            *report.counts.entry(o.status.clone()).or_default() += 1;
            report.items.push(o);
        }
    }
    print_intake(cli.common.json, &report);
    // The folder closes, writing the views once, before the exit.
    drop(f);
    if !all_ok {
        std::process::exit(1);
    }
    Ok(())
}

fn print_intake(json: bool, report: &intake::Report) {
    let v = to_value(report);
    output::emit(json, "intake", v, |v| {
        output::table(&output::table_view(
            &output::rows_of(v, "items"),
            &[
                "status",
                "source",
                "person_name",
                "doc_type",
                "date",
                "path",
                "reasons",
                "error",
            ],
        ));
        let counts: Vec<String> = report
            .counts
            .iter()
            .map(|(k, n)| format!("{n} {k}"))
            .collect();
        if !counts.is_empty() {
            println!("{}", counts.join(", "));
        }
        for i in report
            .items
            .iter()
            .filter(|i| i.same_statement_as.is_some())
        {
            println!(
                "{} is the same statement as {}, filed before",
                medrec::printable(&i.source),
                i.same_statement_as.as_deref().unwrap_or_default()
            );
        }
    });
}

fn review_cmd(cli: &Cli, cmd: &ReviewCmd, store: &ConfigStore) -> Result<(), CliError> {
    let json = cli.common.json;
    let cfg = load(store, cli)?;
    match cmd {
        ReviewCmd::List { list } => {
            let f = Folder::open(&cfg, Access::Read)?;
            let rows = review::list(&f)?;
            output::emit(json, "review-list", items(list.take(&rows)), |v| {
                table(v, &["id", "received_at", "original_name", "reasons"])
            });
        }
        ReviewCmd::Show { id, text } => {
            let f = Folder::open(&cfg, Access::Read)?;
            let d = review::show(&f, id, *text)?;
            output::emit_one(json, "review-item", to_value(&d));
        }
        ReviewCmd::File {
            from: Some(from), ..
        } => review_list(cli, &cfg, from)?,
        ReviewCmd::File { id, given: g, .. } => {
            let id = id.clone().unwrap_or_default();
            let given = given(g, None);
            intake::validate_given(&given)?;
            let f = Folder::open(&cfg, Access::Write)?;
            let r = review::file(
                &f,
                &review::FileRequest {
                    id,
                    given,
                    interactive: cli.common.interactive(),
                },
            )?;
            output::emit(json, "review-filed", to_value(&r), |_| {
                println!(
                    "{} {} as {} for {}: {}",
                    r.status, r.review, r.doc_type, r.person_name, r.path
                )
            });
        }
        ReviewCmd::Drop { id, force } => {
            confirm::gate(
                &cli.common,
                *force,
                &format!("Discard review item {id} and its file?"),
            )?;
            let f = Folder::open(&cfg, Access::Write)?;
            let r = review::drop(&f, id)?;
            output::emit(json, "review-dropped", to_value(&r), |_| {
                println!("dropped {}", r.review)
            });
        }
    }
    Ok(())
}

/// `review file --from`: every line checked, then each item filed in order
/// in one folder session, so the views are written once. A line that fails
/// is reported and the rest are still filed.
fn review_list(cli: &Cli, cfg: &Config, from: &str) -> Result<(), CliError> {
    let lines: Vec<(usize, medrec::batch::ReviewLine)> = medrec::batch::read(from)?;
    let given: Vec<(usize, intake::Given)> = lines.iter().map(|(n, l)| (*n, l.given())).collect();
    medrec::batch::validate(&given)?;
    let f = Folder::open(cfg, Access::Write)?;
    let mut items = Vec::new();
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let total = lines.len();
    for (k, ((n, line), (_, given))) in lines.iter().zip(given).enumerate() {
        note(
            cli,
            &format!("filing {} of {total} (list line {n}) ...", k + 1),
        );
        let row = match review::file(
            &f,
            &review::FileRequest {
                id: line.id.clone(),
                given,
                interactive: false,
            },
        ) {
            Ok(r) => {
                let mut v = to_value(&r);
                v["line"] = (*n).into();
                v
            }
            Err(e) => serde_json::json!({
                "line": n,
                "review": line.id,
                "status": "failed",
                "error": e.message(),
                "error_code": e.code(),
            }),
        };
        let status = row["status"].as_str().unwrap_or("failed").to_string();
        *counts.entry(status).or_default() += 1;
        items.push(row);
    }
    let failed = counts.get("failed").copied().unwrap_or(0);
    output::emit(
        cli.common.json,
        "review-filed-list",
        serde_json::json!({ "items": items, "counts": counts }),
        |v| {
            output::table(&output::table_view(
                &output::rows_of(v, "items"),
                &[
                    "line",
                    "status",
                    "review",
                    "doc_type",
                    "person_name",
                    "path",
                    "error",
                ],
            ));
            let c: Vec<String> = counts.iter().map(|(k, n)| format!("{n} {k}")).collect();
            println!("{}", c.join(", "));
        },
    );
    // The folder closes, writing the views once, before the exit.
    drop(f);
    if failed > 0 {
        std::process::exit(1);
    }
    Ok(())
}

fn refresh_cmd(cli: &Cli, a: &RefreshArgs, store: &ConfigStore) -> Result<(), CliError> {
    use medrec::refresh::{self, Mode, Pick};
    let json = cli.common.json;
    let Some(cmd) = &a.cmd else {
        if a.install {
            let mut cfg = load(store, cli)?;
            let r = refresh::install(&mut cfg, a.pass_every, a.take_over)?;
            cfg.keep_active()?;
            store.save(&cfg)?;
            output::emit(json, "refresh-install", to_value(&r), |v| {
                println!(
                    "installed {} (a pass every {} s), log {}",
                    v["agent"].as_str().unwrap_or(""),
                    v["pass_every_secs"],
                    v["log"].as_str().unwrap_or("")
                );
                if r.took_over {
                    println!(
                        "took refresh over from the other machine; requests are served from one pass from now"
                    );
                }
            });
            return Ok(());
        }
        if a.uninstall {
            let cfg = load(store, cli)?;
            let r = refresh::uninstall(&cfg)?;
            output::emit(json, "refresh-uninstall", to_value(&r), |v| {
                println!(
                    "{}",
                    if v["removed"] == true {
                        "removed the LaunchAgent"
                    } else {
                        "no LaunchAgent was installed for this profile"
                    }
                )
            });
            return Ok(());
        }
        let cfg = load(store, cli)?;
        let pick = match (&a.source, &a.person) {
            (Some(s), _) => Some(Pick::Source(s.clone())),
            (_, Some(p)) => Some(Pick::Person(p.clone())),
            _ => None,
        };
        let mode = if a.scheduled {
            Mode::Scheduled
        } else {
            Mode::Manual
        };
        let r = refresh::pass(&cfg, mode, pick.as_ref()).inspect_err(|_| {
            // The LaunchAgent's log has the time of every line but an error's.
            if mode == Mode::Scheduled && !json {
                println!("{} failed", medrec::model::now_rfc3339());
            }
        })?;
        let failed = r.attempts.iter().any(|x| x.outcome == "failed");
        let needs = r.attempts.iter().any(|x| x.outcome == "needs_person");
        if mode == Mode::Scheduled && r.status == "idle" && !json {
            return Ok(());
        }
        let stamp = medrec::model::now_rfc3339();
        output::emit(json, "refresh-pass", to_value(&r), |v| {
            if mode == Mode::Scheduled {
                print!("{stamp} ");
            }
            println!(
                "{}: {} attempts, {} requests",
                v["status"].as_str().unwrap_or(""),
                r.attempts.len(),
                r.requests.len()
            );
            for x in &r.attempts {
                let mut line = format!("  {} {} {} {}", x.source, x.cli, x.trigger, x.outcome);
                if let Some(e) = &x.error {
                    line.push_str(&format!(" ({e})"));
                }
                if let (Some(a), Some(u)) = (x.records_added, x.records_updated) {
                    line.push_str(&format!(" records +{a} ~{u}"));
                }
                if let Some(d) = x.documents_added {
                    line.push_str(&format!(" documents +{d}"));
                }
                if let Some(c) = &x.command {
                    line.push_str(&format!("; run `{c}`"));
                }
                println!("{line}");
            }
            for q in &r.requests {
                let why = q.cause.as_deref().map(|c| format!(" ({c})"));
                println!(
                    "  request {} {}{}",
                    q.request,
                    q.outcome,
                    why.unwrap_or_default()
                );
            }
        });
        if mode == Mode::Manual && failed {
            std::process::exit(5);
        }
        if mode == Mode::Manual && needs {
            std::process::exit(3);
        }
        return Ok(());
    };
    match cmd {
        RefreshCmd::Add {
            source,
            provider_profile,
            patient,
            org,
            manual,
            every,
        } => {
            // A binding is made only by a command that names both profiles.
            if cli.profile.is_none() {
                return Err(CliError::Usage(
                    "`refresh add` binds a source to this profile: name it with --profile on the command line"
                        .into(),
                ));
            }
            let mut cfg = load(store, cli)?;
            let h = Folder::open(&cfg, Access::Fresh)?.household()?;
            let wanted = medrec::config::Binding {
                cli: String::new(),
                provider_profile: provider_profile.clone(),
                patient: patient.clone(),
                org: org.clone(),
                auto: !manual,
                every_secs: *every,
                paused: None,
            };
            let r = refresh::add(&mut cfg, &h, source, wanted)?;
            cfg.keep_active()?;
            store.save(&cfg)?;
            refresh::publish(&cfg)?;
            output::emit(json, "refresh-binding", to_value(&r), |v| {
                println!(
                    "{} ({}) is refreshed {} through {}",
                    v["source"].as_str().unwrap_or(""),
                    v["cli"].as_str().unwrap_or(""),
                    if r.auto {
                        format!("every {} h and on request", r.every_secs / 3600)
                    } else {
                        "on request only".into()
                    },
                    r.provider_profile
                        .as_deref()
                        .map(|p| format!("login `{p}`"))
                        .unwrap_or_else(|| "no login".into())
                )
            });
            Ok(())
        }
        RefreshCmd::Remove { source } => {
            let mut cfg = load(store, cli)?;
            if cfg.active.refresh.sources.remove(source).is_none() {
                return Err(CliError::NotFound(format!(
                    "source `{source}` is not set up for refresh in this profile"
                )));
            }
            cfg.keep_active()?;
            store.save(&cfg)?;
            refresh::publish(&cfg)?;
            output::emit(
                json,
                "refresh-remove",
                json!({ "source": source, "removed": true }),
                |_| println!("{source} is no longer refreshed; its records stay"),
            );
            Ok(())
        }
        RefreshCmd::List => {
            let cfg = load(store, cli)?;
            let rows = refresh::list(&cfg)?;
            output::emit(json, "refresh-list", json!({ "items": rows }), |v| {
                table(
                    v,
                    &[
                        "source",
                        "cli",
                        "provider_profile",
                        "auto",
                        "paused",
                        "last_outcome",
                        "next_run_at",
                        "needs_person",
                    ],
                )
            });
            Ok(())
        }
        RefreshCmd::Status => {
            let cfg = load(store, cli)?;
            let st = refresh::read_status(&cfg)?;
            let v = to_value(&st);
            let rows: Vec<Value> = st
                .as_ref()
                .map(|s| {
                    s.sources
                        .iter()
                        .map(|e| {
                            json!({
                                "source": e.source,
                                "cli": e.cli,
                                "auto": e.auto,
                                "paused": e.paused.as_ref().map(|p| p.reason.clone()),
                                "sign_in": e.sign_in.state,
                                "last_attempt": e.last_attempt.as_ref().map(|a| a.outcome.clone()),
                                "last_success": e.last_success.as_ref().map(|x| x.finished_at.clone()),
                                "failures": e.failures,
                                "next_run_at": e.next_run_at,
                                "needs_person": e.sign_in.needs_person.as_ref().map(|n| n.command.clone()),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            if json {
                output::json(&json!({ "schema": "refresh-status/v1", "status": v }));
                return Ok(());
            }
            match &st {
                None => println!("refresh has not run for this folder"),
                Some(s) => {
                    println!(
                        "refreshed by {} (medrec {}), last pass {}, next {}",
                        s.refresher.machine,
                        s.refresher.medrec_version,
                        s.refresher.last_pass_at.as_deref().unwrap_or("-"),
                        s.refresher.next_pass_at.as_deref().unwrap_or("-")
                    );
                    table(
                        &json!({ "items": rows }),
                        &[
                            "source",
                            "cli",
                            "auto",
                            "paused",
                            "sign_in",
                            "last_attempt",
                            "last_success",
                            "failures",
                            "next_run_at",
                            "needs_person",
                        ],
                    );
                }
            }
            Ok(())
        }
        RefreshCmd::Pause {
            source,
            reason,
            note,
        } => {
            let mut cfg = load(store, cli)?;
            let b = cfg.active.refresh.sources.get_mut(source).ok_or_else(|| {
                CliError::NotFound(format!(
                    "source `{source}` is not set up for refresh in this profile"
                ))
            })?;
            b.paused = Some(medrec::refresh::contract::Pause {
                reason: match reason {
                    PauseReason::Consent => "consent".into(),
                    PauseReason::User => "user".into(),
                },
                note: note.as_deref().map(|n| {
                    medrec::refresh::contract::clip(n, medrec::refresh::contract::SHOWN_MAX)
                }),
                since: medrec::model::now_rfc3339(),
            });
            cfg.keep_active()?;
            store.save(&cfg)?;
            refresh::publish(&cfg)?;
            output::emit(
                json,
                "refresh-pause",
                json!({ "source": source, "paused": true }),
                |_| println!("{source} is paused until `medrec refresh resume {source}`"),
            );
            Ok(())
        }
        RefreshCmd::Resume { source } => {
            let mut cfg = load(store, cli)?;
            let b = cfg.active.refresh.sources.get_mut(source).ok_or_else(|| {
                CliError::NotFound(format!(
                    "source `{source}` is not set up for refresh in this profile"
                ))
            })?;
            b.paused = None;
            cfg.keep_active()?;
            store.save(&cfg)?;
            refresh::publish(&cfg)?;
            output::emit(
                json,
                "refresh-resume",
                json!({ "source": source, "paused": false }),
                |_| println!("{source} is refreshed again"),
            );
            Ok(())
        }
        RefreshCmd::SignIn { source } => {
            let mut cfg = Config::load(store)?;
            let given = cli.profile.clone().or_else(|| {
                std::env::var(medrec::config::PROFILE_ENV)
                    .ok()
                    .filter(|v| !v.is_empty())
            });
            let name = refresh::binding_profile(&cfg, source, given.as_deref())?;
            cfg.select(Some(name.as_str()))?;
            let pp = cfg
                .active
                .refresh
                .sources
                .get(source)
                .and_then(|b| b.provider_profile.clone());
            // Which logins this is, before anything signs in (names only).
            eprintln!(
                "medrec profile `{name}`, provider profile {}",
                pp.as_deref()
                    .map(|p| format!("`{p}`"))
                    .unwrap_or_else(|| "none (no login)".into())
            );
            let r = refresh::sign_in(&cfg, source)?;
            output::emit(json, "refresh-sign-in", to_value(&r), |v| {
                println!(
                    "{}: sign-in {}{}",
                    v["source"].as_str().unwrap_or(""),
                    v["state"].as_str().unwrap_or(""),
                    if r.needs_person {
                        "; it still needs a person"
                    } else {
                        ""
                    }
                )
            });
            if r.needs_person {
                std::process::exit(3);
            }
            Ok(())
        }
    }
}

fn import_cmd(cli: &Cli, a: &ImportArgs, store: &ConfigStore) -> Result<(), CliError> {
    let json = cli.common.json;
    if let Some(s) = &a.since {
        pk_cli_core::dates::parse_iso(s)?;
    }
    if !a.person.is_empty() && a.person.len() != a.patient.len() {
        return Err(CliError::Usage(format!(
            "{} --patient and {} --person: give one --person per --patient, in the same order, or none",
            a.patient.len(),
            a.person.len()
        )));
    }
    if a.patient.len() > 1 && a.from_files.is_some() {
        return Err(CliError::Usage(
            "--from-files is one member's downloaded files: give one --patient".into(),
        ));
    }
    // Absolute, so the provider reads the same directory and never takes a
    // path that starts with `-` for a flag.
    let from_files = match &a.from_files {
        Some(dir) if dir.is_dir() => Some(
            std::fs::canonicalize(dir)
                .map_err(|e| CliError::Usage(format!("--from-files `{}`: {e}", dir.display())))?,
        ),
        Some(dir) => {
            return Err(CliError::Usage(format!(
                "--from-files `{}` is not a directory",
                dir.display()
            )))
        }
        None => None,
    };
    let cfg = load(store, cli)?;
    let provider = import::probe(&a.from);
    if !provider.usable() {
        return Err(CliError::Usage(format!(
            "`{}` cannot export records ({}{})",
            a.from,
            provider.status,
            provider
                .note
                .as_deref()
                .map(|n| format!(": {n}"))
                .unwrap_or_default()
        )));
    }
    if from_files.is_some() && !provider.records_files {
        return Err(CliError::Usage(format!(
            "`{}` reads no downloaded files (its info lists no `{}`)",
            a.from,
            import::RECORDS_FILES
        )));
    }
    let from_profile = match (provider.login_profiles, &a.from_profile) {
        _ if from_files.is_some() => None,
        (true, Some(p)) => Some(p.clone()),
        (true, None) => return Err(import::missing_profile(&provider)),
        (false, Some(_)) => {
            return Err(CliError::Usage(format!(
                "`{}` needs no login, so it takes no --from-profile",
                a.from
            )))
        }
        (false, None) => None,
    };
    // Read fresh without the lock: `import::run` takes it after the
    // provider's export.
    let f = Folder::open(&cfg, Access::Fresh)?;
    let reqs: Vec<import::Request> = a
        .patient
        .iter()
        .enumerate()
        .map(|(i, patient)| import::Request {
            from: a.from.clone(),
            from_profile: from_profile.clone(),
            files: from_files.clone(),
            patient: patient.clone(),
            person: a.person.get(i).cloned(),
            since: a.since.clone(),
            org: a.org.clone(),
            allow_birth_date_mismatch: a.allow_birth_date_mismatch,
        })
        .collect();
    if let [req] = reqs.as_slice() {
        note(cli, &format!("importing from {} ...", a.from));
        let r = import::run(&f, &cfg, &provider, req)?;
        output::emit(json, "import", to_value(&r), |_| print_import(&r));
        return Ok(());
    }
    // Every member's export first, each spooled on this machine, and only
    // then the filing, which uploads to the folder.
    let n = reqs.len();
    let mut exported = Vec::new();
    for (i, req) in reqs.into_iter().enumerate() {
        note(
            cli,
            &format!("exporting member {} of {n} from {} ...", i + 1, a.from),
        );
        let spooled = import::export(&f, &cfg, &provider, &req);
        exported.push((req, spooled));
    }
    // The views once, after every member is filed.
    let views = medrec::viewfiles::hold(&cfg.folder_location()?, cfg.store_budget());
    let mut results = Vec::new();
    for (i, (req, spooled)) in exported.into_iter().enumerate() {
        note(cli, &format!("filing member {} of {n} ...", i + 1));
        let r = spooled.and_then(|s| import::file_spooled(&f, &cfg, &req, s));
        results.push((req.patient, r));
    }
    let code = results
        .iter()
        .find_map(|(_, r)| r.as_ref().err().map(CliError::exit_code))
        .unwrap_or(0);
    let items: Vec<Value> = results
        .iter()
        .map(|(patient, r)| match r {
            Ok(r) => json!({ "patient": patient, "import": to_value(r) }),
            Err(e) => json!({ "patient": patient, "error": e.to_json()["error"] }),
        })
        .collect();
    output::emit(
        json,
        "import-batch",
        json!({ "ok": code == 0, "items": items }),
        |_| {
            for (patient, r) in &results {
                match r {
                    Ok(r) => print_import(r),
                    Err(e) => {
                        eprintln!("error: {}: {e}", medrec::printable(patient));
                        if let Some(remedy) = e.remedy() {
                            eprintln!("  {remedy}");
                        }
                    }
                }
            }
        },
    );
    // The one document is printed, so a failure exits here rather than
    // printing a second (the family's rule). Every spool was consumed
    // above; `results` is the last value holding anything.
    drop(results);
    drop(views);
    if code != 0 {
        std::process::exit(code);
    }
    Ok(())
}

/// What a run says of the papers' raw files that do not read.
fn papers_unread(n: usize) -> Option<String> {
    (n > 0).then(|| {
        format!(
            "{n} raw file{} of papers could not be read and told nothing",
            if n == 1 { "" } else { "s" }
        )
    })
}

/// One import's lines for a person reading the terminal.
fn print_import(r: &import::Report) {
    let organization = medrec::printable(&r.organization);
    {
        println!(
            "{} resources from {} for {}: {} records added, {} updated, {} unchanged; {} documents added",
            r.resources, organization, r.person_name, r.records_added, r.records_updated, r.records_unchanged, r.documents_added
        );
        if r.documents_unchanged > 0 {
            println!(
                "{} documents were served again with the same text; the copies filed before were kept",
                r.documents_unchanged
            );
        }
        if r.documents_same_statement > 0 {
            println!(
                "{} bills are the same statement as a bill filed before; each points at it",
                r.documents_same_statement
            );
        }
        if let Some(n) = papers_unread(r.papers_unread) {
            println!("{n}");
        }
        if r.records_older > 0 {
            println!(
                "{} records came in older versions than the ones filed (meta.lastUpdated); the filed versions were kept",
                r.records_older
            );
        }
        // Withheld skips are listed below by reason rather than counted.
        let other = if r.withheld_skips > 0 { "other " } else { "" };
        match r.skipped - r.withheld_skips {
            0 => {}
            1 => {
                println!("1 {other}resource or search was skipped by the provider or as too large")
            }
            n => println!(
                "{n} {other}resources or searches were skipped by the provider or as too large"
            ),
        }
        if !r.withheld.is_empty() {
            println!(
                "{organization} withheld some resources of these types, so their records may be incomplete:"
            );
            for w in &r.withheld {
                let reason = if w.reason.is_empty() {
                    "no reason given"
                } else {
                    &w.reason
                };
                println!("  {}: {reason}", w.resource_types.join(", "));
            }
        }
    }
}

fn records_cmd(
    cli: &Cli,
    store: &ConfigStore,
    a: &RecordArgs,
    kinds: &[&str],
    record: &str,
) -> Result<(), CliError> {
    let (f, h) = read(store, cli)?;
    let (rows, notes) = views::records(
        &f,
        &h,
        a.person.as_deref(),
        kinds,
        &a.range.range(),
        a.active,
        a.code.as_deref(),
        &medrec::model::today(),
    )?;
    let out = noted(items(a.list.take(&rows)), &notes);
    output::emit(cli.common.json, &format!("{record}-list"), out, |v| {
        // What all its sources make of it for its status, and what each says
        // for its sources, where it has them.
        let mut v = v.clone();
        for row in v["items"].as_array_mut().into_iter().flatten() {
            if let Some(standing) = row.get("standing").cloned() {
                row["status"] = standing;
            }
            if let Some(says) = row.get("says").and_then(Value::as_array) {
                let each: Vec<&str> = says.iter().filter_map(Value::as_str).collect();
                row["sources"] = each.join("; ").into();
            }
        }
        table(
            &v,
            &[
                "date",
                "person_name",
                "display",
                "detail",
                "status",
                "sources",
            ],
        );
        notes.iter().for_each(|n| note(cli, n));
    });
    Ok(())
}

fn one_record_cmd(cli: &Cli, cmd: &RecordsCmd, store: &ConfigStore) -> Result<(), CliError> {
    let json = cli.common.json;
    let (f, h) = read(store, cli)?;
    match cmd {
        RecordsCmd::Show { id } => {
            let (row, about) = views::record(&f, &h, id)?;
            let said: Vec<String> = about.into_iter().collect();
            said.iter().for_each(|n| note(cli, n));
            output::emit(json, "record", noted(to_value(&row), &said), |v| {
                output::kv(v, 0)
            });
        }
        RecordsCmd::AboutAnother { person, list } => {
            let mut rows = Vec::new();
            for p in medrec::people::selected(&h, person.as_deref())? {
                for id in f.index(p)?.about_another {
                    rows.push(json!({"person": p.id, "id": id}));
                }
            }
            let out = json!({ "items": list.take(&rows) });
            output::emit(json, "about-another-list", out, |v| {
                table(v, &["person", "id"]);
            });
        }
    }
    Ok(())
}

/// Copy one file into a fresh private open folder and return its path.
fn private_copy(f: &Folder, rel: &str, label: &str) -> Result<PathBuf, CliError> {
    let dir = view::new_open_dir(label)?;
    let name = rel.rsplit('/').next().unwrap_or("document");
    let to = dir.join(name);
    let bytes = f.store.read_bytes(rel)?;
    medrec::write_private(&to, &bytes)?;
    Ok(to)
}

fn documents_cmd(cli: &Cli, cmd: &DocumentsCmd, store: &ConfigStore) -> Result<(), CliError> {
    let json = cli.common.json;
    match cmd {
        DocumentsCmd::List {
            person,
            doc_type,
            range,
            list,
        } => {
            let (f, h) = read(store, cli)?;
            let (rows, notes) = views::documents(
                &f,
                &h,
                person.as_deref(),
                doc_type.as_deref(),
                &range.range(),
            )?;
            let out = noted(items(list.take(&rows)), &notes);
            output::emit(json, "document-list", out, |v| {
                table(
                    v,
                    &[
                        "id",
                        "date",
                        "person_name",
                        "doc_type",
                        "organization",
                        "title",
                    ],
                );
                notes.iter().for_each(|n| note(cli, n));
            });
        }
        DocumentsCmd::Open { id, no_launch } => {
            let (f, h) = read(store, cli)?;
            let (p, d) = views::find_document(&f, &h, id, true)?;
            let error = views::paper_note(&f, &h, &p, &d)?;
            let about = medrec::about::of_paper(&p, &f.whole_index(&p)?, id);
            let said: Vec<String> = error.into_iter().chain(about).collect();
            said.iter().for_each(|n| note(cli, n));
            let path = private_copy(&f, &d.path, &d.id)?;
            let launched = !*no_launch && launch(&view::Viewer::for_documents(None), &path)?;
            output::emit(
                json,
                "document-open",
                noted(
                    json!({ "document": d.id, "path": path.display().to_string(), "launched": launched }),
                    &said,
                ),
                |v| println!("{}", v["path"].as_str().unwrap_or("")),
            );
        }
        DocumentsCmd::Merge {
            duplicate,
            into,
            force,
        } => {
            let cfg = load(store, cli)?;
            let f = Folder::open(&cfg, Access::Write)?;
            let h = f.household()?;
            // Every refusal before the prompt: it only asks about a merge
            // that will go ahead.
            let plan = medrec::documents::plan(&f, &h, duplicate, into)?;
            confirm::gate(
                &cli.common,
                *force,
                &format!("Merge document {duplicate} into {into} and remove its file?"),
            )?;
            let m = medrec::documents::apply(&f, plan)?;
            output::emit(json, "document-merge", to_value(&m), |_| {
                println!(
                    "merged {} into {}{}",
                    m.merged,
                    m.document,
                    if m.file_removed {
                        "; its file was removed"
                    } else {
                        "; its file was already gone"
                    }
                )
            });
        }
    }
    Ok(())
}

fn launch(viewer: &view::Viewer, path: &std::path::Path) -> Result<bool, CliError> {
    match viewer.command(path) {
        Some(cmd) => view::launch(&cmd).map(|_| true),
        None => Ok(false),
    }
}

fn note_cmd(cli: &Cli, cmd: &NoteCmd, store: &ConfigStore) -> Result<(), CliError> {
    let json = cli.common.json;
    let cfg = load(store, cli)?;
    match cmd {
        NoteCmd::Add {
            person,
            text,
            date,
            tags,
            about,
        } => {
            let text = if text == "-" {
                let mut s = String::new();
                std::io::stdin()
                    .take(1024 * 1024)
                    .read_to_string(&mut s)
                    .map_err(|e| CliError::Usage(format!("reading the note from stdin: {e}")))?;
                s
            } else {
                text.clone()
            };
            let f = Folder::open(&cfg, Access::Write)?;
            let h = f.household()?;
            let p = people::resolve(&h, person)?.clone();
            let author = cfg
                .active
                .me
                .as_deref()
                .and_then(|m| h.person(m))
                .map(|m| m.name.clone());
            let n = notes::add(
                &f,
                &p,
                &text,
                date.as_deref(),
                tags.clone(),
                about.clone(),
                author,
            )?;
            output::emit(
                json,
                "note",
                json!({ "person": p.id, "person_name": p.name, "note": to_value(&n) }),
                |_| println!("added note {} for {} on {}", n.id, p.name, n.date),
            );
        }
        NoteCmd::List {
            person,
            tag,
            range,
            list,
        } => {
            let f = Folder::open(&cfg, Access::Read)?;
            let h = f.household()?;
            let rows = views::notes(&f, &h, person.as_deref(), tag.as_deref(), &range.range())?;
            output::emit(json, "note-list", items(list.take(&rows)), |v| {
                table(v, &["date", "person_name", "tags", "text"])
            });
        }
    }
    Ok(())
}

fn images_cmd(cli: &Cli, cmd: &ImagesCmd, store: &ConfigStore) -> Result<(), CliError> {
    let json = cli.common.json;
    let cfg = load(store, cli)?;
    match cmd {
        ImagesCmd::Intake {
            path,
            person,
            allow_birth_date_mismatch,
            dry_run,
        } => {
            let f = Folder::open(
                &cfg,
                if *dry_run {
                    Access::Read
                } else {
                    Access::Write
                },
            )?;
            let r = imaging::intake(
                &f,
                &imaging::IntakeRequest {
                    path: path.clone(),
                    person: person.clone(),
                    allow_birth_date_mismatch: *allow_birth_date_mismatch,
                    dry_run: *dry_run,
                    interactive: cli.common.interactive(),
                },
            )?;
            output::emit(json, "image-intake", to_value(&r), |v| {
                println!(
                    "{} files: {} DICOM, {} other, {} unreadable",
                    r.files_seen, r.dicom_files, r.other_files, r.unreadable
                );
                output::table(&output::table_view(
                    &output::rows_of(v, "studies"),
                    &[
                        "status",
                        "date",
                        "modalities",
                        "person_name",
                        "instances_new",
                        "instances_existing",
                        "path",
                        "reason",
                    ],
                ));
                for d in &r.documents_found {
                    println!("not filed (a document; use `medrec intake`): {d}");
                }
            });
        }
        ImagesCmd::List { person, list } => {
            let f = Folder::open(&cfg, Access::Read)?;
            let h = f.household()?;
            let rows = imaging::list(&f, &h, person.as_deref())?;
            output::emit(json, "study-list", items(list.take(&rows)), |v| {
                table(
                    v,
                    &[
                        "id",
                        "date",
                        "person_name",
                        "modalities",
                        "body_part",
                        "facility",
                        "series",
                        "instances",
                    ],
                )
            });
        }
        ImagesCmd::Show { study } => {
            let f = Folder::open(&cfg, Access::Read)?;
            let h = f.household()?;
            let (p, s) = imaging::find(&f, &h, study)?;
            output::emit_one(
                json,
                "study",
                json!({ "person": p.id, "person_name": p.name, "study": to_value(&s) }),
            );
        }
        ImagesCmd::Open { study, no_launch } => {
            let f = Folder::open(&cfg, Access::Read)?;
            let h = f.household()?;
            let (_, s) = imaging::find(&f, &h, study)?;
            let dir = view::new_open_dir(&s.id)?;
            f.store.fetch_tree(&s.path, &dir)?;
            let launched =
                !*no_launch && launch(&view::Viewer::for_images(cfg.viewer.as_deref()), &dir)?;
            output::emit(
                json,
                "study-open",
                json!({ "study": s.id, "path": dir.display().to_string(), "launched": launched }),
                |v| println!("{}", v["path"].as_str().unwrap_or("")),
            );
        }
        ImagesCmd::Export { study, out } => {
            let f = Folder::open(&cfg, Access::Read)?;
            let h = f.household()?;
            let (_, s) = imaging::find(&f, &h, study)?;
            let r = imaging::export(&f, &s, out)?;
            output::emit(json, "study-export", to_value(&r), |_| {
                println!(
                    "exported {} files ({} bytes) to {}",
                    r.files, r.bytes, r.path
                )
            });
        }
    }
    Ok(())
}

fn config_cmd(cli: &Cli, cmd: &ConfigCmd, store: &ConfigStore) -> Result<(), CliError> {
    let json = cli.common.json;
    match cmd {
        ConfigCmd::Path => {
            let path = store.path()?;
            output::emit(
                json,
                "config-path",
                json!({ "path": path.display().to_string() }),
                |v| println!("{}", v["path"].as_str().unwrap_or("")),
            );
        }
        ConfigCmd::Show => {
            let mut cfg = Config::load(store)?;
            let named = cli.profile.is_some()
                || std::env::var(medrec::config::PROFILE_ENV).is_ok_and(|v| !v.is_empty());
            if named {
                cfg.select(cli.profile.as_deref())?;
            }
            output::emit_one(json, "config", config_view(&cfg));
        }
        ConfigCmd::Set { key, value } => {
            let mut cfg = config_for(cli, store, key)?;
            cfg.set(key, value)?;
            save_config(store, cfg, json)?;
        }
        ConfigCmd::Unset { key } => {
            let mut cfg = config_for(cli, store, key)?;
            cfg.unset(key)?;
            save_config(store, cfg, json)?;
        }
    }
    let _ = KEYS;
    Ok(())
}

/// The settings `config set|unset <key>` changes: a profile's key needs the
/// profile, this machine's does not.
fn config_for(cli: &Cli, store: &ConfigStore, key: &str) -> Result<Config, CliError> {
    if Config::is_profile_key(key) {
        load(store, cli)
    } else {
        Config::load(store)
    }
}

fn save_config(store: &ConfigStore, mut cfg: Config, json: bool) -> Result<(), CliError> {
    if cfg.profile.is_some() {
        cfg.keep_active()?;
    }
    store.save(&cfg)?;
    output::emit_one(json, "config", config_view(&cfg));
    Ok(())
}

/// Copy `from` to `to`, readable by this user only: the config names
/// records folders and a person id. A failed write leaves no partial copy.
fn keep_owner_only(from: &std::path::Path, to: &std::path::Path) -> Result<(), CliError> {
    let body = std::fs::read(from)
        .map_err(|e| CliError::Other(format!("reading {}: {e}", from.display())))?;
    medrec::write_private(to, &body)
}

/// `config show|set|unset`'s document: the file (this machine's settings,
/// and `profiles.<NAME>` for each profile's), and with a profile selected,
/// `profile` and that profile's `folder`, `mount`, `inbox` and `me` at the
/// top, where they were before profiles.
fn config_view(cfg: &Config) -> Value {
    let mut v = to_value(cfg);
    if let (Some(name), Some(top)) = (&cfg.profile, v.as_object_mut()) {
        for key in medrec::config::PROFILE_KEYS {
            top.remove(*key);
        }
        if let Value::Object(active) = to_value(&cfg.active) {
            top.extend(active);
        }
        top.insert("profile".into(), json!(name));
    }
    v
}

fn profiles_cmd(cli: &Cli, cmd: &ProfilesCmd, store: &ConfigStore) -> Result<(), CliError> {
    let json = cli.common.json;
    let mut cfg = Config::load(store)?;
    match cmd {
        ProfilesCmd::List => {
            let mut v = to_value(&cfg.profiles.list());
            // profile-list/v1 plus each profile's records folder, optional.
            if let Some(rows) = v["profiles"].as_array_mut() {
                for row in rows {
                    let folder = row["name"]
                        .as_str()
                        .and_then(|n| ProfileName::parse(n).ok())
                        .and_then(|n| cfg.profiles.settings::<FolderSettings>(&n).ok())
                        .and_then(|s| s.folder);
                    if let Some(folder) = folder {
                        row["folder"] = json!(folder);
                    }
                }
            }
            if json {
                output::json(&v);
            } else if cfg.profiles.is_empty() {
                eprintln!("no profiles; add one with `{BIN} profiles add <NAME>`");
                if !cfg.unprofiled.is_empty() {
                    eprintln!(
                        "this machine has a records folder from before profiles; move it into \
                         one with `{BIN} profiles migrate <NAME>`"
                    );
                }
            } else {
                for row in v["profiles"].as_array().into_iter().flatten() {
                    let name = row["name"].as_str().unwrap_or("");
                    match row["folder"].as_str() {
                        Some(folder) => println!("{name}\t{folder}"),
                        None => println!("{name}\t(no records folder yet)"),
                    }
                }
            }
        }
        ProfilesCmd::Add { name } => {
            let added = cfg.profiles.add(name.clone());
            if added {
                store.save(&cfg)?;
            }
            let status = if added { "added" } else { "exists" };
            output::emit(
                json,
                "profile",
                json!({ "name": name, "status": status }),
                |_| {
                    if added {
                        println!("added profile {name}; give it a records folder with `{BIN} --profile {name} init <gdrive:PROFILE/FOLDER-ID|dir>`")
                    } else {
                        println!("profile {name} already exists")
                    }
                },
            );
        }
        ProfilesCmd::Remove { name, force } => {
            if !cfg.profiles.contains(name) {
                return Err(cfg
                    .select_from(Some(name.as_str()), None)
                    .err()
                    .unwrap_or_else(|| CliError::Usage(format!("no profile `{name}`"))));
            }
            confirm::gate(
                &cli.common,
                *force,
                &format!(
                    "Remove profile {name} from this machine? Its records folder is left as it is."
                ),
            )?;
            cfg.profiles.remove(name);
            store.save(&cfg)?;
            output::emit(
                json,
                "profile",
                json!({ "name": name, "status": "removed" }),
                |_| println!("removed profile {name}; its records folder is unchanged"),
            );
        }
        ProfilesCmd::Migrate { name } => {
            let moved = cfg.migrate(name)?;
            let mut v = json!({ "name": name, "status": "nothing-to-migrate" });
            if moved {
                // medrec before 0.2.0 reads no profiles and would save its
                // own settings over them, so the file as it was is kept.
                let path = store.path()?;
                let backup = path.with_file_name("config.json.pre-profiles");
                if !backup.exists() {
                    keep_owner_only(&path, &backup)?;
                }
                store.save(&cfg)?;
                v["status"] = json!("migrated");
                v["backup"] = json!(backup.display().to_string());
            }
            output::emit(json, "profile", v, |v| {
                if moved {
                    println!(
                        "moved this machine's records folder into profile {name}; the file as it was is {}",
                        v["backup"].as_str().unwrap_or("")
                    )
                } else {
                    println!("nothing to move: this machine kept no records folder before profiles")
                }
            });
        }
    }
    Ok(())
}
