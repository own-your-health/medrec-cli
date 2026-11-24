//! What a bill's statement says, read from its text: the statement date, the
//! amount due, the due date, the account number and the number to call; and
//! a receipt's, which adds what was paid and when. Intake reads a statement
//! the user files, and an import one a billing CLI exports, the same way, so
//! two copies of one statement (a scan and the portal's PDF) are known to be
//! one ([`same_statement`]).
//!
//! A field is read only where the text labels it, from the rest of the
//! label's line or, when the label heads a column (`pdftotext -layout`), from
//! the value under it on the next line.

use std::sync::OnceLock;

use regex::Regex;

use crate::intake::classify::all_dates;
use crate::model::{Amount, Document, PersonIndex, PersonText, Statement};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    StatementDate,
    AmountDue,
    DueDate,
    Account,
    AmountPaid,
    PaidOn,
    Phone,
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
    ("amount paid", Field::AmountPaid),
    ("payment amount", Field::AmountPaid),
    ("paid amount", Field::AmountPaid),
    ("total paid", Field::AmountPaid),
    ("you paid", Field::AmountPaid),
    ("payment date", Field::PaidOn),
    ("date of payment", Field::PaidOn),
    ("date paid", Field::PaidOn),
    ("paid on", Field::PaidOn),
    ("questions", Field::Phone),
    ("call us at", Field::Phone),
    ("call", Field::Phone),
    ("customer service", Field::Phone),
    ("phone number", Field::Phone),
    ("phone", Field::Phone),
    ("telephone", Field::Phone),
    ("tel", Field::Phone),
];

/// The word just before a phone label that makes it someone's own number,
/// not the one to call (`Patient phone`, `Home phone`).
const NOT_THE_OFFICE: &[&str] = &["patient", "guarantor", "home", "cell", "mobile", "your"];

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
                Field::AmountPaid => st.amount_paid.is_none(),
                Field::PaidOn => st.paid_on.is_none(),
                Field::Phone => st.phone.is_none(),
            };
            if !slot {
                continue;
            }
            if label.field == Field::Phone {
                let word: String = line[..label.start]
                    .iter()
                    .rev()
                    .skip_while(|c| c.is_whitespace())
                    .take_while(|c| c.is_alphabetic())
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                if NOT_THE_OFFICE.contains(&word.as_str()) {
                    continue;
                }
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
                (Field::AmountPaid, Some(Value::Amount(a))) => st.amount_paid = Some(a),
                (Field::PaidOn, Some(Value::Date(d))) => st.paid_on = Some(d),
                (Field::Phone, Some(Value::Phone(n))) => st.phone = Some(n),
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

/// What follows a label on its line: past whitespace, a colon, and a dash
/// or dot standing alone (`Amount due - $12.00`), so a credit's sign
/// (`-$12.00`) stays; up to the next gap of two or more spaces.
fn segment_after(rest: &[char]) -> String {
    let mut start = 0;
    while let Some(&c) = rest.get(start) {
        let alone =
            matches!(c, '-' | '.') && rest.get(start + 1).is_some_and(|n| n.is_whitespace());
        if c.is_whitespace() || c == ':' || alone {
            start += 1;
        } else {
            break;
        }
    }
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
    Phone(String),
}

fn parse(field: Field, s: &str) -> Option<Value> {
    if s.is_empty() {
        return None;
    }
    match field {
        Field::StatementDate | Field::DueDate | Field::PaidOn => all_dates(s)
            .into_iter()
            .find(|(_, _, at)| *at <= 2)
            .map(|(iso, _, _)| Value::Date(iso)),
        Field::AmountDue | Field::AmountPaid => amount(s).map(Value::Amount),
        Field::Account => account(s).map(Value::Account),
        Field::Phone => phone(s).map(Value::Phone),
    }
}

/// A US phone number anywhere in `s` (`(555) 010-0123`, `1-555-010-0100`,
/// `555.010.0123`), as `NNN-NNN-NNNN`; never part of a longer run of digits.
fn phone(s: &str) -> Option<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(
            r"(?:^|[^\d])(?:\+?1[\s.\-]?)?\(?(\d{3})\)?[\s.\-]?(\d{3})[\s.\-](\d{4})(?:$|[^\d])",
        )
        .unwrap()
    });
    let c = re.captures(s)?;
    Some(format!("{}-{}-{}", &c[1], &c[2], &c[3]))
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
pub(crate) fn same_account(a: &str, b: &str) -> bool {
    let plain = |s: &str| -> String {
        s.chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '*')
            .collect()
    };
    let (a, b) = (plain(a), plain(b));
    if a == b {
        return true;
    }
    // A mask is asterisks anywhere, or a leading run of two or more `X`s.
    let masked = |s: &str| s.contains('*') || s.starts_with("XX");
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

/// What a document of `doc_type` keeps of its text's statement: a bill's
/// and a receipt's, and only a receipt says what was paid (a bill's
/// `Payment date` is an earlier payment's, not this paper's).
fn statement_for(doc_type: Option<&str>, text: &str) -> Option<Statement> {
    let mut st = read(text)?;
    match doc_type {
        Some("receipt") => {}
        Some("bill") => {
            st.amount_paid = None;
            st.paid_on = None;
        }
        _ => return None,
    }
    (st != Statement::default()).then_some(st)
}

/// Give a bill or a receipt its statement, read from `text`, and point a
/// bill at the bill filed before that is the same statement, if one is.
/// The papers filed before it get theirs first ([`backfill`]).
pub fn annotate(doc: &mut Document, text: &str, index: &mut PersonIndex, texts: &PersonText) {
    if !matches!(doc.doc_type.as_deref(), Some("bill" | "receipt")) {
        return;
    }
    backfill(index, texts);
    let Some(st) = statement_for(doc.doc_type.as_deref(), text) else {
        return;
    };
    if doc.doc_type.as_deref() == Some("bill") {
        doc.same_statement_as = first_same(&index.documents, &doc.id, &st);
    }
    doc.statement = Some(st);
}

/// Bills and receipts read again from their stored text, in filing order:
/// one with no statement (filed before 0.2.9, or saved since by an older
/// medrec, which drops the fields it does not know) gets it, and a bill its
/// link to the bill filed before it that is the same statement; one with a
/// statement gains the fields it lacks, those a later medrec reads. A value
/// already there is kept. Returns how many changed.
pub fn backfill(index: &mut PersonIndex, texts: &PersonText) -> usize {
    let mut changed = 0;
    for i in 0..index.documents.len() {
        let d = &index.documents[i];
        let Some(st) = texts
            .texts
            .get(&d.id)
            .and_then(|t| statement_for(d.doc_type.as_deref(), t))
        else {
            continue;
        };
        let merged = match &d.statement {
            None => st,
            Some(old) => Statement {
                statement_date: old.statement_date.clone().or(st.statement_date),
                amount_due: old.amount_due.clone().or(st.amount_due),
                due_date: old.due_date.clone().or(st.due_date),
                account: old.account.clone().or(st.account),
                amount_paid: old.amount_paid.clone().or(st.amount_paid),
                paid_on: old.paid_on.clone().or(st.paid_on),
                phone: old.phone.clone().or(st.phone),
            },
        };
        let same = (d.doc_type.as_deref() == Some("bill") && d.same_statement_as.is_none())
            .then(|| first_same(&index.documents[..i], &d.id, &merged))
            .flatten();
        let d = &mut index.documents[i];
        if d.statement.as_ref() == Some(&merged) && same.is_none() {
            continue;
        }
        if same.is_some() {
            d.same_statement_as = same;
        }
        d.statement = Some(merged);
        changed += 1;
    }
    changed
}

/// The first bill in `docs`, other than `id`, that is the same statement as
/// `st`: the one its chain starts with.
fn first_same(docs: &[Document], id: &str, st: &Statement) -> Option<String> {
    docs.iter()
        .filter(|d| d.id != id && d.doc_type.as_deref() == Some("bill"))
        .find(|d| d.statement.as_ref().is_some_and(|s| same_statement(s, st)))
        .map(|d| d.same_statement_as.clone().unwrap_or_else(|| d.id.clone()))
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
        let mut due_later = b.clone();
        due_later.due_date = Some("2026-12-15".into());
        assert!(!same_statement(&a, &due_later), "a due date that differs");
        let mut undated = b.clone();
        undated.statement_date = None;
        assert!(!same_statement(&a, &undated), "no statement date");
        assert!(!same_statement(&undated, &a), "no statement date");
    }

    #[test]
    fn a_label_followed_by_text_takes_no_value_from_the_next_line() {
        assert_eq!(read("Please pay online\n$5.00\n"), None);
    }

    #[test]
    fn a_credit_read_from_a_line_keeps_its_sign() {
        for text in [
            "Statement date: 11/02/2026\nBalance due: -$12.00\n",
            "Statement date: 11/02/2026\nBalance due: ($12.00)\n",
            "Statement date: 11/02/2026\nBalance due - -12.00\n",
        ] {
            assert_eq!(read(text).unwrap().amount_due, usd("-12.00"), "{text}");
        }
        let plain = read("Balance due - $12.00\n").unwrap();
        assert_eq!(plain.amount_due, usd("12.00"), "a dash standing alone");
    }

    #[test]
    fn a_receipt_has_no_amount_due() {
        let st = read(RECEIPT).unwrap();
        assert_eq!(st.amount_due, None);
        assert_eq!(st.account.as_deref(), Some("EX-000123"));
    }

    #[test]
    fn a_receipt_says_what_was_paid_when_and_whom_to_call() {
        let st = statement_for(Some("receipt"), RECEIPT).unwrap();
        assert_eq!(st.amount_paid, usd("120.00"));
        assert_eq!(st.paid_on.as_deref(), Some("2026-11-20"));
        assert_eq!(st.phone.as_deref(), Some("555-010-0145"));
    }

    #[test]
    fn a_bill_gives_the_number_to_call_and_no_payment() {
        let st = statement_for(Some("bill"), INLINE).unwrap();
        assert_eq!(
            st.phone.as_deref(),
            Some("555-010-0123"),
            "not the patient's own"
        );
        assert_eq!(
            statement_for(Some("bill"), COLUMNS).unwrap().phone,
            None,
            "a phone label with no number"
        );
        // A bill's payment lines are an earlier payment's.
        let st = statement_for(
            Some("bill"),
            "Statement date: 11/02/2026\nPayment date: 10/01/2026\nAmount paid: $20.00\nBalance due: $50.00\n",
        )
        .unwrap();
        assert_eq!((st.amount_paid, st.paid_on), (None, None));
        assert_eq!(st.amount_due, usd("50.00"));
        assert_eq!(statement_for(Some("letter"), INLINE), None);
    }

    #[test]
    fn a_phone_is_ten_digits_in_any_of_the_usual_ways() {
        for text in [
            "(555) 010-0123",
            "1-555-010-0123",
            "555.010.0123",
            "+1 555 010 0123",
            "us at 555-010-0123 today",
        ] {
            assert_eq!(phone(text).as_deref(), Some("555-010-0123"), "{text}");
        }
        for text in ["EX-000123", "12555-010-01234", "11/20/2026", "5550100123"] {
            assert_eq!(phone(text), None, "{text}");
        }
    }

    #[test]
    fn a_paper_read_before_gains_the_fields_it_lacks_and_keeps_its_own() {
        let mut index = PersonIndex::new("p_test");
        let mut texts = no_texts();
        let mut old = bill("d_old");
        old.statement = Some(Statement {
            statement_date: Some("2026-11-02".into()),
            amount_due: usd("1000.00"),
            ..Statement::default()
        });
        index.documents.push(old);
        texts.texts.insert("d_old".into(), INLINE.into());
        let mut receipt = bill("d_receipt");
        receipt.doc_type = Some("receipt".into());
        index.documents.push(receipt);
        texts.texts.insert("d_receipt".into(), RECEIPT.into());
        assert_eq!(backfill(&mut index, &texts), 2);
        let st = index.documents[0].statement.as_ref().unwrap();
        assert_eq!(st.amount_due, usd("1000.00"), "its own value stays");
        assert_eq!(st.phone.as_deref(), Some("555-010-0123"));
        assert_eq!(st.amount_paid, None);
        let r = index.documents[1].statement.as_ref().unwrap();
        assert_eq!(
            (r.amount_paid.clone(), r.paid_on.as_deref()),
            (usd("120.00"), Some("2026-11-20"))
        );
        assert_eq!(index.documents[1].same_statement_as, None);
        assert_eq!(backfill(&mut index, &texts), 0, "nothing left to read");
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
        assert!(
            !same_account("AXX1234", "BXX1234"),
            "letters inside a number are no mask"
        );
        assert!(!same_account("AXX1234", "ZZ01234"));
    }

    fn bill(id: &str) -> Document {
        serde_json::from_value(serde_json::json!({
            "id": id, "path": format!("Robin/Claims/2026/{id}.pdf"), "sha256": id, "md5": id,
            "size": 1, "media_type": "application/pdf", "doc_type": "bill",
            "filed_at": "2026-11-21T00:00:00Z",
            "origin": {"kind": "file", "action": "copied"},
            "provenance": {"source": {"kind": "scan"}, "method": "file",
                           "filed_at": "2026-11-21T00:00:00Z", "filed_by": "medrec 0.2.9"}
        }))
        .unwrap()
    }

    fn no_texts() -> PersonText {
        PersonText {
            schema: "medrec-text/v1".into(),
            person: "p_test".into(),
            texts: Default::default(),
        }
    }

    #[test]
    fn copies_of_one_statement_point_at_the_first_and_old_bills_are_read_again() {
        let mut index = PersonIndex::new("p_test");
        let mut texts = no_texts();
        // Filed before 0.2.9: a bill with its text and no statement.
        index.documents.push(bill("d_old"));
        texts.texts.insert("d_old".into(), INLINE.into());
        let mut second = bill("d_second");
        annotate(&mut second, COLUMNS, &mut index, &texts);
        assert_eq!(
            index.documents[0]
                .statement
                .as_ref()
                .and_then(|s| s.account.as_deref()),
            Some("****0123"),
            "the old bill is read again"
        );
        assert_eq!(second.same_statement_as.as_deref(), Some("d_old"));
        index.documents.push(second);
        // A third copy points at the first, not at the second.
        let mut third = bill("d_third");
        annotate(&mut third, COLUMNS, &mut index, &texts);
        assert_eq!(third.same_statement_as.as_deref(), Some("d_old"));
        // Another document type is left alone.
        let mut letter = bill("d_letter");
        letter.doc_type = Some("letter".into());
        annotate(&mut letter, COLUMNS, &mut index, &texts);
        assert_eq!((letter.statement, letter.same_statement_as), (None, None));
    }

    #[test]
    fn a_bill_saved_by_an_older_medrec_gets_its_link_again() {
        let mut index = PersonIndex::new("p_test");
        let mut texts = no_texts();
        for (id, text) in [("d_a", INLINE), ("d_b", COLUMNS)] {
            index.documents.push(bill(id));
            texts.texts.insert(id.into(), text.into());
        }
        backfill(&mut index, &texts);
        assert!(index.documents[0].statement.is_some());
        assert_eq!(index.documents[0].same_statement_as, None);
        assert_eq!(index.documents[1].same_statement_as.as_deref(), Some("d_a"));
    }
}
