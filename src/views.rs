//! Read views over the index: the timeline, search, the record lists
//! (results, medications, conditions, allergies, immunizations, claims),
//! documents and notes. Everything is computed in memory from a person's
//! `index.json`, `text.json` and `notes.md`; nothing here writes.

use pk_cli_core::CliError;
use serde::Serialize;
use serde_json::Value;

use std::collections::BTreeSet;

use crate::about;
use crate::folder::Folder;
use crate::intake::classify;
use crate::model::{Document, Household, Note, Person, PersonIndex, PersonText, Record};
use crate::parts::{self, Parts, Place};
use crate::people;
use crate::sources::Sources;
use crate::standing::{self, Standings};

#[derive(Debug, Clone, Default)]
pub struct Range {
    pub since: Option<String>,
    pub until: Option<String>,
}

impl Range {
    pub fn validate(&self) -> Result<(), CliError> {
        for d in [&self.since, &self.until].into_iter().flatten() {
            pk_cli_core::dates::parse_iso(d)?;
        }
        Ok(())
    }

    /// An undated item shows only when no range is asked for.
    pub(crate) fn holds(&self, date: Option<&str>) -> bool {
        match date {
            None => self.since.is_none() && self.until.is_none(),
            Some(d) => {
                // The day: the first ten characters (an ISO date or the
                // start of a date-time). By characters, never bytes.
                let end = d.char_indices().nth(10).map_or(d.len(), |(i, _)| i);
                let d = &d[..end];
                self.since.as_deref().is_none_or(|s| d >= s)
                    && self.until.as_deref().is_none_or(|u| d <= u)
            }
        }
    }
}

#[derive(Debug, Serialize)]
pub struct Event {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    pub person: String,
    pub person_name: String,
    /// `document`, `study`, `note`, or a record kind (`encounter`,
    /// `result`, `report`, `condition`, `medication`, `allergy`,
    /// `immunization`, `procedure`, `coverage`, `claim`).
    pub kind: String,
    pub id: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Where it came from: an organization, a provider CLI, a scan.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// What `timeline --kind` takes: documents, studies, notes, and every
/// record kind.
pub const TIMELINE_KINDS: &[&str] = &[
    "document",
    "study",
    "note",
    "encounter",
    "result",
    "report",
    "condition",
    "medication",
    "allergy",
    "immunization",
    "procedure",
    "coverage",
    "claim",
];

/// Refuse a kind `timeline` does not know, so a typo is a usage error and
/// not an empty list.
pub fn validate_kinds(kinds: &[String]) -> Result<(), CliError> {
    for k in kinds {
        if !TIMELINE_KINDS.contains(&k.as_str()) {
            return Err(CliError::Usage(format!(
                "unknown kind `{k}` (known: {})",
                TIMELINE_KINDS.join(", ")
            )));
        }
    }
    Ok(())
}

pub fn timeline(
    folder: &Folder,
    h: &Household,
    person: Option<&str>,
    range: &Range,
    kinds: &[String],
) -> Result<(Vec<Event>, Vec<String>), CliError> {
    let all = everything(folder, h, person, range, kinds, &Parts::default())?;
    let errors = entered_in_error(folder, h, person)?;
    let error = |e: &mut Event| errors.contains(&e.id).then_some(Why::Error);
    let (out, left_out) = LeftOut::split(all, error);
    let mut notes = left_out.notes();
    notes.extend(about_another(folder, h, person)?);
    Ok((out, notes))
}

/// What a list of one person's says of what is about somebody else.
pub(crate) fn about_another(
    folder: &Folder,
    h: &Household,
    person: Option<&str>,
) -> Result<Option<String>, CliError> {
    let mut n = 0;
    if person.is_some() {
        for p in people::selected(h, person)? {
            n += folder.index(p)?.about_another.len();
        }
    }
    Ok(about::line(n, person))
}

/// As `timeline`, those entered in error too, without some parts of them.
pub(crate) fn everything(
    folder: &Folder,
    h: &Household,
    person: Option<&str>,
    range: &Range,
    kinds: &[String],
    parts: &Parts,
) -> Result<Vec<Event>, CliError> {
    range.validate()?;
    validate_kinds(kinds)?;
    let want = |k: &str| kinds.is_empty() || kinds.iter().any(|x| x == k);
    let mut out = Vec::new();
    for p in people::selected(h, person)? {
        let mut idx = folder.index(p)?;
        parts.index(&mut idx);
        let notes = if want("note") {
            folder.notes(p)?
        } else {
            Vec::new()
        };
        person_events(h, p, &idx, &notes, range, &want, &mut out);
    }
    order_events(&mut out);
    Ok(out)
}

/// One person's timeline events, in their files' order: documents,
/// records, studies, then notes.
pub(crate) fn person_events(
    h: &Household,
    p: &Person,
    idx: &PersonIndex,
    notes: &[Note],
    range: &Range,
    want: &dyn Fn(&str) -> bool,
    out: &mut Vec<Event>,
) {
    if want("document") {
        for d in &idx.documents {
            if !range.holds(d.date.as_deref()) {
                continue;
            }
            out.push(Event {
                date: d.date.clone(),
                person: p.id.clone(),
                person_name: p.name.clone(),
                kind: "document".into(),
                id: d.id.clone(),
                title: d
                    .title
                    .clone()
                    .unwrap_or_else(|| label(d.doc_type.as_deref()).to_string()),
                detail: d.original_name.clone(),
                source: d.organization.clone(),
            });
        }
    }
    let from = Sources::of(&h.sources, idx);
    for r in &idx.records {
        if !want(&r.kind) || !range.holds(r.date.as_deref()) {
            continue;
        }
        out.push(Event {
            date: r.date.clone(),
            person: p.id.clone(),
            person_name: p.name.clone(),
            kind: r.kind.clone(),
            id: r.id.clone(),
            title: r.display.clone(),
            detail: record_detail(r),
            source: record_source(from, r),
        });
    }
    if want("study") {
        for s in &idx.studies {
            if !range.holds(s.date.as_deref()) {
                continue;
            }
            out.push(Event {
                date: s.date.clone(),
                person: p.id.clone(),
                person_name: p.name.clone(),
                kind: "study".into(),
                id: s.id.clone(),
                title: format!(
                    "{} {}",
                    s.modalities.join("/"),
                    s.description
                        .clone()
                        .or_else(|| s.body_part.clone())
                        .unwrap_or_default()
                )
                .trim()
                .to_string(),
                detail: Some(format!("{} series, {} images", s.series.len(), s.instances)),
                source: s.facility.clone(),
            });
        }
    }
    if want("note") {
        for n in notes {
            if !range.holds(Some(&n.date)) {
                continue;
            }
            out.push(Event {
                date: Some(n.date.clone()),
                person: p.id.clone(),
                person_name: p.name.clone(),
                kind: "note".into(),
                id: n.id.clone(),
                title: first_line(&n.text, 80),
                detail: (!n.tags.is_empty()).then(|| n.tags.join(", ")),
                source: n.author.clone(),
            });
        }
    }
}

/// medrec's timeline order: newest first, then by kind and title.
pub(crate) fn order_events(out: &mut [Event]) {
    out.sort_by(|a, b| {
        b.date
            .cmp(&a.date)
            .then(a.kind.cmp(&b.kind))
            .then(a.title.cmp(&b.title))
    });
}

pub(crate) fn label(doc_type: Option<&str>) -> &'static str {
    doc_type
        .and_then(classify::doc_type_label)
        .unwrap_or("Document")
}

pub(crate) fn first_line(s: &str, max: usize) -> String {
    let line = s
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    if line.chars().count() > max {
        format!("{}…", line.chars().take(max - 1).collect::<String>())
    } else {
        line.to_string()
    }
}

/// The short line a record shows beside its name in a list.
pub(crate) fn record_detail(r: &Record) -> Option<String> {
    let f = &r.fields;
    let text = |k: &str| f.get(k).and_then(Value::as_str).map(str::to_string);
    match r.kind.as_str() {
        "result" => {
            let v = f.get("value").map(value_text).filter(|s| !s.is_empty());
            let flag = text("interpretation");
            let heads = crate::panels::heads(r).then(|| crate::panels::HEADING.to_string());
            match (v, flag) {
                (Some(v), Some(i)) => Some(format!("{v} ({i})")),
                (v, i) => v.or(i).or(heads),
            }
        }
        "medication" if text("source").as_deref() == Some("dispense") => {
            let filled: Vec<String> = [
                f.get("quantity").map(value_text),
                f.get("days_supply")
                    .map(value_text)
                    .map(|d| format!("{d} days")),
            ]
            .into_iter()
            .flatten()
            .filter(|s| !s.is_empty())
            .collect();
            (!filled.is_empty())
                .then(|| format!("dispensed {}", filled.join(", ")))
                .or_else(|| text("dosage"))
                .or_else(|| r.status.clone())
        }
        "medication" => text("dosage").or_else(|| r.status.clone()),
        "claim"
            if f.get("amounts")
                .is_some_and(|a| a.as_object().is_some_and(|o| !o.is_empty())) =>
        {
            let a = &f["amounts"];
            let mut parts: Vec<String> = [("billed", "billed"), ("plan_paid", "plan paid")]
                .iter()
                .filter_map(|(k, label)| {
                    let v = a.get(*k).map(value_text).filter(|s| !s.is_empty())?;
                    Some(format!("{label} {v}"))
                })
                .collect();
            if let Some((v, summed)) = crate::claims::member_share(a) {
                let v = value_text(&v);
                if !v.is_empty() {
                    let label = if summed { "your share" } else { "you owe" };
                    parts.push(format!("{label} {v}"));
                }
            }
            (!parts.is_empty())
                .then(|| parts.join(", "))
                .or_else(|| r.status.clone())
        }
        // A claim imported by an earlier medrec, with no `amounts`.
        "claim" => {
            let billed = f
                .get("total")
                .and_then(|t| t.get(0))
                .and_then(|t| t.get("amount"))
                .map(value_text)
                .filter(|s| !s.is_empty());
            let paid = f.get("payment").map(value_text).filter(|s| !s.is_empty());
            match (billed, paid) {
                (Some(b), Some(p)) => Some(format!("billed {b}, plan paid {p}")),
                (Some(b), None) => Some(format!("billed {b}")),
                (None, Some(p)) => Some(format!("plan paid {p}")),
                (None, None) => r.status.clone(),
            }
        }
        _ => r.status.clone(),
    }
}

/// Every string and number in `v`, space-separated, for search. An id says
/// nothing of what it is of, and can be of what is not to be shown.
fn flat_text(v: &Value, out: &mut String) {
    match v {
        Value::String(s) if crate::ids::is_id(s) => {}
        Value::String(s) => {
            out.push_str(s);
            out.push(' ');
        }
        Value::Number(n) => {
            out.push_str(&n.to_string());
            out.push(' ');
        }
        Value::Array(a) => a.iter().for_each(|x| flat_text(x, out)),
        Value::Object(o) => o.values().for_each(|x| flat_text(x, out)),
        _ => {}
    }
}

pub(crate) fn value_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Object(o) => {
            let num = o
                .get("value")
                .or_else(|| o.get("amount"))
                .map(value_text)
                .unwrap_or_default();
            let than = o.get("comparator").and_then(Value::as_str).unwrap_or("");
            let num = format!("{than}{num}");
            let unit = o
                .get("unit")
                .or_else(|| o.get("currency"))
                .and_then(Value::as_str)
                .unwrap_or("");
            format!("{num} {unit}").trim().to_string()
        }
        Value::Array(a) => a.iter().map(value_text).collect::<Vec<_>>().join("; "),
        Value::Bool(b) => b.to_string(),
        Value::Null => String::new(),
    }
}

/// The name of the source a record came through ([`crate::sources`]).
pub(crate) fn record_source(from: Sources, r: &Record) -> Option<String> {
    from.label(r.provenance.first()?)
}

/// One row of a record list.
#[derive(Debug, Serialize)]
pub struct RecordRow {
    pub id: String,
    pub person: String,
    pub person_name: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    pub display: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub sources: Vec<String>,
    /// What its sources' words make of a condition, an allergy or a medicine,
    /// or that they differ on another, and what each says.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub standing: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub says: Vec<String>,
    pub fields: serde_json::Map<String, Value>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub documents: Vec<String>,
}

/// Records of `kinds` for the selected people, newest first, and what the
/// list says of those it does not show. Of conditions, allergies and
/// medicines, `active` keeps one line for each that is current by what its
/// sources say, then those on hold, then those ended by their date
/// ([`Standings::list`]); of other kinds, those whose status is not ended.
#[allow(clippy::too_many_arguments)]
pub fn records(
    folder: &Folder,
    h: &Household,
    person: Option<&str>,
    kinds: &[&str],
    range: &Range,
    active: bool,
    code: Option<&str>,
    today: &str,
) -> Result<(Vec<RecordRow>, Vec<String>), CliError> {
    range.validate()?;
    let mut out: Vec<(u8, RecordRow)> = Vec::new();
    let mut notes = Vec::new();
    let mut in_error = 0;
    let mut aside = 0;
    let selected = people::selected(h, person)?;
    for p in &selected {
        let idx = folder.index(p)?;
        if kinds.contains(&"result") {
            aside += idx.about_another.len();
        }
        let from = Sources::of(&h.sources, &idx);
        let asked =
            |r: &Record| range.holds(r.date.as_deref()) && code.is_none_or(|q| matches_code(r, q));
        let st = Standings::of(&h.sources, p, &idx);
        for kind in kinds {
            if !standing::KINDS.contains(kind) {
                let errors = st.in_error.get(*kind);
                for r in idx.records.iter().filter(|r| r.kind == *kind && asked(r)) {
                    if errors.is_some_and(|e| e.contains(&r.id)) {
                        in_error += 1;
                    } else if !active || !is_ended(r.status.as_deref()) {
                        out.push((0, row(from, p, r.clone())));
                    }
                }
                continue;
            }
            let list = st.list(&idx, kind, active, today, &asked, &|_| true);
            for r in list.rows {
                if r.error {
                    in_error += 1;
                    continue;
                }
                let mut one = row(from, p, r.record.clone());
                if r.standing.is_some() {
                    one.standing = r.standing;
                    one.says = r.says;
                }
                out.push((r.group, one));
            }
            notes.extend(list.notes.into_iter().map(|n| match selected.len() {
                1 => n,
                _ => format!("{}: {n}", crate::printable(&p.name)),
            }));
        }
    }
    out.sort_by(|(g, a), (k, b)| {
        let date = || b.date.cmp(&a.date).then(a.display.cmp(&b.display));
        g.cmp(k).then_with(date)
    });
    if in_error > 0 {
        notes.push(standing::in_error(in_error));
    }
    notes.extend(about::line(aside, person));
    Ok((out.into_iter().map(|(_, r)| r).collect(), notes))
}

/// `q` is the record's code (`718-7`, `J45.909`), or words of its name
/// (`hemoglobin`), case-insensitive.
pub(crate) fn matches_code(r: &Record, q: &str) -> bool {
    let q = q.trim().to_lowercase();
    if q.is_empty() {
        return true;
    }
    let code = r.code.as_ref();
    if code
        .and_then(|c| c.code.as_deref())
        .is_some_and(|c| c.to_lowercase() == q)
    {
        return true;
    }
    let names = [
        Some(r.display.as_str()),
        code.and_then(|c| c.display.as_deref()),
    ];
    names
        .into_iter()
        .flatten()
        .any(|n| n.to_lowercase().contains(&q))
}

pub(crate) fn is_ended(status: Option<&str>) -> bool {
    matches!(
        status,
        Some(
            "completed"
                | "stopped"
                | "inactive"
                | "resolved"
                | "remission"
                | "entered-in-error"
                | "cancelled"
                | "refuted"
        )
    )
}

fn row(from: Sources, p: &Person, r: Record) -> RecordRow {
    let mut sources: Vec<String> = r
        .provenance
        .iter()
        .filter_map(|pv| from.label(pv))
        .collect();
    sources.dedup();
    let detail = record_detail(&r).filter(|d| Some(d) != r.status.as_ref());
    let (standing, says) = standing::differ(&standing::record_says(from, &r));
    RecordRow {
        detail,
        id: r.id,
        person: p.id.clone(),
        person_name: p.name.clone(),
        kind: r.kind,
        date: r.date,
        display: r.display,
        status: r.status,
        code: r.code.and_then(|c| c.code),
        sources,
        standing,
        says,
        fields: r.fields,
        documents: r.documents,
    }
}

#[derive(Debug, Serialize)]
pub struct Hit {
    pub person: String,
    pub person_name: String,
    /// `document`, `note`, `study`, or a record kind.
    pub kind: String,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    pub title: String,
    pub snippet: String,
    /// Matching terms counted over the item; higher first.
    pub score: usize,
}

/// Queries that name a kind of record rather than words in one: "blood
/// tests" finds the lab results, which never say so. Folded, words joined
/// by one space.
const LAB_QUERIES: &[&str] = &[
    "blood test",
    "blood tests",
    "bloodwork",
    "blood work",
    "labs",
    "lab work",
    "lab test",
    "lab tests",
    "lab results",
];

/// Whether a record is one a lab query names: a result, a report, or a
/// claim with a lab line.
pub(crate) fn is_lab(r: &Record) -> bool {
    match r.kind.as_str() {
        "result" | "report" => true,
        "claim" => r
            .fields
            .get("items")
            .and_then(Value::as_array)
            .is_some_and(|a| {
                a.iter()
                    .any(|i| i.get("category").and_then(Value::as_str) == Some("lab"))
            }),
        _ => false,
    }
}

/// Every item whose text holds all the query's words (case, accents and
/// punctuation ignored), best first. A query naming lab tests
/// (`LAB_QUERIES`) also finds every result, report and claim for lab work,
/// after the items whose text holds it.
pub fn search(
    folder: &Folder,
    h: &Household,
    person: Option<&str>,
    query: &str,
    range: &Range,
    limit: usize,
) -> Result<(Vec<Hit>, Vec<String>), CliError> {
    range.validate()?;
    let errors = entered_in_error(folder, h, person)?;
    let error = |x: &Hit| errors.contains(&x.id).then_some(Why::Error);
    let whole = Parts::default();
    let (hits, left_out) =
        search_leaving_out(folder, h, person, query, range, limit, &whole, &error)?;
    Ok((hits, left_out.notes()))
}

/// The selected people's records entered in error, which no list shows.
fn entered_in_error(
    folder: &Folder,
    h: &Household,
    person: Option<&str>,
) -> Result<BTreeSet<String>, CliError> {
    let mut out = BTreeSet::new();
    for p in people::selected(h, person)? {
        let st = Standings::of(&h.sources, p, &folder.index(p)?);
        out.extend(st.errors().cloned());
    }
    Ok(out)
}

/// Why a list leaves an item out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Why {
    Private,
    /// Entered in error.
    Error,
    /// A bill entered in error with a payment the household marked.
    PaidError,
}

/// Where the items a list left out sat in it, so that each page can say
/// how many of them it skipped.
#[derive(Debug, Clone, Default)]
pub struct LeftOut {
    /// The whole list's length, before anything was left out.
    pub len: usize,
    /// The place in the whole list of each item kept, in order.
    pub kept_at: Vec<usize>,
    /// The places of the items left out.
    pub held_at: Vec<(usize, Why)>,
}

impl LeftOut {
    /// What a command's list says of those it left out.
    fn notes(&self) -> Vec<String> {
        match self.left_out(Why::Error) {
            0 => Vec::new(),
            n => vec![standing::in_error(n)],
        }
    }

    /// How many items were left out for `why`.
    fn left_out(&self, why: Why) -> usize {
        self.held_at.iter().filter(|(_, w)| *w == why).count()
    }

    /// Sorts `items` out by `held`, keeping their order: what is kept, and
    /// where each was.
    pub fn split<T>(
        items: Vec<T>,
        mut held: impl FnMut(&mut T) -> Option<Why>,
    ) -> (Vec<T>, LeftOut) {
        let mut out = LeftOut {
            len: items.len(),
            ..Default::default()
        };
        let mut kept = Vec::new();
        for (i, mut x) in items.into_iter().enumerate() {
            if let Some(why) = held(&mut x) {
                out.held_at.push((i, why));
            } else {
                out.kept_at.push(i);
                kept.push(x);
            }
        }
        (kept, out)
    }

    /// How many items left out for `why` sort within a page of the kept items that
    /// starts at `offset` and holds `returned`: after the previous page's
    /// last item, up to this page's last, or to the end on the `last` page.
    /// The pages' counts add up to all of them.
    pub fn on_page(&self, why: Why, offset: usize, returned: usize, last: bool) -> usize {
        // A page past the kept items (a cursor from before the list
        // shrank) follows the true last page, which counted the rest.
        if offset > 0 && offset >= self.kept_at.len() {
            return 0;
        }
        let from = offset
            .checked_sub(1)
            .and_then(|i| self.kept_at.get(i))
            .map_or(0, |i| i + 1);
        let to = if last {
            self.len
        } else {
            (offset + returned)
                .checked_sub(1)
                .and_then(|i| self.kept_at.get(i))
                .map_or(self.len, |i| i + 1)
        };
        self.held_at
            .iter()
            .filter(|&&(i, w)| w == why && i >= from && i < to)
            .count()
    }
}

/// As `search`, leaving out the hits `leave_out` names, and those that only
/// the private `parts` hold the words of, before the limit: the hits, and
/// where those left out sat among them.
#[allow(clippy::too_many_arguments)]
pub fn search_leaving_out(
    folder: &Folder,
    h: &Household,
    person: Option<&str>,
    query: &str,
    range: &Range,
    limit: usize,
    parts: &Parts,
    leave_out: &dyn Fn(&Hit) -> Option<Why>,
) -> Result<(Vec<Hit>, LeftOut), CliError> {
    range.validate()?;
    let terms: Vec<String> = classify::fold(query)
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect();
    if terms.is_empty() {
        return Err(CliError::Usage("search needs at least one word".into()));
    }
    let labs = LAB_QUERIES.contains(&terms.join(" ").as_str());
    let mut hits = Vec::new();
    let mut held: BTreeSet<String> = BTreeSet::new();
    for p in people::selected(h, person)? {
        let idx = folder.index(p)?;
        let texts = folder.text(p)?;
        let notes = folder.notes(p)?;
        for s in searchables(&idx, &texts, &notes, labs, parts) {
            if !range.holds(s.date.as_deref()) {
                continue;
            }
            let found = |hay: &str, lab: &Option<String>| {
                let lab = || lab.clone().map(|snippet| (0, snippet));
                matches(hay, &terms).or_else(lab)
            };
            let Some(whole) = found(&s.hay(), &s.lab) else {
                continue;
            };
            let open = s.open.as_ref();
            let part = open.map(|o| found(&o.hay(&s), &o.lab));
            if part == Some(None) {
                held.insert(s.id.clone());
            }
            let (score, snippet) = part.flatten().unwrap_or(whole);
            hits.push(Hit {
                person: p.id.clone(),
                person_name: p.name.clone(),
                title: open.map_or(s.title, |o| o.title.clone()),
                kind: s.kind,
                id: s.id,
                date: s.date,
                snippet,
                score,
            });
        }
    }
    hits.sort_by(|a, b| b.score.cmp(&a.score).then(b.date.cmp(&a.date)));
    let (mut hits, left_out) = LeftOut::split(hits, |x| {
        let held = held.contains(&x.id).then_some(Why::Private);
        held.or_else(|| leave_out(x))
    });
    hits.truncate(limit.max(1));
    Ok((hits, left_out))
}

/// One item `search` looks through: its title and the text after it.
pub(crate) struct Searchable {
    pub kind: String,
    pub id: String,
    pub date: Option<String>,
    pub title: String,
    pub body: String,
    /// For a lab record when lab queries are wanted: what a lab query
    /// shows for it (its detail line).
    pub lab: Option<String>,
    /// What is left of it without its private parts, where it has some.
    pub open: Option<OpenPart>,
}

/// What is left of an item to search without its private parts.
pub(crate) struct OpenPart {
    pub title: String,
    /// What is left out of its text, the title's first.
    pub cuts: Vec<Place>,
    pub lab: Option<String>,
}

impl OpenPart {
    /// The text searched: what is left of the item's, after its title where
    /// that is another. None where the cuts are not of its text.
    pub fn hay(&self, whole: &Searchable) -> String {
        let hay = whole.hay();
        let Some(left) = parts::around(&hay, &self.cuts) else {
            return String::new();
        };
        match self.cuts.first() {
            Some([0, _]) => format!("{}\n{}", self.title, left.concat()),
            _ => left.concat(),
        }
    }
}

impl Searchable {
    /// The text searched: the title, then the body.
    pub fn hay(&self) -> String {
        format!("{}\n{}", self.title, self.body)
    }
}

/// What `search` looks through in one person's files, in their order:
/// documents, records, studies, then notes. `labs` fills each lab
/// record's `lab`, `parts` what is left of each that has some.
pub(crate) fn searchables(
    idx: &PersonIndex,
    texts: &PersonText,
    notes: &[Note],
    labs: bool,
    parts: &Parts,
) -> Vec<Searchable> {
    let mut out = Vec::new();
    for d in &idx.documents {
        let title = d
            .title
            .clone()
            .or_else(|| d.original_name.clone())
            .unwrap_or_else(|| label(d.doc_type.as_deref()).into());
        let meta = format!(
            "{} {}",
            d.organization.as_deref().unwrap_or(""),
            label(d.doc_type.as_deref())
        );
        let body = texts.texts.get(&d.id).map(String::as_str).unwrap_or("");
        let at = title.len() + 1 + meta.len() + 1;
        let cuts: Vec<Place> = parts.places(&d.id);
        out.push(Searchable {
            kind: "document".into(),
            id: d.id.clone(),
            date: d.date.clone(),
            open: (!cuts.is_empty()).then(|| OpenPart {
                title: title.clone(),
                cuts: cuts.into_iter().map(|[s, n]| [at + s, n]).collect(),
                lab: None,
            }),
            title,
            body: format!("{meta}\n{body}"),
            lab: None,
        });
    }
    for r in &idx.records {
        let lab = |r: &Record| (labs && is_lab(r)).then(|| record_detail(r).unwrap_or_default());
        let left = parts.record(r);
        let held = |k: &str, i: usize| parts.leaves_out(&r.id, k, i);
        let mut body = String::new();
        let mut cuts: Vec<Place> = vec![[0, r.display.len() + 1]];
        for (k, v) in &r.fields {
            let at = r.display.len() + 1;
            let mut piece = |v: &Value, cut: bool| {
                let start = body.len();
                flat_text(v, &mut body);
                if cut && body.len() > start {
                    cuts.push([at + start, body.len() - start]);
                }
            };
            match v {
                Value::Array(list) => {
                    for (i, v) in list.iter().enumerate() {
                        piece(v, held(k, i));
                    }
                }
                // Without those lines it is another, or there is none.
                _ => piece(v, left.fields.get(k) != Some(v)),
            }
        }
        out.push(Searchable {
            kind: r.kind.clone(),
            id: r.id.clone(),
            date: r.date.clone(),
            title: r.display.clone(),
            body,
            lab: lab(r),
            open: parts.has(&r.id).then(|| OpenPart {
                title: left.display.clone(),
                cuts,
                lab: lab(&left),
            }),
        });
    }
    for s in &idx.studies {
        let body = format!(
            "{} {} {} {}",
            s.modalities.join(" "),
            s.body_part.as_deref().unwrap_or(""),
            s.facility.as_deref().unwrap_or(""),
            s.series
                .iter()
                .filter_map(|x| x.description.as_deref())
                .collect::<Vec<_>>()
                .join(" ")
        );
        out.push(Searchable {
            kind: "study".into(),
            id: s.id.clone(),
            date: s.date.clone(),
            title: s
                .description
                .clone()
                .unwrap_or_else(|| "Imaging study".into()),
            body,
            lab: None,
            open: None,
        });
    }
    for n in notes {
        // The title is the text's first line; the rest is the body.
        let rest: String = n.text.trim().lines().skip(1).collect::<Vec<_>>().join("\n");
        out.push(Searchable {
            kind: "note".into(),
            id: n.id.clone(),
            date: Some(n.date.clone()),
            title: first_line(&n.text, 80),
            body: format!("{}\n{rest}", n.tags.join(" ")),
            lab: None,
            open: None,
        });
    }
    out
}

/// All terms present (each as a word prefix): the count of their
/// occurrences and a snippet around the first.
pub(crate) fn matches(hay: &str, terms: &[String]) -> Option<(usize, String)> {
    let sq = classify::Squashed::new(hay);
    let mut score = 0;
    let mut first = usize::MAX;
    for t in terms {
        let found = sq.find_prefix(t);
        if found.is_empty() {
            return None;
        }
        score += found.len();
        first = first.min(found[0]);
    }
    let floor = |mut i: usize| {
        i = i.min(hay.len());
        while !hay.is_char_boundary(i) {
            i -= 1;
        }
        i
    };
    let start = floor(first.saturating_sub(60));
    let end = floor(first.saturating_add(100));
    let mut snippet = hay[start..end]
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if start > 0 {
        snippet.insert(0, '…');
    }
    if end < hay.len() {
        snippet.push('…');
    }
    Some((score, snippet))
}

#[derive(Debug, Serialize)]
pub struct DocumentRow {
    pub id: String,
    pub person: String,
    pub person_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub organization: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub path: String,
    pub size: u64,
    pub media_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pages: Option<u32>,
    /// `fhir`, `inbox`, `drive`, `file`.
    pub origin: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub near_duplicates: Vec<String>,
    /// A bill's statement (statement date, amount due, due date, account).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub statement: Option<crate::model::Statement>,
    /// The bill filed before that is the same statement.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub same_statement_as: Option<String>,
    /// That its sources differ on whether it is entered in error, and what
    /// each says.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub standing: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub says: Vec<String>,
}

/// The papers of the people selected, newest first, and what the list says
/// of those it does not show.
pub fn documents(
    folder: &Folder,
    h: &Household,
    person: Option<&str>,
    doc_type: Option<&str>,
    range: &Range,
) -> Result<(Vec<DocumentRow>, Vec<String>), CliError> {
    range.validate()?;
    if let Some(t) = doc_type {
        if !classify::is_doc_type(t) {
            return Err(CliError::Usage(format!("`{t}` is not a document type")));
        }
    }
    let mut out = Vec::new();
    let mut in_error = 0;
    let mut aside = 0;
    for p in people::selected(h, person)? {
        let idx = folder.index(p)?;
        aside += idx.about_another.len();
        let from = Sources::of(&h.sources, &idx);
        let asked = |d: &&Document| {
            doc_type.is_none_or(|t| d.doc_type.as_deref() == Some(t))
                && range.holds(d.date.as_deref())
        };
        let papers = idx.documents.iter().filter(asked);
        in_error += papers.filter(|d| standing::document_in_error(d)).count();
        for d in standing::index(&idx).documents.iter().filter(asked) {
            let d = d.clone();
            let (standing, says) = standing::differ(&standing::document_says(from, &d));
            out.push(DocumentRow {
                id: d.id,
                person: p.id.clone(),
                person_name: p.name.clone(),
                date: d.date,
                doc_type: d.doc_type,
                organization: d.organization,
                title: d.title.or(d.original_name),
                path: d.path,
                size: d.size,
                media_type: d.media_type,
                pages: d.pages,
                origin: d.origin.kind,
                near_duplicates: d.near_duplicates.into_iter().map(|n| n.document).collect(),
                statement: d.statement,
                same_statement_as: d.same_statement_as,
                standing,
                says,
            });
        }
    }
    out.sort_by(|a, b| b.date.cmp(&a.date).then(a.path.cmp(&b.path)));
    let notes = (in_error > 0).then(|| standing::in_error(in_error));
    let notes = notes.into_iter().chain(about::line(aside, person));
    Ok((out, notes.collect()))
}

/// What a reader says of a paper opened by its id that a source says is
/// entered in error, with what the household decided of it, which is kept and
/// not applied. Nothing of any other paper.
pub fn paper_note(
    folder: &Folder,
    h: &Household,
    p: &Person,
    d: &Document,
) -> Result<Option<String>, CliError> {
    let idx = folder.index(p)?;
    let note = standing::paper_note(Sources::of(&h.sources, &idx), d);
    if note.is_none() || !standing::document_in_error(d) {
        return Ok(note);
    }
    let decisions = folder.overrides(p).map(|o| o.events).unwrap_or_default();
    let decisions = decisions.iter().filter(|x| x.members.contains(&d.id));
    let marks = folder.marks_if_readable(p)?;
    let labels = marks.iter().flat_map(|m| &m.providers);
    let labels = labels.filter(|c| c.item.as_ref() == Some(&d.id) && c.kind().is_some());
    let bills = crate::bills::of_person(folder, h, p)?;
    let payments = crate::bills::not_applied_on(&bills, &d.id);
    let kept: Vec<String> = [
        (
            decisions.count(),
            "decision",
            "decisions",
            "about its event",
        ),
        (payments, "payment", "payments", "on it"),
        (
            labels.count(),
            "correction",
            "corrections",
            "of a provider it names",
        ),
    ]
    .into_iter()
    .filter(|(n, ..)| *n > 0)
    .map(|(n, one, many, of)| format!("{n} {} {of}", if n == 1 { one } else { many }))
    .collect();
    Ok(note.map(|note| match kept.is_empty() {
        true => note,
        false => format!("{note} Kept and not applied: {}.", kept.join(", ")),
    }))
}

/// A record by id, whatever lists leave it out, and what is said of one about
/// somebody else.
pub fn record(
    folder: &Folder,
    h: &Household,
    id: &str,
) -> Result<(RecordRow, Option<String>), CliError> {
    for p in &h.people {
        let idx = folder.whole_index(p)?;
        if let Some(r) = idx.records.iter().find(|r| r.id == id) {
            let one = row(Sources::of(&h.sources, &idx), p, r.clone());
            return Ok((one, about::of_record(p, &idx, r)));
        }
    }
    Err(CliError::NotFound(format!("no record `{id}`")))
}

/// A document by id, with whose it is. `aside`: at the Mac, where one that is
/// about somebody else is opened too.
pub fn find_document(
    folder: &Folder,
    h: &Household,
    id: &str,
    aside: bool,
) -> Result<(Person, crate::model::Document), CliError> {
    for p in &h.people {
        let idx = match aside {
            true => folder.whole_index(p)?,
            false => folder.index(p)?,
        };
        if let Some(d) = idx.documents.into_iter().find(|d| d.id == id) {
            return Ok((p.clone(), d));
        }
    }
    Err(CliError::NotFound(format!(
        "no document `{id}`; `medrec documents list` shows them"
    )))
}

#[derive(Debug, Serialize)]
pub struct NoteRow {
    pub person: String,
    pub person_name: String,
    #[serde(flatten)]
    pub note: Note,
}

pub fn notes(
    folder: &Folder,
    h: &Household,
    person: Option<&str>,
    tag: Option<&str>,
    range: &Range,
) -> Result<Vec<NoteRow>, CliError> {
    range.validate()?;
    let mut out = Vec::new();
    for p in people::selected(h, person)? {
        for n in folder.notes(p)? {
            if tag.is_some_and(|t| !n.tags.iter().any(|x| x.eq_ignore_ascii_case(t)))
                || !range.holds(Some(&n.date))
            {
                continue;
            }
            out.push(NoteRow {
                person: p.id.clone(),
                person_name: p.name.clone(),
                note: n,
            });
        }
    }
    out.sort_by(|a, b| b.note.date.cmp(&a.note.date));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What is left of an item by cuts that are not of its text is nothing.
    #[test]
    fn cuts_of_another_text_leave_nothing_to_find() {
        let of = |cuts: Vec<Place>| {
            let open = OpenPart {
                title: "Visit".into(),
                cuts,
                lab: None,
            };
            let item = Searchable {
                kind: "claim".into(),
                id: "r_example".into(),
                date: None,
                title: "Visit and look".into(),
                body: "one two".into(),
                lab: None,
                open: None,
            };
            open.hay(&item)
        };
        assert_eq!(of(vec![[0, 15], [19, 3]]), "Visit\none ");
        assert_eq!(of(vec![[19, 3]]), "Visit and look\none ");
        for cuts in [
            vec![[0, 15], [19, 30]],
            vec![[40, 1]],
            vec![[19, 3], [0, 15]],
        ] {
            assert_eq!(of(cuts), "");
        }
    }

    #[test]
    fn each_page_counts_what_was_left_out_between_it_and_the_last() {
        // Six items, the second and the last two left out; pages of two.
        let (kept, out) = LeftOut::split((0..6).collect::<Vec<_>>(), |i| {
            [1, 4, 5].contains(i).then_some(Why::Private)
        });
        assert_eq!(kept, vec![0, 2, 3]);
        assert_eq!(out.on_page(Why::Private, 0, 2, false), 1);
        assert_eq!(out.on_page(Why::Private, 2, 1, true), 2);
        // One page holding all: all of them.
        assert_eq!(out.on_page(Why::Private, 0, 3, true), 3);
        // Nothing kept: the one page counts everything.
        let (_, none) = LeftOut::split(vec![1, 2], |_| Some(Why::Private));
        assert_eq!(none.on_page(Why::Private, 0, 0, true), 2);
        // A page past the end, from a stale cursor, counts nothing again.
        assert_eq!(out.on_page(Why::Private, 3, 0, true), 0);
        assert_eq!(out.on_page(Why::Private, 5, 0, true), 0);
        assert_eq!(none.on_page(Why::Private, 2, 0, true), 0);
        // Each reason is counted apart.
        let (_, two) = LeftOut::split((0..4).collect::<Vec<_>>(), |i| match i {
            1 => Some(Why::Error),
            2 => Some(Why::Private),
            _ => None,
        });
        assert_eq!(two.on_page(Why::Error, 0, 2, true), 1);
        assert_eq!(two.on_page(Why::Private, 0, 2, true), 1);
        // A command's note is of those entered in error only.
        assert_eq!(two.notes(), ["1 entered in error, not shown"]);
        assert!(out.notes().is_empty());
    }

    #[test]
    fn a_date_is_cut_by_characters_and_an_unknown_kind_is_refused() {
        let r = Range {
            since: Some("2026-01-01".into()),
            until: None,
        };
        // Byte 10 falls inside `é`: slicing by bytes would panic.
        assert!(r.holds(Some("2026-03-0\u{e9}x")));
        assert!(r.holds(Some("2026-03-04T10:00:00Z")));
        assert!(!r.holds(Some("2025-12-31")));
        assert!(validate_kinds(&["result".into(), "claim".into()]).is_ok());
        assert!(matches!(
            validate_kinds(&["results".into()]),
            Err(CliError::Usage(_))
        ));
    }

    #[test]
    fn all_terms_must_match_and_the_snippet_surrounds_the_first() {
        let terms = vec!["hemoglobin".to_string(), "a1c".to_string()];
        let (score, snip) = matches(
            "Results: Hemoglobin A1c 5.4 %, normal. Hemoglobin again",
            &terms,
        )
        .unwrap();
        assert_eq!(score, 3);
        assert!(snip.contains("Hemoglobin A1c"));
        assert!(matches("Hemoglobin only", &terms).is_none());
    }

    #[test]
    fn ranges_include_their_ends_and_hide_undated_items() {
        let r = Range {
            since: Some("2026-01-01".into()),
            until: Some("2026-03-04".into()),
        };
        assert!(r.holds(Some("2026-03-04T10:00:00Z")));
        assert!(!r.holds(Some("2025-12-31")));
        assert!(!r.holds(None));
        assert!(Range::default().holds(None));
    }
}
