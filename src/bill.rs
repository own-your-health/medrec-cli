//! What a bill's statement says, read from its text: the statement date, the
//! amount due, the due date and the account number. Intake reads a
//! statement the user files, and an import one a billing CLI exports, the
//! same way, so two copies of one statement (a scan and the portal's PDF)
//! are known to be one ([`same_statement`]).
//!
//! A field is read only where the text labels it, from the rest of the
//! label's line or, when the label heads a column (`pdftotext -layout`), from
//! the value under it on the next line.

use std::sync::OnceLock;

use regex::Regex;

use crate::intake::classify::all_dates;
use crate::model::{Amount, Document, PersonIndex, Statement};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    StatementDate,
    AmountDue,
    DueDate,
    Account,
}

/// Every label, lower-case. Where two match at one place, the longer wins,
/// so `please pay by` is a due date and `total amount due` one label.
const LABELS: &[(&str, Field)] = &[
    ("statement date", Field::StatementDate),
    ("date of statement", Field::StatementDate),
    ("billing date", Field::StatementDate),
    ("bill date", Field::StatementDate),
    ("total amount due", Field::AmountDue),
    ("amount due now", Field::AmountDue),
    ("amount due", Field::AmountDue),
    ("balance due", Field::AmountDue),
    ("total due", Field::AmountDue),
    ("pay this amount", Field::AmountDue),
    ("amount you owe", Field::AmountDue),
    ("please pay", Field::AmountDue),
    ("payment due date", Field::DueDate),
    ("due date", Field::DueDate),
    ("please pay by", Field::DueDate),
    ("pay by", Field::DueDate),
    ("payment due", Field::DueDate),
    ("due by", Field::DueDate),
    ("guarantor account number", Field::Account),
    ("guarantor account #", Field::Account),
    ("guarantor account", Field::Account),
    ("patient account number", Field::Account),
    ("account number", Field::Account),
    ("account no", Field::Account),
    ("account #", Field::Account),
    ("acct number", Field::Account),
    ("acct no", Field::Account),
    ("acct #", Field::Account),
];

/// A label on a line: its field and its columns, in characters.
struct Label {
    field: Field,
    start: usize,
    end: usize,
}

/// The statement a bill's text gives, or `None` when it labels nothing.
pub fn read(text: &str) -> Option<Statement> {
    let lines: Vec<Vec<char>> = text
        .lines()
        .map(|l| l.chars().map(|c| c.to_ascii_lowercase()).collect())
        .collect();
    let mut st = Statement::default();
    for (i, line) in lines.iter().enumerate() {
        let labels = labels(line);
        for (n, label) in labels.iter().enumerate() {
            let slot = match label.field {
                Field::StatementDate => st.statement_date.is_none(),
                Field::AmountDue => st.amount_due.is_none(),
                Field::DueDate => st.due_date.is_none(),
                Field::Account => st.account.is_none(),
            };
            if !slot {
                continue;
            }
            // The rest of the line up to the next label, else the value
            // under the label on the next line that has text.
            let until = labels.get(n + 1).map_or(line.len(), |l| l.start);
            let inline = segment_after(&line[label.end..until]);
            let below = || {
                lines[i + 1..]
                    .iter()
                    .find(|l| l.iter().any(|c| !c.is_whitespace()))
                    .and_then(|next| under(next, label))
            };
            // A label with text after it takes that text or nothing: only a
            // column's header reads the line below.
            let value = if inline.is_empty() {
                below().and_then(|s| parse(label.field, &s))
            } else {
                parse(label.field, &inline)
            };
            match (label.field, value) {
                (Field::StatementDate, Some(Value::Date(d))) => st.statement_date = Some(d),
                (Field::DueDate, Some(Value::Date(d))) => st.due_date = Some(d),
                (Field::AmountDue, Some(Value::Amount(a))) => st.amount_due = Some(a),
                (Field::Account, Some(Value::Account(a))) => st.account = Some(a),
                _ => {}
            }
        }
    }
    (st != Statement::default()).then_some(st)
}

/// The labels on a line, left to right, the longest at each place.
fn labels(line: &[char]) -> Vec<Label> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < line.len() {
        let boundary = i == 0 || !line[i - 1].is_ascii_alphanumeric();
        let hit = boundary
            .then(|| {
                LABELS
                    .iter()
                    .filter(|(l, _)| starts_with(&line[i..], l))
                    .filter(|(l, _)| {
                        let end = i + l.chars().count();
                        end >= line.len() || !line[end].is_ascii_alphanumeric()
                    })
                    .max_by_key(|(l, _)| l.len())
            })
            .flatten();
        match hit {
            Some((l, field)) => {
                let end = i + l.chars().count();
                out.push(Label {
                    field: *field,
                    start: i,
                    end,
                });
                i = end;
            }
            None => i += 1,
        }
    }
    out
}

fn starts_with(hay: &[char], needle: &str) -> bool {
    let n: Vec<char> = needle.chars().collect();
    hay.len() >= n.len() && hay[..n.len()] == n[..]
}

/// What follows a label on its line: past the separators, up to the next
/// gap of two or more spaces.
fn segment_after(rest: &[char]) -> String {
    let start = rest
        .iter()
        .position(|c| !(c.is_whitespace() || matches!(c, ':' | '-' | '.')))
        .unwrap_or(rest.len());
    let rest = &rest[start..];
    let end = (0..rest.len())
        .find(|&j| rest[j] == ' ' && rest.get(j + 1) == Some(&' '))
        .unwrap_or(rest.len());
    rest[..end].iter().collect::<String>().trim().to_string()
}

/// The text on `next` that sits under `label`: the run of text, between
/// gaps of two or more spaces, overlapping the label's columns the most.
fn under(next: &[char], label: &Label) -> Option<String> {
    let mut best: Option<(usize, String)> = None;
    let mut j = 0;
    while j < next.len() {
        if next[j].is_whitespace() {
            j += 1;
            continue;
        }
        let start = j;
        while j < next.len() && !(next[j] == ' ' && next.get(j + 1) == Some(&' ')) {
            j += 1;
        }
        let end = j;
        let overlap = end.min(label.end).saturating_sub(start.max(label.start));
        if overlap > 0 && best.as_ref().is_none_or(|(o, _)| overlap > *o) {
            best = Some((
                overlap,
                next[start..end]
                    .iter()
                    .collect::<String>()
                    .trim()
                    .to_string(),
            ));
        }
    }
    best.map(|(_, s)| s)
}

enum Value {
    Date(String),
    Amount(Amount),
    Account(String),
}

fn parse(field: Field, s: &str) -> Option<Value> {
    if s.is_empty() {
        return None;
    }
    match field {
        Field::StatementDate | Field::DueDate => all_dates(s)
            .into_iter()
            .find(|(_, _, at)| *at <= 2)
            .map(|(iso, _, _)| Value::Date(iso)),
        Field::AmountDue => amount(s).map(Value::Amount),
        Field::Account => account(s).map(Value::Account),
    }
}

/// `$1,234.56`, `-$12.00`, `($12.00)`: a credit is negative.
fn amount(s: &str) -> Option<Amount> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"^(\()?\s*(-)?\s*\$?\s*(-)?\s*(\d{1,3}(?:,\d{3})+|\d+)\.(\d{2})\b\s*(\))?")
            .unwrap()
    });
    let c = re.captures(s)?;
    let negative =
        c.get(2).is_some() || c.get(3).is_some() || (c.get(1).is_some() && c.get(6).is_some());
    let whole = c[4].replace(',', "");
    let whole = whole.trim_start_matches('0');
    let whole = if whole.is_empty() { "0" } else { whole };
    let sign = if negative && (whole != "0" || &c[5] != "00") {
        "-"
    } else {
        ""
    };
    Some(Amount {
        amount: format!("{sign}{whole}.{}", &c[5]),
        currency: "USD".into(),
    })
}

/// An account number: one token of letters, digits, dashes and a mask
/// (`*`, `x`), with three digits or more.
fn account(s: &str) -> Option<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"^#?\s*([a-z0-9*][a-z0-9*\-]{2,})").unwrap());
    let token = re.captures(s)?.get(1)?.as_str().trim_end_matches('-');
    (token.chars().filter(char::is_ascii_digit).count() >= 3).then(|| token.to_ascii_uppercase())
}

/// Whether two statements are one: the same statement date, nothing they
/// both give differing, and the account number or the amount due given by
/// both and the same.
pub fn same_statement(a: &Statement, b: &Statement) -> bool {
    let Some(date) = a.statement_date.as_ref() else {
        return false;
    };
    if b.statement_date.as_ref() != Some(date) {
        return false;
    }
    let account = a
        .account
        .as_ref()
        .zip(b.account.as_ref())
        .map(|(x, y)| same_account(x, y));
    let amount = a
        .amount_due
        .as_ref()
        .zip(b.amount_due.as_ref())
        .map(|(x, y)| x == y);
    let due = a
        .due_date
        .as_ref()
        .zip(b.due_date.as_ref())
        .map(|(x, y)| x == y);
    ![account, amount, due].contains(&Some(false))
        && (account == Some(true) || amount == Some(true))
}

/// The same account number, or a masked one (`****0123`, `XXXX0123`) whose
/// last four or more digits shown end the other.
fn same_account(a: &str, b: &str) -> bool {
    let plain = |s: &str| -> String {
        s.chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '*')
            .collect()
    };
    let (a, b) = (plain(a), plain(b));
    if a == b {
        return true;
    }
    let masked = |s: &str| s.contains('*') || s.contains("XX");
    let shown = |s: &str| -> String {
        let tail: String = s.chars().rev().take_while(char::is_ascii_digit).collect();
        tail.chars().rev().collect()
    };
    match (masked(&a), masked(&b)) {
        (true, false) => shown(&a).len() >= 4 && b.ends_with(&shown(&a)),
        (false, true) => shown(&b).len() >= 4 && a.ends_with(&shown(&b)),
        (true, true) => shown(&a).len() >= 4 && shown(&a) == shown(&b),
        (false, false) => false,
    }
}

/// Give a bill its statement, read from `text`, and point it at the bill
/// filed before that is the same statement, if one is.
pub fn annotate(doc: &mut Document, text: &str, index: &PersonIndex) {
    if doc.doc_type.as_deref() != Some("bill") {
        return;
    }
    let Some(st) = read(text) else { return };
    doc.same_statement_as = index
        .documents
        .iter()
        .filter(|d| d.id != doc.id && d.doc_type.as_deref() == Some("bill"))
        .find(|d| d.statement.as_ref().is_some_and(|s| same_statement(s, &st)))
        .map(|d| d.same_statement_as.clone().unwrap_or_else(|| d.id.clone()));
    doc.statement = Some(st);
}

#[cfg(test)]
mod tests {
    use super::*;

    const COLUMNS: &str = include_str!("../tests/fixtures/bills/statement-columns.txt");
    const INLINE: &str = include_str!("../tests/fixtures/bills/statement-inline.txt");
    const RECEIPT: &str = include_str!("../tests/fixtures/bills/receipt.txt");

    fn usd(a: &str) -> Option<Amount> {
        Some(Amount {
            amount: a.into(),
            currency: "USD".into(),
        })
    }

    #[test]
    fn a_statement_in_columns_reads_each_value_under_its_label() {
        let st = read(COLUMNS).unwrap();
        assert_eq!(st.statement_date.as_deref(), Some("2026-11-02"));
        assert_eq!(st.account.as_deref(), Some("EX-000123"));
        assert_eq!(st.amount_due, usd("1234.56"));
        assert_eq!(st.due_date.as_deref(), Some("2026-12-01"));
    }

    #[test]
    fn a_statement_with_a_field_a_line_reads_each_from_its_line() {
        let st = read(INLINE).unwrap();
        assert_eq!(st.statement_date.as_deref(), Some("2026-11-02"));
        assert_eq!(st.account.as_deref(), Some("****0123"));
        assert_eq!(st.amount_due, usd("1234.56"));
        // "Please pay by" is the due date, not an amount.
        assert_eq!(st.due_date.as_deref(), Some("2026-12-01"));
    }

    #[test]
    fn a_scan_and_the_portals_pdf_of_one_statement_are_one() {
        let (a, b) = (read(COLUMNS).unwrap(), read(INLINE).unwrap());
        assert!(same_statement(&a, &b));
        assert!(same_statement(&b, &a));
        let mut later = b.clone();
        later.statement_date = Some("2026-12-02".into());
        assert!(!same_statement(&a, &later), "another month's statement");
        let mut other = b.clone();
        other.amount_due = usd("99.00");
        assert!(!same_statement(&a, &other), "an amount that differs");
        let dated_only = Statement {
            statement_date: a.statement_date.clone(),
            ..Statement::default()
        };
        assert!(
            !same_statement(&a, &dated_only),
            "a date alone is not enough"
        );
    }

    #[test]
    fn a_receipt_has_no_amount_due() {
        let st = read(RECEIPT).unwrap();
        assert_eq!(st.amount_due, None);
        assert_eq!(st.account.as_deref(), Some("EX-000123"));
    }

    #[test]
    fn amounts_keep_their_sign_and_two_places() {
        for (text, want) in [
            ("$1,234.56", "1234.56"),
            ("0.00", "0.00"),
            ("-$12.00", "-12.00"),
            ("($12.00)", "-12.00"),
            ("$ 007.50 now", "7.50"),
        ] {
            assert_eq!(
                amount(text).map(|a| a.amount).as_deref(),
                Some(want),
                "{text}"
            );
        }
        assert_eq!(amount("online at example.org"), None);
        assert_eq!(amount("1234"), None, "no cents: an id, not an amount");
    }

    #[test]
    fn masked_accounts_match_by_their_last_digits() {
        assert!(same_account("****0123", "EX-000123"));
        assert!(same_account("XXXX0123", "****0123"));
        assert!(!same_account("****0124", "EX-000123"));
        assert!(!same_account("**23", "EX-000123"), "two digits are too few");
    }
}
