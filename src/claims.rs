//! What a claim paid for and what it cost, in fixed names: each line's
//! category from its service code, its amounts mapped from the payer's
//! adjudication categories to fixed names, the claim's title from its lines
//! and provider, and links to the visit and results of its dates of
//! service. Code ranges and names only; every descriptor comes from the
//! source.

use std::collections::BTreeSet;

use serde_json::{json, Map, Value};

use crate::intake::classify;
use crate::model::Record;

/// A line's category, the first rule that holds, with its label for a
/// claim title. CPT ranges are inclusive. data-contract.md has this table.
pub const CATEGORIES: &[(&str, &str, &str)] = &[
    (
        "pharmacy",
        "Pharmacy",
        "a pharmacy claim, or a line coded NDC or RxNorm",
    ),
    ("visit", "Visit", "CPT 99202-99499"),
    (
        "lab",
        "Lab work",
        "CPT 80047-89398 and 36415-36416, and CPT PLA and MAAA codes (0001U, 0002M)",
    ),
    ("imaging", "Imaging", "CPT 70010-79999"),
    ("vaccine", "Vaccine", "CPT 90460-90756"),
    (
        "medicine",
        "Medical service",
        "other CPT 90281-99199 and 99500-99607",
    ),
    ("procedure", "Procedure", "CPT 10004-69990"),
    ("anesthesia", "Anesthesia", "CPT 00100-01999"),
    ("drug", "Drug", "HCPCS J-codes"),
    ("supply", "Supplies", "HCPCS A-, E- and L-codes"),
    ("dental", "Dental", "CDT D-codes"),
    (
        "facility",
        "Facility",
        "a line with a revenue code and no CPT, HCPCS or CDT code",
    ),
    ("other", "Other", "anything else"),
];

/// The amount names, in the order a claim lists them.
pub const AMOUNT_NAMES: [&str; 9] = [
    "billed",
    "allowed",
    "plan_paid",
    "deductible",
    "copay",
    "coinsurance",
    "not_covered",
    "discount",
    "you_owe",
];

/// Adjudication codes (CARIN Blue Button's C4BBAdjudication and HL7's
/// adjudication codes) and the amount each gives, with a rank: of the
/// entries for one name, those of the best rank count, summed, so a payer
/// that sends both `benefit` and `paidtoprovider` is not counted twice.
const CODES: &[(&str, &str, u8)] = &[
    ("submitted", "billed", 0),
    ("eligible", "allowed", 0),
    ("benefit", "plan_paid", 0),
    ("paidtoprovider", "plan_paid", 1),
    ("paidtopatient", "plan_paid", 1),
    ("deductible", "deductible", 0),
    ("copay", "copay", 0),
    ("coinsurance", "coinsurance", 0),
    ("noncovered", "not_covered", 0),
    ("discount", "discount", 0),
    ("memberliability", "you_owe", 0),
];

/// Payer wording, folded (lower case, letters and digits only), for a
/// category with no code this table knows: the amount it gives, ranked
/// below every code. The first that the words contain wins.
const WORDS: &[(&str, &str)] = &[
    ("notcovered", "not_covered"),
    ("noncovered", "not_covered"),
    ("memberliability", "you_owe"),
    ("patientresponsibility", "you_owe"),
    ("youowe", "you_owe"),
    ("yourshare", "you_owe"),
    ("planpaid", "plan_paid"),
    ("paidbyplan", "plan_paid"),
    ("planpays", "plan_paid"),
    ("submitted", "billed"),
    ("billed", "billed"),
    ("allowed", "allowed"),
    ("eligible", "allowed"),
    ("deductible", "deductible"),
    ("coinsurance", "coinsurance"),
    ("copay", "copay"),
    ("discount", "discount"),
];

const WORDS_RANK: u8 = 2;

const NDC: &str = "http://hl7.org/fhir/sid/ndc";
const RXNORM: &str = "http://www.nlm.nih.gov/research/umls/rxnorm";

/// The category of one service code by its shape and range, or `None` for
/// a code that is not CPT, HCPCS Level II or CDT.
pub fn code_category(code: &str) -> Option<&'static str> {
    let c = code.trim().to_ascii_uppercase();
    let b = c.as_bytes();
    if b.len() != 5 {
        return None;
    }
    let digits = |s: &[u8]| s.iter().all(u8::is_ascii_digit);
    if digits(b) {
        let n: u32 = c.parse().ok()?;
        return Some(match n {
            99202..=99499 => "visit",
            80047..=89398 | 36415..=36416 => "lab",
            70010..=79999 => "imaging",
            90460..=90756 => "vaccine",
            90281..=99199 | 99500..=99607 => "medicine",
            10004..=69990 => "procedure",
            100..=1999 => "anesthesia",
            _ => "other",
        });
    }
    if digits(&b[..4]) {
        // CPT's PLA (`U`) and MAAA (`M`) codes are lab tests; Category II
        // (`F`) and III (`T`) codes are other.
        return match b[4] {
            b'U' | b'M' => Some("lab"),
            b'F' | b'T' => Some("other"),
            _ => None,
        };
    }
    if b[0].is_ascii_alphabetic() && digits(&b[1..]) {
        return Some(match b[0] {
            b'J' => "drug",
            b'A' | b'E' | b'L' => "supply",
            b'D' => "dental",
            _ => "other",
        });
    }
    None
}

fn codings(c: Option<&Value>) -> impl Iterator<Item = &Value> {
    c.and_then(|c| c.get("coding"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}

/// A raw EOB line's category: `pharmacy` on a pharmacy claim or for an NDC
/// or RxNorm code; else its first CPT, HCPCS or CDT code's; else `facility`
/// for a line with a revenue code, `other` for the rest.
pub fn item_category(item: &Value, pharmacy: bool) -> &'static str {
    let service = item.get("productOrService");
    let system = |k: &Value| {
        k.get("system").and_then(Value::as_str) == Some(NDC)
            || k.get("system").and_then(Value::as_str) == Some(RXNORM)
    };
    if pharmacy || codings(service).any(system) {
        return "pharmacy";
    }
    if let Some(c) = codings(service)
        .filter_map(|k| k.get("code").and_then(Value::as_str))
        .find_map(code_category)
    {
        return c;
    }
    if codings(item.get("revenue")).next().is_some() {
        "facility"
    } else {
        "other"
    }
}

fn fold(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect()
}

/// The amount name an adjudication or total category gives, and its rank:
/// by a coding's code, else by the category's words.
pub fn amount_name(category: &Value) -> Option<(&'static str, u8)> {
    let by_code = codings(Some(category))
        .filter_map(|k| k.get("code").and_then(Value::as_str))
        .map(fold)
        .find_map(|code| CODES.iter().find(|(c, _, _)| *c == code))
        .map(|(_, name, rank)| (*name, *rank));
    if by_code.is_some() {
        return by_code;
    }
    let texts = [
        category.get("text").and_then(Value::as_str),
        category
            .pointer("/coding/0/display")
            .and_then(Value::as_str),
    ];
    texts.into_iter().flatten().map(fold).find_map(|w| {
        WORDS
            .iter()
            .find(|(k, _)| w.contains(k))
            .map(|(_, name)| (*name, WORDS_RANK))
    })
}

/// A decimal amount in ten-thousandths, so two of them add exactly.
pub(crate) fn units(s: &str) -> Option<i128> {
    let s = s.trim();
    let (neg, s) = s.strip_prefix('-').map_or((false, s), |r| (true, r));
    let (int, frac) = s.split_once('.').unwrap_or((s, ""));
    let digits = |x: &str| x.bytes().all(|b| b.is_ascii_digit());
    if (int.is_empty() && frac.is_empty()) || !digits(int) || !digits(frac) || frac.len() > 4 {
        return None;
    }
    let int = if int.is_empty() { "0" } else { int };
    let v: i128 = format!("{int}{frac:0<4}").parse().ok()?;
    Some(if neg { -v } else { v })
}

/// Ten-thousandths as a decimal with two places, or four when it has them.
pub(crate) fn decimal(u: i128) -> String {
    let sign = if u < 0 { "-" } else { "" };
    let (int, frac) = (u.abs() / 10_000, u.abs() % 10_000);
    if frac % 100 == 0 {
        format!("{sign}{int}.{:02}", frac / 100)
    } else {
        format!("{sign}{int}.{frac:04}")
    }
}

/// `a + b` for two `{amount, currency}` values of one currency; `None` when
/// the currencies differ or an amount is not a plain decimal.
fn add(a: &Value, b: &Value) -> Option<Value> {
    let cur = |m: &Value| m.get("currency").cloned();
    if cur(a) != cur(b) {
        return None;
    }
    let amt = |m: &Value| m.get("amount").and_then(Value::as_str).and_then(units);
    let sum = amt(a)?.checked_add(amt(b)?)?;
    Some(json!({"amount": decimal(sum), "currency": cur(a)}))
}

/// The member's share of a claim's or line's `amounts`, for a reader: the
/// payer's `you_owe`, else `deductible` + `copay` + `coinsurance` +
/// `not_covered` of those given (never `discount`), and whether it is that
/// sum. Never written into `amounts`, where a missing name stays unknown.
pub fn member_share(amounts: &Value) -> Option<(Value, bool)> {
    if let Some(v) = amounts.get("you_owe") {
        return Some((v.clone(), false));
    }
    let mut parts = ["deductible", "copay", "coinsurance", "not_covered"]
        .iter()
        .filter_map(|k| amounts.get(*k));
    let first = parts.next()?.clone();
    let sum = parts.try_fold(first, |acc, v| add(&acc, v))?;
    Some((sum, true))
}

/// Amounts under fixed names from `(raw category, money)` pairs: of a
/// name's entries those of the best rank, summed. A sum that cannot be
/// made (two currencies) leaves the name out, as unknown.
pub fn amounts<'a>(
    entries: impl Iterator<Item = (&'a Value, Option<Value>)>,
) -> Map<String, Value> {
    let mut best: Vec<(&str, u8, Option<Value>)> = Vec::new();
    for (category, money) in entries {
        let (Some((name, rank)), Some(money)) = (amount_name(category), money) else {
            continue;
        };
        match best.iter_mut().find(|(n, _, _)| *n == name) {
            None => best.push((name, rank, Some(money))),
            Some(have) if rank < have.1 => *have = (name, rank, Some(money)),
            Some(have) if rank == have.1 => {
                have.2 = have.2.as_ref().and_then(|s| add(s, &money));
            }
            Some(_) => {}
        }
    }
    let mut out = Map::new();
    for name in AMOUNT_NAMES {
        if let Some((_, _, Some(v))) = best.iter().find(|(n, _, _)| *n == name) {
            out.insert(name.into(), v.clone());
        }
    }
    out
}

/// The amounts that decide which lines a claim's totals count: what was
/// charged, what the plan paid and what the member owes. The rest (allowed,
/// discounts, what is not covered) payers break down one way on a line and
/// another on the claim, so they are not compared.
const COUNTED_BY: [&str; 3] = ["billed", "plan_paid", "you_owe"];

/// The most lines looked for as left out of one claim's totals, and the
/// most lines a claim may have to be looked at.
const LEFT_OUT_MAX: usize = 3;
const LEFT_OUT_LINES: usize = 64;

/// The sum of amounts in ten-thousandths, `None` past `i128`.
fn sum(mut amounts: impl Iterator<Item = i128>) -> Option<i128> {
    amounts.try_fold(0i128, i128::checked_add)
}

/// Whether a claim's totals count this line: every line but one `left_out`
/// marked `"counted": false`.
pub fn counted(item: &Value) -> bool {
    item.get("counted") != Some(&Value::Bool(false))
}

/// The lines a claim's totals leave out, by their place in `items`. When
/// the lines' sums of `COUNTED_BY` (those the totals give and some line
/// gives, at least two of them) differ from the totals, it is the one
/// smallest set of at most `LEFT_OUT_MAX` lines, never all of them, whose
/// removal makes every sum agree. Empty when the sums agree already, when
/// fewer than two are compared, when no such set exists or two do, or when
/// an amount is in another currency or not a plain decimal. One amount
/// alone is too little to go on: a payer's totals can give a figure that
/// is not its lines' sum.
///
/// A payer can list a line it then leaves out of the claim: one it replaced
/// with another, or wrote off. The same amounts also occur on lines the totals
/// count, so only the totals tell them apart.
pub fn left_out(items: &[Value], totals: &Map<String, Value>) -> Vec<usize> {
    let n = items.len();
    if !(2..=LEFT_OUT_LINES).contains(&n) {
        return Vec::new();
    }
    // Per amount compared: how far the lines' sum exceeds the total, and
    // each line's amount (none given is zero).
    let mut rows: Vec<(i128, Vec<i128>)> = Vec::new();
    for name in COUNTED_BY {
        let Some(total) = totals.get(name) else {
            continue;
        };
        let currency = total.get("currency");
        let parse = |m: &Value| {
            (m.get("currency") == currency)
                .then(|| m.get("amount").and_then(Value::as_str).and_then(units))
                .flatten()
        };
        let Some(total) = parse(total) else {
            return Vec::new();
        };
        let mut given = false;
        let mut lines = Vec::with_capacity(n);
        for it in items {
            match it.get("amounts").and_then(|a| a.get(name)) {
                None => lines.push(0),
                Some(m) => {
                    let Some(u) = parse(m) else {
                        return Vec::new();
                    };
                    given = true;
                    lines.push(u);
                }
            }
        }
        if given {
            let Some(excess) = sum(lines.iter().copied()).and_then(|s| s.checked_sub(total)) else {
                return Vec::new();
            };
            rows.push((excess, lines));
        }
    }
    if rows.len() < 2 || rows.iter().all(|(excess, _)| *excess == 0) {
        return Vec::new();
    }
    for k in 1..=LEFT_OUT_MAX.min(n - 1) {
        let mut found: Vec<Vec<usize>> = Vec::new();
        let mut set: Vec<usize> = (0..k).collect();
        loop {
            if rows
                .iter()
                .all(|(excess, lines)| sum(set.iter().map(|&i| lines[i])) == Some(*excess))
            {
                found.push(set.clone());
                if found.len() > 1 {
                    return Vec::new();
                }
            }
            // The next set of k lines in order, or none left.
            let Some(at) = (0..k).rev().find(|&j| set[j] < n - k + j) else {
                break;
            };
            set[at] += 1;
            for j in at + 1..k {
                set[j] = set[j - 1] + 1;
            }
        }
        if let Some(set) = found.pop() {
            return set;
        }
    }
    Vec::new()
}

/// A claim's amounts: its totals; for a name no total gives, the sum of
/// the lines that give it, of those the totals count; failing both, its
/// payment as `plan_paid`.
pub fn claim_amounts(
    totals: Map<String, Value>,
    items: &[Value],
    payment: Option<Value>,
) -> Map<String, Value> {
    let mut out = Map::new();
    for name in AMOUNT_NAMES {
        if let Some(v) = totals.get(name) {
            out.insert(name.into(), v.clone());
            continue;
        }
        let mut lines = items
            .iter()
            .filter(|i| counted(i))
            .filter_map(|i| i.get("amounts").and_then(|a| a.get(name)));
        let Some(first) = lines.next() else { continue };
        if let Some(sum) = lines.try_fold(first.clone(), |s, m| add(&s, m)) {
            out.insert(name.into(), sum);
        }
    }
    if let Some(p) = payment {
        out.entry("plan_paid").or_insert(p);
    }
    out
}

/// The claim's category: `pharmacy` for a pharmacy claim, else the one
/// category of the lines its totals count, or `mixed`.
pub fn claim_category(items: &[Value], pharmacy: bool) -> Option<&'static str> {
    if pharmacy {
        return Some("pharmacy");
    }
    let cats: BTreeSet<&str> = items
        .iter()
        .filter(|i| counted(i))
        .filter_map(|i| i.get("category").and_then(Value::as_str))
        .collect();
    match cats.len() {
        0 => None,
        1 => CATEGORIES.iter().map(|c| c.0).find(|c| cats.contains(c)),
        _ => Some("mixed"),
    }
}

/// What a claim paid for, from the names of what its lines paid for (`A`,
/// `A and B`, `A, B and 2 more`), else its category's label; then ` · `
/// and its provider.
pub fn title<'a>(
    paid_for: impl IntoIterator<Item = &'a str>,
    category: Option<&str>,
    provider: Option<&str>,
) -> Option<String> {
    let mut names: Vec<&str> = Vec::new();
    for n in paid_for
        .into_iter()
        .map(str::trim)
        .filter(|n| !n.is_empty())
    {
        if !names.iter().any(|x| x.eq_ignore_ascii_case(n)) {
            names.push(n);
        }
    }
    let what = match names.as_slice() {
        [] => CATEGORIES
            .iter()
            .find(|c| Some(c.0) == category && c.0 != "other")
            .map(|c| c.1.to_string()),
        [a] => Some(a.to_string()),
        [a, b] => Some(format!("{a} and {b}")),
        [a, b, rest @ ..] => Some(format!("{a}, {b} and {} more", rest.len())),
    }?;
    Some(match provider.map(str::trim).filter(|p| !p.is_empty()) {
        Some(p) => format!("{what} · {p}"),
        None => what,
    })
}

/// The fields `link` writes. An import compares two versions of a claim
/// without them: they follow from other records, not the claim's resource.
const LINK_FIELDS: [&str; 2] = ["visit", "results"];

/// A claim's fields without its links.
pub fn without_links(fields: &Map<String, Value>) -> Map<String, Value> {
    let mut f = fields.clone();
    for k in LINK_FIELDS {
        f.remove(k);
    }
    if let Some(Value::Array(items)) = f.get_mut("items") {
        for o in items.iter_mut().filter_map(Value::as_object_mut) {
            o.remove("records");
        }
    }
    f
}

/// The words of a name worth matching on: four or more letters, folded.
fn words(s: &str) -> BTreeSet<String> {
    classify::fold(s)
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 4)
        .map(str::to_string)
        .collect()
}

fn day_of(v: &Value) -> Option<String> {
    let s = v
        .as_str()
        .or_else(|| v.get("start").and_then(Value::as_str))?;
    Some(s.chars().take(10).collect())
}

/// Link every claim to the visit and results of its dates of service.
///
/// - `visit`: the one encounter on a date of service; of several, the one
///   sharing a word of four or more letters with the claim's provider or
///   facility; none when that leaves no single one.
/// - `results`: the results and reports dated on a lab line's date of
///   service.
/// - a line's `records`: its claim's visit for a `visit` line, its date's
///   results for a `lab` line.
///
/// Guesses by date and name; missing means none matched. Recomputed whole
/// on every write of the index, so a visit imported after its claim links.
pub fn link(records: &mut [Record]) {
    let encounters: Vec<(String, String, BTreeSet<String>)> = records
        .iter()
        .filter(|r| r.kind == "encounter" && happened(r))
        .filter_map(|r| {
            let names = ["service_provider", "location", "practitioner"]
                .iter()
                .filter_map(|k| r.fields.get(*k).and_then(Value::as_str))
                .flat_map(words)
                .collect();
            Some((r.id.clone(), r.date.clone()?, names))
        })
        .collect();
    let results: Vec<(String, String)> = records
        .iter()
        .filter(|r| (r.kind == "result" || r.kind == "report") && happened(r))
        .filter_map(|r| Some((r.id.clone(), r.date.clone()?)))
        .collect();
    for c in records.iter_mut().filter(|r| r.kind == "claim") {
        let provider: BTreeSet<String> = ["provider", "facility"]
            .iter()
            .filter_map(|k| c.fields.get(*k).and_then(Value::as_str))
            .flat_map(words)
            .collect();
        let line_days: Vec<(Option<String>, Option<String>)> = c
            .fields
            .get("items")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|i| {
                let day = i
                    .get("serviced")
                    .and_then(day_of)
                    .or_else(|| c.date.clone());
                let cat = i
                    .get("category")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                (cat, day)
            })
            .collect();
        // Its lines' dates of service, a line with none taking the claim's
        // date; the claim's date alone when it has no lines.
        let mut days: BTreeSet<String> = line_days.iter().filter_map(|(_, d)| d.clone()).collect();
        if line_days.is_empty() {
            days.extend(c.date.clone());
        }
        let on_days: Vec<_> = encounters
            .iter()
            .filter(|(_, d, _)| days.contains(d))
            .collect();
        let visit = match on_days.as_slice() {
            [one] => Some(one.0.clone()),
            [] => None,
            many => {
                let named: Vec<_> = many
                    .iter()
                    .filter(|(_, _, n)| !n.is_disjoint(&provider))
                    .collect();
                match named.as_slice() {
                    [one] => Some(one.0.clone()),
                    _ => None,
                }
            }
        };
        let on = |day: &Option<String>| -> Vec<String> {
            results
                .iter()
                .filter(|(_, d)| day.as_ref() == Some(d))
                .map(|(id, _)| id.clone())
                .collect()
        };
        let mut linked: Vec<String> = Vec::new();
        let mut per_line: Vec<Vec<String>> = Vec::new();
        for (cat, day) in &line_days {
            let ids: Vec<String> = match cat.as_deref() {
                Some("visit") => visit.iter().cloned().collect(),
                Some("lab") => on(day),
                _ => Vec::new(),
            };
            if cat.as_deref() == Some("lab") {
                for id in &ids {
                    if !linked.contains(id) {
                        linked.push(id.clone());
                    }
                }
            }
            per_line.push(ids);
        }
        match &visit {
            Some(v) => c.fields.insert("visit".into(), json!(v)),
            None => c.fields.remove("visit"),
        };
        if linked.is_empty() {
            c.fields.remove("results");
        } else {
            c.fields.insert("results".into(), json!(linked));
        }
        if let Some(Value::Array(items)) = c.fields.get_mut("items") {
            for (o, ids) in items
                .iter_mut()
                .filter_map(Value::as_object_mut)
                .zip(per_line)
            {
                if ids.is_empty() {
                    o.remove("records");
                } else {
                    o.insert("records".into(), json!(ids));
                }
            }
        }
    }
}

/// A record a claim can have paid for: not one only planned, cancelled or
/// entered in error.
fn happened(r: &Record) -> bool {
    !matches!(r.status.as_deref(), Some("planned" | "cancelled"))
        && !crate::standing::record_in_error(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lines_category_comes_from_its_code_range_or_letter() {
        for (code, want) in [
            ("99213", Some("visit")),
            ("80053", Some("lab")),
            ("36415", Some("lab")),
            ("0001U", Some("lab")),
            ("71046", Some("imaging")),
            ("90686", Some("vaccine")),
            ("96372", Some("medicine")),
            ("99600", Some("medicine")),
            ("45378", Some("procedure")),
            ("00840", Some("anesthesia")),
            ("j1100", Some("drug")),
            ("E0601", Some("supply")),
            ("D0120", Some("dental")),
            ("G0439", Some("other")),
            ("0450", None),
            ("not-applicable", None),
        ] {
            assert_eq!(code_category(code), want, "{code}");
        }
        let line = |system: &str, code: &str, revenue: bool| {
            let mut l = json!({"productOrService": {"coding": [{"system": system, "code": code}]}});
            if revenue {
                l["revenue"] = json!({"coding": [{"code": "0450"}]});
            }
            l
        };
        assert_eq!(
            item_category(&line(NDC, "00000-0001-01", false), false),
            "pharmacy"
        );
        assert_eq!(item_category(&line("x", "99213", false), true), "pharmacy");
        assert_eq!(
            item_category(&line("x", "not-applicable", true), false),
            "facility"
        );
        assert_eq!(item_category(&line("x", "71046", true), false), "imaging");
        assert_eq!(item_category(&json!({}), false), "other");
    }

    #[test]
    fn amounts_map_by_code_then_words_rank_overlaps_and_add_exactly() {
        let cat = |code: Option<&str>, text: &str| match code {
            Some(c) => json!({"coding": [{"code": c}], "text": text}),
            None => json!({"text": text}),
        };
        let usd = |a: &str| Some(json!({"amount": a, "currency": "USD"}));
        let entries = [
            (cat(Some("submitted"), "Billed"), usd("200.10")),
            (cat(Some("paidtoprovider"), "Paid"), usd("120.05")),
            (cat(Some("paidtopatient"), "Paid"), usd("10.0")),
            (cat(None, "Your Copay"), usd("25")),
            (cat(None, "Amount Not Covered"), usd("5.00")),
            (cat(None, "You owe"), usd("1.00")),
            (
                cat(Some("memberliability"), "Member liability"),
                usd("45.00"),
            ),
            (cat(None, "Remark"), usd("9.99")),
        ];
        let got = amounts(entries.iter().map(|(c, m)| (c, m.clone())));
        let a = |k: &str| got[k]["amount"].as_str().unwrap().to_string();
        assert_eq!(a("billed"), "200.10");
        assert_eq!(
            a("plan_paid"),
            "130.05",
            "paid to provider and to patient add"
        );
        assert_eq!(a("copay"), "25");
        assert_eq!(a("not_covered"), "5.00");
        assert_eq!(a("you_owe"), "45.00", "a code outranks words");
        assert_eq!(got.len(), 5);
        let both = [
            (cat(Some("benefit"), "Plan paid"), usd("100")),
            (cat(Some("paidtoprovider"), "Paid"), usd("100")),
        ];
        let got = amounts(both.iter().map(|(c, m)| (c, m.clone())));
        assert_eq!(
            got["plan_paid"]["amount"], "100",
            "benefit is not counted twice"
        );
        assert_eq!(
            decimal(units("-0.5").unwrap() + units("0.25").unwrap()),
            "-0.25"
        );
        assert_eq!(units("1.23456"), None);
        let eur = json!({"amount": "1", "currency": "EUR"});
        assert_eq!(add(&usd("1").unwrap(), &eur), None);
    }

    #[test]
    fn a_claims_amounts_come_from_its_totals_then_its_lines() {
        let usd = |a: &str| json!({"amount": a, "currency": "USD"});
        let mut totals = Map::new();
        totals.insert("billed".into(), usd("300.00"));
        let items = [
            json!({"amounts": {"billed": usd("1"), "copay": usd("10.00")}}),
            json!({"amounts": {"copay": usd("15.50")}}),
            json!({}),
        ];
        let got = claim_amounts(totals, &items, Some(usd("90.00")));
        assert_eq!(got["billed"]["amount"], "300.00");
        assert_eq!(got["copay"]["amount"], "25.50");
        assert_eq!(got["plan_paid"]["amount"], "90.00");
        assert!(!got.contains_key("allowed"), "missing means unknown");
    }

    #[test]
    fn the_lines_a_claims_totals_leave_out_are_the_one_smallest_set() {
        let usd = |a: &str| json!({"amount": a, "currency": "USD"});
        let line = |billed: &str, paid: &str, owed: &str| json!({"amounts": {"billed": usd(billed), "plan_paid": usd(paid), "you_owe": usd(owed)}});
        let totals = |billed: &str, paid: &str, owed: &str| {
            let mut t = Map::new();
            t.insert("billed".into(), usd(billed));
            t.insert("plan_paid".into(), usd(paid));
            t.insert("you_owe".into(), usd(owed));
            t
        };
        // One line of four.
        let four = [
            line("220.00", "130.00", "15.00"),
            line("75.00", "60.00", "0"),
            line("310.00", "200.00", "35.00"),
            line("140.00", "0", "140.00"),
        ];
        let left = totals("605.00", "390.00", "50.00");
        assert_eq!(left_out(&four, &left), [3]);
        // Two, left out together.
        let five = [
            line("80.00", "64.00", "16.00"),
            line("25.00", "0", "0"),
            line("60.00", "48.00", "12.00"),
            line("33.00", "0", "0"),
            line("10.00", "8.00", "2.00"),
        ];
        assert_eq!(
            left_out(&five, &totals("150.00", "120.00", "30.00")),
            [1, 3]
        );
        // Sums that agree leave nothing out.
        assert!(left_out(&four, &totals("745.00", "390.00", "190.00")).is_empty());
        // Two lines alike: either could be the one left out, so neither is.
        let alike = [
            line("15", "0", "0"),
            line("45", "40", "5"),
            line("15", "0", "0"),
        ];
        assert!(left_out(&alike, &totals("60", "40", "5")).is_empty());
        // Totals that no set of lines explains mark nothing.
        let apart = [line("30", "20", "0"), line("30", "20", "0")];
        assert!(left_out(&apart, &totals("30", "40", "0")).is_empty());
        // Never every line, nor one claim of one line.
        assert!(left_out(&four[3..], &totals("0", "0", "0")).is_empty());
        // Another currency, or a name only the totals give, decides nothing
        // and is not compared.
        let mut eur = left.clone();
        eur.insert(
            "billed".into(),
            json!({"amount": "605.00", "currency": "EUR"}),
        );
        assert!(left_out(&four, &eur).is_empty());
        let mut owed_only = Map::new();
        owed_only.insert("you_owe".into(), usd("50.00"));
        let unpriced = [json!({"amounts": {"billed": usd("1")}}), json!({})];
        assert!(left_out(&unpriced, &owed_only).is_empty());
        // An amount too large to add decides nothing: 10^34 parses (10^38
        // ten-thousandths), and two of them pass i128.
        let huge = format!("1{}", "0".repeat(34));
        assert!(units(&huge).is_some());
        let vast = [
            line(&huge, "0", "0"),
            line(&huge, "0", "0"),
            line("1", "1", "0"),
        ];
        assert!(left_out(&vast, &totals("1", "1", "0")).is_empty());
        assert_eq!(add(&usd(&huge), &usd(&huge)), None);
        // One amount compared is too little to go on.
        let mut billed_only = Map::new();
        billed_only.insert("billed".into(), usd("605.00"));
        assert!(left_out(&four, &billed_only).is_empty());
        // A line left out adds to no sum the totals do not give.
        let mut items = four.to_vec();
        items[3]["counted"] = json!(false);
        for it in items.iter_mut() {
            it["amounts"]["copay"] = usd("5.00");
        }
        let got = claim_amounts(left, &items, None);
        assert_eq!(got["copay"]["amount"], "15.00");
        assert!(counted(&items[0]) && !counted(&items[3]));
    }

    fn rec(id: &str, kind: &str, date: &str, fields: Value) -> Record {
        Record {
            id: id.into(),
            kind: kind.into(),
            key: id.into(),
            date: Some(date.into()),
            code: None,
            display: id.into(),
            status: None,
            fields: fields.as_object().cloned().unwrap_or_default(),
            providers: Vec::new(),
            words: Vec::new(),
            errors: Vec::new(),
            fields_from: None,
            documents: Vec::new(),
            provenance: Vec::new(),
        }
    }

    fn claim(date: &str, provider: &str, lines: &[(&str, Option<&str>)]) -> Record {
        let items: Vec<Value> = lines
            .iter()
            .map(|(cat, day)| match day {
                Some(d) => json!({"category": cat, "serviced": d}),
                None => json!({ "category": cat }),
            })
            .collect();
        rec(
            "c",
            "claim",
            date,
            json!({"provider": provider, "items": items}),
        )
    }

    fn linked(records: &mut [Record]) -> Map<String, Value> {
        link(records);
        records
            .iter()
            .find(|r| r.kind == "claim")
            .unwrap()
            .fields
            .clone()
    }

    #[test]
    fn the_members_share_is_the_payers_figure_else_its_parts() {
        let usd = |a: &str| json!({"amount": a, "currency": "USD"});
        let a = json!({"you_owe": usd("12.00"), "copay": usd("30.00")});
        assert_eq!(member_share(&a), Some((usd("12.00"), false)));
        let a = json!({"deductible": usd("100.00"), "copay": usd("30.50"),
                       "coinsurance": usd("4.25"), "discount": usd("50.00")});
        assert_eq!(member_share(&a), Some((usd("134.75"), true)));
        assert_eq!(member_share(&json!({"discount": usd("5.00")})), None);
        let a = json!({"copay": usd("1.00"), "deductible": {"amount": "1.00", "currency": "EUR"}});
        assert_eq!(member_share(&a), None, "two currencies: unknown");
    }

    #[test]
    fn a_claim_links_the_one_visit_of_its_dates_of_service() {
        let visit = |id: &str, date: &str, org: &str| {
            rec(id, "encounter", date, json!({ "service_provider": org }))
        };
        // One encounter that day.
        let mut r = vec![
            claim("2026-03-04", "Example Health", &[("visit", None)]),
            visit("e1", "2026-03-04", "Other Clinic"),
        ];
        let f = linked(&mut r);
        assert_eq!(f["visit"], "e1");
        assert_eq!(f["items"][0]["records"], json!(["e1"]));

        // Two that day: the one sharing a word with the provider.
        let mut r = vec![
            claim("2026-03-04", "Example Health", &[("visit", None)]),
            visit("e1", "2026-03-04", "Other Clinic"),
            visit("e2", "2026-03-04", "Example Pediatrics"),
        ];
        assert_eq!(linked(&mut r)["visit"], "e2");

        // Two that day, both or neither sharing one: no visit.
        for (a, b) in [
            ("Example Clinic", "Example Pediatrics"),
            ("Other Clinic", "Elsewhere"),
        ] {
            let mut r = vec![
                claim("2026-03-04", "Example Health", &[("visit", None)]),
                visit("e1", "2026-03-04", a),
                visit("e2", "2026-03-04", b),
            ];
            let f = linked(&mut r);
            assert!(!f.contains_key("visit"), "{a} / {b}");
            assert!(!f["items"][0].as_object().unwrap().contains_key("records"));
        }

        // A line serviced on another day than the claim's: its day counts,
        // the claim's does not.
        let mut r = vec![
            claim(
                "2026-03-10",
                "Example Health",
                &[("visit", Some("2026-03-04"))],
            ),
            visit("e1", "2026-03-04", "Other Clinic"),
            visit("e9", "2026-03-10", "Example Health"),
        ];
        assert_eq!(linked(&mut r)["visit"], "e1");

        // A claim with no lines goes by its own date.
        let mut r = vec![
            claim("2026-03-10", "Example Health", &[]),
            visit("e9", "2026-03-10", "Elsewhere"),
        ];
        assert_eq!(linked(&mut r)["visit"], "e9");

        // A planned, cancelled or mistaken encounter is not what it paid
        // for, so a finished one that day is the one.
        for status in ["planned", "cancelled", "entered-in-error"] {
            let mut gone = visit("e1", "2026-03-04", "Example Health");
            gone.status = Some(status.into());
            let mut r = vec![
                claim("2026-03-04", "Example Health", &[("visit", None)]),
                gone.clone(),
                visit("e2", "2026-03-04", "Elsewhere"),
            ];
            assert_eq!(linked(&mut r)["visit"], "e2", "{status}");
            let mut r = vec![
                claim("2026-03-04", "Example Health", &[("visit", None)]),
                gone,
            ];
            assert!(!linked(&mut r).contains_key("visit"), "{status}");
        }
    }

    #[test]
    fn a_lab_line_links_its_days_results_and_stale_links_go() {
        let mut r = vec![
            claim(
                "2026-03-04",
                "Example Health",
                &[
                    ("visit", None),
                    ("lab", Some("2026-03-05")),
                    ("imaging", None),
                ],
            ),
            rec("e1", "encounter", "2026-03-04", json!({})),
            rec("o1", "result", "2026-03-05", json!({})),
            rec("o2", "report", "2026-03-05", json!({})),
            rec("o3", "result", "2026-03-04", json!({})),
        ];
        let f = linked(&mut r);
        assert_eq!(
            f["results"],
            json!(["o1", "o2"]),
            "a lab line's day, not the claim's"
        );
        assert_eq!(f["items"][1]["records"], json!(["o1", "o2"]));
        assert!(!f["items"][2].as_object().unwrap().contains_key("records"));

        // The visit and results go away: so do the links.
        r.retain(|x| x.kind == "claim");
        let f = linked(&mut r);
        for k in ["visit", "results"] {
            assert!(!f.contains_key(k), "{k}");
        }
        for i in f["items"].as_array().unwrap() {
            assert!(!i.as_object().unwrap().contains_key("records"));
        }
        assert_eq!(without_links(&f), f);
    }

    #[test]
    fn a_title_names_what_was_paid_for_and_who() {
        assert_eq!(title([], None, Some("Example Health")), None);
        assert_eq!(
            title([], Some("lab"), Some("Example Health")).as_deref(),
            Some("Lab work · Example Health")
        );
        assert_eq!(
            title(["Lipid panel"], None, None).as_deref(),
            Some("Lipid panel")
        );
        let four = [
            "Office visit",
            "Lipid panel",
            "lipid panel",
            "CBC",
            " ",
            "TSH",
        ];
        assert_eq!(
            title(four, Some("mixed"), Some("Example Health")).as_deref(),
            Some("Office visit, Lipid panel and 2 more · Example Health")
        );
        let cat = |c: &str| json!({ "category": c });
        assert_eq!(
            claim_category(&[cat("lab"), cat("visit")], false),
            Some("mixed")
        );
        assert_eq!(claim_category(&[cat("lab")], false), Some("lab"));
        assert_eq!(claim_category(&[], true), Some("pharmacy"));
    }
}
