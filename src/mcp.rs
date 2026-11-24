//! `medrec mcp`: the records folder as four read-only MCP tools, for Claude
//! Desktop and Claude Code on this machine (pk-cli-mcp, the family's MCP
//! profile). Each tool reads through the views the commands use; none
//! writes to the folder or reaches any network but the folder's own: a
//! Drive folder is read through medrec's own Drive sign-in, from its vault.

use std::time::Duration;

use pk_cli_core::CliError;
use pk_cli_mcp::{
    parse_args, ListArgs, Page, PageRequest, Server, ToolDef, ToolError, ToolOutput, Toolset,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::config::Config;
use crate::folder::{Access, Folder};
use crate::intake::classify;
use crate::model::{today, Household, Record};
use crate::people;
use crate::refresh::signin;
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
}

impl MedrecTools {
    pub fn new(cfg: Config) -> Self {
        // A server nobody may be watching: a key another process replaced
        // is read again with the same bound as the first read.
        crate::store::bound_key_reads(signin::wait_or(CALL_BUDGET));
        MedrecTools {
            cfg: Some(cfg),
            keychain_read: Default::default(),
        }
    }

    /// For `mcp --list-tools`: no profile, and a call is refused.
    pub fn listing_only() -> Self {
        MedrecTools {
            cfg: None,
            keychain_read: Default::default(),
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
    /// A name, alias or id; omit for the people only.
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
    pub counts: std::collections::BTreeMap<String, usize>,
    pub sources: Vec<summary::SourceRow>,
    pub sections: Vec<Section>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Section {
    pub section: String,
    pub items: Vec<Line>,
    /// Lines left out for length.
    pub more: usize,
}

#[derive(Debug, Default, Serialize, JsonSchema)]
pub struct Line {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchArgs {
    /// Words that must all appear.
    pub query: String,
    /// A name, alias or id.
    #[serde(default)]
    pub person: Option<String>,
    /// YYYY-MM-DD, inclusive.
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
    /// `visits` are encounters; `tests` each test's latest result; `bills`
    /// statements filed as documents; `timeline` everything.
    pub kind: Kind,
    /// A name, alias or id.
    #[serde(default)]
    pub person: Option<String>,
    /// YYYY-MM-DD, inclusive.
    #[serde(default)]
    pub since: Option<String>,
    /// YYYY-MM-DD, inclusive.
    #[serde(default)]
    pub until: Option<String>,
    /// Current ones only.
    #[serde(default)]
    pub active: Option<bool>,
    /// A code (`4548-4`) or words (`a1c`).
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flag: Option<String>,
    /// For `tests`: results of the test on file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub results: Option<usize>,
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
    pub fields: Option<Map<String, Value>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DocumentArgs {
    /// The document's id.
    pub id: String,
    /// Start at this page (from 1).
    #[serde(default)]
    pub page: Option<u32>,
    /// Start at a previous answer's `next_offset`.
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub amount_due: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub due_date: Option<String>,
    pub text_chars: usize,
    pub offset: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_offset: Option<usize>,
    pub first_page: u32,
    pub last_page: u32,
    pub text_pages: u32,
    pub text: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub records: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
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
                 item per visit, ER trip, stay, test or fill, with the household's share. Give the date and source of each fact you use, and say when the records \
                 do not answer the question. Each result carries its lab's own reference range \
                 and flag, and labs may report one test in different units. For decisions about \
                 care, suggest the user ask their clinician.",
            )
    }

    fn tools(&self) -> Vec<ToolDef> {
        vec![
            ToolDef::read(
                SUMMARY,
                "Household and person summary",
                "Lists the household's people, and with person their summary. Use it first. \
                 A summary has current medications, conditions, allergies, immunizations, \
                 latest results, visits and bills due.",
            )
            .input::<SummaryArgs>()
            .output::<SummaryOut>(),
            ToolDef::read(
                SEARCH,
                "Search records",
                "Finds documents, records, studies and notes with every word of query, best \
                 first; labs or blood tests also finds every lab result. Use it for a named \
                 doctor, test, medicine or clinic. Filter by person and dates.",
            )
            .input::<SearchArgs>()
            .output::<Page<SearchItem>>(),
            ToolDef::read(
                RECORDS,
                "List records by kind",
                "Lists one kind of record, newest first. events: what happened, one per visit, \
                 stay or fill, with you_owe; id gives one in full. results with name: one test \
                 over time, with its range and flag.",
            )
            .input::<RecordsArgs>()
            .output::<Page<Item>>(),
            ToolDef::read(
                DOCUMENT,
                "Read a document's text",
                "Returns a filed document's extracted text, with its \
                 title, date, type and organization. Use it to read a visit summary, lab \
                 report, letter or bill. Long text comes in parts: pass next_offset as \
                 offset, or page to jump.",
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

/// An `events.json` event as a list item: `source` is where it happened,
/// and `detail` what it was, who, how sure the grouping is and what is
/// still expected. `detailed` adds its money, members and evidence in
/// `fields`.
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
    let fields = detailed.then(|| {
        [
            "confidence",
            "money",
            "members",
            "expected",
            "differences",
            "links",
            "could_join",
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
        source: s("place"),
        id: s("id").unwrap_or_default(),
        you_owe: e["money"]["you_owe"]
            .as_str()
            .map(|o| format!("{o} {}", e["money"]["currency"].as_str().unwrap_or("USD"))),
        fields,
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
                let s = summary::person(
                    &f,
                    &h,
                    p,
                    &today(),
                    summary::Limits {
                        per_list: SUMMARY_LINES,
                    },
                )?;
                Some(sections(s))
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
        let hits = views::search(&f, &h, person, &a.query, &r, page.wanted())?;
        let items = hits
            .into_iter()
            .map(|x| SearchItem {
                person: x.person_name,
                kind: x.kind,
                id: x.id,
                date: x.date,
                title: clip(&x.title, CLIP),
                snippet: x.snippet,
            })
            .collect();
        ToolOutput::new(&page.page(items, "person, since/until, or more words")?)
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
        match (a.kind, a.kind.record_kind()) {
            (Kind::Tests, _) => {
                for p in &selected {
                    let idx = f.index(p)?;
                    let mut results: Vec<&Record> = idx
                        .records
                        .iter()
                        .filter(|x| x.kind == "result" && r.holds(x.date.as_deref()))
                        .filter(|x| name.is_none_or(|n| views::matches_code(x, n)))
                        .collect();
                    results.sort_by(|a, b| b.date.cmp(&a.date).then(a.display.cmp(&b.display)));
                    for row in summary::latest_results(&h, &results) {
                        items.push(result_item(&p.name, row));
                    }
                }
            }
            (_, Some(kind)) => {
                for p in &selected {
                    for x in f.index(p)?.records {
                        if x.kind != kind
                            || !r.holds(x.date.as_deref())
                            || (active && is_ended(x.status.as_deref()))
                            || name.is_some_and(|n| !views::matches_code(&x, n))
                        {
                            continue;
                        }
                        items.push(record_item(&h, &p.name, x, detailed));
                    }
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
                for e in views::timeline(&f, &h, person, &r, &[])? {
                    if !named(&e.title) {
                        continue;
                    }
                    items.push(Item {
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
                for p in &selected {
                    for d in f.index(p)?.documents {
                        let is_bill = d.doc_type.as_deref() == Some("bill");
                        if (bills && (!is_bill || d.same_statement_as.is_some()))
                            || doc_type.is_some_and(|t| d.doc_type.as_deref() != Some(t))
                            || !r.holds(d.date.as_deref())
                        {
                            continue;
                        }
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
                        let past_due = due.as_deref().is_some_and(|d| d < today.as_str());
                        items.push(Item {
                            kind: "document".into(),
                            person: p.name.clone(),
                            date: d.date.clone(),
                            title: clip(&title, CLIP),
                            detail: Some(if past_due {
                                format!("{}, past due", label(d.doc_type.as_deref()))
                            } else {
                                label(d.doc_type.as_deref()).to_string()
                            }),
                            source: d.organization.clone(),
                            id: d.id.clone(),
                            amount_due: s.and_then(|s| s.amount_due.as_ref()).map(money),
                            due_date: due,
                            doc_type: d.doc_type.clone(),
                            has_text: Some(d.text.as_ref().is_some_and(|t| t.chars > 0)),
                            ..Default::default()
                        });
                    }
                }
            }
        }
        items.sort_by(|a, b| b.date.cmp(&a.date).then(a.title.cmp(&b.title)));
        ToolOutput::new(&page.page(items, "person, since/until, active or name")?)
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
        people::selected(&h, person).map_err(person_error)?;
        let fold = name.map(classify::fold);
        let items: Vec<Item> = crate::events::list(&f, &h, person, r)?
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
        ToolOutput::new(&page.page(items, "person, since/until or name")?)
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
        let (p, d) = views::find_document(&f, &h, id).map_err(|e| {
            ToolError::from(e).with_fix("Find the id with medrec_search or medrec_records")
        })?;
        let text = f.text(&p)?.texts.remove(&d.id).unwrap_or_default();
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
        let records: Vec<String> = f
            .index(&p)?
            .records
            .iter()
            .filter(|r| r.documents.contains(&d.id))
            .take(20)
            .map(|r| clip(&r.display, CLIP))
            .collect();
        let s = d.statement.as_ref();
        let mut out = DocumentOut {
            id: d.id.clone(),
            person: p.name.clone(),
            title: d.title.clone().or(d.original_name.clone()),
            date: d.date.clone(),
            doc_type: d.doc_type.clone(),
            organization: d.organization.clone(),
            amount_due: s.and_then(|s| s.amount_due.as_ref()).map(money),
            due_date: s.and_then(|s| s.due_date.clone()),
            text_chars: chars.len(),
            offset: start,
            first_page: 1,
            last_page: 1,
            text_pages,
            records,
            ..Default::default()
        };
        if chars.is_empty() {
            out.hint = Some(
                "No text was extracted from this document: an image without OCR, or a file \
                 medrec cannot read."
                    .into(),
            );
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
            out.hint = out
                .next_offset
                .map(|n| format!("More text follows: pass offset {n} for the next part."));
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

/// A summary as sections of short lines.
fn sections(s: summary::Summary) -> PersonSummary {
    let mut out = Vec::new();
    let mut section = |name: &str, more: usize, items: Vec<Line>| {
        out.push(Section {
            section: name.into(),
            items,
            more,
        })
    };
    for (name, list) in [
        ("medications", s.medications),
        ("conditions", s.conditions),
        ("allergies", s.allergies),
    ] {
        section(name, list.more, list.items.into_iter().map(line).collect());
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
                detail: Some(format!(
                    "{} dose{} on file",
                    v.doses,
                    if v.doses == 1 { "" } else { "s" }
                )),
                source: v.source,
                id: None,
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
                id: None,
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
            })
            .collect(),
    );
    PersonSummary {
        person: s.person.name,
        as_of: s.as_of,
        counts: s.counts,
        sources: s.sources,
        sections: out,
    }
}

fn line(l: summary::Line) -> Line {
    let detail = match (l.detail, l.status) {
        (Some(d), Some(s)) => Some(format!("{d} ({s})")),
        (d, s) => d.or(s),
    };
    Line {
        name: l.name,
        date: l.date,
        detail,
        source: l.source,
        id: None,
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

fn record_item(h: &Household, person: &str, r: Record, detailed: bool) -> Item {
    let mut item = Item {
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
        source: record_source(h, &r),
        code: r.code.as_ref().and_then(|c| c.code.clone()),
        fields: detailed.then(|| r.fields.clone()),
        ..Default::default()
    };
    match r.kind.as_str() {
        "result" => {
            let row = summary::result_row(h, &r);
            item.value = row.value;
            item.unit = row.unit;
            item.range = row.range;
            item.flag = row.flag;
            // The value is in its own fields.
            item.detail = None;
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
