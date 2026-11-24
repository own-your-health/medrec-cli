//! Events (`views/<person id>/events.json`, `medrec-events/v1`): what
//! happened, as a person knows it. A visit, an ER trip, a stay, a test or a
//! fill is one event, however many claims, encounters, results and papers
//! it produced. The rules are numbered so that every member names the one
//! that placed it:
//!
//! - N: names (organization tokens, clinician keys, learned aliases);
//! - T: time (a claim's dates, an encounter's local day, the windows);
//! - C: claims into events, in order (C1 facility claims open events, C2
//!   and C3 bring professional claims to them, C4 stays with no facility
//!   claim, C5 the rest day by day, C6 fills and dental);
//! - E: the provider's portal (encounters, their satellite records,
//!   vaccines);
//! - D: papers (organization, clinician, the only visit that day, a
//!   statement's amount, portal notes, prescriptions, copies);
//! - X: across people (a newborn's claims, papers filed under a child);
//! - `household`: a decision of the household's, which outranks them all.
//!
//! Derived on every views write from the person's `index.json`, `text.json`
//! and `overrides.json`, and, for the X rules, a guardian's or child's
//! index. Nothing here is a source.

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::model::{Document, Person, PersonIndex, PersonText, Record};

pub const SCHEMA: &str = "medrec-events/v1";
pub const OVERRIDES_SCHEMA: &str = "medrec-overrides/v1";

/// A person's `overrides.json`: the household's decisions, which outrank
/// every rule.
pub fn overrides_rel(p: &Person) -> String {
    format!("{}/overrides.json", p.folder)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Overrides {
    pub schema: String,
    pub person: String,
    #[serde(default)]
    pub events: Vec<Decision>,
    /// What a later medrec writes here, kept as it is.
    #[serde(flatten)]
    pub other: Map<String, Value>,
}

impl Overrides {
    pub fn new(person: &str) -> Self {
        Overrides {
            schema: OVERRIDES_SCHEMA.into(),
            person: person.into(),
            events: Vec::new(),
            other: Map::new(),
        }
    }
}

/// "Not the same visit" (`apart`) or "Attach to this visit" (`together`),
/// naming two members by their record or document ids, so the decision
/// holds whatever the rules make of the rest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decision {
    pub id: String,
    /// `apart` or `together`.
    pub kind: String,
    pub members: [String; 2],
    pub at: String,
    /// Who decided, as shown (a request's `requested_by`, never trusted).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
    /// The `_requests/` id it came from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<String>,
}

/// The members of a household whose records one person's events read: a
/// guardian or a child with a birth date, for the X rules.
pub fn linked_people<'a>(h: &'a crate::model::Household, p: &Person) -> Vec<&'a Person> {
    h.people
        .iter()
        .filter(|q| q.id != p.id)
        .filter(|q| {
            (p.birth_date.is_some() && p.guardians.contains(&q.id))
                || (q.birth_date.is_some() && q.guardians.contains(&p.id))
        })
        .collect()
}

pub struct Linked<'a> {
    pub person: &'a Person,
    pub index: &'a PersonIndex,
    pub texts: Option<&'a PersonText>,
}

pub struct Input<'a> {
    pub person: &'a Person,
    pub index: &'a PersonIndex,
    pub texts: &'a PersonText,
    pub overrides: Option<&'a Overrides>,
    pub linked: Vec<Linked<'a>>,
    /// The UTC offset, in seconds, at a time: the days an event is shown
    /// on are the calendar days at it (`pk_cli_core::dates::local_utc_offset`,
    /// the machine's zone, outside tests).
    pub utc_offset: fn(i64) -> Option<i64>,
}

// ---------------------------------------------------------------- days

type Day = i64;

/// The day number of a date or a date-time's date part.
pub(crate) fn day(s: &str) -> Option<Day> {
    let d = s.get(..10)?;
    crate::refresh::time::parse(&format!("{d}T00:00:00Z")).map(|t| t.div_euclid(86_400))
}

fn iso(d: Day) -> String {
    crate::refresh::time::fmt(d * 86_400)[..10].to_string()
}

/// T2: the local calendar days a record's time can fall on. A time with an
/// offset is on its own date. A UTC time falls on a US date between UTC−4
/// and UTC−10, so it may be either of two days, except a whole hour from
/// 00:00 to 08:00, which is a date stored at local midnight.
fn local_days(s: &str) -> BTreeSet<Day> {
    let Some(d) = day(s) else {
        return BTreeSet::new();
    };
    let t = s.get(10..).unwrap_or("");
    if !t.starts_with('T') || !(t.ends_with('Z') || t.ends_with("+00:00")) {
        return [d].into();
    }
    let Some(secs) = crate::refresh::time::parse(s) else {
        return [d].into();
    };
    let of_day = secs.rem_euclid(86_400);
    if of_day % 3600 == 0 && of_day <= 8 * 3600 {
        return [d].into();
    }
    [
        (secs - 4 * 3600).div_euclid(86_400),
        (secs - 10 * 3600).div_euclid(86_400),
    ]
    .into()
}

thread_local! {
    /// `Input::utc_offset` while `build` runs.
    static OFFSET: Cell<fn(i64) -> Option<i64>> = const { Cell::new(no_offset) };
}

fn no_offset(_: i64) -> Option<i64> {
    None
}

/// Sets `OFFSET` for one `build`, and puts the one before back after it.
struct Offset(fn(i64) -> Option<i64>);

impl Offset {
    fn set(f: fn(i64) -> Option<i64>) -> Self {
        Offset(OFFSET.with(|o| o.replace(f)))
    }
}

impl Drop for Offset {
    fn drop(&mut self) {
        OFFSET.with(|o| o.set(self.0));
    }
}

/// The day a time is shown on: of its local days (T2), the one it falls
/// on at the offset `build` was given, else the first.
fn shown_day(s: &str) -> Option<Day> {
    let days = local_days(s);
    if days.len() > 1 {
        let at = crate::refresh::time::parse(s)
            .and_then(|secs| Some((secs + OFFSET.with(Cell::get)(secs)?).div_euclid(86_400)));
        if let Some(d) = at.filter(|d| days.contains(d)) {
            return Some(d);
        }
    }
    days.first().copied()
}

// ---------------------------------------------------------------- amounts

fn units(v: Option<&Value>) -> Option<(i128, String)> {
    let v = v?;
    let cur = v.get("currency").and_then(Value::as_str)?.to_string();
    Some((crate::claims::units(v.get("amount")?.as_str()?)?, cur))
}

fn decimal(u: i128) -> String {
    crate::claims::decimal(u)
}

// ---------------------------------------------------------------- N: names

const LEGAL: &[&str] = &[
    "inc", "llc", "llp", "pllc", "pa", "pc", "corp", "co", "of", "the", "and", "dds", "lcsw",
];
const GENERIC: &[&str] = &[
    "medical",
    "center",
    "centre",
    "health",
    "healthcare",
    "hospital",
    "group",
    "services",
    "service",
    "system",
    "network",
    "clinic",
    "contracting",
    "practices",
    "all",
];
/// US states and their postal codes, which name where a practice is, not
/// which one it is.
const STATES: &[&str] = &[
    "alabama",
    "alaska",
    "arizona",
    "arkansas",
    "california",
    "colorado",
    "connecticut",
    "delaware",
    "florida",
    "georgia",
    "hawaii",
    "idaho",
    "illinois",
    "indiana",
    "iowa",
    "kansas",
    "kentucky",
    "louisiana",
    "maine",
    "maryland",
    "massachusetts",
    "michigan",
    "minnesota",
    "mississippi",
    "missouri",
    "montana",
    "nebraska",
    "nevada",
    "ohio",
    "oklahoma",
    "oregon",
    "pennsylvania",
    "tennessee",
    "texas",
    "utah",
    "vermont",
    "virginia",
    "washington",
    "wisconsin",
    "wyoming",
    "al",
    "ak",
    "az",
    "ar",
    "ca",
    "ct",
    "de",
    "fl",
    "ga",
    "ia",
    "id",
    "il",
    "ks",
    "ky",
    "la",
    "ma",
    "md",
    "mi",
    "mn",
    "ms",
    "mo",
    "mt",
    "ne",
    "nv",
    "nh",
    "nj",
    "nm",
    "ny",
    "nc",
    "nd",
    "oh",
    "ok",
    "ri",
    "sc",
    "sd",
    "tn",
    "tx",
    "ut",
    "vt",
    "va",
    "wa",
    "wv",
    "wi",
    "wy",
];

/// N1: an organization's distinctive words.
pub fn org_tokens(name: &str) -> Vec<String> {
    let s = name
        .to_lowercase()
        .replace('&', " and ")
        .replace(['\'', '\u{2019}'], "");
    let words: Vec<String> = s
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .filter(|w| !LEGAL.contains(w) && !w.bytes().all(|b| b.is_ascii_digit()))
        .map(str::to_string)
        .collect();
    let distinct: Vec<String> = words
        .iter()
        .filter(|w| !GENERIC.contains(&w.as_str()) && !STATES.contains(&w.as_str()))
        .cloned()
        .collect();
    if distinct.is_empty() {
        words
    } else {
        distinct
    }
}

/// N2: the same organization. Equal token sets, or the smaller of at least
/// two tokens inside the larger.
pub fn same_org(a: &str, b: &str) -> bool {
    let (ta, tb) = (org_tokens(a), org_tokens(b));
    if ta.is_empty() || tb.is_empty() {
        return false;
    }
    let (sa, sb): (BTreeSet<&String>, BTreeSet<&String>) =
        (ta.iter().collect(), tb.iter().collect());
    if sa == sb {
        return true;
    }
    let (small, big) = if sa.len() <= sb.len() {
        (&sa, &sb)
    } else {
        (&sb, &sa)
    };
    small.len() >= 2 && small.is_subset(big)
}

static CLINICIAN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z] [A-Za-z][A-Za-z'\- ]+$").unwrap());
static ORG_WORDS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(diagnostics|pathology|group|medical|contracting|laboratory|laboratories|labs?|inc|llc|care|imaging|memorial|surgery|pharmacy|corp|clinic|center|hospital|health|associates|partners|services|radiology|anesthesia|specialists|physicians)\b",
    )
    .unwrap()
});

/// N4: a claim provider is a clinician ("S EXAMPLE", "EXAMPLE MD"), not an
/// organization.
pub fn is_clinician(provider: &str) -> bool {
    let p = provider.trim();
    if p.contains('&') || ORG_WORDS.is_match(p) {
        return false;
    }
    CLINICIAN.is_match(p) || p.to_uppercase().ends_with(" MD")
}

const TITLES: &[&str] = &[
    "MD", "M", "D", "DO", "DR", "FRCS", "FAAP", "PHD", "NP", "PA", "RN", "LCSW", "DDS", "DMD",
    "OD", "APRN", "CNM", "FACOG",
];

/// N3: a clinician's (surname, first initial), from "S EXAMPLE",
/// "S. EXAMPLE", "EXAMPLE MD,SAM L" and "Sam Example, M.D.".
pub fn clinician_key(name: &str) -> (String, String) {
    let s = name.replace('.', " ");
    let s = s.trim();
    let not_title = |w: &&str| !TITLES.contains(&w.to_uppercase().as_str());
    let mut s = s.to_string();
    if let Some((left, right)) = s.clone().split_once(',') {
        let rw: Vec<&str> = right.split_whitespace().filter(not_title).collect();
        let lw: Vec<&str> = left.split_whitespace().filter(not_title).collect();
        if !rw.is_empty() && lw.len() == 1 {
            let initial = rw[0].chars().next().map(|c| c.to_uppercase().to_string());
            return (lw[0].to_uppercase(), initial.unwrap_or_default());
        }
        s = left.to_string();
    }
    let words: Vec<&str> = s.split_whitespace().filter(not_title).collect();
    match words.as_slice() {
        [] => (String::new(), String::new()),
        [one] => (one.to_uppercase(), String::new()),
        [first, rest @ ..] => (
            rest.join(" ").to_uppercase(),
            first
                .chars()
                .next()
                .map(|c| c.to_uppercase().to_string())
                .unwrap_or_default(),
        ),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Same {
    Yes,
    InitialDiffers,
    No,
}

/// N3: the same clinician. UHC cuts names to 20 characters, so a surname
/// of 8 or more may match as a prefix; a compound surname matches on its
/// last word when the initial does too. A matching surname with another
/// initial is `InitialDiffers`, a difference rather than a refusal.
pub fn same_clinician(a: &str, b: &str) -> Same {
    let ((sa, ia), (sb, ib)) = (clinician_key(a), clinician_key(b));
    if sa.is_empty() || sb.is_empty() {
        return Same::No;
    }
    let prefix = |x: &str, y: &str| x.len() >= 8 && y.starts_with(x);
    if sa == sb || prefix(&sa, &sb) || prefix(&sb, &sa) {
        return if ia.is_empty() || ib.is_empty() || ia == ib {
            Same::Yes
        } else {
            Same::InitialDiffers
        };
    }
    let last = |s: &str| s.split_whitespace().last().unwrap_or("").to_string();
    if last(&sa) == last(&sb) && !ia.is_empty() && ia == ib {
        return Same::Yes;
    }
    Same::No
}

/// D2b: a clinician's surname in a text, with the first initial on the word
/// before it unless `strict` is off.
fn clinician_in_text(provider: &str, text: &str, strict: bool) -> bool {
    let (surname, initial) = clinician_key(provider);
    if surname.len() < 4 {
        return false;
    }
    let flat = |s: &str| {
        s.to_uppercase()
            .replace('-', " ")
            .chars()
            .map(|c| {
                if c.is_alphabetic() || c == ' ' {
                    c
                } else {
                    ' '
                }
            })
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    };
    let (text, sur) = (flat(text), flat(&surname));
    let words: Vec<&str> = text.split(' ').collect();
    let n = sur.split(' ').count();
    (0..words.len().saturating_sub(n - 1)).any(|i| {
        words[i..i + n].join(" ") == sur
            && (initial.is_empty()
                || !strict
                || (i > 0 && words[i - 1].starts_with(initial.as_str())))
    })
}

// ---------------------------------------------------------------- the pieces

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Conf {
    Unsure,
    Likely,
    Sure,
}

impl Conf {
    fn name(self) -> &'static str {
        match self {
            Conf::Unsure => "unsure",
            Conf::Likely => "likely",
            Conf::Sure => "sure",
        }
    }
}

#[derive(Debug, Clone)]
struct Member {
    id: String,
    document: bool,
    kind: String,
    role: &'static str,
    rule: &'static str,
    conf: Conf,
    /// Another person's (X2).
    person: Option<String>,
    /// Days it spans, for the event's dates (claims, encounters, fills).
    days: Option<(Day, Day)>,
    extra: Map<String, Value>,
}

#[derive(Debug, Clone)]
struct Ev {
    anchor: String,
    kind: &'static str,
    members: Vec<Member>,
    notes: Vec<&'static str>,
    could_join: Vec<String>,
    links: Vec<Value>,
    differences: Vec<Value>,
    /// The place a link gave it (X1).
    linked_place: Option<String>,
}

impl Ev {
    fn new(kind: &'static str, first: Member) -> Self {
        Ev {
            anchor: first.id.clone(),
            kind,
            members: vec![first],
            notes: Vec::new(),
            could_join: Vec::new(),
            links: Vec::new(),
            differences: Vec::new(),
            linked_place: None,
        }
    }

    fn span(&self) -> Option<(Day, Day)> {
        let spans: Vec<(Day, Day)> = self.members.iter().filter_map(|m| m.days).collect();
        Some((
            spans.iter().map(|s| s.0).min()?,
            spans.iter().map(|s| s.1).max()?,
        ))
    }

    fn holds(&self, id: &str) -> bool {
        self.members.iter().any(|m| m.id == id)
    }

    fn claims(&self) -> impl Iterator<Item = &Member> {
        self.members.iter().filter(|m| m.kind == "claim")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ty {
    Facility,
    Professional,
    Pharmacy,
    Dental,
}

struct Claim<'a> {
    r: &'a Record,
    provider: String,
    pkey: String,
    start: Day,
    end: Day,
    ty: Ty,
    fkind: &'static str,
    role: &'static str,
    vaccine: bool,
}

static EMERG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\bemerg").unwrap());
static INPATIENT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)room (and|&) board|inpatient").unwrap());
static HOSPITAL_PHYS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)emergency room|emergency department|admitted to the hospital|hospital care|day of discharge|observation|newborn care").unwrap()
});
static OFFICE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)office visit|preventive|new doctor|periodic physician|office or other outpatient",
    )
    .unwrap()
});
static TEST_TEXT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)blood sample|laboratory|quality measure|imaging|radiology|urine test|ultrasound|test|panel|x-ray|scan").unwrap()
});
const VACCINE_FAMILIES: &[(&str, &str)] = &[
    (
        "flu",
        r"(?i)influenza|\bflu\b|fluzone|fluarix|flucelvax|flulaval|fluad|flumist",
    ),
    ("covid", r"(?i)covid|sars-cov|comirnaty|spikevax|novavax"),
    (
        "hepb",
        r"(?i)hep(atitis)?[ -]?b\b|heplisav|engerix|recombivax",
    ),
    ("hepa", r"(?i)hep(atitis)?[ -]?a\b|havrix|vaqta"),
    (
        "tdap",
        r"(?i)tdap|boostrix|adacel|\btd\b|tetanus|dtap|pediarix|pentacel|infanrix|daptacel",
    ),
    ("zoster", r"(?i)zoster|shingrix|shingles"),
    (
        "pneumo",
        r"(?i)pneumo|prevnar|pneumovax|vaxneuvance|capvaxive",
    ),
    ("hpv", r"(?i)\bhpv\b|papilloma|gardasil"),
    (
        "mmr",
        r"(?i)\bmmr|measles|varicella|varivax|proquad|priorix",
    ),
    (
        "rsv",
        r"(?i)\brsv\b|respiratory syncytial|arexvy|abrysvo|beyfortus|nirsevimab",
    ),
    ("rota", r"(?i)rota(virus|teq|rix)"),
    ("hib", r"(?i)\bhib\b|haemophilus|acthib|pedvaxhib"),
    ("polio", r"(?i)polio|\bipv\b|ipol"),
    (
        "mening",
        r"(?i)mening|menactra|menveo|menquadfi|bexsero|trumenba|penbraya",
    ),
];
static VACCINES: LazyLock<Vec<(&'static str, Regex)>> = LazyLock::new(|| {
    VACCINE_FAMILIES
        .iter()
        .map(|(k, r)| (*k, Regex::new(r).unwrap()))
        .collect()
});

fn vaccine_family(text: &str) -> Option<&'static str> {
    VACCINES
        .iter()
        .find(|(_, r)| r.is_match(text))
        .map(|(k, _)| *k)
}

fn str_field<'a>(r: &'a Record, k: &str) -> Option<&'a str> {
    r.fields
        .get(k)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
}

fn items(r: &Record) -> &[Value] {
    r.fields
        .get("items")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn cpt(item: &Value) -> Option<u32> {
    let c = item.pointer("/service/code")?.as_str()?;
    (c.len() == 5 && c.bytes().all(|b| b.is_ascii_digit()))
        .then(|| c.parse().ok())
        .flatten()
}

fn revenue(item: &Value) -> Option<u32> {
    item.pointer("/revenue/code")?.as_str()?.trim().parse().ok()
}

fn service_text(item: &Value) -> String {
    [
        item.pointer("/service/display").and_then(Value::as_str),
        item.pointer("/revenue/display").and_then(Value::as_str),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" ")
}

fn item_day(item: &Value) -> Option<(Day, Day)> {
    match item.get("serviced")? {
        Value::String(s) => day(s).map(|d| (d, d)),
        Value::Object(o) => {
            let s = day(o.get("start")?.as_str()?)?;
            let e = o
                .get("end")
                .and_then(Value::as_str)
                .and_then(day)
                .unwrap_or(s);
            Some((s, e.max(s)))
        }
        _ => None,
    }
}

/// T1: a claim's dates are its billable period, else its date, else its
/// lines'.
fn claim_days(r: &Record) -> Option<(Day, Day)> {
    if let Some(bp) = r.fields.get("billable_period") {
        if let Some(s) = bp.get("start").and_then(Value::as_str).and_then(day) {
            let e = bp
                .get("end")
                .and_then(Value::as_str)
                .and_then(day)
                .unwrap_or(s);
            return Some((s, e.max(s)));
        }
    }
    if let Some(d) = r.date.as_deref().and_then(day) {
        return Some((d, d));
    }
    let spans: Vec<(Day, Day)> = items(r).iter().filter_map(item_day).collect();
    Some((
        spans.iter().map(|s| s.0).min()?,
        spans.iter().map(|s| s.1).max()?,
    ))
}

/// Hospital physicians' codes: anesthesia (00100–01999, given only in a
/// facility), observation and inpatient care, the ER, critical care, and
/// newborn and neonatal care.
const HOSPITAL_CPT: &[(u32, u32)] = &[
    (100, 1999),
    (99217, 99239),
    (99281, 99285),
    (99291, 99292),
    (99460, 99463),
    (99468, 99480),
];
const OFFICE_CPT: &[(u32, u32)] = &[(99202, 99215), (99241, 99245), (99381, 99429)];
const ER_CPT: (u32, u32) = (99281, 99285);

fn in_ranges(c: u32, ranges: &[(u32, u32)]) -> bool {
    ranges.iter().any(|(a, b)| (*a..=*b).contains(&c))
}

/// C0: a claim's kind, and a facility claim's kind or a professional
/// claim's role.
fn classify(r: &Record) -> Option<Claim<'_>> {
    let (start, end) = claim_days(r)?;
    let provider = str_field(r, "provider")
        .or_else(|| str_field(r, "facility"))
        .unwrap_or("")
        .to_string();
    let category = str_field(r, "category").unwrap_or("");
    let ty = match str_field(r, "type")
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "pharmacy" => Ty::Pharmacy,
        "oral" | "dental" => Ty::Dental,
        "institutional" => Ty::Facility,
        _ if category == "pharmacy" => Ty::Pharmacy,
        _ if category == "dental" => Ty::Dental,
        _ => Ty::Professional,
    };
    let lines = items(r);
    let text: String = lines
        .iter()
        .map(service_text)
        .collect::<Vec<_>>()
        .join(" | ");
    let codes: Vec<u32> = lines.iter().filter_map(cpt).collect();
    let fkind = if ty == Ty::Facility {
        let revs: Vec<u32> = lines.iter().filter_map(revenue).collect();
        if revs.iter().any(|v| (450..=459).contains(v))
            || EMERG.is_match(&text)
            || codes.iter().any(|c| (ER_CPT.0..=ER_CPT.1).contains(c))
        {
            "er"
        } else if revs.iter().any(|v| (100..=219).contains(v)) || INPATIENT.is_match(&text) {
            "inpatient"
        } else if end - start > 1 {
            "series"
        } else {
            "outpatient"
        }
    } else {
        ""
    };
    let line_cat = |i: &Value| {
        i.get("category")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let is_test = |i: &Value| {
        let c = line_cat(i);
        c == "lab"
            || c == "imaging"
            || i.pointer("/service/code")
                .and_then(Value::as_str)
                .is_some_and(|s| s.len() == 5 && s.ends_with('F'))
            || TEST_TEXT.is_match(&service_text(i))
    };
    let office_line = |i: &Value| {
        cpt(i).is_some_and(|c| in_ranges(c, OFFICE_CPT)) || OFFICE.is_match(&service_text(i))
    };
    let counted: Vec<&Value> = lines.iter().filter(|i| crate::claims::counted(i)).collect();
    let role = if ty != Ty::Professional {
        ""
    } else if codes.iter().any(|c| in_ranges(*c, HOSPITAL_CPT)) || HOSPITAL_PHYS.is_match(&text) {
        "hospital"
    } else if (category == "lab" || category == "imaging")
        || (!counted.is_empty()
            && counted.iter().all(|i| is_test(i))
            && !counted.iter().any(|i| office_line(i)))
    {
        "ancillary"
    } else if category == "visit"
        || counted
            .iter()
            .any(|i| line_cat(i) == "visit" || office_line(i))
    {
        "office"
    } else {
        "service"
    };
    let vaccine = ty == Ty::Pharmacy
        && (lines.iter().any(|i| line_cat(i) == "vaccine")
            || vaccine_family(&format!("{} {}", r.display, text)).is_some());
    let clinician = is_clinician(&provider);
    let pkey = if clinician {
        let (s, i) = clinician_key(&provider);
        format!("{s} {i}")
    } else {
        org_tokens(&provider).join(" ")
    };
    Some(Claim {
        r,
        provider,
        pkey,
        start,
        end,
        ty,
        fkind,
        role,
        vaccine,
    })
}

fn amounts_of(r: &Record) -> Option<&Map<String, Value>> {
    r.fields.get("amounts").and_then(Value::as_object)
}

/// A claim member, with its claim-level amounts for "who charged what".
fn claim_member(c: &Claim, role: &'static str, rule: &'static str, conf: Conf) -> Member {
    let mut extra = Map::new();
    extra.insert("provider".into(), json!(c.provider));
    if let Some(a) = amounts_of(c.r) {
        let m = claim_money(a);
        for k in ["billed", "allowed", "plan_paid", "you_owe"] {
            if let Some((u, _)) = m.get(k) {
                extra.insert(k.into(), json!(decimal(*u)));
            }
        }
        if m.added {
            extra.insert("you_owe_added".into(), json!(true));
        }
    }
    Member {
        id: c.r.id.clone(),
        document: false,
        kind: "claim".into(),
        role,
        rule,
        conf,
        person: None,
        days: Some((c.start, c.end)),
        extra,
    }
}

struct ClaimMoney {
    values: BTreeMap<&'static str, (i128, String)>,
    added: bool,
}

impl ClaimMoney {
    fn get(&self, k: &str) -> Option<&(i128, String)> {
        self.values.get(k)
    }
}

const SHARE_PARTS: [&str; 4] = ["deductible", "copay", "coinsurance", "not_covered"];

/// A claim's money for an event (R): its totals, `allowed` as `plan_paid`
/// plus `you_owe` when the payer's is below 0, and `you_owe` the share's
/// parts added up when the payer gives none.
fn claim_money(a: &Map<String, Value>) -> ClaimMoney {
    let mut values = BTreeMap::new();
    for k in [
        "billed",
        "allowed",
        "plan_paid",
        "you_owe",
        "deductible",
        "copay",
        "coinsurance",
        "not_covered",
    ] {
        if let Some(v) = units(a.get(k)) {
            values.insert(k, v);
        }
    }
    let mut added = false;
    if !values.contains_key("you_owe") {
        let parts: Vec<&(i128, String)> =
            SHARE_PARTS.iter().filter_map(|k| values.get(k)).collect();
        if let Some(first) = parts.first() {
            if parts.iter().all(|p| p.1 == first.1) {
                let sum = parts.iter().try_fold(0i128, |acc, p| acc.checked_add(p.0));
                if let Some(sum) = sum {
                    values.insert("you_owe", (sum, first.1.clone()));
                    added = true;
                }
            }
        }
    }
    if values.get("allowed").is_some_and(|v| v.0 < 0) {
        match (values.get("plan_paid"), values.get("you_owe")) {
            (Some(p), Some(y)) if p.1 == y.1 => {
                let price = (p.0.checked_add(y.0).unwrap_or(0), p.1.clone());
                values.insert("allowed", price);
            }
            _ => {
                values.remove("allowed");
            }
        }
    }
    ClaimMoney { values, added }
}

// ---------------------------------------------------------------- C: claims

/// C1 to C6: every dated claim into exactly one event.
fn claims_into_events(cs: &[Claim]) -> Vec<Ev> {
    let mut order: Vec<usize> = (0..cs.len()).collect();
    order.sort_by(|&a, &b| {
        (cs[a].start, &cs[a].provider, &cs[a].r.id).cmp(&(
            cs[b].start,
            &cs[b].provider,
            &cs[b].r.id,
        ))
    });
    let mut used = vec![false; cs.len()];
    let mut evs: Vec<Ev> = Vec::new();
    let span = |ev: &Ev| ev.span().unwrap_or((0, 0));

    // C1: facility claims open events; two from one hospital with
    // overlapping days are one. A series takes no other claims.
    for &i in &order {
        let f = &cs[i];
        if used[i] || f.ty != Ty::Facility {
            continue;
        }
        used[i] = true;
        let mut ev = Ev::new(f.fkind, claim_member(f, "facility", "C1", Conf::Sure));
        if f.fkind == "series" {
            ev.notes
                .push("A course of treatment over several weeks: it takes no other claims.");
            evs.push(ev);
            continue;
        }
        for &j in &order {
            let g = &cs[j];
            if !used[j]
                && g.ty == Ty::Facility
                && g.fkind != "series"
                && g.pkey == f.pkey
                && g.start <= f.end
                && f.start <= g.end
            {
                used[j] = true;
                ev.members
                    .push(claim_member(g, "facility", "C1", Conf::Sure));
            }
        }
        evs.push(ev);
    }

    // C2 and C3: professional claims join a facility event.
    for &i in &order {
        let p = &cs[i];
        if used[i] || p.ty != Ty::Professional {
            continue;
        }
        if matches!(p.role, "hospital" | "ancillary") {
            if let Some(ev) = evs.iter_mut().find(|ev| {
                matches!(ev.kind, "er" | "inpatient") && {
                    let (s, e) = span(ev);
                    s <= p.start && p.start <= e
                }
            }) {
                ev.members.push(claim_member(p, p.role, "C2", Conf::Sure));
                used[i] = true;
                continue;
            }
        }
        if p.role != "hospital" {
            if let Some(ev) = evs
                .iter_mut()
                .find(|ev| ev.kind == "outpatient" && span(ev).0 == p.start)
            {
                ev.members.push(claim_member(p, p.role, "C3", Conf::Likely));
                used[i] = true;
            }
        }
    }

    // C4: a stay with no facility claim seen: one provider's hospital
    // claims on touching days, with that provider's other claims inside.
    for &i in &order {
        let p = &cs[i];
        if used[i] || p.ty != Ty::Professional || p.role != "hospital" {
            continue;
        }
        used[i] = true;
        let mut stay = vec![i];
        let (mut lo, mut hi) = (p.start, p.end);
        let mut grew = true;
        while grew {
            grew = false;
            for &j in &order {
                let q = &cs[j];
                if used[j] || q.ty != Ty::Professional || q.pkey != p.pkey {
                    continue;
                }
                if lo - 1 <= q.start && q.start <= hi + 1 {
                    used[j] = true;
                    stay.push(j);
                    lo = lo.min(q.start);
                    hi = hi.max(q.end);
                    grew = true;
                }
            }
        }
        let er = stay.iter().all(|&j| {
            items(cs[j].r).iter().any(|it| {
                cpt(it).is_some_and(|c| (ER_CPT.0..=ER_CPT.1).contains(&c))
                    || EMERG.is_match(&service_text(it))
            })
        });
        let kind = if er {
            "er_no_facility"
        } else {
            "stay_no_facility"
        };
        let mut ev = Ev::new(kind, claim_member(p, p.role, "C4", Conf::Sure));
        for &j in &stay[1..] {
            ev.members
                .push(claim_member(&cs[j], cs[j].role, "C4", Conf::Sure));
        }
        evs.push(ev);
    }

    // C5: the rest, day by day, grouped by provider.
    let rest: Vec<usize> = order
        .iter()
        .copied()
        .filter(|&i| !used[i] && cs[i].ty == Ty::Professional)
        .collect();
    let days: BTreeSet<Day> = rest.iter().map(|&i| cs[i].start).collect();
    for d in days {
        let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
        for &i in rest.iter().filter(|&&i| cs[i].start == d) {
            match groups.iter_mut().find(|(k, _)| *k == cs[i].pkey) {
                Some((_, g)) => g.push(i),
                None => groups.push((cs[i].pkey.clone(), vec![i])),
            }
        }
        let has = |g: &[usize], roles: &[&str]| g.iter().any(|&i| roles.contains(&cs[i].role));
        let mut office_evs: Vec<usize> = Vec::new();
        let mut ancillary: Vec<Vec<usize>> = Vec::new();
        for (_, g) in &groups {
            if has(g, &["office", "hospital"]) {
                let mut ev = Ev::new(
                    "office",
                    claim_member(&cs[g[0]], cs[g[0]].role, "C5", Conf::Sure),
                );
                for &j in &g[1..] {
                    ev.members
                        .push(claim_member(&cs[j], cs[j].role, "C5", Conf::Sure));
                }
                office_evs.push(evs.len());
                evs.push(ev);
            } else if has(g, &["service"]) {
                let mut ev = Ev::new(
                    "service",
                    claim_member(&cs[g[0]], cs[g[0]].role, "C5", Conf::Sure),
                );
                for &j in &g[1..] {
                    ev.members
                        .push(claim_member(&cs[j], cs[j].role, "C5", Conf::Sure));
                }
                evs.push(ev);
            } else {
                ancillary.push(g.clone());
            }
        }
        for g in &groups {
            for &i in &g.1 {
                used[i] = true;
            }
        }
        if ancillary.is_empty() {
            continue;
        }
        let flat: Vec<usize> = ancillary.iter().flatten().copied().collect();
        if office_evs.len() == 1 {
            let ev = &mut evs[office_evs[0]];
            let office_key = cs
                .iter()
                .find(|c| c.r.id == ev.anchor)
                .map(|c| c.pkey.clone())
                .unwrap_or_default();
            for &j in &flat {
                let conf = if cs[j].pkey == office_key {
                    Conf::Sure
                } else {
                    Conf::Likely
                };
                ev.members
                    .push(claim_member(&cs[j], "ancillary", "C5", conf));
            }
        } else if office_evs.is_empty() {
            let conf = if ancillary.len() == 1 {
                Conf::Sure
            } else {
                Conf::Likely
            };
            let mut ev = Ev::new("tests", claim_member(&cs[flat[0]], "ancillary", "C5", conf));
            for &j in &flat[1..] {
                ev.members
                    .push(claim_member(&cs[j], "ancillary", "C5", conf));
            }
            evs.push(ev);
        } else {
            let mut ev = Ev::new(
                "tests",
                claim_member(&cs[flat[0]], "ancillary", "C5", Conf::Unsure),
            );
            for &j in &flat[1..] {
                ev.members
                    .push(claim_member(&cs[j], "ancillary", "C5", Conf::Unsure));
            }
            ev.notes
                .push("Two or more office visits that day: the tests are not assigned to one.");
            ev.could_join = office_evs.iter().map(|&k| evs[k].anchor.clone()).collect();
            evs.push(ev);
        }
    }

    // C6: fills and vaccines by pharmacy and day; dental by provider and day.
    for ty in [Ty::Pharmacy, Ty::Dental] {
        let mut groups: Vec<((String, Day), Vec<usize>)> = Vec::new();
        for &i in &order {
            if used[i] || cs[i].ty != ty {
                continue;
            }
            used[i] = true;
            let key = (cs[i].pkey.clone(), cs[i].start);
            match groups.iter_mut().find(|(k, _)| *k == key) {
                Some((_, g)) => g.push(i),
                None => groups.push((key, vec![i])),
            }
        }
        for (_, g) in groups {
            let kind = match ty {
                Ty::Dental => "dental",
                _ if g.iter().any(|&i| cs[i].vaccine) => "vaccine",
                _ => "fill",
            };
            let role = if ty == Ty::Dental { "dental" } else { "fill" };
            let mut ev = Ev::new(kind, claim_member(&cs[g[0]], role, "C6", Conf::Sure));
            for &j in &g[1..] {
                ev.members
                    .push(claim_member(&cs[j], role, "C6", Conf::Sure));
            }
            evs.push(ev);
        }
    }
    debug_assert!(
        used.iter().all(|u| *u),
        "every claim lands in exactly one event"
    );
    evs
}

/// The kind of an event a single claim opens on its own (an `apart`).
fn kind_alone(c: &Claim) -> &'static str {
    match c.ty {
        Ty::Facility => c.fkind,
        Ty::Dental => "dental",
        Ty::Pharmacy if c.vaccine => "vaccine",
        Ty::Pharmacy => "fill",
        Ty::Professional => match c.role {
            "hospital" => "stay_no_facility",
            "ancillary" => "tests",
            "office" => "office",
            _ => "service",
        },
    }
}

// ---------------------------------------------------------------- E: the portal

const VISIT_CLASSES: &[&str] = &[
    "appointment",
    "ambulatory",
    "amb",
    "emergency",
    "emer",
    "inpatient",
    "imp",
    "inpatient encounter",
    "observation",
    "obsenc",
    "virtual",
    "vr",
    "acute",
    "nonac",
    "short stay",
    "ss",
    "outpatient",
    "office visit",
    "home health",
    "hh",
];
static CONTACT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)telephone|phone|refill|orders? only|documentation|triage|support|message|letter|history|questionnaire|chart|e-?visit message|patient outreach|results? follow")
        .unwrap()
});

/// E1: a visit-class encounter, which can match a claim; the rest are
/// contacts (a call, a refill, orders only), which never do.
fn is_visit(r: &Record) -> bool {
    let class = str_field(r, "class").unwrap_or("").to_lowercase();
    if VISIT_CLASSES.contains(&class.as_str()) {
        return !CONTACT.is_match(&r.display);
    }
    if !class.is_empty() {
        return false;
    }
    !CONTACT.is_match(&r.display)
}

fn source_key(p: &[crate::model::Provenance]) -> Option<String> {
    let s = &p.first()?.source;
    Some(format!(
        "{}|{}|{}",
        s.cli.as_deref().unwrap_or(""),
        s.organization_id.as_deref().unwrap_or(""),
        s.fhir_base.as_deref().unwrap_or("")
    ))
}

const SKIPPED_STATUS: &[&str] = &["planned", "cancelled", "entered-in-error"];

fn record_start(r: &Record) -> Option<&str> {
    r.fields
        .get("period")
        .and_then(|p| p.get("start"))
        .and_then(Value::as_str)
        .or(r.date.as_deref())
}

/// The local days a record can have started on (T2).
fn record_days(r: &Record) -> BTreeSet<Day> {
    record_start(r).map(local_days).unwrap_or_default()
}

/// The day a record is shown as starting on.
fn record_day(r: &Record) -> Option<Day> {
    record_start(r).and_then(shown_day)
}

/// The longest period, in days, read as one stay rather than an error.
const LONGEST_PERIOD: Day = 90;

/// The days a record is shown as starting and ending on: its period's end,
/// when it has one, on or after the start and at most `LONGEST_PERIOD`
/// days later; else its start.
fn record_dates(r: &Record) -> Option<(Day, Day)> {
    let start = record_day(r)?;
    let end = r
        .fields
        .get("period")
        .and_then(|p| p.get("end"))
        .and_then(Value::as_str)
        .and_then(shown_day)
        .filter(|e| *e >= start && *e - start <= LONGEST_PERIOD)
        .unwrap_or(start);
    Some((start, end))
}

/// Every day a record can fall on: its local start days and each day
/// through its period's end.
fn record_span(r: &Record) -> BTreeSet<Day> {
    let mut days = record_days(r);
    if let Some((s, e)) = record_dates(r) {
        days.extend(s..=e);
    }
    days
}

fn record_member(
    r: &Record,
    role: &'static str,
    rule: &'static str,
    conf: Conf,
    spans: bool,
) -> Member {
    let dates = record_dates(r);
    Member {
        id: r.id.clone(),
        document: false,
        kind: r.kind.clone(),
        role,
        rule,
        conf,
        person: None,
        days: if spans {
            Some(dates.unwrap_or((0, 0)))
        } else {
            None
        },
        extra: Map::new(),
    }
}

/// The claim providers and places an event's papers can name.
fn event_names(ev: &Ev, idx: &BTreeMap<&str, &Record>) -> Vec<String> {
    let mut names = Vec::new();
    for m in &ev.members {
        if let Some(r) = idx.get(m.id.as_str()) {
            for k in [
                "provider",
                "facility",
                "service_provider",
                "location",
                "pharmacy",
            ] {
                if let Some(s) = str_field(r, k) {
                    names.push(s.to_string());
                }
            }
        }
    }
    if let Some(p) = &ev.linked_place {
        names.push(p.clone());
    }
    names
}

/// E2 to E5: portal visits to claim events, and their records to them.
fn portal_into_events(evs: &mut Vec<Ev>, records: &[Record], first_claim: Option<Day>) {
    let idx: BTreeMap<&str, &Record> = records.iter().map(|r| (r.id.as_str(), r)).collect();
    let live = |r: &&Record| {
        !r.status
            .as_deref()
            .is_some_and(|s| SKIPPED_STATUS.contains(&s))
    };
    let encounters: Vec<&Record> = records
        .iter()
        .filter(|r| r.kind == "encounter")
        .filter(live)
        .collect();

    // E2: visit encounters at one place on one local day are one visit.
    struct Pv<'a> {
        encs: Vec<&'a Record>,
        place: String,
        days: BTreeSet<Day>,
    }
    let mut pvs: Vec<Pv> = Vec::new();
    let mut visits: Vec<&Record> = encounters.iter().copied().filter(|r| is_visit(r)).collect();
    visits.sort_by(|a, b| (record_day(a), &a.date, &a.id).cmp(&(record_day(b), &b.date, &b.id)));
    for e in visits {
        let days = record_days(e);
        if days.is_empty() {
            continue;
        }
        let place = str_field(e, "location")
            .or_else(|| str_field(e, "service_provider"))
            .map(str::to_string)
            .or_else(|| source_key(&e.provenance))
            .unwrap_or_default();
        match pvs
            .iter_mut()
            .find(|v| v.place == place && !v.days.is_disjoint(&days))
        {
            Some(v) => {
                v.encs.push(e);
                v.days.extend(days);
            }
            None => pvs.push(Pv {
                encs: vec![e],
                place,
                days,
            }),
        }
    }

    // E3: a portal visit joins the one claim event whose dates hold its day.
    for v in pvs {
        let cands: Vec<usize> = evs
            .iter()
            .enumerate()
            .filter(|(_, ev)| {
                !matches!(ev.kind, "fill" | "vaccine" | "dental" | "series" | "portal")
            })
            .filter(|(_, ev)| {
                ev.span()
                    .is_some_and(|(s, e)| v.days.iter().any(|d| s <= *d && *d <= e))
            })
            .map(|(i, _)| i)
            .collect();
        let members = |rule: &'static str, conf: Conf| -> Vec<Member> {
            v.encs
                .iter()
                .map(|e| record_member(e, "portal", rule, conf, true))
                .collect()
        };
        if cands.len() == 1 {
            let ev = &mut evs[cands[0]];
            let claim_providers: Vec<String> = ev
                .claims()
                .filter_map(|m| idx.get(m.id.as_str()))
                .filter_map(|r| str_field(r, "provider").map(str::to_string))
                .collect();
            let mut conf = Conf::Likely;
            let mut diffs = Vec::new();
            for e in &v.encs {
                for k in ["service_provider", "location"] {
                    if let Some(n) = str_field(e, k) {
                        if claim_providers.iter().any(|c| same_org(n, c)) {
                            conf = Conf::Sure;
                        }
                    }
                }
                if let Some(who) = str_field(e, "practitioner") {
                    for c in &claim_providers {
                        match same_clinician(who, c) {
                            Same::Yes => conf = Conf::Sure,
                            Same::InitialDiffers => {
                                conf = Conf::Sure;
                                diffs.push(json!({"field": "clinician", "values": [
                                    {"from": "portal", "value": who}, {"from": "claims", "value": c}], "rule": "N3"}));
                            }
                            Same::No => {}
                        }
                    }
                }
            }
            let (s, _) = ev.span().unwrap_or((0, 0));
            if !v.days.contains(&s) && v.days.iter().all(|d| *d != s) {
                if let Some(d) = v.days.first() {
                    diffs.push(json!({"field": "date", "values": [
                        {"from": "portal", "value": iso(*d)}, {"from": "claims", "value": iso(s)}], "rule": "T2"}));
                }
            }
            for mut m in members("E3", conf) {
                m.days = None;
                ev.members.push(m);
            }
            ev.differences.extend(diffs);
        } else {
            let mut ms = members(
                "E3",
                if cands.len() > 1 {
                    Conf::Unsure
                } else {
                    Conf::Sure
                },
            );
            let first = ms.remove(0);
            let mut ev = Ev::new("portal", first);
            ev.members.extend(ms);
            if cands.len() > 1 {
                ev.notes
                    .push("Two or more claim groups that day could be this visit.");
                ev.could_join = cands.iter().map(|&i| evs[i].anchor.clone()).collect();
            } else if first_claim.is_some_and(|f| v.days.iter().any(|d| *d >= f)) {
                ev.notes.push("expect_claim");
            }
            evs.push(ev);
        }
    }

    // E1: a contact joins a visit of its organization within 3 days.
    for e in encounters.iter().filter(|r| !is_visit(r)) {
        let Some(d) = record_day(e) else {
            continue;
        };
        let key = source_key(&e.provenance);
        let org = str_field(e, "service_provider");
        let best = evs
            .iter()
            .enumerate()
            .filter(|(_, ev)| !matches!(ev.kind, "fill" | "vaccine" | "dental"))
            .filter(|(_, ev)| {
                let same_source = ev.members.iter().any(|m| {
                    m.role == "portal"
                        && idx
                            .get(m.id.as_str())
                            .is_some_and(|r| source_key(&r.provenance) == key)
                });
                same_source
                    || org.is_some_and(|o| event_names(ev, &idx).iter().any(|n| same_org(o, n)))
            })
            .filter_map(|(i, ev)| {
                let (s, en) = ev.span()?;
                let gap = if d < s {
                    s - d
                } else if d > en {
                    d - en
                } else {
                    0
                };
                (gap <= 3).then_some((gap, i))
            })
            .min();
        if let Some((_, i)) = best {
            evs[i]
                .members
                .push(record_member(e, "contact", "E1", Conf::Sure, false));
        }
    }

    // E4: a portal visit's satellite records, from its source and local
    // day; a result may be dated a day earlier, by its collection.
    let satellites = [
        "result",
        "report",
        "condition",
        "allergy",
        "medication",
        "procedure",
        "immunization",
    ];
    for r in records
        .iter()
        .filter(|r| satellites.contains(&r.kind.as_str()))
        .filter(live)
    {
        if r.kind == "medication" && str_field(r, "source") == Some("dispense") {
            continue;
        }
        let days = record_days(r);
        if days.is_empty() {
            continue;
        }
        let key = source_key(&r.provenance);
        let cands: Vec<usize> = evs
            .iter()
            .enumerate()
            .filter(|(_, ev)| {
                ev.members.iter().any(|m| {
                    m.role == "portal"
                        && idx.get(m.id.as_str()).is_some_and(|enc| {
                            source_key(&enc.provenance) == key
                                && record_span(enc).iter().any(|ed| {
                                    days.contains(ed)
                                        || (matches!(r.kind.as_str(), "result" | "report")
                                            && days.contains(&(ed - 1)))
                                })
                        })
                })
            })
            .map(|(i, _)| i)
            .collect();
        if cands.len() == 1 {
            evs[cands[0]]
                .members
                .push(record_member(r, "satellite", "E4", Conf::Sure, false));
        }
    }

    // The claims' own links: a lab line's results join its claim's event.
    let placed: BTreeSet<String> = evs
        .iter()
        .flat_map(|e| e.members.iter().map(|m| m.id.clone()))
        .collect();
    let mut adds: Vec<(usize, Member)> = Vec::new();
    for (i, ev) in evs.iter().enumerate() {
        for m in ev.claims() {
            let Some(claim) = idx.get(m.id.as_str()) else {
                continue;
            };
            for it in items(claim) {
                let Some(recs) = it.get("records").and_then(Value::as_array) else {
                    continue;
                };
                for id in recs.iter().filter_map(Value::as_str) {
                    let Some(r) = idx.get(id) else { continue };
                    if !matches!(r.kind.as_str(), "result" | "report") {
                        continue;
                    }
                    let mut priced = Map::new();
                    priced.insert("record".into(), json!(claim.id));
                    if let Some(s) = it.get("sequence") {
                        priced.insert("sequence".into(), s.clone());
                    }
                    if let Some(a) = it.pointer("/amounts/allowed/amount") {
                        priced.insert("allowed".into(), a.clone());
                    }
                    if placed.contains(id) {
                        adds.push((
                            usize::MAX,
                            Member {
                                extra: priced,
                                ..record_member(r, "satellite", "", Conf::Sure, false)
                            },
                        ));
                        continue;
                    }
                    if adds.iter().any(|(_, a)| a.id == id) {
                        continue;
                    }
                    let mut mem = record_member(r, "satellite", "claim_link", Conf::Likely, false);
                    mem.extra.insert("priced_by".into(), Value::Object(priced));
                    adds.push((i, mem));
                }
            }
        }
    }
    for (i, m) in adds {
        if i == usize::MAX {
            // Already placed by E4: it takes the claim line's price.
            for ev in evs.iter_mut() {
                if let Some(have) = ev.members.iter_mut().find(|x| x.id == m.id) {
                    have.extra
                        .entry("priced_by")
                        .or_insert(Value::Object(m.extra.clone()));
                }
            }
        } else {
            evs[i].members.push(m);
        }
    }

    // E5: an immunization with no visit joins the vaccine claim of its day
    // and family; with none, it is a vaccine event of its own.
    let placed: BTreeSet<String> = evs
        .iter()
        .flat_map(|e| e.members.iter().map(|m| m.id.clone()))
        .collect();
    for r in records
        .iter()
        .filter(|r| r.kind == "immunization")
        .filter(live)
    {
        if placed.contains(&r.id) {
            continue;
        }
        let Some(d) = record_day(r) else {
            continue;
        };
        let fam = vaccine_family(&r.display);
        let target = evs.iter().position(|ev| {
            matches!(ev.kind, "vaccine" | "fill" | "office" | "service")
                && ev.span().is_some_and(|(s, e)| s <= d && d <= e)
                && ev.claims().any(|m| {
                    idx.get(m.id.as_str()).is_some_and(|c| {
                        let text = format!(
                            "{} {}",
                            c.display,
                            items(c)
                                .iter()
                                .map(service_text)
                                .collect::<Vec<_>>()
                                .join(" ")
                        );
                        fam.is_some() && vaccine_family(&text) == fam
                    })
                })
        });
        // With no claim, a day's doses from one source are one visit.
        let key = source_key(&r.provenance);
        let same_visit = || {
            evs.iter().position(|ev| {
                ev.kind == "vaccine"
                    && ev.claims().next().is_none()
                    && ev.span() == Some((d, d))
                    && ev.members.iter().all(|m| {
                        m.role == "vaccine"
                            && idx
                                .get(m.id.as_str())
                                .is_some_and(|o| source_key(&o.provenance) == key)
                    })
            })
        };
        match target {
            Some(i) => evs[i]
                .members
                .push(record_member(r, "satellite", "E5", Conf::Sure, false)),
            None => match same_visit() {
                Some(i) => evs[i]
                    .members
                    .push(record_member(r, "vaccine", "E5", Conf::Sure, true)),
                None => evs.push(Ev::new(
                    "vaccine",
                    record_member(r, "vaccine", "E5", Conf::Sure, true),
                )),
            },
        }
    }

    // A fill from the pharmacy's records joins the pharmacy claim of its
    // day; with none, the fills of one pharmacy and day are one event.
    for r in records
        .iter()
        .filter(|r| r.kind == "medication" && str_field(r, "source") == Some("dispense"))
        .filter(live)
    {
        let Some(d) = record_day(r) else {
            continue;
        };
        let pharmacy = str_field(r, "pharmacy").unwrap_or("");
        let same_day: Vec<usize> = evs
            .iter()
            .enumerate()
            .filter(|(_, ev)| {
                matches!(ev.kind, "fill" | "vaccine") && ev.span().is_some_and(|(s, _)| s == d)
            })
            .map(|(i, _)| i)
            .collect();
        let by_name: Vec<usize> = same_day
            .iter()
            .copied()
            .filter(|&i| {
                !pharmacy.is_empty()
                    && event_names(&evs[i], &idx)
                        .iter()
                        .any(|n| same_org(pharmacy, n))
            })
            .collect();
        if let [i] = by_name.as_slice() {
            evs[*i]
                .members
                .push(record_member(r, "fill", "C6", Conf::Sure, true));
        } else if let [i] = same_day.as_slice() {
            evs[*i]
                .members
                .push(record_member(r, "fill", "C6", Conf::Likely, true));
        } else {
            evs.push(Ev::new(
                "fill",
                record_member(r, "fill", "C6", Conf::Sure, true),
            ));
        }
    }
}

// ---------------------------------------------------------------- D: papers

const BILLING: &[&str] = &["bill", "receipt"];
const CLINICAL: &[&str] = &[
    "discharge_summary",
    "visit_summary",
    "imaging_report",
    "lab_report",
    "prescription",
    "immunization_record",
];
static PORTAL_NOTE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)progress note|encounter summary|^note$|after visit|discharge|visit summary")
        .unwrap()
});
static DIVISION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)([A-Z][\w&'. ]{2,60}?),? (?:is )?a division of ([A-Z][\w&'. ]{2,60}?)(?:[,.;\n]|$)",
    )
    .unwrap()
});

struct Alias {
    clinician: String,
    organization: String,
    document: String,
    conf: Conf,
}

struct Papers<'a> {
    idx: BTreeMap<&'a str, &'a Record>,
    aliases: Vec<Alias>,
    pending: Vec<Value>,
}

fn doc_member(d: &Document, rule: &'static str, conf: Conf) -> Member {
    Member {
        id: d.id.clone(),
        document: true,
        kind: d.doc_type.clone().unwrap_or_else(|| "other".into()),
        role: "paper",
        rule,
        conf,
        person: None,
        days: None,
        extra: Map::new(),
    }
}

fn event_you_owe(ev: &Ev, idx: &BTreeMap<&str, &Record>) -> Option<i128> {
    let mut sum = 0i128;
    for m in ev.claims() {
        let a = amounts_of(idx.get(m.id.as_str())?)?;
        sum = sum.checked_add(claim_money(a).get("you_owe")?.0)?;
    }
    Some(sum)
}

impl Papers<'_> {
    fn org_matches(&self, org: &str, name: &str) -> bool {
        same_org(org, name)
            || self.aliases.iter().any(|a| {
                same_org(org, &a.organization) && same_clinician(name, &a.clinician) == Same::Yes
            })
    }

    fn by_org(&self, ev: &Ev, org: &str, text: &str, strict: bool) -> Option<&'static str> {
        let names = event_names(ev, &self.idx);
        if names.iter().any(|n| same_org(org, n)) {
            return Some("D2");
        }
        if names.iter().any(|n| self.org_matches(org, n)) {
            return Some("N5");
        }
        let named = ev.claims().any(|m| {
            self.idx.get(m.id.as_str()).is_some_and(|r| {
                str_field(r, "provider").is_some_and(|p| {
                    is_clinician(p) && clinician_in_text(p, &format!("{org} {text}"), strict)
                })
            })
        });
        named.then_some("D2b")
    }
}

/// D2 to D6 and N5: papers into events.
fn papers_into_events(
    evs: &mut [Ev],
    records: &[Record],
    docs: &[Document],
    texts: &PersonText,
) -> (Vec<Value>, Vec<Alias>) {
    let mut p = Papers {
        idx: records.iter().map(|r| (r.id.as_str(), r)).collect(),
        aliases: Vec::new(),
        pending: Vec::new(),
    };
    let mut order: Vec<&Document> = docs.iter().collect();
    order.sort_by(|a, b| (&a.date, &a.filed_at, &a.id).cmp(&(&b.date, &b.filed_at, &b.id)));
    let text_of = |d: &Document| -> String {
        texts
            .texts
            .get(&d.id)
            .map(|t| t.chars().take(4000).collect())
            .unwrap_or_default()
    };
    let mut loose: Vec<&Document> = Vec::new();

    for pass in 0..2 {
        let batch: Vec<&Document> = if pass == 0 {
            order.clone()
        } else {
            std::mem::take(&mut loose)
        };
        for d in batch {
            // D6: a copy goes with the file it copies.
            let copy_of = d
                .near_duplicates
                .iter()
                .map(|n| n.document.as_str())
                .chain(d.same_statement_as.as_deref())
                .find_map(|o| {
                    evs.iter()
                        .enumerate()
                        .find_map(|(i, e)| e.members.iter().position(|m| m.id == o).map(|k| (i, k)))
                });
            if let Some((i, k)) = copy_of {
                let m = &mut evs[i].members[k];
                let copies = m.extra.entry("copies").or_insert_with(|| json!([]));
                if let Some(a) = copies.as_array_mut() {
                    a.push(json!(d.id));
                }
                continue;
            }
            let ty = d.doc_type.as_deref().unwrap_or("other");
            let Some(dd) = d.date.as_deref().and_then(day) else {
                continue;
            };
            if matches!(ty, "eob" | "insurance_card") {
                continue;
            }
            let org = d.organization.as_deref().unwrap_or("");
            let text = text_of(d);
            if pass == 0 {
                for c in DIVISION.captures_iter(&text) {
                    let (a, b) = (c[1].trim().to_string(), c[2].trim().to_string());
                    if !same_org(&a, &b) {
                        p.aliases.push(Alias {
                            clinician: a,
                            organization: b,
                            document: d.id.clone(),
                            conf: Conf::Sure,
                        });
                    }
                }
            }

            // D4: a portal's note joins its visit 0 to 3 days before it.
            let portal_key = d
                .provenance
                .source
                .organization_id
                .as_ref()
                .map(|_| source_key(std::slice::from_ref(&d.provenance)));
            let portal_cands: Vec<usize> = evs
                .iter()
                .enumerate()
                .filter(|(_, ev)| {
                    ev.members.iter().any(|m| {
                        m.role == "portal"
                            && p.idx.get(m.id.as_str()).is_some_and(|e| {
                                portal_key.is_some()
                                    && Some(source_key(&e.provenance)) == portal_key
                                    && record_days(e).iter().any(|x| *x <= dd && dd <= x + 3)
                            })
                    })
                })
                .map(|(i, _)| i)
                .collect();
            let portal_note = matches!(ty, "visit_summary" | "lab_report")
                || d.title.as_deref().is_some_and(|t| PORTAL_NOTE.is_match(t));
            if pass == 0 && portal_note && !portal_cands.is_empty() {
                if let [i] = portal_cands.as_slice() {
                    evs[*i].members.push(doc_member(d, "D4", Conf::Sure));
                } else {
                    p.pending.push(json!({"document": d.id, "could_join":
                        portal_cands.iter().map(|&i| evs[i].anchor.clone()).collect::<Vec<_>>(), "rule": "D4"}));
                }
                continue;
            }

            // D5: a prescription joins the fill of its pharmacy 0 to 1 day later.
            if ty == "prescription" && !org.is_empty() {
                if let Some(i) = evs.iter().position(|ev| {
                    ev.kind == "fill"
                        && ev.span().is_some_and(|(s, _)| dd <= s && s <= dd + 1)
                        && event_names(ev, &p.idx).iter().any(|n| same_org(org, n))
                }) {
                    evs[i].members.push(doc_member(d, "D5", Conf::Sure));
                    continue;
                }
            }

            // D3s: a statement, sent weeks later, whose amount due is a
            // claim's you_owe from its provider in the 6 months before it.
            let due = d.statement.as_ref().and_then(|s| s.amount_due.as_ref());
            if let (Some(due), "bill") = (due, ty) {
                let due_u = crate::claims::units(&due.amount);
                let mut hits: Vec<(usize, String)> = Vec::new();
                for (i, ev) in evs.iter().enumerate() {
                    let Some((_, e)) = ev.span() else { continue };
                    if !(dd - 183 <= e && e <= dd) {
                        continue;
                    }
                    for m in ev.claims() {
                        let Some(r) = p.idx.get(m.id.as_str()) else {
                            continue;
                        };
                        let Some(prov) = str_field(r, "provider") else {
                            continue;
                        };
                        let by_name = p.org_matches(org, prov)
                            || (is_clinician(prov)
                                && clinician_in_text(prov, &format!("{org} {text}"), true));
                        let owe =
                            amounts_of(r).and_then(|a| claim_money(a).get("you_owe").cloned());
                        if by_name
                            && owe.is_some_and(|(u, c)| Some(u) == due_u && c == due.currency)
                        {
                            hits.push((i, r.id.clone()));
                        }
                    }
                }
                if let [(i, claim)] = hits.as_slice() {
                    let mut m = doc_member(d, "D3s", Conf::Sure);
                    m.extra.insert("matches".into(), json!(claim));
                    evs[*i].members.push(m);
                    continue;
                }
            }

            // D2 and D2b: its organization, or a clinician it names, from
            // the event's first day to 3 days after its last (1 day for
            // other papers). The event holding its date wins, then the
            // nearest.
            let late = if BILLING.contains(&ty) || CLINICAL.contains(&ty) {
                3
            } else {
                1
            };
            let mut best: Option<(Day, usize, &'static str)> = None;
            if !org.is_empty() || !text.is_empty() {
                for (i, ev) in evs.iter().enumerate() {
                    if ev.kind == "series" {
                        continue;
                    }
                    let Some((s, e)) = ev.span() else { continue };
                    if !(s <= dd && dd <= e + late) {
                        continue;
                    }
                    if let Some(rule) = p.by_org(ev, org, &text, true) {
                        let gap = (dd - e).max(0);
                        if best.is_none_or(|b| gap < b.0) {
                            best = Some((gap, i, rule));
                        }
                    }
                }
            }
            if let Some((_, i, rule)) = best {
                evs[i].members.push(doc_member(d, rule, Conf::Sure));
                continue;
            }

            // D3: a bill, receipt or report from a practice no claim names
            // joins the only visit that day: likely, or sure when the
            // amount due is the event's you_owe. N5 learns the alias.
            let d3_types = BILLING.contains(&ty)
                || (CLINICAL.contains(&ty)
                    && !matches!(ty, "prescription" | "immunization_record"));
            let pharmacy = org_tokens(org).iter().any(|t| t == "pharmacy")
                || evs.iter().any(|ev| {
                    ev.kind == "fill" && event_names(ev, &p.idx).iter().any(|n| same_org(org, n))
                });
            if pass == 0 && d3_types && !pharmacy {
                let day_evs: Vec<usize> = evs
                    .iter()
                    .enumerate()
                    .filter(|(_, ev)| !matches!(ev.kind, "fill" | "vaccine" | "series"))
                    .filter(|(_, ev)| ev.span().is_some_and(|(s, e)| s <= dd && dd <= e))
                    .map(|(i, _)| i)
                    .collect();
                if let [i] = day_evs.as_slice() {
                    let agrees = due.is_some_and(|due| {
                        crate::claims::units(&due.amount).is_some()
                            && crate::claims::units(&due.amount) == event_you_owe(&evs[*i], &p.idx)
                    });
                    let conf = if agrees { Conf::Sure } else { Conf::Likely };
                    evs[*i].members.push(doc_member(d, "D3", conf));
                    let clinicians: Vec<String> = evs[*i]
                        .claims()
                        .filter_map(|m| p.idx.get(m.id.as_str()))
                        .filter_map(|r| str_field(r, "provider"))
                        .filter(|n| is_clinician(n))
                        .map(str::to_string)
                        .collect();
                    if let ([c], false) = (clinicians.as_slice(), org.is_empty()) {
                        p.aliases.push(Alias {
                            clinician: c.clone(),
                            organization: org.to_string(),
                            document: d.id.clone(),
                            conf,
                        });
                    }
                    continue;
                }
            }
            if pass == 0 {
                loose.push(d);
            }
        }
    }
    (p.pending, p.aliases)
}

// ---------------------------------------------------------------- X: across people

/// The base events of one person: C, E and D, no overrides, no links.
struct Base {
    evs: Vec<Ev>,
    pending: Vec<Value>,
    aliases: Vec<Alias>,
    undated: Vec<String>,
}

fn base(idx: &PersonIndex, texts: Option<&PersonText>) -> Base {
    let live_claims: Vec<&Record> = idx
        .records
        .iter()
        .filter(|r| r.kind == "claim")
        .filter(|r| {
            !r.status
                .as_deref()
                .is_some_and(|s| SKIPPED_STATUS.contains(&s))
        })
        .collect();
    let mut cs = Vec::new();
    let mut undated = Vec::new();
    for r in live_claims {
        match classify(r) {
            Some(c) => cs.push(c),
            None => undated.push(r.id.clone()),
        }
    }
    let first_claim = cs.iter().map(|c| c.start).min();
    let mut evs = claims_into_events(&cs);
    portal_into_events(&mut evs, &idx.records, first_claim);
    let empty = PersonText::new(&idx.person);
    let (pending, aliases) = papers_into_events(
        &mut evs,
        &idx.records,
        &idx.documents,
        texts.unwrap_or(&empty),
    );
    Base {
        evs,
        pending,
        aliases,
        undated,
    }
}

fn event_id(person: &str, anchor: &str) -> String {
    use sha2::Digest;
    let digest = sha2::Sha256::digest(format!("{person}|{anchor}").as_bytes());
    crate::ids::from_digest("e", &digest)
}

// ---------------------------------------------------------------- overrides

/// The household's decisions, after every rule. `together` moves the second
/// member to the event holding the first; `apart` moves the one that is not
/// the event's anchor out, to an event of its own (a claim or an
/// encounter) or out of every event (a paper or a record).
fn apply(evs: &mut Vec<Ev>, idx: &PersonIndex, decisions: &[Decision]) -> usize {
    let recs: BTreeMap<&str, &Record> = idx.records.iter().map(|r| (r.id.as_str(), r)).collect();
    let docs: BTreeMap<&str, &Document> =
        idx.documents.iter().map(|d| (d.id.as_str(), d)).collect();
    let mut applied = 0;
    let take = |evs: &mut Vec<Ev>, id: &str| -> Option<Member> {
        let i = evs.iter().position(|e| e.holds(id))?;
        let k = evs[i].members.iter().position(|m| m.id == id)?;
        let m = evs[i].members.remove(k);
        if evs[i].members.is_empty() {
            evs.remove(i);
        } else if evs[i].anchor == id {
            evs[i].anchor = evs[i].members[0].id.clone();
        }
        Some(m)
    };
    let fresh = |id: &str| -> Option<(Member, &'static str)> {
        if let Some(r) = recs.get(id) {
            if r.kind == "claim" {
                let c = classify(r)?;
                let role = match c.ty {
                    Ty::Facility => "facility",
                    Ty::Pharmacy => "fill",
                    Ty::Dental => "dental",
                    Ty::Professional => c.role,
                };
                return Some((
                    claim_member(&c, role, "household", Conf::Sure),
                    kind_alone(&c),
                ));
            }
            let spans = matches!(r.kind.as_str(), "encounter" | "immunization" | "medication");
            let kind = match r.kind.as_str() {
                "encounter" => "portal",
                "immunization" => "vaccine",
                _ => "fill",
            };
            return Some((
                record_member(
                    r,
                    if r.kind == "encounter" {
                        "portal"
                    } else {
                        "satellite"
                    },
                    "household",
                    Conf::Sure,
                    spans,
                ),
                kind,
            ));
        }
        docs.get(id)
            .map(|d| (doc_member(d, "household", Conf::Sure), ""))
    };
    for dec in decisions {
        let [a, b] = &dec.members;
        match dec.kind.as_str() {
            "together" => {
                if !evs.iter().any(|e| e.holds(a)) {
                    continue;
                }
                let mut m = match take(evs, b) {
                    Some(m) => m,
                    None => match fresh(b) {
                        Some((m, _)) => m,
                        None => continue,
                    },
                };
                m.rule = "household";
                m.conf = Conf::Sure;
                if let Some(ev) = evs.iter_mut().find(|e| e.holds(a)) {
                    ev.members.push(m);
                    applied += 1;
                }
            }
            "apart" => {
                let Some(ev) = evs.iter().find(|e| e.holds(a) && e.holds(b)) else {
                    continue;
                };
                let out = if ev.anchor == *b { a } else { b };
                let Some(m) = take(evs, out) else { continue };
                applied += 1;
                let kind = fresh(out).map(|(_, k)| k).unwrap_or("");
                if (m.kind == "claim" || m.kind == "encounter" || m.days.is_some())
                    && !kind.is_empty()
                {
                    let mut m = m;
                    m.rule = "household";
                    m.conf = Conf::Sure;
                    if m.days.is_none() {
                        m.days = recs
                            .get(out.as_str())
                            .and_then(|r| record_day(r).map(|d| (d, d)));
                    }
                    evs.push(Ev::new(kind, m));
                }
            }
            _ => {}
        }
    }
    applied
}

// ---------------------------------------------------------------- output

/// The kinds of event, each with its title.
pub const KINDS: &[(&str, &str)] = TITLE;

const TITLE: &[(&str, &str)] = &[
    ("er", "ER visit"),
    ("inpatient", "Hospital stay"),
    ("outpatient", "Hospital outpatient visit"),
    ("office", "Office visit"),
    ("service", "Appointment"),
    ("tests", "Tests"),
    ("dental", "Dental visit"),
    ("vaccine", "Vaccine"),
    ("fill", "Pharmacy fill"),
    ("series", "Treatment series"),
    ("stay_no_facility", "Hospital care"),
    ("er_no_facility", "ER care"),
    ("portal", "Visit"),
];

fn money_of(ev: &Ev, recs: &BTreeMap<&str, &Record>) -> Option<Value> {
    let claims: Vec<ClaimMoney> = ev
        .claims()
        .map(|m| {
            recs.get(m.id.as_str())
                .and_then(|r| amounts_of(r))
                .map(claim_money)
                .unwrap_or(ClaimMoney {
                    values: BTreeMap::new(),
                    added: false,
                })
        })
        .collect();
    if claims.is_empty() {
        return None;
    }
    let currency = claims
        .iter()
        .flat_map(|c| c.values.values().map(|v| v.1.clone()))
        .next()?;
    let mut out = Map::new();
    out.insert("currency".into(), json!(currency));
    let mut partial = Vec::new();
    let mut sums: BTreeMap<&str, i128> = BTreeMap::new();
    for k in ["billed", "allowed", "plan_paid", "you_owe"] {
        let all: Option<i128> = claims.iter().try_fold(0i128, |acc, c| {
            let (u, cur) = c.get(k)?;
            (*cur == currency).then_some(())?;
            acc.checked_add(*u)
        });
        match all {
            Some(s) => {
                sums.insert(k, s);
            }
            None => partial.push(k),
        }
    }
    for k in ["billed", "allowed"] {
        if let Some(s) = sums.get(k) {
            out.insert(k.into(), json!(decimal(*s)));
        }
    }
    if let (Some(b), Some(a)) = (sums.get("billed"), sums.get("allowed")) {
        out.insert("discount".into(), json!(decimal(b - a)));
    }
    for k in ["plan_paid", "you_owe"] {
        if let Some(s) = sums.get(k) {
            out.insert(k.into(), json!(decimal(*s)));
        }
    }
    if sums.contains_key("you_owe") && claims.iter().any(|c| c.added) {
        out.insert("you_owe_added".into(), json!(true));
    }
    let mut share = Map::new();
    for k in SHARE_PARTS {
        let given: Vec<i128> = claims
            .iter()
            .filter_map(|c| c.get(k).filter(|v| v.1 == currency).map(|v| v.0))
            .collect();
        if !given.is_empty() {
            let s: i128 = given.iter().sum();
            if s != 0 {
                share.insert(k.into(), json!(decimal(s)));
            }
        }
    }
    if !share.is_empty() {
        out.insert("share".into(), Value::Object(share));
    }
    if !partial.is_empty() {
        out.insert("partial".into(), json!(partial));
    }
    Some(Value::Object(out))
}

/// The longest member title an event carries.
const MEMBER_TITLE: usize = 120;

/// Each record's and document's title and date, by id: the person's and
/// the linked people's, so a reader cites a member from events.json alone.
type Labels<'a> = BTreeMap<&'a str, (String, Option<&'a str>)>;

fn labels_of<'a>(idx: &'a PersonIndex, out: &mut Labels<'a>) {
    for r in &idx.records {
        out.insert(
            &r.id,
            (
                crate::summary::clip(&r.display, MEMBER_TITLE),
                r.date.as_deref(),
            ),
        );
    }
    for d in &idx.documents {
        let title = d
            .title
            .clone()
            .or_else(|| d.original_name.clone())
            .unwrap_or_else(|| crate::views::label(d.doc_type.as_deref()).to_string());
        out.insert(
            &d.id,
            (
                crate::summary::clip(&title, MEMBER_TITLE),
                d.date.as_deref(),
            ),
        );
    }
}

fn render(
    person: &str,
    ev: &Ev,
    recs: &BTreeMap<&str, &Record>,
    labels: &Labels,
    aliases: &[Alias],
) -> Option<(Day, Day, String, Value)> {
    let (start, end) = ev.span()?;
    let id = event_id(person, &ev.anchor);
    let encs: Vec<&Record> = ev
        .members
        .iter()
        .filter(|m| m.role == "portal")
        .filter_map(|m| recs.get(m.id.as_str()).copied())
        .collect();
    let claims: Vec<&Record> = ev
        .claims()
        .filter_map(|m| recs.get(m.id.as_str()).copied())
        .collect();
    let facility = ev
        .claims()
        .find(|m| m.role == "facility")
        .and_then(|m| recs.get(m.id.as_str()).copied());
    // A visit seen only in the portal is a stay or an ER visit when its
    // encounter's class says so.
    let class_is = |classes: &[&str]| {
        encs.iter().any(|e| {
            str_field(e, "class").is_some_and(|c| classes.contains(&c.to_lowercase().as_str()))
        })
    };
    let kind = match ev.kind {
        "portal" if class_is(&["inpatient", "imp", "inpatient encounter", "acute"]) => "inpatient",
        "portal" if class_is(&["emergency", "emer"]) => "er",
        k => k,
    };
    let mut o = Map::new();
    o.insert("id".into(), json!(id));
    o.insert("kind".into(), json!(kind));
    o.insert("start".into(), json!(iso(start)));
    o.insert("end".into(), json!(iso(end)));
    let times: Vec<&str> = encs
        .iter()
        .filter_map(|e| {
            e.fields
                .get("period")
                .and_then(|p| p.get("start"))
                .and_then(Value::as_str)
        })
        .filter(|s| s.contains('T'))
        .collect();
    if !times.is_empty() {
        o.insert("times".into(), json!(times));
    }
    let title = TITLE
        .iter()
        .find(|(k, _)| *k == kind)
        .map(|(_, t)| *t)
        .unwrap_or("Event");
    o.insert("title".into(), json!(title));
    let reason = encs.iter().find_map(|e| str_field(e, "reason"));
    let from_claims = facility.or(claims.first().copied()).and_then(|c| {
        c.display
            .split(" · ")
            .next()
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    });
    if let Some(r) = reason {
        o.insert("what".into(), json!(r));
        o.insert("what_from".into(), json!("portal"));
    } else if encs.is_empty() {
        let listed = |names: Vec<&str>| -> Option<String> {
            let mut seen: Vec<&str> = Vec::new();
            for n in names.into_iter().filter(|s| !s.is_empty()) {
                if !seen.contains(&n) {
                    seen.push(n);
                }
            }
            match seen.as_slice() {
                [] => None,
                [a] => Some(a.to_string()),
                [a, b] => Some(format!("{a} and {b}")),
                [a, b, rest @ ..] => Some(format!("{a}, {b} and {} more", rest.len())),
            }
        };
        let (what, from) = if matches!(ev.kind, "fill" | "vaccine") {
            // The claims' names, else the fills' or doses' own.
            match listed(
                claims
                    .iter()
                    .filter_map(|c| c.display.split(" · ").next())
                    .collect(),
            ) {
                Some(w) => (Some(w), "claims"),
                None => (
                    listed(
                        ev.members
                            .iter()
                            .filter(|m| matches!(m.role, "fill" | "vaccine") && m.kind != "claim")
                            .filter_map(|m| recs.get(m.id.as_str()))
                            .map(|r| r.display.as_str())
                            .collect(),
                    )
                    .map(|w| crate::summary::clip(&w, MEMBER_TITLE)),
                    "portal",
                ),
            }
        } else {
            (from_claims, "claims")
        };
        if let Some(w) = what {
            o.insert("what".into(), json!(w));
            o.insert("what_from".into(), json!(from));
        }
    }
    let portal_place = encs
        .iter()
        .find_map(|e| str_field(e, "service_provider").or_else(|| str_field(e, "location")));
    let first_org = claims
        .iter()
        .filter_map(|c| str_field(c, "provider"))
        .find(|p| !is_clinician(p));
    let alias = claims
        .iter()
        .filter_map(|c| str_field(c, "provider"))
        .find_map(|p| {
            aliases
                .iter()
                .find(|a| same_clinician(p, &a.clinician) == Same::Yes)
                .map(|a| a.organization.clone())
        });
    let (place, from) = if let Some(p) = portal_place {
        (Some(p.to_string()), "portal")
    } else if let Some(f) =
        facility.and_then(|f| str_field(f, "provider").or_else(|| str_field(f, "facility")))
    {
        (Some(f.to_string()), "claims")
    } else if let Some(p) = &ev.linked_place {
        (Some(p.clone()), "linked")
    } else if let Some(p) = first_org {
        (Some(p.to_string()), "claims")
    } else if let Some(a) = alias {
        (Some(a), "alias")
    } else {
        // A clinician's name is not a place: the event has none to give.
        let fill = ev
            .members
            .iter()
            .filter_map(|m| recs.get(m.id.as_str()))
            .find_map(|r| str_field(r, "pharmacy"));
        (fill.map(str::to_string), "claims")
    };
    if let Some(p) = place {
        o.insert("place".into(), json!(p));
        o.insert("place_from".into(), json!(from));
    }
    let mut clinicians: Vec<String> = Vec::new();
    for n in encs
        .iter()
        .filter_map(|e| str_field(e, "practitioner"))
        .chain(
            claims
                .iter()
                .filter_map(|c| str_field(c, "provider"))
                .filter(|p| is_clinician(p)),
        )
    {
        if !clinicians.iter().any(|c| same_clinician(c, n) != Same::No) {
            clinicians.push(n.to_string());
        }
    }
    if !clinicians.is_empty() {
        o.insert("clinicians".into(), json!(clinicians));
    }
    let conf = ev
        .members
        .iter()
        .map(|m| m.conf)
        .min()
        .unwrap_or(Conf::Sure);
    o.insert("confidence".into(), json!(conf.name()));
    let money = money_of(ev, recs);

    let mut expected = Vec::new();
    let has_prof = ev.claims().any(|m| m.role != "facility");
    if matches!(ev.kind, "er" | "inpatient") && !has_prof {
        expected.push(json!({"what": "doctor_claims", "why": "Only the facility's claim is in. Doctors' claims usually follow within 1–2 months.", "since": iso(end)}));
    }
    if matches!(ev.kind, "er_no_facility" | "stay_no_facility") {
        expected.push(json!({"what": "facility_claim", "why": "No facility claim seen for these days.", "since": iso(end)}));
    }
    if ev.kind == "portal" && ev.notes.contains(&"expect_claim") {
        expected.push(json!({"what": "claim", "why": "No insurer claim for this visit yet.", "since": iso(end)}));
    }
    let owes = money
        .as_ref()
        .and_then(|m| m.get("you_owe"))
        .and_then(Value::as_str)
        .and_then(crate::claims::units)
        .is_some_and(|u| u > 0);
    let papered = ev
        .members
        .iter()
        .any(|m| m.document && BILLING.contains(&m.kind.as_str()));
    // A pharmacy's share is paid at the counter.
    if owes && !papered && !matches!(ev.kind, "fill" | "vaccine") {
        expected.push(json!({"what": "bill", "why": "No bill filed yet.", "since": iso(end)}));
    }
    let status = if conf == Conf::Unsure {
        "unsure"
    } else if !expected.is_empty() {
        "waiting"
    } else {
        "complete"
    };
    o.insert("status".into(), json!(status));
    if let Some(m) = money {
        o.insert("money".into(), m);
    }
    let members: Vec<Value> = ev
        .members
        .iter()
        .map(|m| {
            let mut v = Map::new();
            v.insert(
                if m.document { "document" } else { "record" }.into(),
                json!(m.id),
            );
            v.insert("kind".into(), json!(m.kind));
            if let Some((title, date)) = labels.get(m.id.as_str()) {
                v.insert("title".into(), json!(title));
                if let Some(d) = date {
                    v.insert("date".into(), json!(d));
                }
            }
            v.insert("role".into(), json!(m.role));
            v.insert("rule".into(), json!(m.rule));
            v.insert("confidence".into(), json!(m.conf.name()));
            if let Some(p) = &m.person {
                v.insert("person".into(), json!(p));
            }
            for (k, x) in &m.extra {
                v.insert(k.clone(), x.clone());
            }
            Value::Object(v)
        })
        .collect();
    o.insert("members".into(), json!(members));
    let notes: Vec<&str> = ev
        .notes
        .iter()
        .copied()
        .filter(|n| *n != "expect_claim")
        .collect();
    if !notes.is_empty() {
        o.insert("notes".into(), json!(notes));
    }
    if !expected.is_empty() {
        o.insert("expected".into(), json!(expected));
    }
    if !ev.differences.is_empty() {
        o.insert("differences".into(), json!(ev.differences));
    }
    if !ev.links.is_empty() {
        o.insert("links".into(), json!(ev.links));
    }
    if !ev.could_join.is_empty() {
        let ids: Vec<String> = ev.could_join.iter().map(|a| event_id(person, a)).collect();
        o.insert("could_join".into(), json!(ids));
    }
    Some((start, end, id, Value::Object(o)))
}

/// A person's events document.
pub fn build(inp: &Input) -> Value {
    let _offset = Offset::set(inp.utc_offset);
    let pid = &inp.person.id;
    let mut b = base(inp.index, Some(inp.texts));
    let recs: BTreeMap<&str, &Record> = inp
        .index
        .records
        .iter()
        .map(|r| (r.id.as_str(), r))
        .collect();

    // X1 and X2 need the other person's events as the rules make them.
    for l in &inp.linked {
        let other = base(l.index, l.texts);
        let orecs: BTreeMap<&str, &Record> =
            l.index.records.iter().map(|r| (r.id.as_str(), r)).collect();
        let stays = |evs: &[Ev]| -> Vec<(usize, Day, Day)> {
            evs.iter()
                .enumerate()
                .filter(|(_, e)| e.kind == "inpatient")
                .filter_map(|(i, e)| e.span().map(|(s, en)| (i, s, en)))
                .collect()
        };
        // X1, this person the newborn: their events during the other's
        // birth stay link to it and take the hospital's name.
        if let Some(born) = inp.person.birth_date.as_deref().and_then(day) {
            for (i, s, e) in stays(&other.evs) {
                if !(s <= born && born <= e) {
                    continue;
                }
                let hospital = other.evs[i]
                    .claims()
                    .find(|m| m.role == "facility")
                    .and_then(|m| orecs.get(m.id.as_str()))
                    .and_then(|r| str_field(r, "provider").or_else(|| str_field(r, "facility")))
                    .map(str::to_string);
                let stay_id = event_id(&l.person.id, &other.evs[i].anchor);
                for ev in b.evs.iter_mut() {
                    if ev.span().is_some_and(|(a, z)| a <= e && s <= z) {
                        ev.links.push(json!({"event": stay_id, "person": l.person.id, "relation": "born_during"}));
                        if ev.linked_place.is_none() {
                            ev.linked_place = hospital.clone();
                        }
                    }
                }
            }
        }
        // X1, this person the parent: their birth stay links to the
        // newborn's events during it.
        if let Some(born) = l.person.birth_date.as_deref().and_then(day) {
            for (i, s, e) in stays(&b.evs) {
                if !(s <= born && born <= e) {
                    continue;
                }
                let theirs: Vec<String> = other
                    .evs
                    .iter()
                    .filter(|ev| ev.span().is_some_and(|(a, z)| a <= e && s <= z))
                    .map(|ev| event_id(&l.person.id, &ev.anchor))
                    .collect();
                for t in theirs {
                    b.evs[i]
                        .links
                        .push(json!({"event": t, "person": l.person.id, "relation": "newborn"}));
                }
            }
            // X2: a paper filed under the child, dated before their birth,
            // belongs to this person's visit that day.
            let names = |ev: &Ev| event_names(ev, &recs);
            for d in l.index.documents.iter() {
                let Some(dd) = d.date.as_deref().and_then(day) else {
                    continue;
                };
                if dd >= born || matches!(d.doc_type.as_deref(), Some("eob" | "insurance_card")) {
                    continue;
                }
                let org = d.organization.as_deref().unwrap_or("");
                let text: String = l
                    .texts
                    .and_then(|t| t.texts.get(&d.id))
                    .map(|t| t.chars().take(4000).collect())
                    .unwrap_or_default();
                let day_evs: Vec<usize> = b
                    .evs
                    .iter()
                    .enumerate()
                    .filter(|(_, ev)| !matches!(ev.kind, "fill" | "vaccine" | "series"))
                    .filter(|(_, ev)| ev.span().is_some_and(|(s, e)| s <= dd && dd <= e + 1))
                    .map(|(i, _)| i)
                    .collect();
                let named: Vec<usize> = day_evs
                    .iter()
                    .copied()
                    .filter(|&i| {
                        (!org.is_empty() && names(&b.evs[i]).iter().any(|n| same_org(org, n)))
                            || b.evs[i].claims().any(|m| {
                                recs.get(m.id.as_str())
                                    .and_then(|r| str_field(r, "provider"))
                                    .is_some_and(|p| {
                                        is_clinician(p)
                                            && clinician_in_text(p, &format!("{org} {text}"), false)
                                    })
                            })
                    })
                    .collect();
                let pick = match (named.as_slice(), day_evs.as_slice()) {
                    ([i], _) => Some(*i),
                    ([], [i]) => Some(*i),
                    _ => None,
                };
                if let Some(i) = pick {
                    let mut m = doc_member(d, "X2", Conf::Likely);
                    m.person = Some(l.person.id.clone());
                    b.evs[i].members.push(m);
                }
            }
        }
    }

    let applied = inp
        .overrides
        .map(|o| apply(&mut b.evs, inp.index, &o.events))
        .unwrap_or(0);

    let mut labels = Labels::new();
    labels_of(inp.index, &mut labels);
    for l in &inp.linked {
        labels_of(l.index, &mut labels);
    }
    let mut out: Vec<(Day, Day, String, Value)> = b
        .evs
        .iter()
        .filter_map(|ev| render(pid, ev, &recs, &labels, &b.aliases))
        .collect();
    out.sort_by(|a, z| z.0.cmp(&a.0).then(z.1.cmp(&a.1)).then(a.2.cmp(&z.2)));
    let pending: Vec<Value> = b
        .pending
        .iter()
        .map(|p| {
            let mut p = p.clone();
            if let Some(c) = p.get_mut("could_join").and_then(Value::as_array_mut) {
                for x in c.iter_mut() {
                    if let Some(a) = x.as_str() {
                        *x = json!(event_id(pid, a));
                    }
                }
            }
            p
        })
        .collect();
    let aliases: Vec<Value> = b
        .aliases
        .iter()
        .map(|a| json!({"clinician": a.clinician, "organization": a.organization, "rule": "N5", "document": a.document, "confidence": a.conf.name()}))
        .collect();
    let mut doc = Map::new();
    doc.insert("schema".into(), json!(SCHEMA));
    doc.insert("person".into(), json!(pid));
    doc.insert(
        "events".into(),
        Value::Array(out.into_iter().map(|e| e.3).collect()),
    );
    doc.insert("pending".into(), json!(pending));
    doc.insert("aliases".into(), json!(aliases));
    if !b.undated.is_empty() {
        doc.insert("undated".into(), json!(b.undated));
    }
    doc.insert("overrides".into(), json!(applied));
    Value::Object(doc)
}

// ---------------------------------------------------------------- the folder

/// A person's events, from the folder as it is now.
pub fn of_person(
    folder: &crate::folder::Folder,
    h: &crate::model::Household,
    p: &Person,
) -> Result<Value, pk_cli_core::CliError> {
    let idx = folder.index(p)?;
    let texts = folder.text(p)?;
    // As the views do: a decisions file that does not parse is left out;
    // one the store cannot reach fails the call.
    let overrides = match folder.overrides(p) {
        Ok(o) => Ok(o),
        Err(pk_cli_core::CliError::Other(_)) => Err(()),
        Err(e) => return Err(e),
    };
    let linked_people = linked_people(h, p);
    let mut linked = Vec::new();
    for q in &linked_people {
        linked.push((*q, folder.index(q)?, folder.text(q)?));
    }
    let mut v = build(&Input {
        person: p,
        index: &idx,
        texts: &texts,
        overrides: overrides.as_ref().ok(),
        linked: linked
            .iter()
            .map(|(q, i, t)| Linked {
                person: q,
                index: i,
                texts: Some(t),
            })
            .collect(),
        utc_offset: pk_cli_core::dates::local_utc_offset,
    });
    if overrides.is_err() {
        v["overrides_unreadable"] = Value::Bool(true);
    }
    Ok(v)
}

/// Record a decision: `apart` or `together`, naming two of the person's
/// records or documents. The same decision again changes nothing; the
/// other one for the same two members replaces it.
pub fn decide(
    folder: &crate::folder::Folder,
    p: &Person,
    kind: &str,
    a: &str,
    b: &str,
    by: Option<&str>,
    request: Option<&str>,
) -> Result<(Decision, bool), pk_cli_core::CliError> {
    use pk_cli_core::CliError;
    if !matches!(kind, "apart" | "together") {
        return Err(CliError::Usage(format!(
            "`{kind}` is not apart or together"
        )));
    }
    if a == b {
        return Err(CliError::Usage("name two different members".into()));
    }
    let idx = folder.index(p)?;
    for id in [a, b] {
        let known =
            idx.records.iter().any(|r| r.id == id) || idx.documents.iter().any(|d| d.id == id);
        if !known {
            return Err(CliError::NotFound(format!(
                "{} has no record or document {}",
                crate::printable(&p.name),
                crate::printable(id)
            )));
        }
    }
    let mut o = folder.overrides(p)?;
    let same_pair = |d: &Decision| {
        (d.members[0] == a && d.members[1] == b) || (d.members[0] == b && d.members[1] == a)
    };
    if let Some(d) = o.events.iter().find(|d| d.kind == kind && same_pair(d)) {
        return Ok((d.clone(), false));
    }
    o.events.retain(|d| !same_pair(d));
    let d = Decision {
        id: crate::ids::new("o"),
        kind: kind.into(),
        members: [a.into(), b.into()],
        at: crate::refresh::time::fmt(crate::refresh::time::now()),
        by: by.map(str::to_string),
        request: request.map(str::to_string),
    };
    o.events.push(d.clone());
    folder.save_overrides(p, &o)?;
    Ok((d, true))
}

/// Drop a decision by its id; the rules decide again.
pub fn undo(
    folder: &crate::folder::Folder,
    p: &Person,
    id: &str,
) -> Result<Decision, pk_cli_core::CliError> {
    let mut o = folder.overrides(p)?;
    let Some(i) = o.events.iter().position(|d| d.id == id) else {
        return Err(pk_cli_core::CliError::NotFound(format!(
            "no decision {} for {}",
            crate::printable(id),
            crate::printable(&p.name)
        )));
    };
    let d = o.events.remove(i);
    folder.save_overrides(p, &o)?;
    Ok(d)
}

/// The events of the people selected, each with its `person` and
/// `person_name`, newest first, those overlapping `range`.
pub fn list(
    folder: &crate::folder::Folder,
    h: &crate::model::Household,
    person: Option<&str>,
    range: &crate::views::Range,
) -> Result<Vec<Value>, pk_cli_core::CliError> {
    range.validate()?;
    let mut rows = Vec::new();
    for p in crate::people::selected(h, person)? {
        let v = of_person(folder, h, p)?;
        for mut e in v["events"].as_array().cloned().unwrap_or_default() {
            let (start, end) = (
                e["start"].as_str().unwrap_or("").to_string(),
                e["end"].as_str().unwrap_or("").to_string(),
            );
            if range.since.as_deref().is_some_and(|s| end.as_str() < s)
                || range.until.as_deref().is_some_and(|u| start.as_str() > u)
            {
                continue;
            }
            e["person"] = json!(p.id);
            e["person_name"] = json!(p.name);
            rows.push(e);
        }
    }
    let k = |v: &Value, f: &str| v[f].as_str().unwrap_or("").to_string();
    rows.sort_by(|a, b| {
        k(b, "start")
            .cmp(&k(a, "start"))
            .then(k(b, "end").cmp(&k(a, "end")))
            .then(k(a, "id").cmp(&k(b, "id")))
    });
    Ok(rows)
}
