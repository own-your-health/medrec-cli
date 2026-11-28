//! What is current among a person's conditions, allergies and medicines, by
//! what each of their sources says of it (docs/data-contract.md, "Standing").
//! No source's word wins over another's, and nothing here changes a record.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::import;
use crate::model::{Document, Person, PersonIndex, Provenance, Record, Source, Word};
use crate::sources::Sources;
use crate::summary::{day, series_key};

pub const SCHEMA: &str = "medrec-standing/v1";
pub const FILE: &str = "standing.json";

/// The kinds that have a standing.
pub const KINDS: &[&str] = &["condition", "allergy", "medication"];
/// The papers' kind in [`Standings::in_error`].
pub const DOCUMENT: &str = "document";

/// What one word, or all of a record's together, says of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Is {
    Current,
    /// A medicine on hold.
    OnHold,
    /// An order never signed.
    Draft,
    Ended,
    /// A condition or an allergy it is said not to be.
    Refuted,
    /// A diagnosis given for a visit, on no problem list.
    Visit,
    /// Entered in error.
    Error,
}

/// One source's word.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Said {
    /// Its place in [`Standings::sources`].
    pub source: usize,
    /// The day its source was last read saying it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read: Option<String>,
    pub is: Is,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abatement: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_of: Option<String>,
}

/// A fill of the same medicine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Filled {
    pub record: String,
    pub date: String,
    /// A payer's claim says so, and no record of the pharmacy's.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub from_claim: bool,
}

impl Filled {
    /// The day, as a reader says it.
    fn text(&self) -> String {
        let from = if self.from_claim {
            " (from a claim)"
        } else {
            ""
        };
        format!("{}{from}", self.date)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Standing {
    pub kind: String,
    /// Whatever the day: a medicine that is current is not one after `until`.
    pub is: Is,
    /// What it is the same as another by: its code, or `~` and its folded
    /// name.
    pub line: String,
    /// The last day of a current medicine, where each word that says so has
    /// an end, and which end it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until_of: Option<String>,
    /// Its words differ.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub differs: bool,
    /// They differ after this day, by which one that says it is current has
    /// ended and another has not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub differs_after: Option<String>,
    /// The fills of the medicine after `until`, newest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub filled: Vec<Filled>,
    pub words: Vec<Said>,
    /// The sources of which the record keeps no word.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unheard: Vec<usize>,
}

/// A source that sends diagnoses of visits and no problem list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoProblemList {
    pub source: usize,
    /// Its diagnoses: it is named only where one of them may be shown.
    pub records: Vec<String>,
}

/// `standing.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Standings {
    pub schema: String,
    pub person: String,
    /// The names of the sources, each once by its place here: two can have
    /// one name.
    pub sources: Vec<Option<String>>,
    /// Each kind's records, newest first, then by name.
    pub order: BTreeMap<String, Vec<String>>,
    pub records: BTreeMap<String, Standing>,
    /// The records of every kind, and the papers ([`DOCUMENT`]), entered in
    /// error.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub in_error: BTreeMap<String, Vec<String>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub no_problem_list: Vec<NoProblemList>,
}

/// A code as its letters, whatever its case and its spaces.
pub(crate) fn code(s: Option<&str>) -> String {
    let s = s.unwrap_or("").trim().to_lowercase();
    s.replace([' ', '_'], "-")
}

fn is_visit_diagnosis(categories: &[String]) -> bool {
    let has = |want: &str| categories.iter().any(|c| code(Some(c)) == want);
    has("encounter-diagnosis") && !has("problem-list-item")
}

/// What a word says by itself, whatever the day.
fn of_word(kind: &str, w: &Word) -> Is {
    let status = code(w.status.as_deref());
    if w.in_error() {
        return Is::Error;
    }
    if kind == "medication" {
        return match status.as_str() {
            "on-hold" => Is::OnHold,
            "draft" => Is::Draft,
            "not-taken" => Is::Ended,
            s if crate::views::is_ended(Some(s)) => Is::Ended,
            _ => Is::Current,
        };
    }
    if status == "refuted" || code(w.verification.as_deref()) == "refuted" {
        return Is::Refuted;
    }
    let ended = match status.as_str() {
        "" => kind == "condition" && w.abatement.is_some(),
        s => crate::views::is_ended(Some(s)),
    };
    if ended {
        Is::Ended
    } else if kind == "condition" && is_visit_diagnosis(&w.categories) {
        Is::Visit
    } else {
        Is::Current
    }
}

/// The last day of a date as written: `2025-02` ends with its month.
fn last_day(end: &str) -> Option<String> {
    let d = day(Some(end))?;
    let mut parts = d.split('-').map(|n| n.parse::<u32>().ok());
    let year = parts.next()??;
    let month = match parts.next() {
        None => 12,
        Some(m) => m?,
    };
    let last = match month {
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        1..=12 => 31,
        _ => return None,
    };
    let of_month = match parts.next() {
        None => last,
        Some(d) => d.filter(|d| (1..=last).contains(d))?,
    };
    Some(format!("{year:04}-{month:02}-{of_month:02}"))
}

/// The day a word was last read at its source.
fn read_on(p: &Provenance) -> Option<String> {
    day(p.fetched_at.as_deref()).or_else(|| day(Some(&p.filed_at)))
}

/// Whether a record has a standing: a condition, an allergy or a medicine
/// that is not a fill.
pub(crate) fn stands(r: &Record) -> bool {
    KINDS.contains(&r.kind.as_str()) && !is_fill(r)
}

/// Whether a record is entered in error: every one of its sources says so.
pub fn record_in_error(r: &Record) -> bool {
    let (said, unheard) = words(r);
    unheard.is_empty() && said.iter().all(|(w, _)| w.in_error())
}

/// Whether a paper is entered in error: every one of its sources says so. One
/// whose word is not kept does not.
pub fn document_in_error(d: &Document) -> bool {
    let word = |p: &Provenance| {
        d.words
            .iter()
            .find(|w| Some(&w.raw_path) == p.raw_path.as_ref())
    };
    d.provenances().all(|p| word(p).is_some_and(Word::in_error))
}

/// Whether some source says a paper is entered in error and another does not.
pub fn document_differs(d: &Document) -> bool {
    d.words.iter().any(Word::in_error) && !document_in_error(d)
}

/// Whether some source says a record is entered in error and another does
/// not.
pub fn record_differs(r: &Record) -> bool {
    let (said, unheard) = words(r);
    let errors = said.iter().filter(|(w, _)| w.in_error()).count();
    errors > 0 && (errors < said.len() || !unheard.is_empty())
}

/// The index without what is entered in error, as events, bills and labels
/// are made of it. A statement whose first paper is entered in error is its
/// next copy's, and a study whose report is has its report found again.
pub fn index(idx: &PersonIndex) -> Cow<'_, PersonIndex> {
    let gone: BTreeSet<String> = errors(idx).map(str::to_string).collect();
    without(idx, &gone)
}

/// The index without these, and without what it says of them.
pub(crate) fn without<'a>(idx: &'a PersonIndex, gone: &BTreeSet<String>) -> Cow<'a, PersonIndex> {
    if gone.is_empty() {
        return Cow::Borrowed(idx);
    }
    let mut out = idx.clone();
    out.records.retain(|r| !gone.contains(&r.id));
    out.documents.retain(|d| !gone.contains(&d.id));
    for r in &mut out.records {
        r.documents.retain(|d| !gone.contains(d));
        crate::panels::without(&mut r.fields, gone);
    }
    let mut first: BTreeMap<String, String> = BTreeMap::new();
    for d in &mut out.documents {
        d.near_duplicates.retain(|n| !gone.contains(&n.document));
        let of = d.same_statement_as.take();
        d.same_statement_as = match of {
            Some(of) if gone.contains(&of) => match first.get(&of) {
                Some(f) => Some(f.clone()),
                None => {
                    first.insert(of, d.id.clone());
                    None
                }
            },
            of => of,
        };
    }
    let mut lost: BTreeSet<String> = BTreeSet::new();
    for s in &mut out.studies {
        if s.report.as_ref().is_some_and(|r| gone.contains(r)) {
            s.report = None;
            lost.insert(s.id.clone());
        }
    }
    // No other: what is entered in error links no study that had no report.
    crate::imaging::link(&mut out, |s| lost.contains(&s.id));
    Cow::Owned(out)
}

/// The ids of a person's records and papers entered in error.
pub fn errors(idx: &PersonIndex) -> impl Iterator<Item = &str> {
    let records = idx.records.iter().filter(|r| record_in_error(r));
    let papers = idx.documents.iter().filter(|d| document_in_error(d));
    records
        .map(|r| r.id.as_str())
        .chain(papers.map(|d| d.id.as_str()))
}

/// One source's word of an item that some source says is entered in error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Says {
    /// Its source's name.
    pub source: Option<String>,
    /// The day its source was last read saying it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification: Option<String>,
    /// No word of it is kept.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unheard: bool,
    /// The household's own file.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub household: bool,
}

impl Says {
    fn of(from: Sources, p: Option<&Provenance>, w: Option<&Word>) -> Says {
        let fetched = || p.and_then(|p| day(p.fetched_at.as_deref()));
        Says {
            source: p.and_then(|p| from.label(p)),
            read: w.and_then(|w| day(w.read.as_deref()).or_else(fetched)),
            status: w.and_then(|w| w.status.clone()),
            verification: w.and_then(|w| w.verification.clone()),
            unheard: w.is_none(),
            household: w.is_none() && p.is_some_and(|p| p.source.cli.is_none()),
        }
    }

    pub fn text(&self) -> String {
        if self.household {
            return "the household's own file".into();
        }
        let source = self.source.as_deref().unwrap_or("a source with no name");
        if self.unheard {
            return format!("{source}: not heard");
        }
        let said = Said {
            source: 0,
            read: self.read.clone(),
            is: Is::Current,
            status: self.status.clone(),
            verification: self.verification.clone(),
            abatement: None,
            end: None,
            end_of: None,
        };
        format!("{source}: {}", said.text())
    }
}

/// What a reader says of an item its sources differ on.
pub const DIFFER: &str = "sources differ";

/// What each source says of a record of which one says it is entered in
/// error; of one with a standing, [`Standings`] has it.
pub fn record_says(from: Sources, r: &Record) -> Vec<Says> {
    let (said, unheard) = words(r);
    if stands(r) || !said.iter().any(|(w, _)| w.in_error()) {
        return Vec::new();
    }
    let said = said.iter().map(|(w, p)| Says::of(from, *p, Some(w)));
    said.chain(unheard.iter().map(|p| Says::of(from, Some(p), None)))
        .collect()
}

/// As [`record_says`], of a paper.
pub fn document_says(from: Sources, d: &Document) -> Vec<Says> {
    if !d.words.iter().any(Word::in_error) {
        return Vec::new();
    }
    let word = |p: &Provenance| {
        let mut of = d.words.iter();
        of.find(|w| p.raw_path.as_ref() == Some(&w.raw_path))
    };
    d.provenances()
        .map(|p| Says::of(from, Some(p), word(p)))
        .collect()
}

/// The standing and the words of an item that its sources differ on, as a
/// list shows them.
pub fn differ(says: &[Says]) -> (Option<String>, Vec<String>) {
    let standing = (!says.is_empty()).then(|| DIFFER.to_string());
    (standing, says.iter().map(Says::text).collect())
}

/// What a reader says of a paper opened by its id that a source says is
/// entered in error.
pub fn paper_note(from: Sources, d: &Document) -> Option<String> {
    let says: Vec<String> = document_says(from, d).iter().map(Says::text).collect();
    let says = says.join("; ");
    if document_in_error(d) {
        return Some(format!(
            "Entered in error ({says}): it is in no list, event, bill or total."
        ));
    }
    (!says.is_empty()).then(|| format!("Its sources differ ({says})."))
}

pub(crate) fn is_fill(r: &Record) -> bool {
    r.kind == "medication" && r.fields.get("source") == Some(&"dispense".into())
}

/// A fill the pharmacy says it gave: completed, or of no or an unknown status.
/// One cancelled, declined, stopped or still being made gave nothing.
pub(crate) fn gave_fill(r: &Record) -> bool {
    is_fill(r) && matches!(r.status.as_deref(), None | Some("completed" | "unknown"))
}

/// Each word of a record with the provenance it is of, and the provenances it
/// keeps none of. Its own status is the word of the one that wrote its fields
/// or is its only one. With none of any, it is the word of one it does not
/// say.
#[allow(clippy::type_complexity)]
fn words(r: &Record) -> (Vec<(Word, Option<&Provenance>)>, Vec<&Provenance>) {
    let (mut said, mut unheard) = (Vec::new(), Vec::new());
    let writer = import::writer(r);
    for p in &r.provenance {
        let path = p.raw_path.as_deref();
        let wrote = r.provenance.len() == 1 || (path.is_some() && path == writer);
        match r.words.iter().find(|w| path == Some(&w.raw_path)) {
            Some(w) => said.push((w.clone(), Some(p))),
            None if wrote => said.push((own(r), Some(p))),
            None => unheard.push(p),
        }
    }
    if said.is_empty() {
        said.push((own(r), None));
    }
    (said, unheard)
}

/// What a record's own status and fields say.
fn own(r: &Record) -> Word {
    let text = |k: &str| {
        let t = r.fields.get(k).and_then(|v| v.as_str());
        t.filter(|s| !s.is_empty()).map(str::to_string)
    };
    let categories = match r.fields.get("categories").and_then(|c| c.as_array()) {
        Some(a) => a
            .iter()
            .filter_map(|c| c.as_str().map(str::to_string))
            .collect(),
        None => text("category").into_iter().collect(),
    };
    Word {
        raw_path: String::new(),
        status: r.status.clone(),
        verification: text("verification_status"),
        categories,
        abatement: text("abatement").or_else(|| text("abatement_text")),
        end: text("end"),
        end_of: text("end_of"),
        ..Word::default()
    }
}

impl Standings {
    pub fn of(sources: &[Source], p: &Person, idx: &PersonIndex) -> Standings {
        let from = Sources::of(sources, idx);
        let mut at: BTreeMap<String, usize> = BTreeMap::new();
        let mut names: Vec<Option<String>> = Vec::new();
        let mut place = |p: Option<&Provenance>| {
            let key = p.map(|p| from.key(p)).unwrap_or_default();
            *at.entry(key).or_insert_with(|| {
                names.push(p.and_then(|p| from.label(p)));
                names.len() - 1
            })
        };
        let mut records: BTreeMap<String, Standing> = BTreeMap::new();
        let mut in_error: BTreeMap<String, Vec<String>> = BTreeMap::new();
        // Of each source: its visits' diagnoses, and whether it says, or may
        // say, anything else of a condition.
        let mut diagnoses: BTreeMap<usize, Vec<String>> = BTreeMap::new();
        let mut lists: BTreeSet<usize> = BTreeSet::new();
        for d in idx.documents.iter().filter(|d| document_in_error(d)) {
            let of = in_error.entry(DOCUMENT.into()).or_default();
            of.push(d.id.clone());
        }
        for r in &idx.records {
            let error = record_in_error(r);
            if stands(r) {
                let (heard, unheard) = words(r);
                let mut said: Vec<Said> = Vec::new();
                for (w, p) in heard {
                    let source = place(p);
                    let is = of_word(&r.kind, &w);
                    if r.kind == "condition" && is != Is::Error && p.is_some() {
                        if is_visit_diagnosis(&w.categories) {
                            let of = diagnoses.entry(source).or_default();
                            if of.last() != Some(&r.id) {
                                of.push(r.id.clone());
                            }
                        } else {
                            lists.insert(source);
                        }
                    }
                    said.push(Said {
                        source,
                        read: p.and_then(read_on),
                        is,
                        status: w.status,
                        verification: w.verification,
                        abatement: w.abatement,
                        end: w.end,
                        end_of: w.end_of,
                    });
                }
                let mut missing: Vec<usize> = Vec::new();
                for p in unheard {
                    let source = place(Some(p));
                    if !missing.contains(&source) {
                        missing.push(source);
                    }
                    if r.kind == "condition" {
                        lists.insert(source);
                    }
                }
                records.insert(r.id.clone(), standing(r, said, missing));
            }
            if error {
                let of = in_error.entry(r.kind.clone()).or_default();
                of.push(r.id.clone());
            }
        }
        let mut fills = crate::fills::of_claims(idx);
        for r in idx.records.iter().filter(|r| gave_fill(r)) {
            if let Some(date) = day(r.date.as_deref()) {
                fills.entry(series_key(r)).or_default().push(Filled {
                    record: r.id.clone(),
                    date,
                    from_claim: false,
                });
            }
        }
        for s in records.values_mut() {
            let (Some(until), Some(of)) = (&s.until, fills.get(&s.line)) else {
                continue;
            };
            s.filled = of.iter().filter(|f| f.date > *until).cloned().collect();
            s.filled
                .sort_by(|a, b| (&b.date, &b.record).cmp(&(&a.date, &a.record)));
        }
        let mut listed: Vec<&Record> = idx
            .records
            .iter()
            .filter(|r| records.contains_key(&r.id))
            .collect();
        listed.sort_by(|a, b| b.date.cmp(&a.date).then(a.display.cmp(&b.display)));
        let mut order: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for r in listed {
            let of = order.entry(r.kind.clone()).or_default();
            of.push(r.id.clone());
        }
        Standings {
            schema: SCHEMA.into(),
            person: p.id.clone(),
            sources: names,
            order,
            records,
            in_error,
            no_problem_list: diagnoses
                .into_iter()
                .filter(|(source, _)| !lists.contains(source))
                .map(|(source, records)| NoProblemList { source, records })
                .collect(),
        }
    }
}

/// What a record's words say together. One not heard may say it is current.
fn standing(r: &Record, words: Vec<Said>, unheard: Vec<usize>) -> Standing {
    let all: BTreeSet<Is> = words.iter().map(|w| w.is).collect();
    let is = match () {
        _ if all.contains(&Is::Current) || !unheard.is_empty() => Is::Current,
        _ if all.contains(&Is::OnHold) => Is::OnHold,
        _ if all.contains(&Is::Draft) => Is::Draft,
        _ if all.contains(&Is::Ended) => Is::Ended,
        _ if all.contains(&Is::Refuted) => Is::Refuted,
        _ if all.contains(&Is::Visit) => Is::Visit,
        _ => Is::Error,
    };
    let counted = |w: &&Said| !matches!(w.is, Is::Error | Is::Visit);
    let says: BTreeSet<Is> = words.iter().filter(counted).map(|w| w.is).collect();
    let sure: BTreeSet<String> = words
        .iter()
        .filter(counted)
        .filter_map(|w| w.verification.as_deref())
        .map(|v| code(Some(v)))
        .collect();
    let differs = says.len() > 1 || sure.len() > 1;

    let mut s = Standing {
        kind: r.kind.clone(),
        is,
        line: series_key(r),
        until: None,
        until_of: None,
        differs,
        differs_after: None,
        filled: Vec::new(),
        words,
        unheard,
    };
    if r.kind != "medication" || is != Is::Current {
        return s;
    }
    let ends: Vec<Option<(String, Option<String>)>> = s
        .words
        .iter()
        .filter(|w| w.is == Is::Current)
        .map(|w| Some((last_day(w.end.as_deref()?)?, w.end_of.clone())))
        .collect();
    let first = ends.iter().flatten().map(|(d, _)| d).min().cloned();
    let last = ends.iter().flatten().max_by(|a, b| a.0.cmp(&b.0)).cloned();
    let all_end = ends.iter().all(Option::is_some);
    if !differs && (!all_end || first.as_ref() != last.as_ref().map(|l| &l.0)) {
        s.differs_after = first;
    }
    // One not heard may not end.
    if let (true, true, Some((day, of))) = (all_end, s.unheard.is_empty(), last) {
        s.until = Some(day);
        s.until_of = of;
    }
    s
}

/// One line of a list: a condition or an allergy of one code or name, or one
/// medicine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Line {
    /// Its records, newest first; the first gives its name.
    pub records: Vec<String>,
    /// Its records of other sources that say otherwise.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub against: Vec<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub differs: bool,
    /// The last fill the answer may show of one ended by its date.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filled: Option<Filled>,
    /// Its sources not heard.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unheard: Vec<usize>,
}

/// What is current of one kind, as a reader lists it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lists {
    pub current: Vec<Line>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub on_hold: Vec<Line>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ended_by_date: Vec<Line>,
    /// How many are entered in error, and not shown.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub in_error: usize,
    /// The sources that send no problem list.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub without_problem_list: Vec<usize>,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// Where a record is for a reader on a day.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum At {
    Current,
    OnHold,
    EndedByDate,
    /// In the list of all only.
    All,
    /// Asked for by id only.
    Error,
}

impl Standing {
    pub fn on(&self, today: &str) -> At {
        let past = |d: &Option<String>| d.as_deref().is_some_and(|d| d < today);
        match self.is {
            Is::Current if past(&self.until) => At::EndedByDate,
            Is::Current => At::Current,
            Is::OnHold => At::OnHold,
            Is::Error => At::Error,
            Is::Draft | Is::Ended | Is::Refuted | Is::Visit => At::All,
        }
    }

    /// Its own words differ on that day.
    pub fn differs_on(&self, today: &str) -> bool {
        self.differs || self.differs_after.as_deref().is_some_and(|d| d < today)
    }

    /// What it is on a day, as a reader says it of its line.
    pub fn title(&self, today: &str, line: &Line) -> String {
        let mut is = match (self.on(today), self.is) {
            (At::Current, _) => "current".to_string(),
            (At::OnHold, _) => "on hold".into(),
            (At::Error, _) => "entered in error".into(),
            (At::EndedByDate, _) => {
                let of = self.until_of.as_deref();
                let mut s = format!(
                    "ended by its date {}{}",
                    self.until.as_deref().unwrap_or_default(),
                    of.map(|of| format!(" ({of})")).unwrap_or_default()
                );
                if let Some(f) = &line.filled {
                    s.push_str(&format!(", filled {}", f.text()));
                }
                s
            }
            (At::All, Is::Draft) => "draft".into(),
            (At::All, Is::Refuted) => "refuted".into(),
            (At::All, Is::Visit) => "diagnosis from a visit".into(),
            (At::All, _) => "ended".into(),
        };
        if line.differs {
            is.push_str(", sources differ");
        }
        if !line.unheard.is_empty() {
            is.push_str(", not every source heard");
        }
        is
    }

    /// The last of its fills that an answer may show.
    pub fn fill(&self, may_show: &dyn Fn(&str) -> bool) -> Option<&Filled> {
        self.filled.iter().find(|f| may_show(&f.record))
    }

    fn has(&self, source: usize, which: impl Fn(Is) -> bool) -> bool {
        self.words.iter().any(|w| w.source == source && which(w.is))
    }
}

impl Said {
    /// Its word, and the day it was read.
    fn text(&self) -> String {
        let mut said = vec![self.status.as_deref().unwrap_or("no status").to_string()];
        let sure = self.verification.as_deref();
        said.extend(
            sure.filter(|v| code(Some(v)) != "confirmed")
                .map(str::to_string),
        );
        said.extend(self.abatement.as_ref().map(|a| format!("ended {a}")));
        if let Some(end) = &self.end {
            let of = self.end_of.as_deref();
            let of = of.map(|of| format!(" ({of})")).unwrap_or_default();
            said.push(format!("ends {end}{of}"));
        }
        let read = self.read.as_ref().map(|d| format!(" (read {d})"));
        format!("{}{}", said.join(", "), read.unwrap_or_default())
    }
}

/// What a list says of a source that sends no problem list.
pub fn no_problem_list(source: &str) -> String {
    format!("No problem list from {source}; diagnoses from visits are listed under All")
}

/// What a list says of those it does not show.
pub fn in_error(n: usize) -> String {
    format!("{n} entered in error, not shown")
}

impl Standings {
    /// A source's name, as a reader says it.
    pub fn name(&self, source: usize) -> &str {
        let name = self.sources.get(source).and_then(Option::as_deref);
        name.unwrap_or("a source with no name")
    }

    /// What each source says on a line, with its name, each once, and those
    /// not heard.
    pub fn says(&self, line: &Line) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let ids = line.records.iter().chain(&line.against);
        let words = ids
            .filter_map(|id| self.records.get(id))
            .flat_map(|s| &s.words);
        let said = words.map(|w| format!("{}: {}", self.name(w.source), w.text()));
        let unheard = line.unheard.iter();
        let unheard = unheard.map(|n| format!("{}: not heard", self.name(*n)));
        for x in said.chain(unheard) {
            if !out.contains(&x) {
                out.push(x);
            }
        }
        out
    }

    /// A record by itself.
    fn alone(&self, id: &str, s: &Standing, may_show: &dyn Fn(&str) -> bool, today: &str) -> Line {
        let filled = s.fill(may_show);
        Line {
            records: vec![id.to_string()],
            against: Vec::new(),
            differs: s.differs_on(today),
            filled: filled.filter(|_| s.on(today) == At::EndedByDate).cloned(),
            unheard: s.unheard.clone(),
        }
    }

    /// Everything entered in error, by id.
    pub fn errors(&self) -> impl Iterator<Item = &String> {
        self.in_error.values().flatten()
    }

    /// The lists of one kind ([`KINDS`]; another has none), of the records a
    /// reader may show.
    pub fn lists(&self, kind: &str, may_show: &dyn Fn(&str) -> bool, today: &str) -> Lists {
        let shown: Vec<(&str, &Standing)> = self
            .order
            .get(kind)
            .into_iter()
            .flatten()
            .filter(|id| may_show(id))
            .filter_map(|id| Some((id.as_str(), self.records.get(id)?)))
            .collect();
        let mut out = Lists::default();
        let mut lines: BTreeMap<&str, Vec<(&str, &Standing)>> = BTreeMap::new();
        for (id, s) in &shown {
            if s.is != Is::Error && kind != "medication" {
                lines.entry(&s.line).or_default().push((id, s));
            }
        }
        for (id, s) in &shown {
            let one = || self.alone(id, s, may_show, today);
            match s.on(today) {
                At::Error | At::All => {}
                At::OnHold => out.on_hold.push(one()),
                At::EndedByDate => out.ended_by_date.push(one()),
                At::Current if kind == "medication" => out.current.push(one()),
                At::Current => {
                    // At the first of its line, and once.
                    if let Some(of) = lines.remove(s.line.as_str()) {
                        out.current.push(merged(&of, today));
                    }
                }
            }
        }
        let errors = self.in_error.get(kind).into_iter().flatten();
        out.in_error = errors.filter(|id| may_show(id)).count();
        if kind == "condition" {
            let named = self.no_problem_list.iter();
            out.without_problem_list = named
                .filter(|n| n.records.iter().any(|id| may_show(id)))
                .map(|n| n.source)
                .collect();
        }
        out
    }
}

/// A row of a list of conditions, allergies or medicines.
pub struct Row<'a> {
    pub record: &'a Record,
    /// What it is, as a reader says it; none of a fill, of which
    /// [`record_says`] tells.
    pub standing: Option<String>,
    pub says: Vec<String>,
    /// Current ones first, then those on hold, then those ended by their
    /// date.
    pub group: u8,
    /// Entered in error, to be counted and not shown.
    pub error: bool,
}

/// A list, and what it says of sources with no problem list.
pub struct List<'a> {
    pub rows: Vec<Row<'a>>,
    pub notes: Vec<String>,
}

impl Standings {
    /// A list of one kind: with `active` a row for each line of what is
    /// current, on hold or ended by its date, without it one for each record.
    /// Of those `asked` for. A row of a record that may not be shown (`shows`)
    /// is there to be left out and counted, as one entered in error is.
    pub fn list<'a>(
        &self,
        idx: &'a PersonIndex,
        kind: &str,
        active: bool,
        today: &str,
        asked: &dyn Fn(&Record) -> bool,
        shows: &dyn Fn(&Record) -> bool,
    ) -> List<'a> {
        // Of every kind: a medicine's fill can be a claim.
        let by: BTreeMap<&str, &'a Record> =
            idx.records.iter().map(|r| (r.id.as_str(), r)).collect();
        let may_show = |id: &str| by.get(id).is_some_and(|r| shows(r));
        let errors: BTreeSet<&String> = self.in_error.get(kind).into_iter().flatten().collect();
        let row = |x: &'a Record, of: Option<&Line>, group: u8| {
            let s = self.records.get(&x.id);
            let s = s.zip(of);
            Row {
                record: x,
                standing: s.map(|(s, of)| s.title(today, of)),
                says: s.map(|(_, of)| self.says(of)).unwrap_or_default(),
                group,
                error: errors.contains(&x.id),
            }
        };
        let alone = |x: &'a Record, group: u8| {
            let s = self.records.get(&x.id);
            let of = s.map(|s| self.alone(&x.id, s, &may_show, today));
            row(x, of.as_ref(), group)
        };
        let asked_for = idx.records.iter().filter(|x| x.kind == kind && asked(x));
        if !active {
            return List {
                rows: asked_for.map(|x| alone(x, 0)).collect(),
                notes: Vec::new(),
            };
        }
        let lists = self.lists(kind, &may_show, today);
        let mut rows: Vec<Row<'a>> = Vec::new();
        for (lines, group) in [
            (&lists.current, 0),
            (&lists.on_hold, 1),
            (&lists.ended_by_date, 2),
        ] {
            for l in lines {
                let mut of = l.records.iter().filter_map(|id| by.get(id.as_str()));
                rows.extend(of.find(|x| asked(x)).map(|x| row(x, Some(l), group)));
            }
        }
        for x in asked_for {
            let at = self.records.get(&x.id).map(|s| s.on(today));
            let group = match (shows(x), at) {
                (true, _) if errors.contains(&x.id) => 0,
                (false, Some(At::Current)) => 0,
                (false, Some(At::OnHold)) => 1,
                (false, Some(At::EndedByDate)) => 2,
                _ => continue,
            };
            rows.push(alone(x, group));
        }
        let none = lists.without_problem_list.iter();
        List {
            rows,
            notes: none.map(|n| no_problem_list(self.name(*n))).collect(),
        }
    }
}

/// One line of the records of one code or name.
fn merged(of: &[(&str, &Standing)], today: &str) -> Line {
    let current = |i: Is| i == Is::Current;
    let ended = |i: Is| matches!(i, Is::Ended | Is::Refuted);
    let sources: BTreeSet<usize> = of
        .iter()
        .flat_map(|(_, s)| s.words.iter().map(|w| w.source))
        .collect();
    // Sources that say it is not so, and nowhere that it is.
    let other: Vec<usize> = sources
        .into_iter()
        .filter(|n| of.iter().any(|(_, s)| s.has(*n, ended)))
        .filter(|n| !of.iter().any(|(_, s)| s.has(*n, current)))
        .collect();
    let is_current = |s: &Standing| s.on(today) == At::Current;
    let ids = |which: &dyn Fn(&Standing) -> bool| -> Vec<String> {
        of.iter()
            .filter(|(_, s)| which(s))
            .map(|(id, _)| id.to_string())
            .collect()
    };
    let records = ids(&is_current);
    let against = ids(&|s| !is_current(s) && other.iter().any(|n| s.has(*n, ended)));
    let says_so = of
        .iter()
        .any(|(_, s)| s.words.iter().any(|w| current(w.is)));
    let differs = (says_so && !other.is_empty())
        || of.iter().any(|(_, s)| is_current(s) && s.differs_on(today));
    let mut unheard: Vec<usize> = Vec::new();
    for n in of.iter().flat_map(|(_, s)| &s.unheard) {
        if !unheard.contains(n) {
            unheard.push(*n);
        }
    }
    Line {
        records,
        against,
        differs,
        filled: None,
        unheard,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Household;
    use serde_json::{json, Value};

    macro_rules! fixture {
        ($f:literal) => {
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/standing/",
                $f
            ))
        };
    }

    const DAY: &str = "2024-06-15";

    fn of(index: Value) -> Standings {
        let h: Household = serde_json::from_str(fixture!("household.json")).unwrap();
        let idx: PersonIndex = serde_json::from_value(index).unwrap();
        Standings::of(&h.sources, &h.people[0], &idx)
    }

    fn fixture() -> Standings {
        of(serde_json::from_str(fixture!("index.json")).unwrap())
    }

    /// The fixture's record whose id starts so.
    fn one<'a>(s: &'a Standings, stem: &str) -> &'a Standing {
        let id = format!("r_{stem:a<20}");
        s.records.get(&id).unwrap_or_else(|| panic!("no {id}"))
    }

    fn at(stem: &str) -> At {
        one(&fixture(), stem).on(DAY)
    }

    /// The lists of these records alone.
    fn only(s: &Standings, kind: &str, stems: &[&str]) -> Lists {
        let ids: Vec<String> = stems.iter().map(|s| format!("r_{s:a<20}")).collect();
        s.lists(kind, &|id| ids.iter().any(|i| i == id), DAY)
    }

    #[test]
    fn the_fixture_gives_its_view() {
        let s = serde_json::to_value(fixture()).unwrap();
        let want: Value = serde_json::from_str(fixture!("standing.json")).unwrap();
        assert_eq!(s, want, "{}", serde_json::to_string_pretty(&s).unwrap());
    }

    #[test]
    fn a_reader_lists_them_by_the_rules() {
        let s = fixture();
        let v: Value = serde_json::from_str(fixture!("lists.json")).unwrap();
        for c in v["cases"].as_array().unwrap() {
            let hidden = c["hidden"].as_array().unwrap();
            let may_show = |id: &str| !hidden.iter().any(|h| h == id);
            let got = s.lists(
                c["kind"].as_str().unwrap(),
                &may_show,
                c["day"].as_str().unwrap(),
            );
            assert_eq!(
                serde_json::to_value(got).unwrap(),
                c["lists"],
                "{}",
                c["why"]
            );
        }
    }

    // One test for each rule of "Standing" in docs/data-contract.md.

    #[test]
    fn a_record_is_current_while_any_source_says_so_and_differs() {
        let s = fixture();
        let r = one(&s, "bothsay");
        assert_eq!((r.on(DAY), r.differs_on(DAY)), (At::Current, true));
        let words: Vec<(usize, Option<&str>, Option<&str>)> = r
            .words
            .iter()
            .map(|w| (w.source, w.status.as_deref(), w.read.as_deref()))
            .collect();
        assert_eq!(
            words,
            [
                (0, Some("active"), Some("2024-05-02")),
                (1, Some("resolved"), Some("2024-05-03"))
            ]
        );
        assert_eq!(one(&s, "bothended").on(DAY), At::All);
    }

    #[test]
    fn a_visits_diagnosis_is_current_only_where_a_problem_list_has_it() {
        assert_eq!(at("visitonly"), At::All);
        let s = fixture();
        let l = only(&s, "condition", &["onlistvisit", "onlist"]);
        assert_eq!(l.current.len(), 1);
        assert_eq!(
            (l.current[0].records.len(), l.current[0].differs),
            (1, false)
        );
    }

    #[test]
    fn a_source_with_no_problem_list_is_named_where_its_diagnosis_may_be_shown() {
        let s = fixture();
        let named = |stem: &str| -> Vec<Option<String>> {
            let l = only(&s, "condition", &[stem]);
            l.without_problem_list
                .iter()
                .map(|n| s.sources[*n].clone())
                .collect()
        };
        assert_eq!(named("visitonly"), [Some("Example Walk-in".to_string())]);
        assert!(named("plain").is_empty());
        // Nor one that may have one, by a word not heard.
        assert_eq!(at("mayhavelist"), At::All);
        assert!(named("mayhavelist").is_empty());
    }

    #[test]
    fn refuted_is_never_current_and_beside_an_active_word_differs() {
        assert_eq!(at("refuted"), At::All);
        assert_eq!(at("refutedallergy"), At::All);
        let s = fixture();
        let r = one(&s, "refutedbyone");
        assert_eq!((r.on(DAY), r.differs_on(DAY)), (At::Current, true));
    }

    #[test]
    fn entered_in_error_is_out_of_the_lists_and_counted_and_nothing_beside_another_word() {
        for stem in ["error", "errorallergy", "errormedicine"] {
            assert_eq!(at(stem), At::Error, "{stem}");
        }
        let s = fixture();
        let r = one(&s, "errorbeside");
        assert_eq!((r.on(DAY), r.differs_on(DAY)), (At::Current, false));
        let l = only(&s, "allergy", &["errorallergy"]);
        assert_eq!((l.current.len(), l.in_error), (0, 1));
        // A fill and a result are by their own status, and count in theirs.
        let l = only(&s, "medication", &["fillerror"]);
        assert_eq!((l.current.len(), l.in_error), (0, 1));
        assert_eq!(s.in_error["result"], ["r_resulterroraaaaaaaaa"]);
        assert!(only(&s, "result", &["resulterror"]).in_error == 1);
    }

    #[test]
    fn what_is_not_sure_is_current_with_its_word() {
        let s = fixture();
        let r = one(&s, "provisional");
        assert_eq!(r.on(DAY), At::Current);
        assert_eq!(r.words[0].verification.as_deref(), Some("provisional"));
        let r = one(&s, "suretwice");
        assert_eq!((r.on(DAY), r.differs_on(DAY)), (At::Current, true));
    }

    #[test]
    fn a_condition_with_no_status_and_an_abatement_is_ended() {
        assert_eq!(at("abated"), At::All);
        // Not an allergy, whatever it says.
        let w = Word {
            abatement: Some("2020-01-02".into()),
            ..Word::default()
        };
        assert_eq!(of_word("allergy", &w), Is::Current);
    }

    #[test]
    fn a_medicine_is_current_through_its_last_day_and_then_ended_by_its_date() {
        let s = fixture();
        let r = one(&s, "ends");
        assert_eq!(
            (r.until.as_deref(), r.until_of.as_deref()),
            (Some("2024-03-31"), Some("prescription"))
        );
        assert_eq!(r.on("2024-03-31"), At::Current);
        assert_eq!(r.on("2024-04-01"), At::EndedByDate);
        assert_eq!(one(&s, "endsmonth").until.as_deref(), Some("2024-06-30"));
    }

    #[test]
    fn a_fill_after_the_end_is_a_fact_beside_it_and_changes_nothing() {
        let s = fixture();
        let r = one(&s, "ends");
        let fills: Vec<(&str, &str)> = r
            .filled
            .iter()
            .map(|f| (f.record.as_str(), f.date.as_str()))
            .collect();
        assert_eq!(
            fills,
            [
                ("r_fillafteraaaaaaaaaaa", "2024-05-02"),
                ("r_fillsoonaaaaaaaaaaaa", "2024-04-10")
            ]
        );
        assert_eq!(r.on(DAY), At::EndedByDate);
        for stem in ["fillbefore", "fillsoon", "fillafter", "fillreturned"] {
            assert!(!s.records.contains_key(&format!("r_{stem:a<20}")), "{stem}");
        }
        // The last the answer may show.
        let shown = |stems: &[&str]| -> Option<String> {
            let l = only(&s, "medication", stems);
            l.ended_by_date[0].filled.as_ref().map(|f| f.date.clone())
        };
        assert_eq!(
            shown(&["ends", "fillafter", "fillsoon"]).as_deref(),
            Some("2024-05-02")
        );
        assert_eq!(shown(&["ends", "fillsoon"]).as_deref(), Some("2024-04-10"));
        assert_eq!(shown(&["ends"]), None);
    }

    #[test]
    fn a_source_not_heard_never_lets_the_others_end_it() {
        let s = fixture();
        let r = one(&s, "oneheard");
        assert_eq!((r.is, r.words.len()), (Is::Current, 1));
        assert_eq!((r.words[0].is, r.differs), (Is::Ended, false));
        let named: Vec<&str> = r
            .unheard
            .iter()
            .map(|n| s.sources[*n].as_deref().unwrap())
            .collect();
        assert_eq!(named, ["Example Health"]);
        let l = only(&s, "allergy", &["oneheard"]);
        assert_eq!(l.current[0].unheard, r.unheard);
        // Nor one's word that it is in error.
        assert_eq!(at("errorunheard"), At::Current);
        // Nor its own end.
        let r = one(&s, "endsunheard");
        assert_eq!(
            (r.until.as_deref(), r.on("2099-01-01")),
            (None, At::Current)
        );
        assert!(!r.differs_on("2099-01-01"));
        // One that keeps none has its own status, of one it does not say.
        let r = one(&s, "noneheard");
        assert_eq!((r.is, r.words[0].is), (Is::Current, Is::Ended));
        assert_eq!((&s.sources[r.words[0].source], r.unheard.len()), (&None, 2));
        assert_eq!(r.words[0].read, None);
    }

    /// Its own status is its word.
    #[test]
    fn the_source_that_wrote_the_fields_is_heard() {
        let s = fixture();
        let r = one(&s, "wrote");
        assert_eq!((r.is, r.words.len()), (Is::Ended, 2));
        assert!(r.unheard.is_empty());
    }

    #[test]
    fn a_medicine_one_source_ends_is_current_by_the_other_and_differs_after() {
        let s = fixture();
        let r = one(&s, "endsatone");
        assert_eq!(
            (r.until.as_deref(), r.differs_after.as_deref()),
            (None, Some("2024-03-31"))
        );
        assert_eq!(
            (r.on("2024-03-31"), r.differs_on("2024-03-31")),
            (At::Current, false)
        );
        assert_eq!(
            (r.on("2024-04-01"), r.differs_on("2024-04-01")),
            (At::Current, true)
        );
        let r = one(&s, "endstwice");
        assert_eq!(
            (r.until.as_deref(), r.until_of.as_deref()),
            (Some("2024-06-30"), Some("taking"))
        );
    }

    #[test]
    fn unknown_is_current_with_its_word() {
        let s = fixture();
        let r = one(&s, "unknown");
        assert_eq!(
            (r.on(DAY), r.words[0].status.as_deref()),
            (At::Current, Some("unknown"))
        );
    }

    #[test]
    fn on_hold_is_a_group_of_its_own() {
        assert_eq!(at("onhold"), At::OnHold);
    }

    #[test]
    fn a_draft_is_in_the_list_of_all() {
        assert_eq!(at("draft"), At::All);
    }

    #[test]
    fn not_taken_is_not_current() {
        assert_eq!(at("nottaken"), At::All);
    }

    #[test]
    fn an_allergy_never_ends_by_a_date() {
        let s = fixture();
        assert_eq!(one(&s, "oldpollen").on("2099-01-01"), At::Current);
        assert_eq!(at("inactive"), At::All);
    }

    #[test]
    fn one_line_differs_by_another_source_and_not_by_the_same_ones_earlier_episode() {
        let s = fixture();
        let l = |a: &str, b: &str| only(&s, "condition", &[a, b]).current;
        let rash = l("tworecords", "tworecordsother");
        assert_eq!(
            (rash.len(), rash[0].differs, rash[0].against.len()),
            (1, true, 1)
        );
        let cough = l("again", "againbefore");
        assert_eq!(
            (cough.len(), cough[0].differs, cough[0].against.len()),
            (1, false, 0)
        );
    }

    #[test]
    fn a_medicine_is_never_one_line_with_another() {
        let s = fixture();
        let l = only(&s, "medication", &["noend", "noendtwice"]);
        assert_eq!(l.current.len(), 2);
        // Of one heading, by which a reader may group them.
        assert_eq!(one(&s, "noend").line, one(&s, "noendtwice").line);
    }

    #[test]
    fn uncoded_names_alike_are_one_line_and_each_source_is_named() {
        let s = fixture();
        let l = only(&s, "allergy", &["nut", "nutagain"]);
        assert_eq!(l.current.len(), 1);
        let named: Vec<&str> = l.current[0]
            .records
            .iter()
            .flat_map(|id| &s.records[id].words)
            .map(|w| s.sources[w.source].as_deref().unwrap())
            .collect();
        assert_eq!(named, ["Example Health", "Sample Clinic East"]);
    }

    #[test]
    fn a_record_with_no_words_has_its_own_status() {
        let s = of(
            json!({"schema": SCHEMA.replace("standing", "index"), "person": "p_robinaaaaaaaaaaaaaaa",
            "rev": 1, "documents": [], "records": [
            {"id": "r_oneaaaaaaaaaaaaaaaaa", "kind": "condition", "key": "k1", "display": "Sample ache",
             "status": "resolved", "fields": {}, "provenance": [{"source": {"kind": "manual"}, "method": "manual",
             "filed_at": "2024-01-02T10:00:00Z", "filed_by": "medrec 0.9.9"}]},
            {"id": "r_twoaaaaaaaaaaaaaaaaa", "kind": "condition", "key": "k2", "display": "Sample itch",
             "fields": {"category": "encounter-diagnosis", "verification_status": "confirmed"},
             "provenance": [{"source": {"kind": "manual"}, "method": "manual",
             "filed_at": "2024-01-02T10:00:00Z", "filed_by": "medrec 0.9.9"}]},
            {"id": "r_threeaaaaaaaaaaaaaaa", "kind": "allergy", "key": "k3", "display": "Sample moss",
             "fields": {}, "provenance": []}]}),
        );
        assert_eq!(one(&s, "one").on(DAY), At::All);
        let r = one(&s, "two");
        assert_eq!(
            (r.is, r.words[0].read.as_deref()),
            (Is::Visit, Some("2024-01-02"))
        );
        // Nor does one of no source end.
        let r = one(&s, "three");
        assert_eq!((r.is, r.words[0].read.as_deref()), (Is::Current, None));
    }

    #[test]
    fn the_last_day_of_a_date_as_written() {
        for (end, day) in [
            ("2024-02", Some("2024-02-29")),
            ("2023-02", Some("2023-02-28")),
            ("2100-02", Some("2100-02-28")),
            ("2024", Some("2024-12-31")),
            ("2024-04-05T10:00:00Z", Some("2024-04-05")),
            ("2024-13", None),
            ("2024-04-31", None),
            ("later", None),
        ] {
            assert_eq!(last_day(end).as_deref(), day, "{end}");
        }
    }

    /// What is made of an index is made of it without what is entered in
    /// error, and of it as it is when nothing is.
    #[test]
    fn an_index_is_without_what_is_entered_in_error() {
        let (record, paper) = ("r_recordaaaaaaaaaaaaaa", "d_paperaaaaaaaaaaaaaaa");
        let copies = [
            "d_firstaaaaaaaaaaaaaaa",
            "d_nextaaaaaaaaaaaaaaaa",
            "d_lastaaaaaaaaaaaaaaaa",
        ];
        let document = |id: &str, same: Option<&str>, near: &[&str]| {
            let near: Vec<Value> = near
                .iter()
                .map(|d| json!({"document": d, "similarity": 0.9}))
                .collect();
            json!({"id": id, "path": "a", "sha256": "a", "md5": "a", "size": 1,
                "media_type": "application/pdf", "doc_type": "bill", "filed_at": "2024-06-01T10:00:00Z",
                "same_statement_as": same, "near_duplicates": near,
                "origin": {"kind": "fhir", "action": "imported"},
                "provenance": {"source": {"kind": "fhir"}, "method": "fhir-api",
                    "raw_path": format!("{id}.json"),
                    "filed_at": "2024-06-01T10:00:00Z", "filed_by": "medrec 0.9.9"}})
        };
        let mut idx: PersonIndex = serde_json::from_value(json!({
            "schema": "medrec-index/v1", "person": "p_robinaaaaaaaaaaaaaaa", "rev": 1, "imports": [],
            "documents": [
                document(copies[0], None, &[]),
                document(copies[1], Some(copies[0]), &[copies[0]]),
                document(copies[2], Some(copies[0]), &[]),
                document(paper, None, &[]),
            ],
            "records": [{"id": record, "kind": "encounter", "key": "k", "display": "Visit",
                "fields": {}, "documents": [copies[0], paper], "provenance": [{"source": {"kind": "fhir"},
                    "method": "fhir-api", "filed_at": "2024-06-01T10:00:00Z", "filed_by": "medrec 0.9.9"}]}],
        }))
        .unwrap();
        assert!(matches!(index(&idx), Cow::Borrowed(_)));
        assert_eq!(errors(&idx).count(), 0);
        let error = |idx: &mut PersonIndex, id: &str| {
            let d = idx.documents.iter_mut().find(|d| d.id == id).unwrap();
            d.words = serde_json::from_value(
                json!([{"raw_path": format!("{id}.json"), "status": "entered-in-error"}]),
            )
            .unwrap();
        };
        error(&mut idx, copies[0]);
        let now = index(&idx);
        let same: Vec<(&str, Option<&str>)> = now
            .documents
            .iter()
            .map(|d| (d.id.as_str(), d.same_statement_as.as_deref()))
            .collect();
        assert_eq!(
            same,
            [
                (copies[1], None),
                (copies[2], Some(copies[1])),
                (paper, None)
            ]
        );
        assert!(now.documents[0].near_duplicates.is_empty());
        assert_eq!(now.records[0].documents, [paper]);
        // What is kept is as it was.
        assert_eq!(idx.documents.len(), 4);
        assert_eq!(
            idx.documents[2].same_statement_as.as_deref(),
            Some(copies[0])
        );
        assert_eq!(errors(&idx).collect::<Vec<_>>(), [copies[0]]);
        // A copy that is in error is not the first of those left.
        error(&mut idx, copies[1]);
        let now = index(&idx);
        assert_eq!(now.documents[0].id, copies[2]);
        assert_eq!(now.documents[0].same_statement_as, None);
        idx.records[0].status = Some("entered-in-error".into());
        assert!(index(&idx).records.is_empty());
        assert_eq!(
            errors(&idx).collect::<Vec<_>>(),
            [record, copies[0], copies[1]]
        );
    }
}
