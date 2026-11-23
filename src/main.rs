//! medrec: a household's medical records in a plain Drive folder
//! (piekstra-cli/1). The command tree and output live here; the mechanisms
//! live in the library.

use std::io::Read;
use std::path::PathBuf;

use clap::{Args, CommandFactory, Parser, Subcommand};
use clap_complete::Shell;
use pk_cli_auth::AuthStatus;
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
use medrec::{imaging, import, maint, notes, people, review, store, view, views};

const REPO: &str = "own-your-health/medrec-cli";

/// A household's medical records in a plain Drive folder: people, intake of
/// scans and files with local OCR, imaging studies, provider and insurer
/// imports, timelines, search and notes.
#[derive(Parser, Debug)]
#[command(name = BIN, version, about, long_about = None)]
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
    /// Filed documents.
    #[command(subcommand)]
    Documents(DocumentsCmd),
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
    /// The scheduled refresh's check that this binary reads its Drive
    /// sign-in from the Keychain without an approval: one Keychain read,
    /// no request to Google. Exit 0 when it is there, 3 when it is not.
    #[command(hide = true)]
    DriveKeychainProbe {
        #[arg(long = "drive-profile", value_name = "NAME")]
        drive_profile: String,
    },
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
    /// Set up medrec in a folder that already holds other files.
    #[arg(long)]
    allow_other_files: bool,
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
        id: String,
        #[command(flatten)]
        given: GivenArgs,
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
    /// The patient (or member) id at that provider.
    #[arg(long)]
    patient: String,
    /// Whose records they are (remembered for the next import).
    #[arg(long)]
    person: Option<String>,
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
    /// Only current ones (not completed, stopped, resolved or inactive).
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
}

#[derive(Subcommand, Debug)]
enum CacheCmd {
    /// Remove every copy medrec keeps on this machine (the records folder
    /// itself is untouched). The next command reads from Drive again. The
    /// run locks and the refresh state stay.
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

fn run(cli: &Cli) -> Result<(), CliError> {
    let json = cli.common.json;
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
        Command::Timeline(a) => {
            let (f, h) = read(&store, cli)?;
            let rows = views::timeline(&f, &h, a.person.as_deref(), &a.range.range(), &a.kinds)?;
            output::emit(json, "timeline", items(a.list.take(&rows)), |v| {
                table(
                    v,
                    &["date", "person_name", "kind", "title", "detail", "source"],
                )
            });
            Ok(())
        }
        Command::Search(a) => {
            let (f, h) = read(&store, cli)?;
            let q = a.query.join(" ");
            let rows = views::search(
                &f,
                &h,
                a.person.as_deref(),
                &q,
                &a.range.range(),
                a.limit as usize,
            )?;
            output::emit(json, "search", items(&rows), |v| {
                table(
                    v,
                    &["date", "person_name", "kind", "id", "title", "snippet"],
                )
            });
            Ok(())
        }
        Command::Results(a) => records_cmd(cli, &store, a, &["result", "report"], "result"),
        Command::Meds(a) => records_cmd(cli, &store, a, &["medication"], "medication"),
        Command::Conditions(a) => records_cmd(cli, &store, a, &["condition"], "condition"),
        Command::Allergies(a) => records_cmd(cli, &store, a, &["allergy"], "allergy"),
        Command::Immunizations(a) => records_cmd(cli, &store, a, &["immunization"], "immunization"),
        Command::Documents(cmd) => documents_cmd(cli, cmd, &store),
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
        Command::DriveKeychainProbe { drive_profile } => {
            let profile = ProfileName::parse(drive_profile)?;
            store::drive_cli()?.account(&profile).stored()?;
            output::emit(
                json,
                "drive-keychain-probe",
                json!({ "drive_profile": profile.as_str(), "signed_in": true }),
                |_| println!("Drive profile `{profile}` is signed in"),
            );
            Ok(())
        }
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
            );
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
    if a.files.is_empty() && !a.inbox && a.drive.is_none() {
        return Err(CliError::Usage(
            "name files, or pass --inbox or --drive <folder>".into(),
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
    let v = to_value(&report);
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
    if !ok {
        std::process::exit(1);
    }
    Ok(())
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
        ReviewCmd::File { id, given: g } => {
            let given = given(g, None);
            intake::validate_given(&given)?;
            let f = Folder::open(&cfg, Access::Write)?;
            let r = review::file(
                &f,
                &review::FileRequest {
                    id: id.clone(),
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
        let r = refresh::pass(&cfg, mode, pick.as_ref())?;
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
                println!("  request {} {}", q.request, q.outcome);
            }
            if let Some(c) = &r.command {
                println!(
                    "  the Drive sign-in waits on a Keychain approval; run `{c}` at the screen"
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
    note(cli, &format!("importing from {} ...", a.from));
    let r = import::run(
        &f,
        &cfg,
        &provider,
        &import::Request {
            from: a.from.clone(),
            from_profile,
            files: from_files,
            patient: a.patient.clone(),
            person: a.person.clone(),
            since: a.since.clone(),
            org: a.org.clone(),
            allow_birth_date_mismatch: a.allow_birth_date_mismatch,
        },
    )?;
    let organization = medrec::printable(&r.organization);
    output::emit(json, "import", to_value(&r), |_| {
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
    });
    Ok(())
}

fn records_cmd(
    cli: &Cli,
    store: &ConfigStore,
    a: &RecordArgs,
    kinds: &[&str],
    record: &str,
) -> Result<(), CliError> {
    let (f, h) = read(store, cli)?;
    let rows = views::records(
        &f,
        &h,
        a.person.as_deref(),
        kinds,
        &a.range.range(),
        a.active,
        a.code.as_deref(),
    )?;
    output::emit(
        cli.common.json,
        &format!("{record}-list"),
        items(a.list.take(&rows)),
        |v| {
            table(
                v,
                &[
                    "date",
                    "person_name",
                    "display",
                    "detail",
                    "status",
                    "sources",
                ],
            )
        },
    );
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
            let rows = views::documents(
                &f,
                &h,
                person.as_deref(),
                doc_type.as_deref(),
                &range.range(),
            )?;
            output::emit(json, "document-list", items(list.take(&rows)), |v| {
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
                )
            });
        }
        DocumentsCmd::Open { id, no_launch } => {
            let (f, h) = read(store, cli)?;
            let (_, d) = views::find_document(&f, &h, id)?;
            let path = private_copy(&f, &d.path, &d.id)?;
            let launched = !*no_launch && launch(&view::Viewer::for_documents(None), &path)?;
            output::emit(
                json,
                "document-open",
                json!({ "document": d.id, "path": path.display().to_string(), "launched": launched }),
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
