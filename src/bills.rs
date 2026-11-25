//! Bills (`views/<person id>/bills.json`, `medrec-bills/v1`): each bill
//! with the claim and event it bills, what was paid against it (receipts,
//! and payments the household marked), whether it is settled, the bills and
//! desk payments worth a call, and the shares the claims say are owed that
//! no bill or payment has met yet. Built from the person's documents, their
//! events (`crate::events`) and the payments in their `marks.json`.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::claims::{decimal, units};
use crate::events::same_org;
use crate::marks::{failed, Failed, READ, RECORDS, WRITE};
use crate::model::{Amount, Document, Person, PersonIndex, PersonText};

pub const SCHEMA: &str = "medrec-bills/v1";
/// How long after a bill's date a receipt can be its payment.
const RECEIPT_DAYS: i64 = 120;

/// A payment the household marked on a bill: "Paid".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Payment {
    pub id: String,
    /// The bill (`d_…`).
    pub document: String,
    /// `YYYY-MM-DD`.
    pub on: String,
    pub amount: Amount,
    pub at: String,
    /// Who marked it, as shown (a request's `requested_by`, never trusted).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
    /// The `_requests/` id it came from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<String>,
}

/// A payment the household took back (`marks.json` `payments_undone`), by
/// the request it came from: served again, that request records none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Undone {
    pub request: String,
    pub at: String,
    /// What a later medrec writes here, kept as it is.
    #[serde(flatten)]
    pub other: serde_json::Map<String, Value>,
}

/// What became of a payment to mark.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Marked {
    Recorded(Payment),
    Already(Payment),
    /// Its request recorded it before, and the household took it back.
    TakenBack,
}

/// A bill's own facts, from its document and statement.
struct Bill<'a> {
    doc: &'a Document,
    date: Option<String>,
    due: Option<(i128, String)>,
    copies: Vec<String>,
}

fn amount_units(a: &Amount) -> Option<(i128, String)> {
    Some((units(&a.amount)?, a.currency.clone()))
}

fn day(s: &str) -> Option<i64> {
    crate::events::day(s)
}

/// An event member's field as text.
fn s<'a>(m: &'a Value, k: &str) -> Option<&'a str> {
    m.get(k).and_then(Value::as_str)
}

/// The amount a claim member says is owed, in units.
fn owed(m: &Value) -> Option<i128> {
    s(m, "you_owe").and_then(units)
}

fn currency_of(ev: &Value) -> String {
    ev.pointer("/money/currency")
        .and_then(Value::as_str)
        .unwrap_or("USD")
        .to_string()
}

fn claims_of(ev: &Value) -> impl Iterator<Item = &Value> {
    ev.get("members")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|m| s(m, "kind") == Some("claim") && m.get("record").is_some())
}

fn holds(ev: &Value, doc: &str) -> Option<Value> {
    ev.get("members")
        .and_then(Value::as_array)?
        .iter()
        .find(|m| {
            s(m, "document") == Some(doc)
                || m.get("copies")
                    .and_then(Value::as_array)
                    .is_some_and(|c| c.iter().any(|x| x.as_str() == Some(doc)))
        })
        .cloned()
}

/// Whether two papers come from one biller: the same organization, or the
/// same account number where both give one.
fn same_biller(a: &Document, b: &Document) -> bool {
    let account = |d: &Document| d.statement.as_ref().and_then(|s| s.account.clone());
    if let (Some(x), Some(y)) = (account(a), account(b)) {
        return crate::bill::same_account(&x, &y);
    }
    match (a.organization.as_deref(), b.organization.as_deref()) {
        (Some(x), Some(y)) => same_org(x, y),
        _ => false,
    }
}

fn money(u: i128) -> String {
    decimal(u)
}

/// The person's `bills.json` from their index and its texts, their built
/// events (`crate::events::build`) and the payments the household marked.
pub fn build(
    p: &Person,
    idx: &PersonIndex,
    texts: &PersonText,
    events: &Value,
    payments: &[Payment],
) -> Value {
    let docs = &idx.documents;
    let evs: Vec<&Value> = events
        .get("events")
        .and_then(Value::as_array)
        .map(|a| a.iter().collect())
        .unwrap_or_default();

    // Bills: each statement once, its later copies listed with it.
    let mut bills: Vec<Bill> = docs
        .iter()
        .filter(|d| d.doc_type.as_deref() == Some("bill") && d.same_statement_as.is_none())
        .map(|d| Bill {
            doc: d,
            date: d
                .statement
                .as_ref()
                .and_then(|s| s.statement_date.clone())
                .or_else(|| d.date.clone()),
            due: d
                .statement
                .as_ref()
                .and_then(|s| s.amount_due.as_ref())
                .and_then(amount_units),
            copies: docs
                .iter()
                .filter(|c| c.same_statement_as.as_deref() == Some(d.id.as_str()))
                .map(|c| c.id.clone())
                .collect(),
        })
        .collect();
    bills.sort_by(|a, b| b.date.cmp(&a.date).then(a.doc.id.cmp(&b.doc.id)));
    let first_of: BTreeMap<&str, &str> = bills
        .iter()
        .flat_map(|b| {
            std::iter::once((b.doc.id.as_str(), b.doc.id.as_str())).chain(
                b.copies
                    .iter()
                    .map(move |c| (c.as_str(), b.doc.id.as_str())),
            )
        })
        .collect();

    // Receipts: to the bill they pay, else to the visit they were paid at.
    struct Receipt<'a> {
        doc: &'a Document,
        on: Option<String>,
        paid: (i128, String),
        bill: Option<&'a str>,
        event: Option<usize>,
    }
    let mut receipts: Vec<Receipt> = Vec::new();
    for d in docs
        .iter()
        .filter(|d| d.doc_type.as_deref() == Some("receipt"))
    {
        let Some(paid) = d
            .statement
            .as_ref()
            .and_then(|s| s.amount_paid.as_ref())
            .and_then(amount_units)
        else {
            continue;
        };
        let on = d
            .statement
            .as_ref()
            .and_then(|s| s.paid_on.clone())
            .or_else(|| d.date.clone());
        let on_day = on.as_deref().and_then(day);
        let cands: Vec<&Bill> = bills
            .iter()
            .filter(|b| same_biller(b.doc, d))
            .filter(|b| match (b.date.as_deref().and_then(day), on_day) {
                (Some(bd), Some(od)) => bd <= od && od <= bd + RECEIPT_DAYS,
                _ => false,
            })
            .collect();
        let equal: Vec<&&Bill> = cands
            .iter()
            .filter(|b| b.due == Some(paid.clone()))
            .collect();
        let bill = match (equal.as_slice(), cands.as_slice()) {
            ([b], _) => Some(b.doc.id.as_str()),
            ([], [b]) => Some(b.doc.id.as_str()),
            _ => None,
        };
        let event = if bill.is_some() {
            None
        } else {
            let org = d.organization.as_deref().unwrap_or("");
            on_day.and_then(|od| {
                let hits: Vec<usize> = evs
                    .iter()
                    .enumerate()
                    .filter(|(_, ev)| {
                        let (Some(st), Some(en)) =
                            (s(ev, "start").and_then(day), s(ev, "end").and_then(day))
                        else {
                            return false;
                        };
                        st <= od
                            && od <= en
                            && !org.is_empty()
                            && (s(ev, "place").is_some_and(|pl| same_org(org, pl))
                                || claims_of(ev)
                                    .filter_map(|m| s(m, "provider"))
                                    .any(|pr| same_org(org, pr)))
                    })
                    .map(|(i, _)| i)
                    .collect();
                match hits.as_slice() {
                    [i] => Some(*i),
                    _ => None,
                }
            })
        };
        receipts.push(Receipt {
            doc: d,
            on,
            paid,
            bill,
            event,
        });
    }

    let mut out_bills = Map::new();
    let mut calls = Vec::new();
    // The claims a bill or a payment has met: no longer "not billed yet".
    let mut met: BTreeSet<String> = BTreeSet::new();
    for b in &bills {
        let d = b.doc;
        let org = d.organization.as_deref().unwrap_or("");
        let held = evs
            .iter()
            .enumerate()
            .find_map(|(i, ev)| holds(ev, &d.id).map(|m| (i, m)));
        let (event, rule, claim) = match &held {
            Some((i, m)) => {
                let ev = evs[*i];
                let claims: Vec<&Value> = claims_of(ev).collect();
                let by_org: Vec<&&Value> = claims
                    .iter()
                    .filter(|c| {
                        s(c, "provider").is_some_and(|pr| !org.is_empty() && same_org(org, pr))
                    })
                    .collect();
                let claim = s(m, "matches")
                    .and_then(|r| claims.iter().find(|c| s(c, "record") == Some(r)).copied())
                    .or(match (by_org.as_slice(), claims.as_slice()) {
                        ([c], _) => Some(**c),
                        ([], [c]) => Some(*c),
                        _ => None,
                    });
                (Some(*i), s(m, "rule").map(str::to_string), claim)
            }
            None => (None, None, None),
        };
        if let Some(c) = claim {
            if let Some(r) = s(c, "record") {
                met.insert(r.to_string());
            }
        }
        if let Some(i) = event {
            // A bill from a provider meets that provider's claims in its event.
            for c in claims_of(evs[i]) {
                if s(c, "provider").is_some_and(|pr| !org.is_empty() && same_org(org, pr)) {
                    if let Some(r) = s(c, "record") {
                        met.insert(r.to_string());
                    }
                }
            }
        }
        let currency = b
            .due
            .as_ref()
            .map(|(_, c)| c.clone())
            .unwrap_or_else(|| "USD".into());
        let mut paid: Vec<Value> = Vec::new();
        let mut paid_u: i128 = 0;
        // A payment in another currency is listed, not counted.
        let mut count = |u: i128, c: &str, mut row: Value| {
            if c == currency {
                paid_u = paid_u.saturating_add(u);
            } else {
                row["counted"] = json!(false);
            }
            paid.push(row);
        };
        for r in receipts.iter().filter(|r| r.bill == Some(d.id.as_str())) {
            count(
                r.paid.0,
                &r.paid.1,
                json!({"from": "receipt", "document": r.doc.id, "on": r.on,
                       "amount": money(r.paid.0), "currency": r.paid.1}),
            );
        }
        for pm in payments
            .iter()
            .filter(|pm| first_of.get(pm.document.as_str()) == Some(&d.id.as_str()))
        {
            let Some((u, c)) = amount_units(&pm.amount) else {
                continue;
            };
            count(
                u,
                &c,
                json!({"from": "household", "id": pm.id, "on": pm.on,
                       "amount": money(u), "currency": c}),
            );
        }
        paid.sort_by(|a, b| {
            (s(a, "on"), s(a, "document").or(s(a, "id")))
                .cmp(&(s(b, "on"), s(b, "document").or(s(b, "id"))))
        });
        let superseded_by = bills
            .iter()
            .filter(|o| o.doc.id != d.id && o.date > b.date && same_biller(o.doc, d))
            .map(|o| o.doc.id.clone())
            .next();
        let mut row = Map::new();
        if !b.copies.is_empty() {
            row.insert("copies".into(), json!(b.copies));
        }
        let st = d.statement.as_ref();
        row.insert("organization".into(), json!(d.organization));
        row.insert("title".into(), json!(d.title));
        row.insert("date".into(), json!(b.date));
        row.insert("currency".into(), json!(currency));
        if let Some((u, _)) = &b.due {
            row.insert("amount_due".into(), json!(money(*u)));
        }
        if let Some(v) = st.and_then(|s| s.due_date.clone()) {
            row.insert("due_date".into(), json!(v));
        }
        if let Some(v) = st.and_then(|s| s.phone.clone()) {
            row.insert("phone".into(), json!(v));
        }
        if let Some(i) = event {
            row.insert("event".into(), json!(s(evs[i], "id")));
        }
        if let Some(r) = &rule {
            row.insert("rule".into(), json!(r));
        }
        if let Some(c) = claim {
            row.insert("claim".into(), json!(s(c, "record")));
            if let Some(v) = s(c, "you_owe") {
                row.insert("claim_you_owe".into(), json!(v));
            }
        }
        row.insert("paid".into(), json!(paid));
        row.insert("paid_total".into(), json!(money(paid_u)));
        let left = b.due.as_ref().map(|(due, _)| due.saturating_sub(paid_u));
        let settled = left.is_some_and(|l| l <= 0);
        if let Some(left) = left {
            row.insert("left".into(), json!(money(left.max(0))));
        }
        row.insert("settled".into(), json!(settled));
        if let Some(o) = &superseded_by {
            row.insert("superseded_by".into(), json!(o));
        }
        // What an insurer was billed for is not one no insurer is.
        let self_pay = texts
            .texts
            .get(&d.id)
            .filter(|_| claim.is_none())
            .and_then(|t| crate::bill::self_pay(d, t));
        if let Some(kind) = self_pay {
            row.insert("self_pay".into(), json!(kind));
        }
        out_bills.insert(d.id.clone(), Value::Object(row));

        // Worth a call: what the bill asks against what the claim says.
        let due = b
            .due
            .as_ref()
            .filter(|(_, c)| *c == currency)
            .map(|(u, _)| *u);
        let ev_id = event.and_then(|i| s(evs[i], "id"));
        let claim_owed = claim.and_then(owed);
        // A later statement from a biller repeats an earlier one's balance:
        // its claim is that bill's, not missing.
        let follows = bills
            .iter()
            .any(|o| o.doc.id != d.id && o.date < b.date && same_biller(o.doc, d));
        let reason = match (due, claim_owed, event) {
            (Some(due), Some(o), _) if due > o => Some("asks_more"),
            (Some(due), Some(o), _) if 0 < due && due < o => Some("asks_less"),
            (Some(due), None, ev)
                if due > 0
                    && !follows
                    && self_pay.is_none()
                    && ev.is_none_or(|i| claims_of(evs[i]).next().is_none()) =>
            {
                Some("no_claim")
            }
            _ => None,
        };
        // A settled bill asks for nothing more.
        if let Some(reason) = reason.filter(|_| !settled) {
            calls.push(json!({
                "reason": reason,
                "bill": d.id,
                "claim": claim.and_then(|c| s(c, "record")),
                "event": ev_id,
                "asked": due.map(money),
                "owed": claim_owed.map(money),
                "currency": currency,
            }));
        }
    }

    // Desk payments: paid at a visit, and more than its claims say is owed.
    let mut out_receipts = Vec::new();
    for r in &receipts {
        let mut row = json!({
            "document": r.doc.id,
            "organization": r.doc.organization,
            "on": r.on,
            "amount": money(r.paid.0),
            "currency": r.paid.1,
        });
        if let Some(b) = r.bill {
            row["bill"] = json!(b);
        }
        if let Some(i) = r.event {
            let ev = evs[i];
            row["event"] = json!(s(ev, "id"));
            let org = r.doc.organization.as_deref().unwrap_or("");
            let from_org: Vec<&Value> = claims_of(ev)
                .filter(|c| s(c, "provider").is_some_and(|pr| same_org(org, pr)))
                .collect();
            let these: Vec<&Value> = if from_org.is_empty() {
                claims_of(ev).collect()
            } else {
                from_org
            };
            for c in &these {
                if let Some(id) = s(c, "record") {
                    met.insert(id.to_string());
                }
            }
            let owed_sum: Option<i128> = these
                .iter()
                .try_fold(0i128, |sum, c| sum.checked_add(owed(c)?));
            if let (Some(o), false) = (owed_sum, these.is_empty()) {
                if r.paid.1 == currency_of(ev) && r.paid.0 > o {
                    calls.push(json!({
                        "reason": "desk_copay",
                        "bill": r.doc.id,
                        "claim": match these.as_slice() { [c] => s(c, "record"), _ => None },
                        "event": s(ev, "id"),
                        "asked": money(r.paid.0),
                        "owed": money(o),
                        "currency": r.paid.1,
                    }));
                }
            }
        }
        out_receipts.push(row);
    }
    out_receipts.sort_by(|a, b| {
        s(b, "on")
            .cmp(&s(a, "on"))
            .then(s(a, "document").cmp(&s(b, "document")))
    });

    // Not billed yet: each event's shares, by provider, nothing has met.
    let mut not_billed = Vec::new();
    for ev in &evs {
        let mut by_provider: BTreeMap<String, (Vec<String>, i128)> = BTreeMap::new();
        for c in claims_of(ev) {
            let (Some(id), Some(o)) = (s(c, "record"), owed(c)) else {
                continue;
            };
            if o <= 0 || met.contains(id) {
                continue;
            }
            let e = by_provider
                .entry(s(c, "provider").unwrap_or("").to_string())
                .or_default();
            e.0.push(id.to_string());
            e.1 = e.1.saturating_add(o);
        }
        for (provider, (claims, sum)) in by_provider {
            let fill = s(ev, "kind") == Some("fill")
                || claims_of(ev).any(|c| {
                    claims.iter().any(|x| s(c, "record") == Some(x)) && s(c, "role") == Some("fill")
                });
            let mut row = json!({
                "event": s(ev, "id"),
                "start": s(ev, "start"),
                "provider": if provider.is_empty() { Value::Null } else { json!(provider) },
                "claims": claims,
                "you_owe": money(sum),
                "currency": currency_of(ev),
            });
            if fill {
                row["fill"] = json!(true);
            }
            not_billed.push(row);
        }
    }

    json!({
        "schema": SCHEMA,
        "person": p.id,
        "bills": out_bills,
        "copy_of": first_of
            .iter()
            .filter(|(copy, first)| copy != first)
            .map(|(copy, first)| (copy.to_string(), json!(first)))
            .collect::<Map<String, Value>>(),
        "receipts": out_receipts,
        "worth_a_call": calls,
        "not_billed": not_billed,
    })
}

/// The pairs of documents a `bills.json` ties to one balance: a bill and its
/// copy, a bill and the one that supersedes it, a receipt and the bill it
/// pays.
pub fn ties(bills: &Value) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut pair = |a: &str, b: Option<&str>| out.extend(b.map(|b| (a.to_string(), b.to_string())));
    for (id, b) in bills["bills"].as_object().into_iter().flatten() {
        pair(id, b["superseded_by"].as_str());
        for c in b["copies"].as_array().into_iter().flatten() {
            pair(id, c.as_str());
        }
    }
    for r in bills["receipts"].as_array().into_iter().flatten() {
        if let Some(d) = r["document"].as_str() {
            pair(d, r["bill"].as_str());
        }
    }
    out
}

/// The bills a `bills.json` says are settled, by document id.
pub fn settled(bills: &Value) -> BTreeSet<String> {
    bills["bills"]
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(_, b)| b["settled"] == true)
        .map(|(id, _)| id.clone())
        .collect()
}

/// The person's `bills.json` as `medrec views write` would write it now.
pub fn of_person(
    folder: &crate::folder::Folder,
    h: &crate::model::Household,
    p: &Person,
) -> Result<Value, pk_cli_core::CliError> {
    let events = crate::events::of_person(folder, h, p)?;
    let idx = folder.index(p)?;
    // A marks file that does not read is left out, and the bills say so.
    let marks = folder.marks_if_readable(p)?;
    let mut v = build(
        p,
        &idx,
        &folder.text(p)?,
        &events,
        marks
            .as_ref()
            .map(|m| m.payments.as_slice())
            .unwrap_or_default(),
    );
    if marks.is_none() {
        v["marks_unreadable"] = Value::Bool(true);
    }
    Ok(v)
}

/// Mark a bill paid: `amount` on `on`. The same payment again (the same
/// bill, day and amount) changes nothing, and neither does a request whose
/// payment was taken back.
pub fn mark_paid(
    folder: &crate::folder::Folder,
    p: &Person,
    document: &str,
    on: &str,
    amount: &Amount,
    by: Option<&str>,
    request: Option<&str>,
) -> Result<Marked, Failed> {
    use pk_cli_core::CliError;
    if day(on).is_none() {
        return Err(Failed::Refused(CliError::Usage(format!(
            "`{}` is not a date (YYYY-MM-DD)",
            crate::printable(on)
        ))));
    }
    if !units(&amount.amount).is_some_and(|u| u > 0)
        || amount.amount.split('.').nth(1).is_some_and(|c| c.len() > 2)
    {
        return Err(Failed::Refused(CliError::Usage(format!(
            "`{}` is not an amount above zero with at most two places",
            crate::printable(&amount.amount)
        ))));
    }
    let idx = folder.index(p).map_err(failed(RECORDS))?;
    let Some(d) = idx.documents.iter().find(|d| d.id == document) else {
        return Err(Failed::Refused(CliError::NotFound(format!(
            "{} has no document {}",
            crate::printable(&p.name),
            crate::printable(document)
        ))));
    };
    if d.doc_type.as_deref() != Some("bill") {
        return Err(Failed::Refused(CliError::Usage(format!(
            "{} is not a bill",
            crate::printable(document)
        ))));
    }
    let amount = Amount {
        amount: decimal(units(&amount.amount).unwrap_or(0)),
        currency: amount.currency.clone(),
    };
    let mut o = folder.marks(p).map_err(failed(READ))?;
    if let Some(pm) = o
        .payments
        .iter()
        .find(|pm| pm.document == document && pm.on == on && pm.amount == amount)
    {
        return Ok(Marked::Already(pm.clone()));
    }
    if request.is_some_and(|r| o.payments_undone.iter().any(|u| u.request == r)) {
        return Ok(Marked::TakenBack);
    }
    let pm = Payment {
        id: crate::ids::new("o"),
        document: document.into(),
        on: on.into(),
        amount,
        at: crate::refresh::time::fmt(crate::refresh::time::now()),
        by: by.map(str::to_string),
        request: request.map(str::to_string),
    };
    o.payments.push(pm.clone());
    folder.save_marks(p, &o).map_err(failed(WRITE))?;
    Ok(Marked::Recorded(pm))
}

/// Drop a marked payment by its id; refused when the person has none of it.
pub fn undo(folder: &crate::folder::Folder, p: &Person, id: &str) -> Result<Payment, Failed> {
    use crate::refresh::time;
    let mut o = folder.marks(p).map_err(failed(READ))?;
    let Some(i) = o.payments.iter().position(|pm| pm.id == id) else {
        return Err(Failed::Refused(pk_cli_core::CliError::NotFound(format!(
            "no payment {}",
            crate::printable(id)
        ))));
    };
    let pm = o.payments.remove(i);
    let now = time::now();
    o.payments_undone
        .retain(|u| now - time::parse(&u.at).unwrap_or(0) < crate::marks::UNDONE_KEEP_SECS);
    if let Some(request) = &pm.request {
        o.payments_undone.push(Undone {
            request: request.clone(),
            at: time::fmt(now),
            other: serde_json::Map::new(),
        });
    }
    folder.save_marks(p, &o).map_err(failed(WRITE))?;
    Ok(pm)
}

#[cfg(test)]
mod tests {
    use super::*;

    const EVENTS: &str = include_str!("../tests/fixtures/bills/events.json");
    const PAPERS: &str = include_str!("../tests/fixtures/bills/papers.json");

    /// The scenario's papers as filed documents.
    fn index() -> PersonIndex {
        let mut idx = PersonIndex::new("p_test");
        let papers: Vec<Value> = serde_json::from_str(PAPERS).unwrap();
        for p in papers {
            let id = p["id"].as_str().unwrap();
            let mut d = json!({
                "id": id, "path": format!("Robin/Claims/2026/{id}.pdf"), "sha256": id, "md5": id,
                "size": 1, "media_type": "application/pdf", "doc_type": p["type"],
                "organization": p["org"], "date": p["date"], "statement": p["statement"],
                "filed_at": "2026-01-25T00:00:00Z",
                "origin": {"kind": "file", "action": "copied"},
                "provenance": {"source": {"kind": "scan"}, "method": "file",
                               "filed_at": "2026-01-25T00:00:00Z", "filed_by": "medrec 0.9.1"}
            });
            if let Some(s) = p.get("same_statement_as") {
                d["same_statement_as"] = s.clone();
            }
            idx.documents.push(serde_json::from_value(d).unwrap());
        }
        idx
    }

    fn person() -> Person {
        serde_json::from_value(json!({"id": "p_test", "name": "Robin Testperson",
            "folder": "Robin Testperson", "created_at": "2025-03-07T00:00:00Z"}))
        .unwrap()
    }

    fn bill<'a>(v: &'a Value, id: &str) -> &'a Value {
        v["bills"]
            .get(id)
            .unwrap_or_else(|| panic!("no bill {id}: {v}"))
    }

    fn row<'a>(v: &'a Value, list: &str, key: &str, id: &str) -> &'a Value {
        v[list]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r[key] == id)
            .unwrap_or_else(|| panic!("no {id} in {list}: {v}"))
    }

    #[test]
    fn each_bill_has_its_claim_what_was_paid_and_what_is_left() {
        let events: Value = serde_json::from_str(EVENTS).unwrap();
        let paid = Payment {
            id: "o_paidaaaaaaaaaaaaaaaa".into(),
            document: "d_readerbillaaaaaaaaaa".into(),
            on: "2024-11-12".into(),
            amount: Amount {
                amount: "12.00".into(),
                currency: "USD".into(),
            },
            at: "2024-11-12T12:00:00Z".into(),
            by: None,
            request: None,
        };
        // A payment marked in another currency is listed and not counted.
        let euros = Payment {
            id: "o_euroaaaaaaaaaaaaaaaa".into(),
            on: "2024-11-13".into(),
            amount: Amount {
                amount: "7.00".into(),
                currency: "EUR".into(),
            },
            ..paid.clone()
        };
        let v = build(
            &person(),
            &index(),
            &PersonText::new("p_test"),
            &events,
            &[paid, euros],
        );
        assert_eq!(v["schema"], SCHEMA);
        let order: Vec<&str> = v["bills"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            order,
            [
                "d_readerbillaaaaaaaaaa",
                "d_visionlateraaaaaaaaa",
                "d_scanbillaaaaaaaaaaaa",
                "d_visionbillaaaaaaaaaa",
                "d_allergybillaaaaaaaaa"
            ],
            "keyed by document, newest first, a copy only under its first"
        );
        assert_eq!(
            v["copy_of"],
            json!({"d_scancopyoneaaaaaaaaa": "d_scanbillaaaaaaaaaaaa",
                "d_scancopytwoaaaaaaaaa": "d_scanbillaaaaaaaaaaaa"})
        );

        // A statement whose claim D3s matched, paid by two receipts.
        let scan = bill(&v, "d_scanbillaaaaaaaaaaaa");
        assert_eq!(
            scan["copies"],
            json!(["d_scancopyoneaaaaaaaaa", "d_scancopytwoaaaaaaaaa"])
        );
        assert_eq!(
            (&scan["event"], &scan["rule"]),
            (&json!("e_scanaaaaaaaaaaaaaaaa"), &json!("D3s"))
        );
        assert_eq!(scan["claim"], "r_scanroomaaaaaaaaaaaa");
        assert_eq!(scan["phone"], "555-010-0155");
        let by: Vec<&str> = scan["paid"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["document"].as_str().unwrap())
            .collect();
        assert_eq!(by, ["d_scanpaidoneaaaaaaaaa", "d_scanpaidtwoaaaaaaaaa"]);
        assert_eq!(
            (&scan["paid_total"], &scan["left"], &scan["settled"]),
            (&json!("90.00"), &json!("0.00"), &json!(true))
        );

        // One that asks more than its claim's share, part of it marked paid.
        let reader = bill(&v, "d_readerbillaaaaaaaaaa");
        assert_eq!(
            reader["claim"], "r_scanreaderaaaaaaaaaa",
            "the event's claim from its biller"
        );
        assert_eq!(reader["paid"][0]["from"], "household");
        assert!(reader["paid"][0].get("counted").is_none());
        assert_eq!(
            (
                &reader["paid"][1]["currency"],
                &reader["paid"][1]["counted"]
            ),
            (&json!("EUR"), &json!(false))
        );
        assert_eq!(
            (&reader["paid_total"], &reader["left"], &reader["settled"]),
            (&json!("12.00"), &json!("18.00"), &json!(false))
        );

        // A first statement, and the later one from its biller.
        let vision = bill(&v, "d_visionbillaaaaaaaaaa");
        assert_eq!(vision["superseded_by"], "d_visionlateraaaaaaaaa");
        assert!(bill(&v, "d_visionlateraaaaaaaaa")
            .get("superseded_by")
            .is_none());

        // Worth a call, in the bills' order, then the desk payments.
        let calls: Vec<(&str, &str)> = v["worth_a_call"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| (c["reason"].as_str().unwrap(), c["bill"].as_str().unwrap()))
            .collect();
        assert_eq!(
            calls,
            [
                ("asks_more", "d_readerbillaaaaaaaaaa"),
                ("asks_less", "d_visionbillaaaaaaaaaa"),
                ("no_claim", "d_allergybillaaaaaaaaa"),
                ("desk_copay", "d_deskaaaaaaaaaaaaaaaa"),
            ],
            "a later statement repeating a balance is no missing claim"
        );
        let desk = row(&v, "worth_a_call", "reason", "desk_copay");
        assert_eq!(
            (&desk["asked"], &desk["owed"], &desk["claim"]),
            (
                &json!("25.00"),
                &json!("10.00"),
                &json!("r_rehabaaaaaaaaaaaaaaa")
            )
        );
        let more = row(&v, "worth_a_call", "reason", "asks_more");
        assert_eq!(
            (&more["asked"], &more["owed"]),
            (&json!("30.00"), &json!("22.00"))
        );

        // Receipts: to a bill, to a visit, or to neither.
        assert_eq!(
            row(&v, "receipts", "document", "d_scanpaidtwoaaaaaaaaa")["bill"],
            "d_scanbillaaaaaaaaaaaa"
        );
        assert_eq!(
            row(&v, "receipts", "document", "d_deskaaaaaaaaaaaaaaaa")["event"],
            "e_rehabaaaaaaaaaaaaaaa"
        );
        let stray = row(&v, "receipts", "document", "d_strayaaaaaaaaaaaaaaa");
        assert!(
            stray.get("bill").is_none() && stray.get("event").is_none(),
            "{stray}"
        );

        // Not billed yet: the shares nothing has met, summed by provider, a
        // fill marked.
        let nb: Vec<(&str, &str, Option<bool>)> = v["not_billed"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| {
                (
                    n["event"].as_str().unwrap(),
                    n["you_owe"].as_str().unwrap(),
                    n["fill"].as_bool(),
                )
            })
            .collect();
        assert_eq!(
            nb,
            [
                ("e_fillaaaaaaaaaaaaaaaa", "7.50", Some(true)),
                ("e_fillingaaaaaaaaaaaaa", "31.00", None),
            ]
        );
    }

    fn reasons(v: &Value) -> Vec<(&str, &str)> {
        v["worth_a_call"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| (c["bill"].as_str().unwrap(), c["reason"].as_str().unwrap()))
            .collect()
    }

    /// A bill no insurer is billed for has no claim, and isn't asked for one.
    #[test]
    fn a_self_pay_bill_is_not_missing_a_claim() {
        let events: Value = serde_json::from_str(EVENTS).unwrap();
        let (alone, claimed) = ("d_allergybillaaaaaaaaa", "d_readerbillaaaaaaaaaa");
        let says = |id: &str, text: &str| {
            let mut texts = PersonText::new("p_test");
            texts.texts.insert(id.into(), text.into());
            build(&person(), &index(), &texts, &events, &[])
        };
        for (text, kind) in [
            ("Sample storage fee\nAmount due $75.00", Some("storage")),
            ("Specimen\n  storage, 12 months", Some("storage")),
            ("Yearly tissue\nbank storage", Some("storage")),
            ("Insurance: Self-pay", Some("stated")),
            ("Financial class SELF PAY", Some("stated")),
            ("Self-pay account", Some("stated")),
            // What any bill may say, and what its other pages do.
            ("If you self pay, ask about our discounts", None),
            ("Office visit\n\u{c}Storage fee", None),
            ("Skin test", None),
        ] {
            let v = says(alone, text);
            assert_eq!(bill(&v, alone)["self_pay"].as_str(), kind, "{text}");
            let asked = reasons(&v).contains(&(alone, "no_claim"));
            assert_eq!(asked, kind.is_none(), "{text}");
        }
        // One with a claim was billed to an insurer, whatever it says.
        let v = says(claimed, "Insurance: Self-pay");
        assert!(bill(&v, claimed).get("self_pay").is_none());
        assert!(reasons(&v).contains(&(claimed, "asks_more")));
    }

    #[test]
    fn a_settled_bill_is_not_worth_a_call() {
        let events: Value = serde_json::from_str(EVENTS).unwrap();
        let no_text = PersonText::new("p_test");
        for (id, amount) in [
            ("d_allergybillaaaaaaaaa", "75.00"),
            ("d_readerbillaaaaaaaaaa", "30.00"),
            ("d_visionbillaaaaaaaaaa", "9.00"),
        ] {
            let paid = |amount: &str| {
                let p = Payment {
                    id: "o_paidaaaaaaaaaaaaaaaa".into(),
                    document: id.into(),
                    on: "2025-01-14".into(),
                    amount: Amount {
                        amount: amount.into(),
                        currency: "USD".into(),
                    },
                    at: "2025-01-14T10:00:00Z".into(),
                    by: None,
                    request: None,
                };
                build(&person(), &index(), &no_text, &events, &[p])
            };
            let part = paid("1.00");
            assert!(reasons(&part).iter().any(|(b, _)| *b == id), "{id}");
            let full = paid(amount);
            assert_eq!(bill(&full, id)["settled"], true);
            assert!(!reasons(&full).iter().any(|(b, _)| *b == id), "{id}");
        }
    }
}
