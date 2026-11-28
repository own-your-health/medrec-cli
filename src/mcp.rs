//! `medrec mcp`: the records folder as four read-only MCP tools, for Claude
//! Desktop and Claude Code on this machine (pk-cli-mcp, the family's MCP
//! profile). Each tool reads through the views the commands use; none
//! writes to the folder or reaches any network but the folder's own: a
//! Drive folder is read through medrec's own Drive sign-in, from its vault.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use pk_cli_core::CliError;
use pk_cli_mcp::shape::{ENVELOPE_RESERVE, SIZE_BUDGET};
use pk_cli_mcp::{
    parse_args, ListArgs, Page, PageRequest, Server, ToolDef, ToolError, ToolOutput, Toolset,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::code_names::Names;
use crate::config::Config;
use crate::folder::{Access, Folder};
use crate::intake::classify;
use crate::model::{today, Household, Record};
use crate::panels;
use crate::parts::Parts;
use crate::people;
use crate::provider_labels::{Asked, Labels, Printed};
use crate::refresh::signin;
use crate::sources::Sources;
use crate::standing::{self, Standings};
use crate::store::{Location, BIN};
use crate::summary::{self, clip, day, PersonRow, ResultRow, CLIP};
use crate::views::{self, first_line, is_ended, label, record_detail, record_source, Range};

pub const SUMMARY: &str = "medrec_summary";
pub const SEARCH: &str = "medrec_search";
pub const RECORDS: &str = "medrec_records";
pub const DOCUMENT: &str = "medrec_document";

/// Lines per section of a summary.
pub(crate) const SUMMARY_LINES: usize = 25;
/// Characters of a document's text one call returns at most, before JSON
/// escaping; the part is cut further if escaping takes it over budget.
const TEXT_CHUNK: usize = 16_000;
/// Characters of compact JSON a document answer stays under.
const DOCUMENT_BUDGET: usize = 30_000;
/// The longest note text a list carries.
const NOTE_CLIP: usize = 2_000;
/// How long one call's reads of a Drive folder may take in all. Claude's
/// clients give up on a tool call within minutes; a stalled Drive read
/// answers `upstream` well before that.
pub const CALL_BUDGET: Duration = Duration::from_secs(45);
/// How old a cached copy of a folder file may be and still answer a call:
/// the cache's own freshness, so nothing older is served while a background
/// refresh catches up.
pub const MAX_STALE_SECS: &str = "900";

/// The tools over one profile's records folder.
pub struct MedrecTools {
    /// `None` only to list the tools, which reads nothing.
    cfg: Option<Config>,
    /// This process has read the Drive sign-in from the Keychain once
    /// without waiting on a dialog, so a call reads it in place.
    keychain_read: std::sync::atomic::AtomicBool,
    /// Where the code tables are: this machine's, or a test's.
    tables: Option<std::path::PathBuf>,
}

impl MedrecTools {
    pub fn new(cfg: Config) -> Self {
        // A server nobody may be watching: a key another process replaced
        // is read again with the same bound as the first read.
        crate::store::bound_key_reads(signin::wait_or(CALL_BUDGET));
        MedrecTools {
            cfg: Some(cfg),
            keychain_read: Default::default(),
            tables: crate::tables::dir(),
        }
    }

    /// The code tables of `dir` in place of this machine's, as a test keeps
    /// them.
    pub fn with_tables(mut self, dir: std::path::PathBuf) -> Self {
        self.tables = Some(dir);
        self
    }

    /// What CDC's tables say of vaccine codes.
    fn codes(&self) -> crate::vaccines::Codes {
        let dir = self.tables.as_deref();
        dir.map_or_else(Default::default, crate::vaccines::Codes::load)
    }

    /// For `mcp --list-tools`: no profile, and a call is refused.
    pub fn listing_only() -> Self {
        MedrecTools {
            cfg: None,
            keychain_read: Default::default(),
            tables: None,
        }
    }

    /// Before serving: cached copies of a Drive folder's files answer only
    /// while fresh, unless `MEDREC_CACHE_MAX_STALE` says otherwise. Call it
    /// before any thread starts.
    pub fn limit_staleness() {
        if std::env::var_os("MEDREC_CACHE_MAX_STALE").is_none() {
            std::env::set_var("MEDREC_CACHE_MAX_STALE", MAX_STALE_SECS);
        }
    }

    fn open(&self) -> Result<(Folder, Household), ToolError> {
        let cfg = self
            .cfg
            .as_ref()
            .ok_or_else(|| ToolError::other("no profile selected"))?;
        let budget = cfg.store_budget().min(CALL_BUDGET);
        let location = cfg.folder_location()?;
        if let Location::Drive(spec) = &location {
            self.read_keychain(spec.profile().as_str())?;
        }
        let f = Folder::open_at(&location, Access::Read, budget)?;
        let h = f.household()?;
        Ok((f, h))
    }

    /// The first call's read of the Drive sign-in, bounded by
    /// [`CALL_BUDGET`] ([`signin::read_drive`]). This copy of medrec (the
    /// extension's own) asks macOS before it may read the item. A dialog
    /// nobody answers ends the call with `auth` instead of holding it, and
    /// the next call waits on the same dialog rather than raising another.
    /// Once read, the key stays in the process's one vault, so no later call
    /// asks again.
    fn read_keychain(&self, profile: &str) -> Result<(), ToolError> {
        use std::sync::atomic::Ordering;
        if self.keychain_read.load(Ordering::Relaxed) {
            return Ok(());
        }
        let wait = signin::wait_or(CALL_BUDGET);
        match signin::read_drive(profile, wait) {
            signin::Read::Read { state, .. } if signin::usable(state) => {
                self.keychain_read.store(true, Ordering::Relaxed);
                Ok(())
            }
            signin::Read::Read { .. } => Err(ToolError::from(CliError::Auth(format!(
                "the records folder's Drive profile `{profile}` is not signed in on this computer"
            )))
            .with_fix(format!(
                "Ask the user to run `{BIN} drive login --drive-profile {profile}` in a \
                 terminal, then call the tool again"
            ))),
            signin::Read::TimedOut => Err(ToolError::from(CliError::Auth(format!(
                "macOS asked whether medrec may use its Keychain item, and nobody answered \
                 within {} s",
                wait.as_secs()
            )))
            .with_fix(
                "Ask the user to answer the macOS Keychain dialog for medrec, choosing Always \
                 Allow so it does not ask again, then call the tool again",
            )),
            signin::Read::Failed(m) => Err(CliError::Upstream(m).into()),
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SummaryArgs {
    /// Name, alias or id; omit for the people.
    #[serde(default)]
    pub person: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SummaryOut {
    pub people: Vec<PersonRow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<PersonSummary>,
}

/// One person's records at a glance, in sections.
#[derive(Debug, Serialize, JsonSchema)]
pub struct PersonSummary {
    pub person: String,
    pub as_of: String,
    /// Records per kind.
    #[schemars(with = "std::collections::BTreeMap<String, i64>")]
    pub counts: std::collections::BTreeMap<String, usize>,
    pub sources: Vec<summary::SourceRow>,
    pub sections: Vec<Section>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Section {
    pub section: String,
    pub items: Vec<Line>,
    /// Lines left out for length.
    #[schemars(with = "i64")]
    pub more: usize,
    // How many are entered in error, and which sources send no problem list.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Default, Serialize, JsonSchema)]
pub struct Line {
    pub name: String,
    // A condition's plain name, from `code-names.json`; `name` is the
    // source's own. Not a doc comment: the tools' descriptions have a budget.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plain_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    // As an item's.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub standing: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub says: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub private: Option<String>,
    /// The key of a condition's plain name.
    #[serde(skip)]
    pub name_key: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchArgs {
    /// Words to find.
    pub query: String,
    /// Name, alias or id.
    #[serde(default)]
    pub person: Option<String>,
    /// YYYY-MM-DD.
    #[serde(default)]
    pub since: Option<String>,
    /// YYYY-MM-DD, inclusive.
    #[serde(default)]
    pub until: Option<String>,
    #[serde(flatten)]
    pub list: ListArgs,
}

#[derive(Serialize)]
struct SearchFilter<'a> {
    query: &'a str,
    person: Option<&'a str>,
    since: Option<&'a str>,
    until: Option<&'a str>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SearchItem {
    pub person: String,
    pub kind: String,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    pub title: String,
    pub snippet: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub not_shown: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub private: Option<String>,
}

/// What `medrec_records` lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Medications,
    Conditions,
    Allergies,
    Immunizations,
    Visits,
    Procedures,
    Results,
    Tests,
    Reports,
    Claims,
    Coverage,
    Bills,
    Documents,
    Notes,
    Timeline,
    Events,
}

impl Kind {
    pub const ALL: [Kind; 16] = [
        Kind::Medications,
        Kind::Conditions,
        Kind::Allergies,
        Kind::Immunizations,
        Kind::Visits,
        Kind::Procedures,
        Kind::Results,
        Kind::Tests,
        Kind::Reports,
        Kind::Claims,
        Kind::Coverage,
        Kind::Bills,
        Kind::Documents,
        Kind::Notes,
        Kind::Timeline,
        Kind::Events,
    ];

    /// Its name in calls, `medications`.
    pub fn name(self) -> String {
        serde_json::to_value(self)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default()
    }

    /// The record kind it lists, for a kind of record.
    fn record_kind(self) -> Option<&'static str> {
        Some(match self {
            Kind::Medications => "medication",
            Kind::Conditions => "condition",
            Kind::Allergies => "allergy",
            Kind::Immunizations => "immunization",
            Kind::Visits => "encounter",
            Kind::Procedures => "procedure",
            Kind::Results | Kind::Tests => "result",
            Kind::Reports => "report",
            Kind::Claims => "claim",
            Kind::Coverage => "coverage",
            Kind::Bills | Kind::Documents | Kind::Notes | Kind::Timeline | Kind::Events => {
                return None
            }
        })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RecordsArgs {
    /// visits: encounters. tests: each test's latest result. bills: filed
    /// statements. timeline: all.
    pub kind: Kind,
    /// Name, alias or id.
    #[serde(default)]
    pub person: Option<String>,
    /// YYYY-MM-DD.
    #[serde(default)]
    pub since: Option<String>,
    /// YYYY-MM-DD, inclusive.
    #[serde(default)]
    pub until: Option<String>,
    /// Current ones only.
    #[serde(default)]
    pub active: Option<bool>,
    /// A code (`4548-4`) or words.
    #[serde(default)]
    pub name: Option<String>,
    /// `lab_report`, `bill`, `eob`... (`documents`).
    #[serde(default)]
    pub doc_type: Option<String>,
    /// One event (`events`).
    #[serde(default)]
    pub id: Option<String>,
    #[serde(flatten)]
    pub list: ListArgs,
}

#[derive(Serialize)]
struct RecordsFilter<'a> {
    kind: Kind,
    person: Option<&'a str>,
    since: Option<&'a str>,
    until: Option<&'a str>,
    active: bool,
    name: Option<&'a str>,
    doc_type: Option<&'a str>,
    id: Option<&'a str>,
}

#[derive(Debug, Default, Serialize, JsonSchema)]
pub struct Item {
    pub kind: String,
    pub person: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end: Option<String>,
    pub title: String,
    // For `conditions`; `title` is the source's own.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plain_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    // What is left out of it, in an answer that names no person.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub not_shown: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    // What its sources' words make of a condition, an allergy or a medicine,
    // and what each says. No doc comments, as above.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub standing: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub says: Option<String>,
    /// Its group in a list of current ones, the first first.
    #[serde(skip)]
    pub group: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// For `conditions`: the key of the plain name.
    #[serde(skip)]
    pub name_key: Option<String>,
    /// Whose it is.
    #[serde(skip)]
    pub person_id: String,
    /// Some of its `provider_labels` did not fit beside it.
    #[serde(skip)]
    pub labels_left_out: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flag: Option<String>,
    /// For `tests`: how many.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<i64>")]
    pub results: Option<usize>,
    /// For `tests`: the ids of every result in the series, the latest's
    /// included, so that a private one among them holds the series back.
    #[serde(skip)]
    pub members: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub amount_due: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub due_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_text: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub you_owe: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub private: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub provider_labels: Vec<Map<String, Value>>,
    /// The providers it prints, to label.
    #[serde(skip)]
    pub printed: Vec<Printed>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fields: Option<Map<String, Value>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DocumentArgs {
    /// The document's id.
    pub id: String,
    /// Its owner, for its private lines too.
    #[serde(default)]
    pub person: Option<String>,
    /// First page.
    #[serde(default)]
    pub page: Option<u32>,
    /// An earlier `next_offset`.
    #[serde(default)]
    pub offset: Option<u64>,
}

#[derive(Debug, Default, Serialize, JsonSchema)]
pub struct DocumentOut {
    pub id: String,
    pub person: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub organization: Option<String>,
    // The definitions have a budget: these schemas say no minimum.
    #[schemars(with = "i64")]
    pub text_chars: usize,
    #[schemars(with = "i64")]
    pub offset: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<i64>")]
    pub next_offset: Option<usize>,
    #[schemars(with = "i64")]
    pub first_page: u32,
    #[schemars(with = "i64")]
    pub last_page: u32,
    #[schemars(with = "i64")]
    pub text_pages: u32,
    pub text: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub records: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub private: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub provider_labels: Vec<Map<String, Value>>,
    // What its text says on the lines it labels, a line each: a bill's
    // amount due, follow-ups and drugs (`fields_of`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

/// A bill's amount due and its due date as a line.
fn due_line(amount: Option<String>, due: Option<&str>) -> Option<String> {
    match (amount, due) {
        (Some(a), Some(d)) => Some(format!("Amount due: {a}, by {d}")),
        (Some(a), None) => Some(format!("Amount due: {a}")),
        (None, Some(d)) => Some(format!("Due by {d}")),
        (None, None) => None,
    }
}

/// A follow-up as a line: its words, and the day they tell and how.
fn follow_up_line(f: &crate::paper::FollowUp) -> String {
    use crate::paper::Dated;
    let how = |d: Dated| match d {
        Dated::Printed => "as printed",
        Dated::Relative => "from the paper's date",
        Dated::Resolved => "printed with no year",
    };
    match (&f.date, f.dated) {
        (Some(day), Some(d)) => format!("Follow-up: {} ({day}, {})", f.words, how(d)),
        _ => format!("Follow-up: {}", f.words),
    }
}

/// A drug as a line: as printed, its directions, its codes and the person's
/// medicine of an equal code.
fn drug_line(d: &crate::paper::Drug, medicine: Option<&str>) -> String {
    let mut out = format!("Drug, from a paper: {}", d.drug);
    if let Some(how) = &d.directions {
        out.push_str(&format!(": {how}"));
    }
    let codes = [("RxNorm", &d.rxnorm), ("NDC", &d.ndc)];
    let codes = codes
        .iter()
        .filter_map(|(k, v)| Some(format!("{k} {}", v.as_ref()?)));
    let mut said: Vec<String> = codes.collect();
    said.extend(medicine.map(|id| format!("the person's medicine {id}")));
    if !said.is_empty() {
        out.push_str(&format!(" ({})", said.join("; ")));
    }
    out
}

impl Toolset for MedrecTools {
    fn server(&self) -> Server {
        Server::new(BIN, env!("CARGO_PKG_VERSION"), "medical records", "medrec")
            .sign_in_command(format!("{BIN} --profile <NAME> auth status"))
            .instructions(
                "medrec keeps the household's medical records, imported from patient portals \
                 and insurers and filed from scans, in one folder; these tools read it and \
                 change nothing. Start with medrec_summary; medrec_search finds anything by its \
                 words. For what happened or what it cost, use medrec_records kind events: one \
                 item per visit, ER trip, stay, test or fill, with the household's share. Items \
                 about mental health, reproductive or sexual health or substance use carry \
                 private, with its area: an answer about the whole household leaves them out \
                 and its warnings say how many, and leaves such lines out of a claim, an event \
                 or a paper's text, which not_shown or hint says; they are in answers about that \
                 person, or that name the item. Keep them out of anything shared. Give the date and \
                 source of each fact you use, and say when the records \
                 do not answer the question. Each result carries its lab's own reference range \
                 and flag, and labs may report one test in different units. provider_labels say \
                 who a printed provider is, beside the source's words: call a likely one likely \
                 and give its evidence, and say where a phone is from. standing is what a \
                 condition, an allergy or a medicine is by all of its sources, and says has each \
                 source's word and the day it was read: where sources differ, give each word and \
                 choose none. For decisions about care, suggest the user ask their clinician.",
            )
    }

    fn tools(&self) -> Vec<ToolDef> {
        vec![
            ToolDef::read(
                SUMMARY,
                "Summary",
                "The people, and with person their summary. Use it first. Medicines, \
                 conditions, allergies, vaccines, results, visits, bills.",
            )
            .input::<SummaryArgs>()
            .output::<SummaryOut>(),
            ToolDef::read(
                SEARCH,
                "Search records",
                "What has every word of query, best first. A doctor, test, medicine or clinic. \
                 labs or blood tests finds every lab result.",
            )
            .input::<SearchArgs>()
            .output::<Page<SearchItem>>(),
            ToolDef::read(
                RECORDS,
                "List records",
                "One kind of record, newest first. events: what happened, one per visit, stay \
                 or fill, with you_owe; id gives one in full. results with name: one test over \
                 time.",
            )
            .input::<RecordsArgs>()
            .output::<Page<Item>>(),
            ToolDef::read(
                DOCUMENT,
                "Read a document",
                "A filed document's text. A visit summary, lab report, letter or bill. Long text \
                 comes in parts: pass next_offset as offset.",
            )
            .input::<DocumentArgs>()
            .output::<DocumentOut>(),
        ]
    }

    async fn call(&self, name: &str, args: Value) -> Result<ToolOutput, ToolError> {
        match name {
            SUMMARY => self.summary(parse_args(args)?),
            SEARCH => self.search(parse_args(args)?),
            RECORDS => self.records(parse_args(args)?),
            DOCUMENT => self.document(parse_args(args)?),
            other => Err(ToolError::other(format!("no handler for {other}"))),
        }
    }
}

/// An argument with surrounding space removed, or none when blank.
fn given(s: &Option<String>) -> Option<&str> {
    s.as_deref().map(str::trim).filter(|s| !s.is_empty())
}

fn range(since: &Option<String>, until: &Option<String>) -> Result<Range, ToolError> {
    let r = Range {
        since: given(since).map(str::to_string),
        until: given(until).map(str::to_string),
    };
    r.validate().map_err(|e| {
        ToolError::from(e).with_fix("Pass since and until as YYYY-MM-DD, e.g. 2026-01-31")
    })?;
    Ok(r)
}

fn person_error(e: CliError) -> ToolError {
    ToolError::from(e).with_fix("Call medrec_summary with no arguments for the household's people")
}

/// What of a person's that decides what is private does not read.
#[derive(Debug, Clone, Copy)]
enum Unreadable {
    Marks,
    Labels,
}

/// The private items of the people an answer covers (`private.json`).
#[derive(Debug, Default)]
struct PrivateItems {
    /// By id, with their area.
    areas: std::collections::BTreeMap<String, String>,
    /// The people whose `marks.json` or `provider-state.json` does not read,
    /// by name as shown. Any of their items may be one the household marked
    /// private or a label makes private, so an answer about the whole
    /// household holds all of them back (`held`).
    unreadable: Vec<(String, String, Unreadable)>,
    /// The record, document, event and study ids of those people, and
    /// every study with no report, which nothing judges.
    held: BTreeSet<String>,
    /// What is entered in error, which no list shows.
    errors: BTreeSet<String>,
    /// The bills entered in error the household marked a payment on.
    paid_errors: BTreeSet<String>,
    /// Whether the list is of bills.
    bills: bool,
    /// The people these are the items of.
    people: BTreeSet<String>,
    /// What an answer that names no person leaves out of open items.
    parts: Parts,
    /// The events whose place another person's may have.
    clear: BTreeSet<String>,
}

impl PrivateItems {
    fn of(f: &Folder, h: &Household, people: &[&crate::model::Person]) -> Result<Self, CliError> {
        let mut out = PrivateItems::default();
        for p in people {
            out.people.insert(p.id.clone());
            let idx = f.index(p)?;
            let (v, events) = crate::private::with_events(f, h, p, &idx)?;
            if v.get("marks_unreadable").is_some() {
                let what = match v.get("labels_unreadable") {
                    Some(_) => Unreadable::Labels,
                    None => Unreadable::Marks,
                };
                let name = crate::printable(&p.name);
                out.unreadable.push((p.id.clone(), name, what));
                out.held.extend(
                    idx.records
                        .iter()
                        .map(|r| r.id.clone())
                        .chain(idx.documents.iter().map(|d| d.id.clone()))
                        .chain(idx.studies.iter().map(|s| s.id.clone()))
                        .chain(
                            ["events", "events_in_error"]
                                .iter()
                                .filter_map(|k| events.events[*k].as_array())
                                .flatten()
                                .filter_map(|e| e["id"].as_str().map(str::to_string)),
                        ),
                );
            }
            let standings = Standings::of(&h.sources, p, &idx);
            out.errors.extend(standings.errors().cloned());
            out.parts.extend(Parts::of(&v));
            let items = crate::private::Private::of(&v);
            let clear = v["clear_events"].as_array().into_iter().flatten();
            out.clear
                .extend(clear.filter_map(|id| id.as_str().map(str::to_string)));
            out.held.extend(items.unreported_studies);
            out.areas.extend(items.items);
        }
        Ok(out)
    }

    fn area(&self, id: &str) -> Option<String> {
        self.areas.get(id).cloned()
    }

    /// Whether an item stays out of an answer about the whole household
    /// that does not name it.
    fn held_back(&self, id: &str) -> bool {
        self.areas.contains_key(id) || self.held.contains(id)
    }

    /// Takes the ids of what is held back out of a value. Whether any of it
    /// is left.
    fn without_held(&self, v: &mut Value) -> bool {
        match v {
            Value::String(s) => !self.held_back(s),
            Value::Array(list) => {
                let all = list.len();
                list.retain_mut(|v| self.without_held(v));
                all == 0 || !list.is_empty()
            }
            Value::Object(map) => {
                map.retain(|_, v| self.without_held(v));
                true
            }
            _ => true,
        }
    }

    /// Leaves out of an event what an answer may not show with it, counted in
    /// `private_members`: its private members, in an answer about the whole
    /// household (`whole`) that the event itself is not private in; and
    /// another person's (of `self`, else `others`), in any. So with the
    /// events it tells of, and a place it has of one that is private or holds
    /// something private. The ids of the other people whose are left out
    /// because what is private of theirs does not read.
    fn members(&self, others: &PrivateItems, e: &mut Value, whole: bool) -> BTreeSet<String> {
        let named = e["id"]
            .as_str()
            .is_some_and(|id| self.areas.contains_key(id));
        let own = |id: &str| !whole || named || !self.held_back(id);
        let of = |p: &str| {
            let mut all = [self, others].into_iter();
            all.find(|of| of.people.contains(p))
        };
        let mut unknown = BTreeSet::new();
        let mut shown = |x: &Value, id: &Value| {
            let Some(id) = id.as_str() else {
                return false;
            };
            let Some(p) = x.get("person") else {
                return own(id);
            };
            let p = p.as_str().unwrap_or_default();
            let Some(of) = of(p) else {
                return false;
            };
            let shown = !of.held_back(id);
            if !shown && of.unreadable.iter().any(|(of, ..)| of == p) {
                unknown.insert(p.to_string());
            }
            shown
        };
        if e["place_from"] == "linked" {
            let links = e["links"].as_array().into_iter().flatten();
            let mut stays = links.filter(|l| l["relation"] == "born_during").peekable();
            let known = stays.peek().is_some();
            let clear = stays.all(|l| {
                let id = l["event"].as_str().unwrap_or_default();
                let clear = |of: &PrivateItems| of.clear.contains(id);
                shown(l, &l["event"]) && (clear(self) || clear(others))
            });
            if !(known && clear) {
                for k in ["place", "place_from"] {
                    e[k].take();
                }
            }
        }
        let mut left_out = 0;
        if let Some(members) = e["members"].as_array_mut() {
            let all = members.len();
            members.retain(|m| shown(m, m.get("record").unwrap_or(&m["document"])));
            left_out = all - members.len();
        }
        if let Some(links) = e["links"].as_array_mut() {
            links.retain(|l| shown(l, &l["event"]));
        }
        if left_out > 0 {
            e["private_members"] = left_out.into();
        }
        if let Some(ids) = e["could_join"].as_array_mut() {
            ids.retain(|x| x.as_str().is_some_and(|id| !whole || !self.held_back(id)));
        }
        for list in ["links", "could_join"] {
            if e[list].as_array().is_some_and(Vec::is_empty) {
                if let Some(e) = e.as_object_mut() {
                    e.remove(list);
                }
            }
        }
        unknown
    }

    /// Marks an answer's private items with their area when the call is
    /// about one person or names the item (`open`); otherwise leaves them
    /// out, as it does what is entered in error. What is kept, and where
    /// those left out sat.
    /// A series of tests counts as private when any of its results does.
    fn keep(&self, items: Vec<Item>, open: impl Fn(&Item) -> bool) -> (Vec<Item>, views::LeftOut) {
        views::LeftOut::split(items, |i| {
            let ids = || std::iter::once(&i.id).chain(&i.members);
            let private = ids().any(|id| self.held_back(id));
            if private && !open(i) {
                return Some(views::Why::Private);
            }
            if self.paid_errors.contains(&i.id) {
                return Some(views::Why::PaidError);
            }
            if self.errors.contains(&i.id) {
                return Some(views::Why::Error);
            }
            if private {
                i.private = ids().find_map(|id| self.area(id));
            }
            None
        })
    }

    /// The labels of what an item prints, for an answer about the household
    /// (`whole`) or one person.
    fn labels(
        &self,
        labels: &Labels,
        item: &str,
        printed: &[Printed],
        whole: bool,
    ) -> Vec<Map<String, Value>> {
        let asked = Asked {
            whole,
            private: &|id| self.areas.contains_key(id),
            held_back: &|id| self.held_back(id),
        };
        labels.shown(item, printed, &asked)
    }

    /// The labels of the items a page can have, which count in its size.
    fn label(
        &self,
        f: &Folder,
        h: &Household,
        people: &[&crate::model::Person],
        page: &PageRequest,
        items: &mut [Item],
        whole: bool,
    ) {
        let start = page.offset().min(items.len());
        let end = start.saturating_add(page.limit()).min(items.len());
        let items = &mut items[start..end];
        for p in people {
            let theirs = |i: &&mut Item| i.person_id == p.id && !i.printed.is_empty();
            if !items.iter_mut().any(|i| theirs(&i)) {
                continue;
            }
            let labels = Labels::read(f, h, p);
            for i in items.iter_mut().filter(theirs) {
                i.provider_labels = self.labels(&labels, &i.id, &i.printed, whole);
                i.fit_labels();
            }
        }
    }

    /// Which other people's parts a page's events are without, since what is
    /// private of theirs does not read.
    fn warn_of_parts<T>(&self, page: &mut Page<T>, people: &BTreeSet<String>) {
        for (_, name, what) in self.unreadable.iter().filter(|(p, ..)| people.contains(p)) {
            let (code, file) = match what {
                Unreadable::Marks => ("private-marks-unreadable", "marks.json"),
                Unreadable::Labels => ("private-labels-unreadable", "provider-state.json"),
            };
            page.warn(
                code,
                format!(
                    "Which of {name}'s items are private is not known (their {file} does not read): the events on this page leave out {name}'s documents and events. Ask about {name} for them"
                ),
            );
        }
    }

    /// Says on a page, starting at `offset`, how many items it left out,
    /// and whose marks did not read.
    fn warn<T>(&self, page: &mut Page<T>, offset: usize, left_out: &views::LeftOut) {
        let (returned, last) = (page.info.returned, page.info.next_cursor.is_none());
        let n = left_out.on_page(views::Why::Error, offset, returned, last);
        let paid = left_out.on_page(views::Why::PaidError, offset, returned, last);
        if n + paid > 0 {
            let line = match self.bills {
                true => crate::bills::in_error_line(n + paid, paid),
                false => standing::in_error(n),
            };
            page.warn("entered-in-error", line);
        }
        let n = left_out.on_page(views::Why::Private, offset, returned, last);
        if n > 0 {
            page.warn(
                "private-left-out",
                format!(
                    "{n} private item{} left out of this page of a household-wide answer: ask about one person, or name the item, to include {}",
                    if n == 1 { "" } else { "s" },
                    if n == 1 { "it" } else { "them" },
                ),
            );
        }
        for (_, name, what) in &self.unreadable {
            let (code, message) = match what {
                Unreadable::Marks => (
                    "private-marks-unreadable",
                    format!(
                        "{name}'s marks.json does not read, so the household's private marks are not applied: {name}'s items are left out of household-wide answers until it is fixed"
                    ),
                ),
                Unreadable::Labels => (
                    "private-labels-unreadable",
                    format!(
                        "{name}'s provider-state.json does not read, so which items provider labels make private is not known: {name}'s items are left out of household-wide answers until medrec tables fetch nppes, then medrec views write, run on one machine (a newer medrec's file needs this one updated)"
                    ),
                ),
            };
            page.warn(code, message);
        }
    }
}

/// Characters of JSON one item of a page may have: the page's, less its
/// brackets and the item's comma.
const ITEM_ROOM: usize = SIZE_BUDGET - ENVELOPE_RESERVE - 3;

impl Item {
    /// A label yields to the record: an item that fits a page without its
    /// labels is never too large for one with them. It keeps the first that
    /// fit.
    fn fit_labels(&mut self) {
        let had = self.provider_labels.len();
        let size = |i: &Item| serde_json::to_string(i).map_or(0, |j| j.chars().count());
        while !self.provider_labels.is_empty() && size(self) > ITEM_ROOM {
            self.provider_labels.pop();
        }
        self.labels_left_out = self.provider_labels.len() < had;
    }
}

/// Says on a page how many of its items' labels did not fit.
fn warn_labels(page: &mut Page<Item>) {
    let n = page.items.iter().filter(|i| i.labels_left_out).count();
    if n > 0 {
        page.warn(
            "provider-labels-left-out",
            format!(
                "{n} item{} on this page: some of the provider_labels did not fit and are left out, the rest of it is whole",
                if n == 1 { "" } else { "s" },
            ),
        );
    }
}

/// The people an event shows a paper or an event of beside its own.
fn others_of(e: &Value) -> impl Iterator<Item = &str> {
    ["members", "links"]
        .into_iter()
        .filter_map(|k| e[k].as_array())
        .flatten()
        .filter_map(|x| x["person"].as_str())
}

/// An `events.json` event as a list item: `source` is where it happened,
/// and `detail` what it was, who, how sure the grouping is and what is
/// still expected. `detailed` adds its money, members and evidence in
/// `fields`, and its members' labels.
fn event_item(e: &Value, detailed: bool) -> Item {
    let s = |k: &str| e[k].as_str().map(str::to_string);
    let mut parts: Vec<String> = Vec::new();
    if let Some(w) = s("what") {
        parts.push(w);
    }
    let clinicians: Vec<&str> = e["clinicians"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    if !clinicians.is_empty() {
        parts.push(clinicians.join(", "));
    }
    if let Some(c) = s("confidence").filter(|c| c != "sure") {
        parts.push(format!("grouping {c}"));
    }
    let expected: Vec<&str> = e["expected"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|x| x["what"].as_str())
        .map(|w| match w {
            "doctor_claims" => "doctors' claims",
            "facility_claim" => "the facility's claim",
            "claim" => "a claim",
            "bill" => "a bill",
            other => other,
        })
        .collect();
    if !expected.is_empty() {
        parts.push(format!("still expected: {}", expected.join(", ")));
    }
    match e["in_error"].as_array().map_or(0, Vec::len) {
        0 => {}
        n => parts.push(standing::in_error(n)),
    }
    let fields = detailed.then(|| {
        [
            "confidence",
            "money",
            "members",
            "private_members",
            "expected",
            "differences",
            "links",
            "could_join",
            "in_error",
            "not_applied",
        ]
        .into_iter()
        .filter_map(|k| {
            e.get(k)
                .filter(|v| !v.is_null())
                .map(|v| (k.to_string(), v.clone()))
        })
        .collect()
    });
    Item {
        kind: s("kind").unwrap_or_default(),
        person: s("person_name").unwrap_or_default(),
        date: s("start"),
        end: s("end").filter(|end| Some(end) != s("start").as_ref()),
        title: s("title").unwrap_or_default(),
        detail: (!parts.is_empty()).then(|| clip(&parts.join("; "), CLIP)),
        status: s("status"),
        standing: (e["entered_in_error"] == true).then(|| "entered in error".into()),
        source: s("place"),
        id: s("id").unwrap_or_default(),
        you_owe: e["money"]["you_owe"]
            .as_str()
            .map(|o| format!("{o} {}", e["money"]["currency"].as_str().unwrap_or("USD"))),
        fields,
        person_id: s("person").unwrap_or_default(),
        printed: Printed::of_event(e, detailed),
        ..Default::default()
    }
}

fn money(m: &crate::model::Amount) -> String {
    format!("{} {}", m.amount, m.currency)
}

impl MedrecTools {
    fn summary(&self, a: SummaryArgs) -> Result<ToolOutput, ToolError> {
        let (f, h) = self.open()?;
        let people = summary::people(&f, &h)?;
        let summary = match given(&a.person) {
            None => None,
            Some(q) => {
                let p = people::resolve(&h, q).map_err(person_error)?;
                // An answer that names the person leaves nothing out of a
                // claim (`Parts::named`), nor the vaccine a line tells of.
                let s = summary::person(
                    &f,
                    &h,
                    p,
                    &today(),
                    summary::Limits {
                        per_list: SUMMARY_LINES,
                    },
                    &self.codes(),
                    &crate::parts::Parts::default(),
                )?;
                let mut s = sections(s);
                let private = PrivateItems::of(&f, &h, &[p])?;
                let names = Names::read(&f, &h, p);
                for line in s.sections.iter_mut().flat_map(|s| s.items.iter_mut()) {
                    line.private = line.id.as_deref().and_then(|id| private.area(id));
                    line.plain_name = line
                        .name_key
                        .as_deref()
                        .and_then(|k| names.name(k, false, line.private.is_some()))
                        .map(str::to_string);
                }
                Some(s)
            }
        };
        ToolOutput::new(&SummaryOut { people, summary })
    }

    fn search(&self, a: SearchArgs) -> Result<ToolOutput, ToolError> {
        let person = given(&a.person);
        let r = range(&a.since, &a.until)?;
        let page = PageRequest::new(
            &a.list,
            &SearchFilter {
                query: a.query.trim(),
                person,
                since: r.since.as_deref(),
                until: r.until.as_deref(),
            },
        )?;
        let (f, h) = self.open()?;
        if let Some(q) = person {
            people::resolve(&h, q).map_err(person_error)?;
        }
        let mut items = PrivateItems::of(&f, &h, &people::selected(&h, person)?)?;
        items.parts.named(person);
        // Household-wide, a private hit is left out unless the query names it.
        let leave_out = |x: &views::Hit| {
            let private = person.is_none()
                && items.held_back(&x.id)
                && !crate::private::names(&x.title, &a.query);
            match private {
                true => Some(views::Why::Private),
                false => items.errors.contains(&x.id).then_some(views::Why::Error),
            }
        };
        let (hits, left_out) = views::search_leaving_out(
            &f,
            &h,
            person,
            &a.query,
            &r,
            page.wanted(),
            &items.parts,
            &leave_out,
        )?;
        let private = items;
        let items = hits
            .into_iter()
            .map(|x| SearchItem {
                private: private.area(&x.id),
                not_shown: private.parts.says(&x.id).map(str::to_string),
                person: x.person_name,
                kind: x.kind,
                id: x.id,
                date: x.date,
                title: clip(&x.title, CLIP),
                snippet: x.snippet,
            })
            .collect();
        let mut out = page.page(items, "person, since/until, or more words")?;
        private.warn(&mut out, page.offset(), &left_out);
        ToolOutput::new(&out)
    }

    fn records(&self, a: RecordsArgs) -> Result<ToolOutput, ToolError> {
        let person = given(&a.person);
        let name = given(&a.name);
        let doc_type = given(&a.doc_type);
        let active = a.active.unwrap_or(false);
        let r = range(&a.since, &a.until)?;
        let id = given(&a.id);
        if id.is_some() && a.kind != Kind::Events {
            return Err(ToolError::usage("id applies to kind `events` only"));
        }
        if active && a.kind == Kind::Events {
            return Err(ToolError::usage(
                "active does not apply to kind `events`; status says which are still waiting",
            ));
        }
        if let Some(t) = doc_type {
            if a.kind != Kind::Documents {
                return Err(ToolError::usage(
                    "doc_type applies to kind `documents` only",
                ));
            }
            if !classify::is_doc_type(t) {
                let known: Vec<&str> = classify::DOC_TYPES.iter().map(|(c, _, _)| *c).collect();
                return Err(ToolError::usage(format!(
                    "`{t}` is not a document type (known: {})",
                    known.join(", ")
                )));
            }
        }
        let page = PageRequest::new(
            &a.list,
            &RecordsFilter {
                kind: a.kind,
                person,
                since: r.since.as_deref(),
                until: r.until.as_deref(),
                active,
                name,
                doc_type,
                id,
            },
        )?;
        let detailed = a.list.format().is_detailed();
        if a.kind == Kind::Events {
            return self.events(person, name, &r, id, detailed, page);
        }
        let (f, h) = self.open()?;
        let selected = people::selected(&h, person).map_err(person_error)?;
        let named =
            |title: &str| name.is_none_or(|n| classify::fold(title).contains(&classify::fold(n)));
        let today = today();
        let mut items: Vec<Item> = Vec::new();
        let mut private = PrivateItems::of(&f, &h, &selected)?;
        private.parts.named(person);
        let open =
            |title: &str| person.is_some() || name.is_some_and(|n| crate::private::names(title, n));
        let mut notes: Vec<String> = Vec::new();
        match (a.kind, a.kind.record_kind()) {
            (Kind::Tests, _) => {
                for p in &selected {
                    let idx = f.index(p)?;
                    let mut results: Vec<&Record> = idx
                        .records
                        .iter()
                        .filter(|x| x.kind == "result" && r.holds(x.date.as_deref()))
                        .filter(|x| !private.errors.contains(&x.id) && !panels::heads(x))
                        .filter(|x| name.is_none_or(|n| views::matches_code(x, n)))
                        .collect();
                    results.sort_by(|a, b| b.date.cmp(&a.date).then(a.display.cmp(&b.display)));
                    let mut series: std::collections::BTreeMap<String, Vec<String>> =
                        Default::default();
                    for x in &results {
                        series
                            .entry(summary::series_key(x))
                            .or_default()
                            .push(x.id.clone());
                    }
                    for row in summary::latest_results(Sources::of(&h.sources, &idx), &results) {
                        let members = results
                            .iter()
                            .find(|x| Some(x.id.as_str()) == row.id.as_deref())
                            .and_then(|x| series.get(&summary::series_key(x)))
                            .cloned()
                            .unwrap_or_default();
                        let mut item = result_item(&p.name, row);
                        item.members = members;
                        items.push(item);
                    }
                }
            }
            (_, Some(kind)) => {
                for p in &selected {
                    let mut idx = f.index(p)?;
                    private.parts.index(&mut idx);
                    let from = Sources::of(&h.sources, &idx);
                    let asked = |x: &Record| {
                        x.kind == kind
                            && r.holds(x.date.as_deref())
                            && name.is_none_or(|n| views::matches_code(x, n))
                    };
                    let item = |x: &Record| {
                        let mut item = record_item(from, &p.name, x.clone(), detailed);
                        item.person_id = p.id.clone();
                        item
                    };
                    if !standing::KINDS.contains(&kind) {
                        // The vaccines claims tell of are immunizations too.
                        let claimed = match kind {
                            "immunization" => {
                                // What an answer leaves out of a claim is out
                                // of `idx` already.
                                let all: Vec<&Record> = idx.records.iter().collect();
                                let whole = crate::parts::Parts::default();
                                crate::vaccines::of_claims(&all, &self.codes(), &whole)
                            }
                            _ => Vec::new(),
                        };
                        items.extend(
                            idx.records
                                .iter()
                                .chain(&claimed)
                                .filter(|x| asked(x))
                                .filter(|x| {
                                    !active
                                        || !is_ended(x.status.as_deref())
                                        || private.errors.contains(&x.id)
                                })
                                .map(item),
                        );
                        continue;
                    }
                    let st = Standings::of(&h.sources, p, &idx);
                    // A private one counts on a line of an answer about the
                    // whole household only where the answer lists it.
                    let listed = |x: &Record| asked(x) && open(&clip(&x.display, CLIP));
                    let shows =
                        |x: &Record| person.is_some() || !private.held_back(&x.id) || listed(x);
                    let list = st.list(&idx, kind, active, &today, &asked, &shows);
                    items.extend(list.rows.into_iter().map(|row| {
                        let mut one = item(row.record);
                        one.group = row.group;
                        if row.standing.is_some() {
                            // Its words say its status, each source's.
                            one.status = None;
                            one.standing = row.standing;
                            one.says = joined(row.says);
                        }
                        one
                    }));
                    notes.extend(list.notes.into_iter().map(|n| match person {
                        Some(_) => n,
                        None => format!("{}: {n}", crate::printable(&p.name)),
                    }));
                }
            }
            (Kind::Notes, None) => {
                for n in views::notes(&f, &h, person, None, &r)? {
                    let text = n.note.text.trim();
                    if !named(text) {
                        continue;
                    }
                    items.push(Item {
                        kind: "note".into(),
                        person: n.person_name.clone(),
                        date: Some(n.note.date.clone()),
                        title: first_line(text, 120),
                        detail: Some(clip(text, NOTE_CLIP)),
                        source: n.note.author.clone(),
                        id: n.note.id.clone(),
                        ..Default::default()
                    });
                }
            }
            (Kind::Timeline, None) => {
                let mut claims = std::collections::BTreeMap::new();
                for p in &selected {
                    let of = |x: &Record| (x.id.clone(), Printed::of_claim(&x.fields));
                    let idx = f.index(p)?;
                    claims.extend(idx.records.iter().filter(|x| x.kind == "claim").map(of));
                }
                for e in views::everything(&f, &h, person, &r, &[], &private.parts)? {
                    if !named(&e.title) {
                        continue;
                    }
                    let printed = match e.kind.as_str() {
                        "document" => Printed::of_document(e.source.as_deref()),
                        _ => claims.remove(&e.id).unwrap_or_default(),
                    };
                    items.push(Item {
                        person_id: e.person,
                        printed,
                        kind: e.kind,
                        person: e.person_name,
                        date: e.date,
                        title: clip(&e.title, CLIP),
                        detail: e.detail.map(|d| clip(&d, CLIP)),
                        source: e.source,
                        id: e.id,
                        ..Default::default()
                    });
                }
            }
            (_, None) => {
                let bills = a.kind == Kind::Bills;
                private.bills = bills;
                for p in &selected {
                    // What is paid and left on each bill (`bills.json`).
                    let of_bills = match bills {
                        true => crate::bills::of_person(&f, &h, p)?,
                        false => Value::Null,
                    };
                    let standing: std::collections::BTreeMap<String, (bool, Option<String>)> =
                        of_bills["bills"]
                            .as_object()
                            .into_iter()
                            .flatten()
                            .map(|(id, b)| {
                                (
                                    id.clone(),
                                    (b["settled"] == true, b["left"].as_str().map(str::to_string)),
                                )
                            })
                            .collect();
                    let lost = crate::bills::lost(&of_bills);
                    private.paid_errors.extend(
                        lost.iter()
                            .filter(|(_, paid)| **paid)
                            .map(|(id, _)| id.to_string()),
                    );
                    let mut idx = f.index(p)?;
                    let documents = std::mem::take(&mut idx.documents);
                    let from = Sources::of(&h.sources, &idx);
                    for d in documents {
                        // A bill is there by the first of its papers that
                        // stands, else to be counted.
                        let is_bill =
                            standing.contains_key(&d.id) || lost.contains_key(d.id.as_str());
                        if (bills && !is_bill)
                            || doc_type.is_some_and(|t| d.doc_type.as_deref() != Some(t))
                            || !r.holds(d.date.as_deref())
                        {
                            continue;
                        }
                        let (differ, says) =
                            standing::differ(&match private.errors.contains(&d.id) {
                                true => Vec::new(),
                                false => standing::document_says(from, &d),
                            });
                        let title = d
                            .title
                            .clone()
                            .or_else(|| d.original_name.clone())
                            .unwrap_or_else(|| label(d.doc_type.as_deref()).to_string());
                        if !named(&title) {
                            continue;
                        }
                        let s = d.statement.as_ref();
                        let due = s.and_then(|s| s.due_date.clone());
                        let (settled, left) = standing.get(&d.id).cloned().unwrap_or((false, None));
                        let past_due =
                            !settled && due.as_deref().is_some_and(|d| d < today.as_str());
                        let mut detail = label(d.doc_type.as_deref()).to_string();
                        if past_due {
                            detail.push_str(", past due");
                        }
                        if settled {
                            detail.push_str(", paid");
                        } else if let Some(left) = left {
                            let paid_some = s
                                .and_then(|s| s.amount_due.as_ref())
                                .is_some_and(|a| a.amount != left);
                            if paid_some {
                                detail.push_str(&format!(", {left} left"));
                            }
                        }
                        items.push(Item {
                            kind: "document".into(),
                            person: p.name.clone(),
                            date: d.date.clone(),
                            title: clip(&title, CLIP),
                            detail: Some(detail),
                            source: d.organization.clone(),
                            id: d.id.clone(),
                            amount_due: s.and_then(|s| s.amount_due.as_ref()).map(money),
                            due_date: due,
                            doc_type: d.doc_type.clone(),
                            has_text: Some(d.text.as_ref().is_some_and(|t| t.chars > 0)),
                            person_id: p.id.clone(),
                            printed: Printed::of_document(d.organization.as_deref()),
                            standing: differ,
                            says: joined(says),
                            ..Default::default()
                        });
                    }
                }
            }
        }
        items.sort_by(|a, b| {
            let date = || b.date.cmp(&a.date).then(a.title.cmp(&b.title));
            a.group.cmp(&b.group).then_with(date)
        });
        let (mut items, left_out) = private.keep(items, |i| open(&i.title));
        // A document's row shows none of its text.
        for i in items.iter_mut().filter(|i| i.kind != "document") {
            i.not_shown = private.parts.says(&i.id).map(str::to_string);
        }
        if person.is_none() {
            for fields in items.iter_mut().filter_map(|i| i.fields.as_mut()) {
                fields.retain(|_, v| private.without_held(v));
            }
        }
        private.label(&f, &h, &selected, &page, &mut items, person.is_none());
        let mut out = page.page(items, "person, since/until, active or name")?;
        warn_labels(&mut out);
        // Only for the people on the page, and after the private items are
        // out: what is kept is decided from the source's own words.
        if a.kind == Kind::Conditions {
            for p in &selected {
                if out.items.iter().any(|i| i.person_id == p.id) {
                    let names = Names::read(&f, &h, p);
                    for i in out.items.iter_mut().filter(|i| i.person_id == p.id) {
                        i.plain_name = i
                            .name_key
                            .as_deref()
                            .and_then(|k| names.name(k, person.is_none(), i.private.is_some()))
                            .map(str::to_string);
                    }
                }
            }
        }
        for note in notes {
            out.warn("no-problem-list", note);
        }
        let lists = matches!(
            a.kind,
            Kind::Results | Kind::Tests | Kind::Timeline | Kind::Documents
        );
        if lists && page.offset() == 0 {
            if let Some(line) = views::about_another(&f, &h, person)? {
                out.warn("about-another", line);
            }
        }
        private.warn(&mut out, page.offset(), &left_out);
        ToolOutput::new(&out)
    }

    /// `kind: "events"`: what happened, in the order of `events.json`;
    /// `id` one event with its money, members and evidence.
    fn events(
        &self,
        person: Option<&str>,
        name: Option<&str>,
        r: &Range,
        id: Option<&str>,
        detailed: bool,
        page: PageRequest,
    ) -> Result<ToolOutput, ToolError> {
        if let Some(id) = id {
            if person.is_some() || name.is_some() || r.since.is_some() || r.until.is_some() {
                return Err(ToolError::usage(
                    "id names one event: pass it without person, name, since or until",
                ));
            }
            if !crate::ids::is_valid("e", id) {
                return Err(ToolError::usage(format!("`{id}` is not an event id (e_…)")));
            }
        }
        let (f, h) = self.open()?;
        let selected = people::selected(&h, person).map_err(person_error)?;
        let fold = name.map(classify::fold);
        let mut private = PrivateItems::of(&f, &h, &selected)?;
        private.parts.named(person);
        let mut events = crate::events::list(&f, &h, person, r)?;
        for e in &mut events {
            private.parts.event(e);
            if e["entered_in_error"] == true && id.is_none() {
                private.errors.extend(e["id"].as_str().map(str::to_string));
            }
            // What an answer could not show it does not count or name. One that
            // is private itself has all of its own.
            let whole = e["id"].as_str();
            let whole = whole.is_some_and(|id| private.areas.contains_key(id));
            if person.is_none() && !whole {
                let shown = |x: &Value| x.as_str().is_some_and(|x| !private.held_back(x));
                if let Some(ids) = e["in_error"].as_array_mut() {
                    ids.retain(shown);
                }
                if let Some(decisions) = e["not_applied"].as_array_mut() {
                    decisions.retain(|d| {
                        let members = d["members"].as_array().into_iter().flatten();
                        members.into_iter().all(shown)
                    });
                }
                for list in ["in_error", "not_applied"] {
                    if e[list].as_array().is_some_and(Vec::is_empty) {
                        e[list].take();
                    }
                }
            }
        }
        // Those the events show a paper or an event of, judged as theirs.
        let others = {
            let shown: BTreeSet<&str> = events.iter().flat_map(others_of).collect();
            let others = h.people.iter().filter(|p| {
                shown.contains(p.id.as_str()) && !selected.iter().any(|s| s.id == p.id)
            });
            PrivateItems::of(&f, &h, &others.collect::<Vec<_>>())?
        };
        let mut unknown: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for e in &mut events {
            let of = private.members(&others, e, person.is_none());
            if let (false, Some(id)) = (of.is_empty(), e["id"].as_str()) {
                unknown.insert(id.to_string(), of);
            }
        }
        let items: Vec<Item> = events
            .iter()
            .filter(|e| id.is_none_or(|id| e["id"] == id))
            .filter(|e| {
                fold.as_ref().is_none_or(|n| {
                    ["kind", "title", "what", "place"]
                        .iter()
                        .filter_map(|k| e[*k].as_str())
                        .chain(
                            e["clinicians"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .filter_map(Value::as_str),
                        )
                        .any(|t| classify::fold(t).contains(n.as_str()))
                })
            })
            .map(|e| event_item(e, detailed || id.is_some()))
            .collect();
        if let (Some(id), true) = (id, items.is_empty()) {
            return Err(
                ToolError::from(CliError::NotFound(format!("no event {id}")))
                    .with_fix("List events with medrec_records kind events to find the id"),
            );
        }
        let (mut items, left_out) = private.keep(items, |i| {
            person.is_some()
                || id.is_some()
                || name.is_some_and(|n| {
                    [
                        Some(i.title.as_str()),
                        i.detail.as_deref(),
                        i.source.as_deref(),
                    ]
                    .into_iter()
                    .flatten()
                    .any(|t| crate::private::names(t, n))
                })
        });
        for i in &mut items {
            i.not_shown = private.parts.says(&i.id).map(str::to_string);
        }
        private.label(&f, &h, &selected, &page, &mut items, person.is_none());
        let mut out = page.page(items, "person, since/until or name")?;
        warn_labels(&mut out);
        private.warn(&mut out, page.offset(), &left_out);
        // A row without detail shows no member of it.
        if person.is_some() && detailed {
            let of = |i: &Item| unknown.get(&i.id).into_iter().flatten();
            let of: BTreeSet<String> = out.items.iter().flat_map(of).cloned().collect();
            others.warn_of_parts(&mut out, &of);
        }
        ToolOutput::new(&out)
    }

    fn document(&self, a: DocumentArgs) -> Result<ToolOutput, ToolError> {
        let id = a.id.trim();
        if a.page.is_some() && a.offset.is_some() {
            return Err(ToolError::usage("pass page or offset, not both"));
        }
        if a.page == Some(0) {
            return Err(ToolError::usage("pages count from 1"));
        }
        let (f, h) = self.open()?;
        let (p, d) = views::find_document(&f, &h, id, false).map_err(|e| {
            ToolError::from(e).with_fix("Find the id with medrec_search or medrec_records")
        })?;
        let mut private = PrivateItems::of(&f, &h, &[&p])?;
        let whole = given(&a.person).is_none();
        if let Some(q) = given(&a.person) {
            if people::resolve(&h, q).map_err(person_error)?.id != p.id {
                return Err(ToolError::usage(format!(
                    "{id} is a document of {}",
                    p.name
                )));
            }
            private.parts = Parts::default();
        }
        let text = f.text(&p)?.texts.remove(&d.id).unwrap_or_default();
        let text = private.parts.text(&d.id, &text);
        let chars: Vec<char> = text.chars().collect();
        let breaks: Vec<usize> = chars
            .iter()
            .enumerate()
            .filter(|(_, c)| **c == '\u{c}')
            .map(|(i, _)| i)
            .collect();
        let text_pages = breaks.len() as u32 + 1;
        let start = match (a.page, a.offset) {
            (Some(n), _) if n > text_pages => {
                return Err(ToolError::usage(format!(
                    "the text has {text_pages} pages; page {n} is past its end"
                )))
            }
            (Some(1), _) | (None, None) => 0,
            (Some(n), _) => breaks[n as usize - 2] + 1,
            (None, Some(o)) => usize::try_from(o).unwrap_or(usize::MAX).min(chars.len()),
        };
        // Asked for by id: a private document is the person's, named. Its
        // labels, and a private record's name, are in no answer that names no
        // person.
        let idx = f.index(&p)?;
        let records: Vec<String> = idx
            .records
            .iter()
            .filter(|r| r.documents.contains(&d.id) && !private.errors.contains(&r.id))
            .take(20)
            .filter(|r| !whole || private.area(&d.id).is_some() || !private.held_back(&r.id))
            .map(|r| clip(&private.parts.record(r).display, CLIP))
            .collect();
        let s = d.statement.as_ref();
        let printed = Printed::of_document(d.organization.as_deref());
        let provider_labels = match printed.is_empty() {
            true => Vec::new(),
            false => private.labels(&Labels::read(&f, &h, &p), &d.id, &printed, whole),
        };
        let note = views::paper_note(&f, &h, &p, &d)?;
        let left_out = private.parts.says(&d.id);
        let left_out = left_out.map(|n| format!("{n}: pass {} as person for all of it.", p.name));
        let hint = |text: Option<String>| {
            let both = [note.clone(), left_out.clone(), text];
            let both: Vec<String> = both.into_iter().flatten().collect();
            (!both.is_empty()).then(|| both.join(" "))
        };
        let mut out = DocumentOut {
            id: d.id.clone(),
            person: p.name.clone(),
            title: d.title.clone().or(d.original_name.clone()),
            date: d.date.clone(),
            doc_type: d.doc_type.clone(),
            organization: d.organization.clone(),
            text_chars: chars.len(),
            offset: start,
            first_page: 1,
            last_page: 1,
            text_pages,
            records,
            private: private.area(&d.id),
            provider_labels,
            ..Default::default()
        };
        // A field on a line left out is left out with it.
        let places = private.parts.places(&d.id);
        let shown = |lines: &[crate::parts::Place]| !crate::paper::left_out(lines, &places);
        let due = s.and_then(|s| s.amount_due.as_ref()).map(money);
        out.fields
            .extend(due_line(due, s.and_then(|s| s.due_date.as_deref())));
        if let Some(fields) = &d.fields {
            let follow = fields.follow_up.iter().filter(|x| shown(&x.lines));
            out.fields.extend(follow.map(follow_up_line));
            let drugs = fields.drugs.iter().filter(|x| shown(&x.lines));
            out.fields.extend(drugs.map(|x| {
                let medicine = crate::paper::medicine(x, &idx.records);
                let medicine = medicine.filter(|r| !whole || !private.held_back(&r.id));
                drug_line(x, medicine.map(|r| r.id.as_str()))
            }));
        }
        if chars.is_empty() {
            out.hint = hint(Some(
                "No text was extracted from this document: an image without OCR, or a file \
                 medrec cannot read."
                    .into(),
            ));
            return ToolOutput::new(&out);
        }
        let page_of = |i: usize| breaks.iter().filter(|b| **b < i).count() as u32 + 1;
        let mut chunk = TEXT_CHUNK;
        loop {
            let end = cut_at(&chars, start, chunk);
            out.first_page = page_of(start);
            out.last_page = page_of(end.saturating_sub(1).max(start));
            out.text = marked(&chars[start..end], out.first_page);
            out.next_offset = (end < chars.len()).then_some(end);
            let more = |n| format!("More text follows: pass offset {n} for the next part.");
            out.hint = hint(out.next_offset.map(more));
            let size = serde_json::to_string(&out)
                .map(|s| s.chars().count())
                .unwrap_or(0);
            if size <= DOCUMENT_BUDGET || chunk <= 1_000 {
                break;
            }
            chunk = (chunk * DOCUMENT_BUDGET / size * 9 / 10).max(1_000);
        }
        ToolOutput::new(&out)
    }
}

/// What each source says, as one text.
fn joined(says: Vec<String>) -> Option<String> {
    (!says.is_empty()).then(|| says.join("; "))
}

/// A summary as sections of short lines.
fn sections(s: summary::Summary) -> PersonSummary {
    let mut out: Vec<Section> = Vec::new();
    let mut section = |name: &str, more: usize, items: Vec<Line>| {
        out.push(Section {
            section: name.into(),
            items,
            more,
            note: None,
        })
    };
    for (name, list, always) in [
        ("medications", s.medications, true),
        ("medications_on_hold", s.on_hold, false),
        ("medications_ended_by_date", s.ended_by_date, false),
        ("conditions", s.conditions, true),
        ("allergies", s.allergies, true),
    ] {
        if always || !list.items.is_empty() {
            section(name, list.more, list.items.into_iter().map(line).collect());
        }
    }
    section(
        "immunizations",
        s.immunizations.more,
        s.immunizations
            .items
            .into_iter()
            .map(|v| Line {
                name: v.vaccine,
                date: v.last_date,
                detail: Some(match v.from_claim {
                    true => "from a claim".into(),
                    false => format!(
                        "{} dose{} on file",
                        v.doses,
                        if v.doses == 1 { "" } else { "s" }
                    ),
                }),
                source: v.source,
                id: v.id,
                ..Line::default()
            })
            .collect(),
    );
    section(
        "latest_results",
        s.latest_results.more,
        s.latest_results
            .items
            .into_iter()
            .map(|r| Line {
                detail: Some(result_text(&r)),
                name: r.test,
                date: day(r.date.as_deref()),
                source: r.source,
                id: r.id,
                ..Line::default()
            })
            .collect(),
    );
    section(
        "recent_visits",
        s.recent_visits.more,
        s.recent_visits.items.into_iter().map(line).collect(),
    );
    section(
        "bills",
        s.bills.more,
        s.bills
            .items
            .into_iter()
            .map(|b| Line {
                name: b.title.unwrap_or_else(|| "Bill".into()),
                date: b.statement_date,
                detail: Some(format!(
                    "{} due{}{}",
                    b.amount_due,
                    b.due_date.map(|d| format!(" {d}")).unwrap_or_default(),
                    if b.past_due { ", past due" } else { "" }
                )),
                source: b.organization,
                id: Some(b.document),
                ..Line::default()
            })
            .collect(),
    );
    for (name, note) in s.notes {
        if let Some(of) = out.iter_mut().find(|x| x.section == name) {
            of.note = Some(match of.note.take() {
                Some(had) => format!("{had}. {note}"),
                None => note,
            });
        }
    }
    PersonSummary {
        person: s.person.name,
        as_of: s.as_of,
        counts: s.counts,
        sources: s.sources,
        sections: out,
    }
}

fn line(l: summary::Line) -> Line {
    // Its words say its status.
    let status = l.status.filter(|_| l.standing.is_none());
    let detail = match (l.detail, status) {
        (Some(d), Some(s)) => Some(format!("{d} ({s})")),
        (d, s) => d.or(s),
    };
    Line {
        name: l.name,
        date: l.date,
        detail,
        standing: l.standing,
        says: joined(l.says),
        source: l.source,
        id: l.id,
        name_key: l.name_key,
        ..Line::default()
    }
}

/// `5.4 % (H), range 4.0 – 5.6 %, 3 results since 2024-01-02`.
fn result_text(r: &ResultRow) -> String {
    let mut s = [r.value.as_deref(), r.unit.as_deref()]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ");
    if let Some(f) = &r.flag {
        s.push_str(&format!(" ({f})"));
    }
    if let Some(range) = &r.range {
        s.push_str(&format!(", range {range}"));
    }
    if let (Some(n), Some(first)) = (r.results, &r.first_date) {
        if n > 1 {
            s.push_str(&format!(", {n} results since {first}"));
        }
    }
    s.trim_start_matches(", ").trim().to_string()
}

fn result_item(person: &str, r: ResultRow) -> Item {
    Item {
        kind: "result".into(),
        person: person.to_string(),
        date: r.date,
        title: r.test,
        source: r.source,
        id: r.id.unwrap_or_default(),
        code: r.code,
        value: r.value,
        unit: r.unit,
        range: r.range,
        flag: r.flag,
        results: r.results,
        ..Default::default()
    }
}

/// Where a part of at most `max` characters from `start` ends: at a page
/// break or a line end in its second half when it does not reach the end.
fn cut_at(chars: &[char], start: usize, max: usize) -> usize {
    let end = (start + max).min(chars.len());
    if end == chars.len() {
        return end;
    }
    let floor = start + max / 2;
    if let Some(i) = (floor..end).rev().find(|&i| chars[i] == '\u{c}') {
        return i;
    }
    if let Some(i) = (floor..end).rev().find(|&i| chars[i] == '\n') {
        return i + 1;
    }
    end
}

/// `chars` with each page break shown as `--- page N ---`, the first part
/// being on page `first`.
fn marked(chars: &[char], first: u32) -> String {
    let mut out = String::with_capacity(chars.len());
    let mut page = first;
    for &c in chars {
        if c == '\u{c}' {
            page += 1;
            out.push_str(&format!("--- page {page} ---"));
        } else {
            out.push(c);
        }
    }
    out
}

fn record_item(from: Sources, person: &str, r: Record, detailed: bool) -> Item {
    let (standing, says) = standing::differ(&standing::record_says(from, &r));
    let mut item = Item {
        standing,
        says: joined(says),
        kind: r.kind.clone(),
        person: person.to_string(),
        date: if detailed {
            r.date.clone()
        } else {
            day(r.date.as_deref())
        },
        title: clip(&r.display, CLIP),
        detail: record_detail(&r)
            .filter(|d| Some(d) != r.status.as_ref())
            .map(|d| clip(&d, CLIP)),
        status: r.status.clone(),
        source: record_source(from, &r),
        code: r.code.as_ref().and_then(|c| c.code.clone()),
        name_key: crate::code_names::condition_key(&r),
        printed: match (detailed, r.kind.as_str()) {
            (true, _) => Printed::of_record(&r.fields),
            (false, "claim") => Printed::of_claim(&r.fields),
            (false, _) => Vec::new(),
        },
        fields: detailed.then(|| r.fields.clone()),
        ..Default::default()
    };
    match r.kind.as_str() {
        "result" => {
            let row = summary::result_row(from, &r);
            item.value = row.value;
            item.unit = row.unit;
            item.range = row.range;
            item.flag = row.flag;
            // The value and the flag are in their own fields.
            item.detail = panels::heads(&r).then(|| panels::HEADING.to_string());
        }
        "claim" => {
            if let Some(owes) = member_owes(&r.fields) {
                item.detail = Some(match item.detail {
                    Some(d) => format!("{d}, member owes {owes}"),
                    None => format!("member owes {owes}"),
                });
            }
        }
        _ => {}
    }
    item.id = r.id;
    item
}

/// What a claim imported by an earlier medrec, with no `amounts`, leaves the
/// member to pay: its total named for the member's share. A claim with
/// `amounts` says it in its detail already, as "you owe" for the payer's
/// own figure or "your share" for medrec's sum of its parts.
fn member_owes(fields: &Map<String, Value>) -> Option<String> {
    if fields
        .get("amounts")
        .and_then(Value::as_object)
        .is_some_and(|a| !a.is_empty())
    {
        return None;
    }
    fields
        .get("total")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|t| {
            let cat: String = t
                .get("category")
                .map(views::value_text)
                .unwrap_or_default()
                .to_lowercase()
                .chars()
                .filter(|c| c.is_alphanumeric())
                .collect();
            ["memberliability", "patientpay", "patientresponsibility"]
                .iter()
                .any(|w| cat.contains(w))
        })
        .and_then(|t| t.get("amount"))
        .map(views::value_text)
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_part_ends_at_a_page_break_and_marks_the_pages_it_crosses() {
        let text: Vec<char> = "one\n\u{c}\ntwo two\n\u{c}\nthree".chars().collect();
        assert_eq!(cut_at(&text, 0, 1000), text.len());
        // A page break in the part's second half ends it, the break
        // starting the next part; failing that, a line end.
        let end = cut_at(&text, 0, 16);
        assert_eq!((end, text[end]), (14, '\u{c}'));
        assert_eq!(cut_at(&text, 0, 10), 6);
        assert_eq!(
            marked(&text, 1),
            "one\n--- page 2 ---\ntwo two\n--- page 3 ---\nthree"
        );
    }

    #[test]
    fn a_claims_member_share_comes_from_its_totals_by_category() {
        let fields: Map<String, Value> = serde_json::from_str(
            r#"{"total":[
                {"category":"Submitted Amount","amount":{"amount":"200.00","currency":"USD"}},
                {"category":"memberliability","amount":{"amount":"40.00","currency":"USD"}}]}"#,
        )
        .unwrap();
        assert_eq!(member_owes(&fields).as_deref(), Some("40.00 USD"));
        assert_eq!(member_owes(&Map::new()), None);
        // A claim with amounts says it in its own detail, labelled.
        let mut shaped = fields.clone();
        shaped.insert(
            "amounts".into(),
            serde_json::json!({"copay": {"amount": "30.00", "currency": "USD"}}),
        );
        assert_eq!(member_owes(&shaped), None);
    }
}

#[cfg(test)]
mod conformance {
    use super::*;
    use pk_cli_mcp::conformance::{assert_conforms, definitions_size};

    /// The Claude Desktop manifest names the same tools and every kind
    /// `medrec_records` takes, at this version.
    #[test]
    fn the_desktop_manifest_matches_the_tools() {
        let m: Value =
            serde_json::from_str(include_str!("../packaging/mcpb/manifest.json")).unwrap();
        assert_eq!(m["version"], env!("CARGO_PKG_VERSION"));
        let listed: Vec<&str> = m["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        let served: Vec<String> = MedrecTools::listing_only()
            .tools()
            .into_iter()
            .map(|t| t.name.to_string())
            .collect();
        assert_eq!(listed, served);
        let records = m["tools"][2]["description"].as_str().unwrap();
        let readme = include_str!("../README.md");
        for k in Kind::ALL {
            let name = k.name();
            assert!(records.contains(&name), "manifest lacks `{name}`");
            assert!(
                readme.contains(&format!("`{name}`")),
                "README lacks `{name}`"
            );
        }
    }

    #[test]
    fn the_tools_conform_to_the_mcp_profile() {
        let tools = MedrecTools::listing_only();
        assert_conforms(&tools);
        eprintln!("definitions: {} characters", definitions_size(&tools));
    }
}
