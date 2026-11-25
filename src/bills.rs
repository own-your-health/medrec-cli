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
use crate::standing;

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

impl Bill<'_> {
    fn papers(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.doc.id.as_str()).chain(self.copies.iter().map(String::as_str))
    }
}

/// Each statement of these papers once, its later copies listed with it.
fn bills_of(docs: &[Document]) -> Vec<Bill<'_>> {
    docs.iter()
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
        .collect()
}

/// What became of a person's paper or claim entered in error.
#[derive(Serialize)]
#[serde(tag = "as", rename_all = "snake_case")]
enum Gone<'a> {
    /// A bill every paper of which is, and the payments on it, which pay
    /// nothing else.
    Bill {
        #[serde(skip_serializing_if = "Vec::is_empty")]
        not_applied: Vec<Value>,
    },
    /// Another paper of a bill.
    Copy {
        bill: &'a str,
    },
    Receipt,
    Claim,
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

/// The bill of these a receipt pays, and whether to the cent.
fn pays<'a>(
    bills: &[&Bill<'a>],
    receipt: &Document,
    paid: &(i128, String),
    on: Option<i64>,
) -> Option<(&'a str, bool)> {
    let cands: Vec<&&Bill> = bills
        .iter()
        .filter(|b| same_biller(b.doc, receipt))
        .filter(|b| match (b.date.as_deref().and_then(day), on) {
            (Some(bd), Some(od)) => bd <= od && od <= bd + RECEIPT_DAYS,
            _ => false,
        })
        .collect();
    let equal: Vec<&&&Bill> = cands
        .iter()
        .filter(|b| b.due.as_ref() == Some(paid))
        .collect();
    match (equal.as_slice(), cands.as_slice()) {
        ([b], _) => Some((b.doc.id.as_str(), true)),
        ([], [b]) => Some((b.doc.id.as_str(), false)),
        _ => None,
    }
}

fn of_receipt(r: &Receipt) -> Value {
    json!({"from": "receipt", "document": r.doc.id, "on": r.on,
           "amount": money(r.paid.0), "currency": r.paid.1})
}

fn of_household(pm: &Payment, u: i128, currency: &str) -> Value {
    json!({"from": "household", "id": pm.id, "on": pm.on,
           "amount": money(u), "currency": currency})
}

/// In the order of a bill's payments.
fn in_order(rows: &mut [Value]) {
    rows.sort_by(|a, b| {
        (s(a, "on"), s(a, "document").or(s(a, "id")))
            .cmp(&(s(b, "on"), s(b, "document").or(s(b, "id"))))
    });
}

struct Receipt<'a> {
    doc: &'a Document,
    on: Option<String>,
    paid: (i128, String),
    bill: Option<&'a str>,
    /// The bill entered in error it paid.
    lost: Option<&'a str>,
    event: Option<usize>,
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
    table(p, &standing::index(idx), idx, texts, events, payments)
}

/// The pairs of [`ties`] with what is entered in error as well, of the events
/// as they are with it.
pub fn whole_ties(
    p: &Person,
    idx: &PersonIndex,
    texts: &PersonText,
    events: &Value,
) -> Vec<(String, String)> {
    ties(&table(p, idx, idx, texts, events, &[]))
}

/// The bills of the papers in `now`, of those the person has (`all`).
fn table(
    p: &Person,
    now: &PersonIndex,
    all: &PersonIndex,
    texts: &PersonText,
    events: &Value,
    payments: &[Payment],
) -> Value {
    let docs = &now.documents;
    let evs: Vec<&Value> = events
        .get("events")
        .and_then(Value::as_array)
        .map(|a| a.iter().collect())
        .unwrap_or_default();

    let mut bills = bills_of(docs);
    bills.sort_by(|a, b| b.date.cmp(&a.date).then(a.doc.id.cmp(&b.doc.id)));
    let first_of: BTreeMap<&str, &str> = bills
        .iter()
        .flat_map(|b| b.papers().map(|id| (id, b.doc.id.as_str())))
        .collect();

    // A payment is of the statement, whichever of its papers it is marked on;
    // one is entered in error when every paper of it is.
    let statements = bills_of(&all.documents);
    let statement_of: BTreeMap<&str, usize> = statements
        .iter()
        .enumerate()
        .flat_map(|(i, b)| b.papers().map(move |id| (id, i)))
        .collect();
    let mut bill_of: BTreeMap<usize, &str> = BTreeMap::new();
    for b in &bills {
        if let Some(i) = statement_of.get(b.doc.id.as_str()) {
            bill_of.entry(*i).or_insert(b.doc.id.as_str());
        }
    }
    let lost: Vec<&Bill> = statements
        .iter()
        .enumerate()
        .filter(|(i, _)| !bill_of.contains_key(i))
        .map(|(_, b)| b)
        .collect();
    let here: BTreeSet<&str> = docs
        .iter()
        .map(|d| d.id.as_str())
        .chain(now.records.iter().map(|r| r.id.as_str()))
        .collect();

    // Receipts: to the bill they pay, else to the visit they were paid at.
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
        // One entered in error takes from no bill that stands what is its to
        // the cent, and what may be its pays no other.
        let standing: Vec<&Bill> = bills.iter().collect();
        let exact = pays(&standing, d, &paid, on_day).filter(|(_, exact)| *exact);
        let of = exact.or_else(|| pays(&[&standing[..], &lost[..]].concat(), d, &paid, on_day));
        let (bill, lost) = match of {
            Some((id, _)) if lost.iter().any(|b| b.doc.id == id) => (None, Some(id)),
            of => (of.map(|(id, _)| id), None),
        };
        let event = if bill.is_some() || lost.is_some() {
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
            lost,
            event,
        });
    }

    // The payments the household marked on a statement.
    let marked = |of: Option<&usize>| -> Vec<(&Payment, i128, String)> {
        payments
            .iter()
            .filter(|pm| of.is_some() && statement_of.get(pm.document.as_str()) == of)
            .filter_map(|pm| amount_units(&pm.amount).map(|(u, c)| (pm, u, c)))
            .collect()
    };

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
            count(r.paid.0, &r.paid.1, of_receipt(r));
        }
        let of = statement_of.get(d.id.as_str());
        for (pm, u, c) in marked(of) {
            count(u, &c, of_household(pm, u, &c));
        }
        in_order(&mut paid);
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
        let gone: Vec<&str> = of
            .into_iter()
            .flat_map(|i| statements[*i].papers())
            .filter(|id| !here.contains(id))
            .collect();
        if !gone.is_empty() {
            row.insert("in_error".into(), json!(gone));
        }
        if standing::document_differs(d) {
            row.insert("differs".into(), json!(true));
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
        if let Some(b) = r.lost {
            row["bill_in_error"] = json!(b);
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

    // What is entered in error, and the payments that stay on a bill that is.
    let mut in_error: BTreeMap<&str, Gone> = BTreeMap::new();
    let gone_records = all.records.iter().filter(|r| !here.contains(r.id.as_str()));
    for r in gone_records.filter(|r| r.kind == "claim") {
        in_error.insert(&r.id, Gone::Claim);
    }
    for d in all
        .documents
        .iter()
        .filter(|d| !here.contains(d.id.as_str()))
    {
        let of = statement_of.get(d.id.as_str());
        let first = of.map(|i| statements[*i].doc.id.as_str());
        let stands = of.and_then(|i| bill_of.get(i).copied());
        let gone = match (d.doc_type.as_deref(), stands.or(first)) {
            (Some("receipt"), _) => Gone::Receipt,
            (_, Some(bill)) if bill != d.id => Gone::Copy { bill },
            (_, Some(_)) => {
                let receipts = receipts.iter().filter(|r| r.lost == first);
                let mut not_applied: Vec<Value> = receipts.map(of_receipt).collect();
                let marked = marked(of);
                not_applied.extend(marked.iter().map(|(pm, u, c)| of_household(pm, *u, c)));
                in_order(&mut not_applied);
                Gone::Bill { not_applied }
            }
            (_, None) => continue,
        };
        in_error.insert(&d.id, gone);
    }

    let mut out = json!({
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
    });
    if !in_error.is_empty() {
        out["in_error"] = json!(in_error);
    }
    out
}

/// The bills a `bills.json` says are entered in error, by document id, and
/// whether the household marked a payment on it.
pub fn lost(bills: &Value) -> BTreeMap<&str, bool> {
    let gone = bills["in_error"].as_object().into_iter().flatten();
    gone.filter(|(_, g)| g["as"] == "bill")
        .map(|(id, g)| {
            let mut paid = g["not_applied"].as_array().into_iter().flatten();
            (id.as_str(), paid.any(|x| x["from"] == "household"))
        })
        .collect()
}

/// The payments the household marked that are kept on a bill entered in
/// error and not applied, by id.
pub fn not_applied(bills: &Value) -> BTreeSet<&str> {
    let gone = bills["in_error"].as_object().into_iter().flatten();
    gone.flat_map(|(_, g)| g["not_applied"].as_array().into_iter().flatten())
        .filter_map(|x| s(x, "id"))
        .collect()
}

/// How many payments, marked or by receipt, are kept on the bill a paper
/// entered in error is of, and not applied.
pub fn not_applied_on(bills: &Value, paper: &str) -> usize {
    let gone = &bills["in_error"];
    let of = s(&gone[paper], "bill").unwrap_or(paper);
    gone[of]["not_applied"].as_array().map_or(0, Vec::len)
}

/// What a page says of the bills entered in error it does not show, of those
/// it may.
pub fn in_error(bills: &Value, may_show: &dyn Fn(&str) -> bool) -> Option<String> {
    let lost = lost(bills);
    let shown = || lost.iter().filter(|(id, _)| may_show(id));
    let n = shown().count();
    (n > 0).then(|| in_error_line(n, shown().filter(|(_, paid)| **paid).count()))
}

/// The same of `n` bills, `paid` of them with a payment the household marked.
pub fn in_error_line(n: usize, paid: usize) -> String {
    let bills = if n == 1 { "bill" } else { "bills" };
    match paid {
        0 => format!("{n} {bills} entered in error, not shown"),
        1 if n == 1 => "1 bill entered in error, with a payment you marked".into(),
        _ if paid == n => format!("{n} bills entered in error, each with a payment you marked"),
        _ => format!("{n} {bills} entered in error, {paid} with a payment you marked"),
    }
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

    /// The scenario's papers, of which these are entered in error.
    fn index_without(gone: &[&str]) -> PersonIndex {
        let mut idx = index();
        for d in &mut idx.documents {
            if gone.contains(&d.id.as_str()) {
                says(d, &["entered-in-error"]);
            }
        }
        idx
    }

    /// What each of a paper's sources says of it.
    fn says(d: &mut Document, statuses: &[&str]) {
        let raw = |i: usize| format!("Records/example-{i}/Binary/{}.json", d.id);
        let words: Vec<Value> = statuses
            .iter()
            .enumerate()
            .map(|(i, s)| json!({"raw_path": raw(i), "status": s}))
            .collect();
        d.words = serde_json::from_value(json!(words)).unwrap();
        d.provenance.raw_path = Some(raw(0));
        for i in 1..statuses.len() {
            let mut p = d.provenance.clone();
            p.raw_path = Some(raw(i));
            d.also_from.push(p);
        }
    }

    /// The scenario's events as they are made without these: a statement is
    /// its next copy's, and an event with no member is none.
    fn events_without(gone: &[&str]) -> Value {
        let mut v: Value = serde_json::from_str(EVENTS).unwrap();
        let stands = |id: &Value| !gone.contains(&id.as_str().unwrap());
        let evs = v["events"].as_array_mut().unwrap();
        for e in evs.iter_mut() {
            e["members"].as_array_mut().unwrap().retain_mut(|m| {
                let copies = m["copies"].as_array().cloned().unwrap_or_default();
                let mut left: Vec<Value> = copies.into_iter().filter(|c| stands(c)).collect();
                if m["document"].is_string() && !stands(&m["document"]) {
                    if left.is_empty() {
                        return false;
                    }
                    m["document"] = left.remove(0);
                }
                if m["copies"].is_array() {
                    m["copies"] = json!(left);
                }
                !m["record"].is_string() || stands(&m["record"])
            });
        }
        evs.retain(|e| !e["members"].as_array().unwrap().is_empty());
        v
    }

    fn payment(id: &str, document: &str, amount: &str) -> Payment {
        Payment {
            id: id.into(),
            document: document.into(),
            on: "2024-11-12".into(),
            amount: Amount {
                amount: amount.into(),
                currency: "USD".into(),
            },
            at: "2024-11-12T10:00:00Z".into(),
            by: None,
            request: None,
        }
    }

    fn built(idx: &PersonIndex, events: &Value, payments: &[Payment]) -> Value {
        build(&person(), idx, &PersonText::new("p_test"), events, payments)
    }

    const SCAN: [&str; 3] = [
        "d_scanbillaaaaaaaaaaaa",
        "d_scancopyoneaaaaaaaaa",
        "d_scancopytwoaaaaaaaaa",
    ];

    fn all(_: &str) -> bool {
        true
    }

    /// A statement whose first paper is entered in error is its next copy's,
    /// with every payment.
    #[test]
    fn a_statement_is_its_next_copys_while_its_first_paper_is_in_error() {
        let [first, next, last] = SCAN;
        let marked = [payment("o_paidaaaaaaaaaaaaaaaa", first, "3.00")];
        let before = built(&index(), &events_without(&[]), &marked);
        let gone = [first];
        let v = built(&index_without(&gone), &events_without(&gone), &marked);
        assert!(v["bills"].get(first).is_none());
        let b = bill(&v, next);
        assert_eq!(b["copies"], json!([last]));
        assert_eq!(b["in_error"], json!([first]));
        for k in ["paid", "paid_total", "left", "settled", "event", "claim"] {
            assert_eq!(b[k], bill(&before, first)[k], "{k}");
        }
        assert_eq!(b["paid_total"], "93.00");
        assert_eq!(v["copy_of"], json!({last: next}));
        assert_eq!(v["in_error"], json!({first: {"as": "copy", "bill": next}}));
        assert_eq!(in_error(&v, &all), None);
        // It is as it was once it comes back.
        assert_eq!(built(&index(), &events_without(&[]), &marked), before);
        assert!(before.get("in_error").is_none());
    }

    /// One every paper of which is entered in error is in no total and worth
    /// no call, settles no bill, and what paid it pays nothing else.
    #[test]
    fn a_bill_in_error_is_in_no_total_and_what_paid_it_pays_no_other() {
        let [first, next, last] = SCAN;
        let mark = payment("o_paidaaaaaaaaaaaaaaaa", last, "3.00");
        let v = built(
            &index_without(&SCAN),
            &events_without(&SCAN),
            std::slice::from_ref(&mark),
        );
        assert!(SCAN.iter().all(|id| v["bills"].get(id).is_none()));
        assert!(!reasons(&v).iter().any(|(b, _)| SCAN.contains(b)));
        let paid = bill(&built(&index(), &events_without(&[]), &[mark]), first)["paid"].clone();
        assert_eq!(paid.as_array().unwrap().len(), 3);
        assert_eq!(
            v["in_error"],
            json!({
                first: {"as": "bill", "not_applied": paid},
                next: {"as": "copy", "bill": first},
                last: {"as": "copy", "bill": first},
            })
        );
        for id in ["d_scanpaidoneaaaaaaaaa", "d_scanpaidtwoaaaaaaaaa"] {
            let r = row(&v, "receipts", "document", id);
            assert_eq!(r["bill_in_error"], first);
            assert!(r.get("bill").is_none() && r.get("event").is_none(), "{r}");
        }
        // Nothing has asked for its claim's share.
        let nb = row(&v, "not_billed", "provider", "Sample Imaging House");
        assert_eq!(nb["you_owe"], "90.00");
        // Another bill of that biller's is not paid by them, but for one that
        // is a receipt's amount to the cent.
        for (due, bill_of) in [("41.00", None), ("50.00", Some("d_scanpaidoneaaaaaaaaa"))] {
            let mut idx = index_without(&SCAN);
            let mut other = index().documents[0].clone();
            other.id = "d_scanotheraaaaaaaaaaa".into();
            let st = other.statement.as_mut().unwrap();
            st.amount_due.as_mut().unwrap().amount = due.into();
            st.statement_date = Some("2024-10-15".into());
            idx.documents.push(other);
            let v = built(&idx, &events_without(&SCAN), &[]);
            let b = bill(&v, "d_scanotheraaaaaaaaaaa");
            let paid = b["paid"].as_array().unwrap();
            let by: Vec<Option<&str>> = paid.iter().map(|p| p["document"].as_str()).collect();
            assert_eq!(by, Vec::from_iter(bill_of.map(Some)), "{due}");
            assert_eq!(b["settled"], bill_of.is_some());
        }
    }

    /// A payment the household marked on one is said by the page that loses
    /// it, and one it may not show is not.
    #[test]
    fn a_payment_marked_on_a_bill_in_error_is_said() {
        let (paid, unpaid) = ("d_readerbillaaaaaaaaaa", "d_allergybillaaaaaaaaa");
        let gone = [paid, unpaid];
        let marked = [payment("o_paidaaaaaaaaaaaaaaaa", paid, "12.00")];
        let v = built(&index_without(&gone), &events_without(&gone), &marked);
        let line = |may: &[&str]| in_error(&v, &|id| may.contains(&id));
        let says = |s: &str| Some(s.to_string());
        assert_eq!(
            line(&[paid]),
            says("1 bill entered in error, with a payment you marked")
        );
        assert_eq!(
            line(&gone),
            says("2 bills entered in error, 1 with a payment you marked")
        );
        assert_eq!(line(&[unpaid]), says("1 bill entered in error, not shown"));
        assert_eq!(line(&[]), None);
        assert_eq!(not_applied(&v), BTreeSet::from(["o_paidaaaaaaaaaaaaaaaa"]));
        assert_eq!(not_applied_on(&v, paid), 1);
        assert_eq!(not_applied_on(&v, unpaid), 0);
        assert!(!reasons(&v).iter().any(|(b, _)| gone.contains(b)));
        let both = [
            payment("o_paidaaaaaaaaaaaaaaaa", paid, "12.00"),
            payment("o_paidbbbbbbbbbbbbbbbb", unpaid, "12.00"),
        ];
        let v = built(&index_without(&gone), &events_without(&gone), &both);
        assert_eq!(
            in_error(&v, &all),
            says("2 bills entered in error, each with a payment you marked")
        );
    }

    #[test]
    fn a_receipt_in_error_pays_nothing_and_a_later_bill_in_error_follows_none() {
        let (receipt, later) = ("d_scanpaidtwoaaaaaaaaa", "d_visionlateraaaaaaaaa");
        let gone = [receipt, later];
        let v = built(&index_without(&gone), &events_without(&gone), &[]);
        let scan = bill(&v, SCAN[0]);
        assert_eq!(
            (&scan["paid_total"], &scan["left"], &scan["settled"]),
            (&json!("50.00"), &json!("40.00"), &json!(false))
        );
        let receipts = v["receipts"].as_array().unwrap();
        assert!(receipts.iter().all(|r| r["document"] != receipt));
        assert!(bill(&v, "d_visionbillaaaaaaaaaa")
            .get("superseded_by")
            .is_none());
        assert_eq!(
            v["in_error"],
            json!({receipt: {"as": "receipt"}, later: {"as": "bill"}})
        );
        // A desk payment entered in error is none.
        let desk = ["d_deskaaaaaaaaaaaaaaaa"];
        let v = built(&index_without(&desk), &events_without(&desk), &[]);
        assert!(!reasons(&v).iter().any(|(b, _)| *b == desk[0]));
    }

    /// A bill only one of its sources calls in error is counted once, and
    /// says that they differ.
    #[test]
    fn a_bill_its_sources_differ_on_is_counted_once_and_says_so() {
        let before = built(&index(), &events_without(&[]), &[]);
        let mut idx = index();
        says(&mut idx.documents[0], &["entered-in-error", "current"]);
        let mut v = built(&idx, &events_without(&[]), &[]);
        let b = v["bills"][SCAN[0]].as_object_mut().unwrap();
        assert_eq!(b.remove("differs"), Some(json!(true)));
        assert_eq!(v, before);
    }

    /// A claim entered in error is owed nothing, and is named.
    #[test]
    fn a_claim_in_error_is_in_no_share() {
        let claim = "r_fillingaaaaaaaaaaaaa";
        let mut idx = index();
        let source = serde_json::to_value(&idx.documents[0].provenance).unwrap();
        let record = json!({"id": claim, "kind": "claim", "key": "k", "display": "Filling",
            "status": "entered-in-error", "fields": {}, "provenance": [source]});
        idx.records = vec![serde_json::from_value(record).unwrap()];
        let v = built(&idx, &events_without(&[claim]), &[]);
        let nb = v["not_billed"].as_array().unwrap();
        assert!(nb.iter().all(|n| n["you_owe"] != "31.00"), "{nb:?}");
        assert_eq!(v["in_error"], json!({claim: {"as": "claim"}}));
        assert_eq!(in_error(&v, &all), None);
    }
}
