//! What one person's records hold at a glance: current medicines,
//! conditions and allergies, immunizations, the latest result of each test,
//! recent visits, and the bills on file with an amount due. Computed in
//! memory from the person's `index.json` and `notes.md`; nothing here writes.

use std::collections::BTreeMap;

use pk_cli_core::CliError;
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::Value;

use crate::folder::Folder;
use crate::intake::classify;
use crate::model::{Household, Person, PersonIndex, Record};
use crate::sources::Sources;
use crate::views::{is_ended, record_detail, record_source, value_text};

/// Longest name or detail a summary line carries, in characters.
pub const CLIP: usize = 200;

/// How many lines each list of a summary shows.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub per_list: usize,
}

/// One person, with what their records hold.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct PersonRow {
    pub id: String,
    pub name: String,
    /// `YYYY-MM-DD`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub birth_date: Option<String>,
    pub adult: bool,
    pub documents: usize,
    pub records: usize,
    pub studies: usize,
    pub notes: usize,
    /// The date of the newest dated document, record or study.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub newest: Option<String>,
    /// When records were last imported from a provider or insurer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_import: Option<String>,
}

/// A list, cut to a summary's length.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Listed<T> {
    pub items: Vec<T>,
    /// Entries left out for length.
    pub more: usize,
}

impl<T> Listed<T> {
    fn cut(mut all: Vec<T>, n: usize) -> Self {
        let more = all.len().saturating_sub(n);
        all.truncate(n);
        Listed { items: all, more }
    }
}

/// A medicine, condition, allergy or visit.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Line {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    /// A dose, a dispensed quantity, a visit's type.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// The organization or provider CLI it came from.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// The record's id (0.9.1 and later).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// For a condition: its key in `code-names.json` (0.9.4 and later).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name_key: Option<String>,
}

/// One vaccine: its doses on file and the last one.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Vaccine {
    pub vaccine: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    pub doses: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// The last dose's record id (0.9.1 and later).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

/// One lab result: its value as the lab reported it, with the lab's own
/// reference range and flag.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ResultRow {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub person: Option<String>,
    pub test: String,
    /// The test's code (LOINC, or the lab's own).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// The lab's reference range, as it wrote it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<String>,
    /// The lab's interpretation: high, low, abnormal, normal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Results of this test on file (in a list of tests).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub results: Option<usize>,
    /// The first result's date (in a list of tests).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_date: Option<String>,
    /// The record's id; of a test's latest result (0.9.1 and later).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

/// A bill on file with an amount due: the newest statement of its account.
/// medrec does not know whether it was paid.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Bill {
    /// The document's id.
    pub document: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub organization: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub statement_date: Option<String>,
    /// `123.45 USD`.
    pub amount_due: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub due_date: Option<String>,
    /// The due date is before today.
    pub past_due: bool,
    /// Statements of this account on file.
    pub statements: usize,
}

/// Where a person's records come from.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct SourceRow {
    pub source: String,
    pub records: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub newest: Option<String>,
}

/// One person's records at a glance.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Summary {
    pub person: PersonRow,
    /// The day it was computed, `YYYY-MM-DD`.
    pub as_of: String,
    /// Records per kind (`result`, `medication`, `encounter`, `claim`...).
    pub counts: BTreeMap<String, usize>,
    pub sources: Vec<SourceRow>,
    /// Medicines not marked stopped or completed, one per medicine, newest
    /// first.
    pub medications: Listed<Line>,
    /// Conditions not marked resolved or inactive.
    pub conditions: Listed<Line>,
    pub allergies: Listed<Line>,
    pub immunizations: Listed<Vaccine>,
    /// The latest result of each test, newest first.
    pub latest_results: Listed<ResultRow>,
    pub recent_visits: Listed<Line>,
    /// The newest statement of each billing account with an amount due.
    pub bills: Listed<Bill>,
}

/// Everyone in the household, with what their records hold.
pub fn people(folder: &Folder, h: &Household) -> Result<Vec<PersonRow>, CliError> {
    h.people
        .iter()
        .map(|p| Ok(person_row(p, &folder.index(p)?, folder.notes(p)?.len())))
        .collect()
}

pub fn person(
    folder: &Folder,
    h: &Household,
    p: &Person,
    today: &str,
    limits: Limits,
) -> Result<Summary, CliError> {
    let idx = folder.index(p)?;
    let notes = folder.notes(p)?.len();
    let settled = crate::bills::settled(&crate::bills::of_person(folder, h, p)?);
    Ok(of_index(h, p, &idx, notes, today, limits, &settled))
}

/// [`person`] from the person's index already read, with `notes` notes and
/// the bills `bills.json` says are settled.
pub(crate) fn of_index(
    h: &Household,
    p: &Person,
    idx: &PersonIndex,
    notes: usize,
    today: &str,
    limits: Limits,
    settled: &std::collections::BTreeSet<String>,
) -> Summary {
    let n = limits.per_list;
    let mut records: Vec<&Record> = idx.records.iter().collect();
    records.sort_by(|a, b| b.date.cmp(&a.date).then(a.display.cmp(&b.display)));

    let from = Sources::of(&h.sources, idx);
    let mut counts = BTreeMap::new();
    let mut sources: BTreeMap<String, SourceRow> = BTreeMap::new();
    for r in &records {
        *counts.entry(r.kind.clone()).or_insert(0) += 1;
        if let Some(s) = record_source(from, r) {
            let row = sources.entry(s.clone()).or_insert(SourceRow {
                source: s,
                records: 0,
                newest: None,
            });
            row.records += 1;
            let d = day(r.date.as_deref());
            if d > row.newest {
                row.newest = d;
            }
        }
    }
    let mut sources: Vec<SourceRow> = sources.into_values().collect();
    sources.sort_by(|a, b| b.records.cmp(&a.records).then(a.source.cmp(&b.source)));

    let current = |kind: &str| {
        let mut seen = std::collections::BTreeSet::new();
        records
            .iter()
            .filter(|r| r.kind == kind && !is_ended(r.status.as_deref()))
            .filter(|r| seen.insert(series_key(r)))
            .map(|r| line(from, r))
            .collect::<Vec<_>>()
    };

    Summary {
        person: person_row(p, idx, notes),
        as_of: today.to_string(),
        counts,
        sources,
        medications: Listed::cut(current("medication"), n),
        conditions: Listed::cut(current("condition"), n),
        allergies: Listed::cut(current("allergy"), n),
        immunizations: Listed::cut(vaccines(from, &records), n),
        latest_results: Listed::cut(latest_results(from, &records), n),
        recent_visits: Listed::cut(
            records
                .iter()
                .filter(|r| r.kind == "encounter")
                .map(|r| line(from, r))
                .collect(),
            n,
        ),
        bills: Listed::cut(bills(idx, today, settled), n),
    }
}

pub(crate) fn person_row(p: &Person, idx: &PersonIndex, notes: usize) -> PersonRow {
    let newest = idx
        .documents
        .iter()
        .map(|d| d.date.as_deref())
        .chain(idx.records.iter().map(|r| r.date.as_deref()))
        .chain(idx.studies.iter().map(|s| s.date.as_deref()))
        .filter_map(|d| day(d))
        .max();
    PersonRow {
        id: p.id.clone(),
        name: p.name.clone(),
        birth_date: p.birth_date.clone(),
        adult: p.adult,
        documents: idx.documents.len(),
        records: idx.records.len(),
        studies: idx.studies.len(),
        notes,
        newest,
        last_import: idx.imports.iter().map(|i| i.imported_at.clone()).max(),
    }
}

/// The day of an ISO date or date-time: its first ten characters.
pub(crate) fn day(d: Option<&str>) -> Option<String> {
    d.map(|d| d.chars().take(10).collect())
}

/// What makes two records the same test, medicine or vaccine: the code with
/// its system, else the name with case and accents folded.
pub fn series_key(r: &Record) -> String {
    match r.code.as_ref().and_then(|c| c.code.as_deref()) {
        Some(code) => format!(
            "{}|{code}",
            r.code
                .as_ref()
                .and_then(|c| c.system.as_deref())
                .unwrap_or("")
        ),
        None => format!("~{}", classify::fold(&r.display)),
    }
}

pub(crate) fn clip(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        format!("{}…", s.chars().take(max - 1).collect::<String>())
    } else {
        s.to_string()
    }
}

fn line(from: Sources, r: &Record) -> Line {
    Line {
        name: clip(&r.display, CLIP),
        date: day(r.date.as_deref()),
        detail: record_detail(r)
            .filter(|d| Some(d) != r.status.as_ref())
            .map(|d| clip(&d, CLIP)),
        status: r.status.clone(),
        source: record_source(from, r),
        id: Some(r.id.clone()),
        name_key: crate::code_names::condition_key(r),
    }
}

fn vaccines(from: Sources, records: &[&Record]) -> Vec<Vaccine> {
    let mut by: Vec<(String, Vaccine)> = Vec::new();
    for r in records.iter().filter(|r| r.kind == "immunization") {
        if matches!(r.status.as_deref(), Some("not-done" | "entered-in-error")) {
            continue;
        }
        let key = series_key(r);
        match by.iter_mut().find(|(k, _)| *k == key) {
            // Records are newest first, so the first seen is the last dose.
            Some((_, v)) => v.doses += 1,
            None => by.push((
                key,
                Vaccine {
                    vaccine: clip(&r.display, CLIP),
                    code: r.code.as_ref().and_then(|c| c.code.clone()),
                    doses: 1,
                    last_date: day(r.date.as_deref()),
                    source: record_source(from, r),
                    id: Some(r.id.clone()),
                },
            )),
        }
    }
    by.into_iter().map(|(_, v)| v).collect()
}

/// A result's value and unit as the lab reported them.
pub(crate) fn reading(r: &Record) -> (Option<String>, Option<String>) {
    let text = |v: &Value| Some(value_text(v)).filter(|s| !s.is_empty());
    match r.fields.get("value") {
        Some(Value::Object(o)) => (
            o.get("value").and_then(text),
            o.get("unit").and_then(Value::as_str).map(str::to_string),
        ),
        Some(v) => (text(v).map(|s| clip(&s, CLIP)), None),
        None => (None, None),
    }
}

/// One result as a row: the value, the lab's range and flag.
pub(crate) fn result_row(from: Sources, r: &Record) -> ResultRow {
    let (value, unit) = reading(r);
    let text = |k: &str| {
        r.fields
            .get(k)
            .map(value_text)
            .filter(|s| !s.is_empty())
            .map(|s| clip(&s, CLIP))
    };
    ResultRow {
        person: None,
        test: clip(&r.display, CLIP),
        code: r.code.as_ref().and_then(|c| c.code.clone()),
        date: r.date.clone(),
        value,
        unit,
        range: text("reference_range"),
        flag: text("interpretation"),
        source: record_source(from, r),
        results: None,
        first_date: None,
        id: Some(r.id.clone()),
    }
}

/// The latest result of each test, with how many are on file and since
/// when; `records` newest first.
pub(crate) fn latest_results(from: Sources, records: &[&Record]) -> Vec<ResultRow> {
    let mut by: Vec<(String, ResultRow)> = Vec::new();
    for r in records.iter().filter(|r| r.kind == "result") {
        let key = series_key(r);
        match by.iter_mut().find(|(k, _)| *k == key) {
            Some((_, row)) => {
                row.results = row.results.map(|n| n + 1);
                if r.date.is_some() {
                    row.first_date = day(r.date.as_deref());
                }
            }
            None => {
                let mut row = result_row(from, r);
                row.results = Some(1);
                row.first_date = day(r.date.as_deref());
                by.push((key, row));
            }
        }
    }
    by.into_iter().map(|(_, r)| r).collect()
}

/// The newest statement of each account with an amount due over zero,
/// unless it is settled.
fn bills(
    idx: &PersonIndex,
    today: &str,
    settled: &std::collections::BTreeSet<String>,
) -> Vec<Bill> {
    let mut docs: Vec<&crate::model::Document> = idx
        .documents
        .iter()
        .filter(|d| d.doc_type.as_deref() == Some("bill") && d.same_statement_as.is_none())
        .filter(|d| d.statement.is_some())
        .collect();
    let when = |d: &crate::model::Document| {
        let s = d.statement.as_ref();
        s.and_then(|s| s.statement_date.clone())
            .or_else(|| d.date.clone())
    };
    docs.sort_by_key(|d| std::cmp::Reverse(when(d)));
    let mut out: Vec<(String, Bill)> = Vec::new();
    for d in docs {
        let s = d.statement.as_ref().expect("filtered on a statement");
        let account = match &s.account {
            Some(a) => format!("{}|{a}", d.organization.as_deref().unwrap_or("")),
            None => format!("doc|{}", d.id),
        };
        if let Some((_, b)) = out.iter_mut().find(|(k, _)| *k == account) {
            b.statements += 1;
            continue;
        }
        let Some(due) = &s.amount_due else {
            out.push((account, unpaid_unknown(d)));
            continue;
        };
        out.push((
            account,
            Bill {
                document: d.id.clone(),
                title: d.title.clone().map(|t| clip(&t, CLIP)),
                organization: d.organization.clone(),
                statement_date: s.statement_date.clone(),
                amount_due: format!("{} {}", due.amount, due.currency),
                due_date: s.due_date.clone(),
                past_due: s.due_date.as_deref().is_some_and(|d| d < today),
                statements: 1,
            },
        ));
    }
    out.into_iter()
        .map(|(_, b)| b)
        .filter(|b| owes(&b.amount_due) && !settled.contains(&b.document))
        .collect()
}

/// A newest statement without an amount: kept as a marker so an older
/// statement of the account is not shown in its place, then dropped.
fn unpaid_unknown(d: &crate::model::Document) -> Bill {
    Bill {
        document: d.id.clone(),
        title: None,
        organization: None,
        statement_date: None,
        amount_due: String::new(),
        due_date: None,
        past_due: false,
        statements: 1,
    }
}

/// `amount_due` is over zero.
fn owes(amount_due: &str) -> bool {
    amount_due
        .split_whitespace()
        .next()
        .and_then(|a| a.replace(',', "").parse::<f64>().ok())
        .is_some_and(|a| a > 0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_amount_owed_is_over_zero_and_clipping_counts_characters() {
        assert!(owes("12.50 USD"));
        assert!(owes("1,204.00 USD"));
        assert!(!owes("0.00 USD"));
        assert!(!owes("-5.00 USD"));
        assert!(!owes(""));
        assert_eq!(clip("Hémoglobine", 4), "Hém…");
        assert_eq!(clip("A1c", 4), "A1c");
        assert_eq!(
            day(Some("2026-03-04T10:00:00Z")).as_deref(),
            Some("2026-03-04")
        );
    }
}

#[cfg(test)]
mod bill_tests {
    use super::*;

    fn bill(id: &str, account: &str, date: &str, amount: &str, due: &str, dup: bool) -> Value {
        serde_json::json!({
            "id": id, "path": format!("A/Claims/2026/{id}.pdf"), "sha256": "x", "md5": "x",
            "size": 1, "media_type": "application/pdf", "doc_type": "bill",
            "organization": "Example Clinic", "filed_at": "2026-01-01T00:00:00Z",
            "origin": {"kind": "file", "action": "copied"},
            "statement": {"statement_date": date, "due_date": due, "account": account,
                          "amount_due": {"amount": amount, "currency": "USD"}},
            "same_statement_as": if dup { Some("d_first") } else { None },
            "provenance": {"source": {"kind": "scan"}, "method": "intake",
                           "filed_at": "2026-01-01T00:00:00Z", "filed_by": "medrec 0.0.0"}
        })
    }

    #[test]
    fn a_bill_is_the_newest_statement_of_its_account_with_an_amount_due() {
        let idx: PersonIndex = serde_json::from_value(serde_json::json!({
            "schema": "medrec-index/v1", "person": "p_x", "rev": 1,
            "documents": [
                bill("d_jan", "****0123", "2026-01-05", "50.00", "2026-02-01", false),
                bill("d_feb", "****0123", "2026-02-05", "75.00", "2026-03-01", false),
                bill("d_feb2", "****0123", "2026-02-05", "75.00", "2026-03-01", true),
                bill("d_paid", "****0456", "2026-02-10", "0.00", "2026-03-10", false),
                bill("d_next", "****0789", "2026-11-01", "1,204.00", "2026-12-01", false),
            ],
        }))
        .unwrap();
        let b = bills(&idx, "2026-11-22", &Default::default());
        let got: Vec<(&str, &str, bool, usize)> = b
            .iter()
            .map(|b| {
                (
                    b.document.as_str(),
                    b.amount_due.as_str(),
                    b.past_due,
                    b.statements,
                )
            })
            .collect();
        assert_eq!(
            got,
            [
                ("d_next", "1,204.00 USD", false, 1),
                ("d_feb", "75.00 USD", true, 2),
            ]
        );
    }
}
