//! Provider labels (`views/<pid>/providers.json`, `medrec-providers/v1`): a
//! name, practice, role and phone for a provider a person's items print,
//! with how sure it is and why. A label stands beside the source's own words
//! and changes none of them. It is sure when the record carries the
//! provider's NPI; likely when the directory has one provider it can be, or
//! a paper of the same event prints the name with one practice; and there is
//! none otherwise.
//!
//! What private items say stays with them: a label in a private area, and
//! what only private items give, are in the file's `private` part.
//!
//! What the directory gave is kept in the person's folder
//! (`provider-state.json`), so a machine with no directory, or an older one,
//! judges and labels as the one that has it.

use std::cell::OnceCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::events;
use crate::model::{Document, Person, PersonIndex, PersonText, Record};
use crate::nppes::{self, Provider, Stored};
use crate::private;
use crate::roles::{self, Service};

pub const FILE: &str = "providers.json";
pub const SCHEMA: &str = "medrec-providers/v1";
pub const STATE_SCHEMA: &str = "medrec-provider-state/v1";
const STATE_FAMILY: &str = "medrec-provider-state/";

/// The papers a label gives as its evidence, at most.
const MAX_PAPERS: usize = 5;
/// The shortest surname looked for in a paper's text.
const SURNAME: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    Sure,
    Likely,
}

impl Confidence {
    fn word(self) -> &'static str {
        match self {
            Confidence::Sure => "sure",
            Confidence::Likely => "likely",
        }
    }
}

/// Why a label is.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "from", rename_all = "snake_case")]
pub enum Evidence {
    /// The record carries the provider's NPI.
    Npi,
    /// The directory has one provider the printed name can be.
    OnlyMatch { state: String },
    /// A paper of the same event prints the name with one practice.
    Paper { document: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "from", rename_all = "snake_case")]
pub enum PhoneFrom {
    /// As of the last day the directory's file covers.
    Directory {
        date: String,
    },
    Paper {
        document: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Label {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub practice: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phone: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phone_from: Option<PhoneFrom>,
    pub confidence: Confidence,
    pub evidence: Vec<Evidence>,
}

impl Label {
    /// The private area the label itself is of, by its role, name or
    /// practice.
    pub fn area(&self) -> Option<&'static str> {
        self.role
            .as_deref()
            .and_then(roles::named)
            .and_then(|r| r.area)
            .or_else(|| {
                [&self.name, &self.practice, &self.role]
                    .into_iter()
                    .flatten()
                    .find_map(|t| private::place_area(t))
            })
    }

    /// The same for the same provider in every person's file of one folder.
    /// Salted, so that it cannot be tested against a public list; a label
    /// with no name of its own is of the printed name.
    fn id(&self, salt: &str, printed: &str) -> String {
        let of = |x: &Option<String>| x.clone().unwrap_or_default();
        let parts = [
            salt.to_string(),
            self.confidence.word().to_string(),
            self.name.clone().unwrap_or_else(|| printed.to_string()),
            of(&self.practice),
            of(&self.role),
            of(&self.phone),
        ];
        crate::ids::from_digest("v", &Sha256::digest(parts.join("\u{0}")))
    }

    /// Its reasons, and those of another under the same id.
    fn and(&mut self, other: Label) {
        self.evidence.extend(other.evidence);
        self.evidence.sort();
        self.evidence.dedup();
        let mut papers = 0;
        self.evidence.retain(|e| {
            papers += usize::from(matches!(e, Evidence::Paper { .. }));
            papers <= MAX_PAPERS
        });
        if self.phone_from.is_none() {
            self.phone_from = other.phone_from;
        }
    }
}

/// The table a file's labels came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Table {
    id: String,
    version: String,
    date: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Part {
    #[serde(default)]
    labels: BTreeMap<String, Label>,
    /// By the text printed, exactly.
    #[serde(default)]
    by_name: BTreeMap<String, String>,
    /// By item, then by the text it prints.
    #[serde(default)]
    by_item: BTreeMap<String, BTreeMap<String, String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct File {
    schema: String,
    person: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    directory: Option<Table>,
    #[serde(flatten)]
    open: Part,
    #[serde(default)]
    private: Part,
}

// ---------------------------------------------------------------- sources

/// The directory on this machine, for the folder being written.
pub struct Directory {
    stored: Stored,
    rows: OnceCell<Rows>,
}

#[derive(Default)]
struct Rows {
    /// Where each provider's line is, by NPI.
    at: HashMap<String, std::ops::Range<usize>>,
    people: HashMap<(String, char), Vec<Provider>>,
}

impl Directory {
    pub fn open(path: &Path) -> Result<Directory, nppes::Unused> {
        Ok(Directory {
            stored: Stored::open(path)?,
            rows: OnceCell::new(),
        })
    }

    /// Which kept file it is, so that a newer one sorts after: the last day
    /// its providers are of, then when they were fetched.
    pub fn made(&self) -> String {
        let a = &self.stored.about;
        format!("{} {}", a.date, a.fetched_at)
    }

    fn rows(&self) -> &Rows {
        self.rows.get_or_init(|| {
            let mut rows = Rows::default();
            for (at, npi, person) in self.stored.lines() {
                if person {
                    if let Some(p) = self.stored.provider(at.clone()) {
                        let keys: BTreeSet<_> =
                            [(&p.last, &p.first), (&p.other_last, &p.other_first)]
                                .into_iter()
                                .filter_map(|(l, f)| nppes::person(l, f))
                                .collect();
                        for k in keys {
                            rows.people.entry(k).or_default().push(p.clone());
                        }
                    }
                }
                rows.at.insert(npi, at);
            }
            rows
        })
    }

    fn provider(&self, npi: &str) -> Option<Provider> {
        self.stored.provider(self.rows().at.get(npi)?.clone())
    }

    /// Whether it was fetched for this NPI: without a row, CMS's file has
    /// none.
    fn asked(&self, npi: &str) -> bool {
        self.stored.about.npis.contains(npi)
    }

    fn table(&self) -> Table {
        let a = &self.stored.about;
        Table {
            id: nppes::ID.into(),
            version: a.month.clone(),
            date: a.date.clone(),
        }
    }
}

// ---------------------------------------------------------------- what the directory gave

/// A person's `provider-state.json`: what a directory said of the providers
/// their records name, the labels of their NPIs and those of the printed
/// names only one provider can be. Nothing else in the folder says it again,
/// so it is not one of the views. medrec's own; no reader uses it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    schema: String,
    person: String,
    /// The newest directory that gave any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    directory: Option<Table>,
    #[serde(default)]
    by_npi: BTreeMap<String, Label>,
    /// By record, then by the text it prints.
    #[serde(default)]
    matched: BTreeMap<String, BTreeMap<String, Label>>,
    /// The state before this one did not read, and no directory has been
    /// asked of every NPI since: which of the records a label makes private is
    /// not known.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    lost: bool,
}

/// The state the last pass left.
#[derive(Debug, Clone)]
pub enum Last {
    /// None, and the views were made with none.
    None,
    State(State),
    /// There was one, and it is gone.
    Gone,
    /// It is there and does not read.
    Unreadable,
    /// It is there, as only a later medrec writes it.
    Later,
}

impl State {
    fn new(person: &str) -> State {
        State {
            schema: STATE_SCHEMA.into(),
            person: person.into(),
            directory: None,
            by_npi: BTreeMap::new(),
            matched: BTreeMap::new(),
            lost: false,
        }
    }

    pub fn parse(text: &str, person: &str) -> Last {
        let schema = || -> Option<String> {
            let v: Value = serde_json::from_str(text).ok()?;
            Some(v["schema"].as_str()?.to_string())
        };
        match serde_json::from_str::<State>(text) {
            Ok(s) if s.schema == STATE_SCHEMA && s.person == person => Last::State(s),
            _ => match schema() {
                Some(s) if s != STATE_SCHEMA && s.starts_with(STATE_FAMILY) => Last::Later,
                _ => Last::Unreadable,
            },
        }
    }

    /// The last pass's, as it is: none when there never was one.
    pub fn last(last: Last, person: &str) -> State {
        match last {
            Last::State(s) => s,
            Last::None => State::new(person),
            Last::Gone | Last::Unreadable | Last::Later => State {
                lost: true,
                ..State::new(person)
            },
        }
    }

    /// The state now: the newer of the directory on this machine and the
    /// last state first, then the other. What either says of an NPI stays,
    /// though no record carries it any longer: an index an earlier medrec
    /// saved has none until `reindex`.
    pub fn of(idx: &PersonIndex, directory: Option<&Directory>, last: Last) -> State {
        // What a later medrec knows is not this one's to say is complete.
        let later = matches!(last, Last::Later);
        let before = State::last(last, &idx.person);
        let Some(d) = directory else {
            return before.still(idx);
        };
        let table = d.table();
        let older = before
            .directory
            .as_ref()
            .is_some_and(|b| b.date > table.date);
        let mut now = before.still(idx);
        if !older {
            now.directory = Some(table);
        }
        for r in &idx.records {
            for p in &r.providers {
                let (Some(npi), Some(_)) = (p.npi.as_deref(), printed(r, &p.field)) else {
                    continue;
                };
                if older && now.by_npi.contains_key(npi) {
                    continue;
                }
                if let Some(l) = d.provider(npi).and_then(|p| sure(&p, &d.stored.about.date)) {
                    now.by_npi.insert(npi.to_string(), l);
                }
            }
            if older {
                continue;
            }
            for (field, text) in people_printed(r) {
                // Only of names and states the kept file is whole in; of
                // others what the last state says stays.
                let Some(found) = only_match(d, r, field, text) else {
                    continue;
                };
                let of_record = now.matched.entry(r.id.clone()).or_default();
                match found {
                    Some(l) => of_record.insert(text.to_string(), l),
                    None => of_record.remove(text),
                };
            }
        }
        now.matched.retain(|_, m| !m.is_empty());
        now.lost &= later || !npis(idx).all(|n| now.by_npi.contains_key(n) || d.asked(n));
        if now.by_npi.is_empty() && now.matched.is_empty() {
            now.directory = None;
        }
        now
    }

    /// It, with the matches of what the records still print.
    fn still(mut self, idx: &PersonIndex) -> State {
        let records: HashMap<&str, &Record> =
            idx.records.iter().map(|r| (r.id.as_str(), r)).collect();
        self.matched.retain(|id, texts| {
            let Some(r) = records.get(id.as_str()) else {
                return false;
            };
            texts.retain(|text, l| {
                people_printed(r).any(|(field, t)| t == text && still_matches(r, field, l))
            });
            !texts.is_empty()
        });
        self
    }

    fn is_empty(&self) -> bool {
        self.by_npi.is_empty() && self.matched.is_empty() && !self.lost
    }

    /// What to write, when the file has `held`: none when it is to stay as
    /// it is. One that does not read is replaced only by a state that says
    /// all there is to know.
    pub fn replaces(&self, held: Option<&str>) -> Option<String> {
        let reads = held.map(|t| State::parse(t, &self.person));
        match reads {
            Some(Last::Unreadable | Last::Later) if self.lost => return None,
            None if self.is_empty() => return None,
            _ => {}
        }
        let mut text = serde_json::to_string_pretty(self).expect("labels are plain JSON");
        text.push('\n');
        (held != Some(text.as_str())).then_some(text)
    }

    /// Whether which records a label makes private is not known.
    pub fn lost(&self) -> bool {
        self.lost
    }

    /// The records a sure label makes private, with their area.
    pub fn areas(&self, idx: &PersonIndex) -> BTreeMap<String, &'static str> {
        idx.records
            .iter()
            .filter_map(|r| {
                let area = r
                    .providers
                    .iter()
                    .find_map(|p| self.sure(r, &p.field)?.area())?;
                Some((r.id.clone(), area))
            })
            .collect()
    }

    /// The label the NPI of what `field` prints gives.
    fn sure(&self, r: &Record, field: &str) -> Option<&Label> {
        printed(r, field)?;
        r.providers
            .iter()
            .filter(|p| p.field == field)
            .filter_map(|p| p.npi.as_deref())
            .find_map(|npi| self.by_npi.get(npi))
    }

    fn matched(&self, r: &Record, text: &str) -> Option<&Label> {
        self.matched.get(&r.id)?.get(text)
    }
}

pub fn state_rel(p: &Person) -> String {
    format!("{}/provider-state.json", p.folder)
}

fn printed<'a>(r: &'a Record, field: &str) -> Option<&'a str> {
    r.fields.get(field).and_then(Value::as_str)
}

/// The NPIs of the names the records print.
fn npis(idx: &PersonIndex) -> impl Iterator<Item = &str> {
    idx.records
        .iter()
        .flat_map(|r| r.providers.iter().map(move |p| (r, p)))
        .filter(|(r, p)| printed(r, &p.field).is_some())
        .filter_map(|(_, p)| p.npi.as_deref())
}

/// The texts of a record that print a person, each with the first field that
/// does. One with an NPI is that NPI's, whether a directory has it or not.
fn people_printed(r: &Record) -> impl Iterator<Item = (&'static str, &str)> {
    let with_npi: BTreeSet<&str> = r
        .providers
        .iter()
        .filter(|p| p.npi.is_some())
        .filter_map(|p| printed(r, &p.field))
        .collect();
    let mut seen = BTreeSet::new();
    FIELDS
        .iter()
        .filter_map(|f| Some((*f, printed(r, f)?)))
        .filter(|(f, text)| a_person(f, text))
        .filter(move |(_, text)| !with_npi.contains(text) && seen.insert(*text))
}

/// What a directory row says of the provider whose NPI a record carries.
fn sure(p: &Provider, date: &str) -> Option<Label> {
    let mut l = of_row(p, date)?;
    l.confidence = Confidence::Sure;
    l.evidence = vec![Evidence::Npi];
    Some(l)
}

fn of_row(p: &Provider, date: &str) -> Option<Label> {
    let organization = !p.organization.is_empty();
    let name = if organization {
        organization_name(p)
    } else {
        person_name(p)
    }?;
    let phone = phone(&p.phone);
    Some(Label {
        name: Some(name),
        practice: None,
        role: roles::words(&p.taxonomy, organization),
        phone_from: phone
            .as_ref()
            .map(|_| PhoneFrom::Directory { date: date.into() }),
        phone,
        confidence: Confidence::Likely,
        evidence: Vec::new(),
    })
}

fn phone(s: &str) -> Option<String> {
    let d: String = s.chars().filter(char::is_ascii_digit).collect();
    let d = match d.len() {
        11 => d.strip_prefix('1')?,
        _ => &d,
    };
    (d.len() == 10).then(|| format!("{}-{}-{}", &d[..3], &d[3..6], &d[6..]))
}

// ---------------------------------------------------------------- names

/// Credentials as they are written, by their letters.
const CREDENTIALS: &[(&str, &str)] = &[
    ("APRN", "APRN"),
    ("ARNP", "ARNP"),
    ("AUD", "AuD"),
    ("BCBA", "BCBA"),
    ("CADC", "CADC"),
    ("CASAC", "CASAC"),
    ("CNM", "CNM"),
    ("CNP", "CNP"),
    ("CNS", "CNS"),
    ("CRNA", "CRNA"),
    ("DC", "DC"),
    ("DDS", "DDS"),
    ("DMD", "DMD"),
    ("DNP", "DNP"),
    ("DO", "DO"),
    ("DPM", "DPM"),
    ("DPT", "DPT"),
    ("FNP", "FNP"),
    ("FNPBC", "FNP"),
    ("FNPC", "FNP"),
    ("LADC", "LADC"),
    ("LCADC", "LCADC"),
    ("LCSW", "LCSW"),
    ("LICSW", "LICSW"),
    ("LMFT", "LMFT"),
    ("LMHC", "LMHC"),
    ("LPC", "LPC"),
    ("LPCC", "LPCC"),
    ("LPN", "LPN"),
    ("MBBS", "MBBS"),
    ("MD", "MD"),
    ("ND", "ND"),
    ("NP", "NP"),
    ("NPC", "NP"),
    ("OD", "OD"),
    ("PA", "PA"),
    ("PAC", "PA-C"),
    ("PHARMD", "PharmD"),
    ("PHD", "PhD"),
    ("PMHNP", "PMHNP"),
    ("PMHNPBC", "PMHNP"),
    ("PNP", "PNP"),
    ("PSYD", "PsyD"),
    ("PT", "PT"),
    ("RD", "RD"),
    ("RN", "RN"),
    ("RPH", "RPh"),
    ("WHNP", "WHNP"),
];

/// Company forms at the end of an organization's name.
const FORMS: &[&str] = &[
    "CORP", "INC", "LLC", "LLP", "LP", "LTD", "PA", "PC", "PL", "PLC", "PLLC",
];

/// Short words in capitals that are words.
const WORDS: &[&str] = &["DR", "FT", "JR", "MR", "MRS", "MS", "MT", "SR", "ST"];

/// Words an organization's name keeps in lower case after its first.
const SMALL: &[&str] = &["an", "and", "at", "by", "for", "in", "of", "on", "the"];

/// `Jordan Sample, MD`.
fn person_name(p: &Provider) -> Option<String> {
    let name = [&p.first, &p.last]
        .into_iter()
        .filter(|s| !s.is_empty())
        .map(|s| cased(s, false))
        .collect::<Vec<_>>()
        .join(" ");
    if name.is_empty() {
        return None;
    }
    let mut credentials: Vec<&str> = p
        .credential
        .split(|c: char| c.is_whitespace() || matches!(c, ',' | '/' | ';'))
        .filter_map(|c| {
            let c = nppes::fold(c);
            CREDENTIALS.iter().find(|(k, _)| *k == c).map(|(_, v)| *v)
        })
        .collect();
    credentials.dedup();
    credentials.truncate(2);
    credentials.insert(0, &name);
    Some(credentials.join(", "))
}

/// The name an organization does business as, else its own without its
/// company form.
fn organization_name(p: &Provider) -> Option<String> {
    let mut words: Vec<&str> = if p.business_name.is_empty() {
        p.organization.split_whitespace().collect()
    } else {
        p.business_name.split_whitespace().collect()
    };
    while words.len() > 1
        && words
            .last()
            .is_some_and(|w| FORMS.contains(&nppes::fold(w).as_str()))
    {
        words.pop();
    }
    let name = words.join(" ");
    let name = name.trim_end_matches([',', ' ']);
    (!name.is_empty()).then(|| cased(name, true))
}

/// `s` as a name is written, when it is all in capitals.
fn cased(s: &str, organization: bool) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().any(char::is_lowercase) {
        return s;
    }
    s.split(' ')
        .enumerate()
        .map(|(i, w)| word(w, i == 0, organization))
        .collect::<Vec<_>>()
        .join(" ")
}

fn word(w: &str, first: bool, organization: bool) -> String {
    let letters: String = w.chars().filter(|c| c.is_alphabetic()).collect();
    if matches!(letters.as_str(), "II" | "III" | "IV") {
        return w.to_string();
    }
    if organization {
        let credential = CREDENTIALS
            .iter()
            .any(|(k, _)| *k == letters && !matches!(*k, "DO" | "PA"));
        let unvoiced = (2..=5).contains(&letters.len())
            && !letters.contains(['A', 'E', 'I', 'O', 'U', 'Y'])
            && !WORDS.contains(&letters.as_str());
        if credential || unvoiced {
            return w.to_string();
        }
        if !first && SMALL.contains(&w.to_lowercase().as_str()) {
            return w.to_lowercase();
        }
    }
    let mut out = String::with_capacity(w.len());
    let mut run = String::new();
    let mut before = ' ';
    for c in w.chars().chain(std::iter::once(' ')) {
        if c.is_alphabetic() {
            run.push(c);
            continue;
        }
        out.push_str(&capitalized(&run, before));
        run.clear();
        before = c;
        out.push(c);
    }
    out.pop();
    out
}

/// One run of letters: `JOSEPH`, the `S` after an apostrophe, `MCSAMPLE`.
fn capitalized(run: &str, before: char) -> String {
    let lower = run.to_lowercase();
    if matches!(before, '\'' | '\u{2019}') && run.chars().count() == 1 {
        return lower;
    }
    let up = |s: &str| {
        let mut cs = s.chars();
        cs.next()
            .map(|c| c.to_uppercase().chain(cs).collect::<String>())
            .unwrap_or_default()
    };
    match lower.strip_prefix("mc") {
        Some(rest) if rest.len() >= 3 => format!("Mc{}", up(rest)),
        _ => up(&lower),
    }
}

// ---------------------------------------------------------------- what is likely

/// The fields that can print a person, and so a name to match.
const PERSON_FIELDS: &[&str] = &["practitioner", "provider", "performer", "prescriber"];

/// Every field that prints a provider, in the order one text of a record
/// takes its label from.
const FIELDS: &[&str] = &[
    "provider",
    "practitioner",
    "performer",
    "prescriber",
    "service_provider",
    "facility",
    "location",
    "pharmacy",
];

/// What after a name says it is a person's.
const DEGREES: &[&str] = &[
    "APRN", "ARNP", "CNM", "CRNA", "DDS", "DMD", "DO", "DPM", "DPT", "FNP", "LCSW", "LMFT", "LMHC",
    "MD", "NP", "OD", "PAC", "PHD", "PSYD", "RN",
];

/// Whether what `field` prints is a person's name.
fn a_person(field: &str, text: &str) -> bool {
    let degree = || {
        text.split(|c: char| c.is_whitespace() || c == ',')
            .any(|w| DEGREES.contains(&nppes::fold(w).as_str()))
    };
    PERSON_FIELDS.contains(&field)
        && !events::says_organization(text)
        && (field != "provider" || events::is_clinician(text) || degree())
}

fn service(r: &Record, field: &str) -> Service {
    match r.kind.as_str() {
        "claim" => match r.fields.get("category").and_then(Value::as_str) {
            Some("visit") => Service::Visit,
            Some("imaging") => Service::Imaging,
            Some("lab") => Service::Lab,
            Some("procedure") => Service::Procedure,
            Some("anesthesia") => Service::Anesthesia,
            Some("dental") => Service::Dental,
            Some("vaccine") => Service::Vaccine,
            _ => Service::Any,
        },
        "encounter" => Service::Encounter,
        "medication" if field == "prescriber" => Service::Prescription,
        "immunization" => Service::Vaccine,
        "procedure" => Service::Procedure,
        "result" | "report" => Service::Lab,
        _ => Service::Any,
    }
}

/// The states the record's source gives its providers: this field's, else
/// any of them.
fn states(r: &Record, field: &str) -> BTreeSet<String> {
    let own: BTreeSet<String> = r
        .providers
        .iter()
        .filter(|p| p.field == field)
        .filter_map(|p| p.state.clone())
        .collect();
    if own.is_empty() {
        r.providers.iter().filter_map(|p| p.state.clone()).collect()
    } else {
        own
    }
}

/// The one provider of the directory a printed name can be: of the surname
/// and first initial, in the record's state, and with a role that gives the
/// service. A provider whose role medrec has no words for counts as one it
/// can be, and is never the label. None when the kept file is not whole in
/// the name or the state.
fn only_match(d: &Directory, r: &Record, field: &str, text: &str) -> Option<Option<Label>> {
    let service = service(r, field);
    let about = &d.stored.about;
    let readings = nppes::people(text);
    let mut states = states(r, field);
    if states.is_empty() {
        states = about.states.clone();
    }
    if readings.is_empty()
        || states.is_empty()
        || !readings.is_subset(&about.people)
        || !states.is_subset(&about.states)
    {
        return None;
    }
    let found: BTreeMap<&str, &Provider> = readings
        .iter()
        .filter_map(|k| d.rows().people.get(k))
        .flatten()
        .filter(|p| states.contains(&p.state))
        .filter(|p| roles::of(&p.taxonomy).is_none_or(|x| service.fits(x.group)))
        .map(|p| (p.npi.as_str(), p))
        .collect();
    let [p] = found.into_values().collect::<Vec<_>>()[..] else {
        return Some(None);
    };
    if roles::of(&p.taxonomy).is_none() || !given_names_agree(p, text) {
        return Some(None);
    }
    Some(of_row(p, &about.date).map(|l| Label {
        evidence: vec![Evidence::OnlyMatch {
            state: p.state.clone(),
        }],
        ..l
    }))
}

/// Whether a match an earlier pass made still stands for the record: its
/// state and its service may have changed.
fn still_matches(r: &Record, field: &str, l: &Label) -> bool {
    let states = states(r, field);
    let in_state = l.evidence.iter().any(|e| match e {
        Evidence::OnlyMatch { state } => states.is_empty() || states.contains(state),
        _ => false,
    });
    in_state
        && l.role
            .as_deref()
            .and_then(roles::named)
            .is_some_and(|role| service(r, field).fits(role.group))
}

/// Whether each given name the text spells out is one of the provider's.
fn given_names_agree(p: &Provider, text: &str) -> bool {
    let surnames = [&p.last, &p.other_last].map(|s| nppes::fold(s));
    let given: Vec<String> = [&p.first, &p.middle, &p.other_first]
        .into_iter()
        .flat_map(|s| s.split_whitespace())
        .map(nppes::fold)
        .filter(|s| !s.is_empty())
        .collect();
    nppes::name_words(text)
        .iter()
        .filter(|w| w.len() > 1 && !surnames.iter().any(|s| s.contains(w.as_str())))
        .all(|w| {
            given
                .iter()
                .any(|g| g.starts_with(w.as_str()) || w.starts_with(g.as_str()))
        })
}

/// The papers of one event that print a name, and the practice they are
/// all from.
struct Papers {
    practice: String,
    documents: BTreeSet<String>,
    phone: Option<(String, String)>,
}

/// The papers that carry a practice's own heading.
const HEADED: &[&str] = &[
    "bill",
    "receipt",
    "visit_summary",
    "discharge_summary",
    "prescription",
    "immunization_record",
];

/// Words of an organization that prints another practice's clinicians.
const ORDERED_FROM: &[&str] = &[
    "diagnostics",
    "imaging",
    "lab",
    "laboratories",
    "laboratory",
    "labs",
    "pharmacy",
    "radiology",
];

/// The organization at the head of a paper, when it can be the practice of a
/// clinician the paper prints.
fn heading(d: &Document) -> Option<&str> {
    let org = d.organization.as_deref()?.trim();
    let headed = d.doc_type.as_deref().is_some_and(|t| HEADED.contains(&t));
    let of_others = || {
        events::org_tokens(org)
            .iter()
            .any(|t| ORDERED_FROM.contains(&t.as_str()))
    };
    (headed
        && !org.is_empty()
        && !crate::plain::is_connection(org)
        && !a_person("provider", org)
        && !of_others())
    .then_some(org)
}

/// Whether a paper prints one of these names: the surname after a word of
/// the first initial, or before one when a comma follows it. Both are
/// written as names are.
fn prints_name(body: &str, readings: &BTreeSet<(String, char)>) -> bool {
    let words: Vec<(&str, String)> = body
        .split_whitespace()
        .map(|w| (w, nppes::fold(w)))
        .filter(|(_, f)| !f.is_empty())
        .collect();
    let proper = |i: usize| {
        words[i]
            .0
            .chars()
            .find(|c| c.is_alphabetic())
            .is_some_and(char::is_uppercase)
    };
    let given = |i: usize, initial: char| proper(i) && words[i].1.starts_with(initial);
    readings.iter().any(|(surname, initial)| {
        surname.len() >= SURNAME
            && (0..words.len()).any(|i| {
                let Some(n) = (1..=3).find(|n| {
                    words.get(i..i + n).is_some_and(|w| {
                        w.iter().map(|x| x.1.as_str()).collect::<String>() == *surname
                    })
                }) else {
                    return false;
                };
                if !proper(i) {
                    return false;
                }
                let before = i.checked_sub(1).is_some_and(|b| {
                    given(b, *initial)
                        || (words[b].1.len() == 1
                            && b.checked_sub(1).is_some_and(|b| given(b, *initial)))
                });
                let after = words[i + n - 1].0.ends_with(',')
                    && i + n < words.len()
                    && given(i + n, *initial);
                before || after
            })
    })
}

impl Papers {
    fn of(text: &str, papers: &[(&Document, &str, &str)]) -> Option<Papers> {
        let readings = nppes::people(text);
        let naming: Vec<&(&Document, &str, &str)> = papers
            .iter()
            .filter(|(_, _, body)| prints_name(body, &readings))
            .collect();
        let (_, first, _) = naming.first()?;
        if !naming.iter().all(|(_, o, _)| events::same_org(first, o)) {
            return None;
        }
        let phones: BTreeSet<(&str, &str)> = naming
            .iter()
            .filter_map(|(d, _, _)| {
                let st = d.statement.as_ref()?;
                Some((st.phone.as_deref()?, d.id.as_str()))
            })
            .collect();
        let one = phones.iter().map(|(n, _)| n).collect::<BTreeSet<_>>().len() == 1;
        Some(Papers {
            practice: cased(first, true),
            documents: naming.iter().map(|(d, _, _)| d.id.clone()).collect(),
            phone: phones
                .first()
                .filter(|_| one)
                .map(|(n, d)| (n.to_string(), d.to_string())),
        })
    }

    /// The label of the papers, or the one given with their practice.
    fn label(&self, only: Option<Label>) -> Label {
        let mut l = only.unwrap_or(Label {
            name: None,
            practice: None,
            role: None,
            phone: None,
            phone_from: None,
            confidence: Confidence::Likely,
            evidence: Vec::new(),
        });
        l.practice = Some(self.practice.clone());
        if let (None, Some((n, d))) = (&l.phone, &self.phone) {
            l.phone = Some(n.clone());
            l.phone_from = Some(PhoneFrom::Paper {
                document: d.clone(),
            });
        }
        let papers = self.documents.iter().map(|d| Evidence::Paper {
            document: d.clone(),
        });
        l.evidence.extend(papers.take(MAX_PAPERS));
        l
    }
}

// ---------------------------------------------------------------- the view

pub struct Input<'a> {
    pub person: &'a Person,
    pub index: &'a PersonIndex,
    pub texts: &'a PersonText,
    pub events: &'a Value,
    /// The person's `private.json`, made with [`State::areas`].
    pub private: &'a Value,
    pub state: &'a State,
    /// The folder's salt, made when it is first needed.
    pub salt: &'a dyn Fn() -> String,
}

/// A provider's name where an item prints it.
struct Use {
    item: String,
    printed: String,
    label: Option<Label>,
    /// Kept by item whatever the printed name's label is.
    always: bool,
    open: bool,
}

/// The person's `providers.json`, or none when nothing of theirs has a
/// label.
pub fn build(inp: &Input) -> Option<Value> {
    let uses = uses(inp);
    if uses.iter().all(|u| u.label.is_none()) {
        return None;
    }
    let salt = (inp.salt)();
    let part = |open: bool| {
        Part::of(
            uses.iter().filter(|u| u.open == open).collect::<Vec<_>>(),
            &salt,
        )
    };
    let open = part(true);
    let mut private = part(false);
    private
        .labels
        .retain(|id, l| open.labels.get(id) != Some(l));
    let file = File {
        schema: SCHEMA.into(),
        person: inp.person.id.clone(),
        directory: inp.state.directory.clone(),
        open,
        private,
    };
    Some(serde_json::to_value(file).expect("labels are plain JSON"))
}

impl Part {
    fn of(uses: Vec<&Use>, salt: &str) -> Part {
        let mut part = Part::default();
        // Each printed name's labels, by how many items have each, and
        // whether every item that prints it has one.
        let mut names: BTreeMap<&str, (bool, BTreeMap<String, usize>)> = BTreeMap::new();
        let mut ids: Vec<Option<String>> = Vec::with_capacity(uses.len());
        for u in &uses {
            let id = u.label.as_ref().map(|l| l.id(salt, &u.printed));
            let n = names
                .entry(u.printed.as_str())
                .or_insert_with(|| (true, BTreeMap::new()));
            match (&id, &u.label) {
                (Some(id), Some(l)) => {
                    *n.1.entry(id.clone()).or_default() += 1;
                    match part.labels.get_mut(id) {
                        Some(held) => held.and(l.clone()),
                        None => {
                            part.labels.insert(id.clone(), l.clone());
                        }
                    }
                }
                _ => n.0 = false,
            }
            ids.push(id);
        }
        for (printed, (every, counts)) in names {
            let most = counts
                .into_iter()
                .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(&a.0)));
            if let (true, Some((id, _))) = (every, most) {
                part.by_name.insert(printed.to_string(), id);
            }
        }
        for (u, id) in uses.iter().zip(ids) {
            let Some(id) = id else { continue };
            if u.always || part.by_name.get(&u.printed) != Some(&id) {
                part.by_item
                    .entry(u.item.clone())
                    .or_default()
                    .insert(u.printed.clone(), id);
            }
        }
        part
    }
}

fn uses(inp: &Input) -> Vec<Use> {
    let hidden = inp.private.get("marks_unreadable").is_some();
    let private = |kind: &str, id: &str| hidden || inp.private[kind].get(id).is_some();
    let documents: HashMap<&str, &Document> = inp
        .index
        .documents
        .iter()
        .map(|d| (d.id.as_str(), d))
        .collect();
    let records: HashMap<&str, &Record> = inp
        .index
        .records
        .iter()
        .map(|r| (r.id.as_str(), r))
        .collect();
    let list = || inp.events["events"].as_array().into_iter().flatten();
    let own = |e: &Value, k: &str| -> Vec<String> {
        e["members"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|m| m.get("person").is_none())
            .filter_map(|m| m[k].as_str().map(str::to_string))
            .collect()
    };

    // Each event's papers that carry a practice's heading, for the records
    // in it.
    let mut papers: HashMap<String, Vec<(&Document, &str, &str)>> = HashMap::new();
    for e in list() {
        let headed: Vec<(&Document, &str, &str)> = own(e, "document")
            .iter()
            .filter_map(|id| {
                let d = *documents.get(id.as_str())?;
                Some((d, heading(d)?, inp.texts.texts.get(id)?.as_str()))
            })
            .collect();
        if !headed.is_empty() {
            for id in own(e, "record") {
                papers.entry(id).or_insert_with(|| headed.clone());
            }
        }
    }

    let mut out = Vec::new();
    // What each record's texts are labelled, for its event.
    let mut labelled: HashMap<(String, String), Label> = HashMap::new();
    for r in &inp.index.records {
        let open = !private("records", &r.id);
        let mut done = BTreeSet::new();
        for field in FIELDS {
            let Some(text) = printed(r, field) else {
                continue;
            };
            if !done.insert(text) {
                continue;
            }
            // The first field with a sure label decides for its text.
            let same = || FIELDS.iter().filter(|f| printed(r, f) == Some(text));
            let label = same()
                .find_map(|f| inp.state.sure(r, f))
                .cloned()
                .or_else(|| {
                    same().find(|f| a_person(f, text))?;
                    let only = inp.state.matched(r, text).cloned();
                    let here: Vec<(&Document, &str, &str)> = papers
                        .get(&r.id)
                        .into_iter()
                        .flatten()
                        .filter(|(d, _, _)| !open || !private("documents", &d.id))
                        .copied()
                        .collect();
                    match Papers::of(text, &here) {
                        Some(p) => Some(p.label(only)),
                        None => only,
                    }
                })
                // A label in a private area is never an open item's.
                .filter(|l| !open || l.area().is_none());
            if let Some(l) = &label {
                labelled.insert((r.id.clone(), text.to_string()), l.clone());
            }
            out.push(Use {
                item: r.id.clone(),
                printed: text.to_string(),
                label,
                always: r.kind == "claim" && *field == "provider",
                open,
            });
        }
    }
    for d in &inp.index.documents {
        if let Some(o) = &d.organization {
            out.push(Use {
                item: d.id.clone(),
                printed: o.clone(),
                label: None,
                always: false,
                open: !private("documents", &d.id),
            });
        }
    }
    for e in list() {
        let Some(id) = e["id"].as_str() else {
            continue;
        };
        let open = !private("events", id);
        let members: Vec<String> = own(e, "record")
            .into_iter()
            .filter(|m| records.contains_key(m.as_str()) && (!open || !private("records", m)))
            .collect();
        let texts: BTreeSet<&str> = e["place"]
            .as_str()
            .into_iter()
            .chain(
                e["clinicians"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str),
            )
            .collect();
        for text in texts {
            // What its members' records are labelled, when they agree.
            let mut of_members = members
                .iter()
                .filter_map(|m| labelled.get(&(m.clone(), text.to_string())));
            let label = of_members.next().and_then(|first| {
                of_members.try_fold(first.clone(), |mut l, x| {
                    let same = l.id("", text) == x.id("", text);
                    same.then(|| {
                        l.and(x.clone());
                        l
                    })
                })
            });
            out.push(Use {
                item: id.to_string(),
                printed: text.to_string(),
                label: label.filter(|l| !open || l.area().is_none()),
                always: false,
                open,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use serde_json::json;

    use crate::folder::{Access, Folder};
    use crate::model::Household;
    use crate::nppes::Wanted;

    macro_rules! fixture {
        ($f:literal) => {
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/labels/",
                $f
            ))
        };
    }

    const PERSON: &str = "p_robinaaaaaaaaaaaaaaa";
    const SALT: &str = "s_exampleaaaaaaaaaaaaa";

    /// The fixture folder, on this machine with its directory or without.
    struct Case {
        root: PathBuf,
        index: PersonIndex,
        marks: Value,
    }

    impl Case {
        fn new() -> Case {
            static N: AtomicUsize = AtomicUsize::new(0);
            let root = std::env::temp_dir().join(format!(
                "medrec-labels-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::SeqCst)
            ));
            let _ = std::fs::remove_dir_all(&root);
            let index: PersonIndex = serde_json::from_str(fixture!("index.json")).unwrap();
            let c = Case {
                root,
                index,
                marks: serde_json::from_str(fixture!("marks.json")).unwrap(),
            };
            c.put("household.json", fixture!("household.json"));
            c.put("Robin Example/text.json", fixture!("text.json"));
            let h: Household = serde_json::from_str(fixture!("household.json")).unwrap();
            c.keep(&Wanted::of([&c.index]), fixture!("directory.jsonl"));
            assert!(Directory::open(&nppes::path(&c.directories(), &h.folder_id)).is_ok());
            c.put(
                "views/views.json",
                &json!({"schema": "medrec-views/v2", "medrec": "0.0.0", "household": "",
                    "label_salt": SALT, "people": {}})
                .to_string(),
            );
            c
        }

        fn directories(&self) -> PathBuf {
            self.root.with_extension("directory")
        }

        fn keep(&self, w: &Wanted, rows: &str) {
            self.keep_of("2026-03-08", w, rows);
        }

        /// The kept file of the month whose last day is `date`.
        fn keep_of(&self, date: &str, w: &Wanted, rows: &str) {
            let path = nppes::path(&self.directories(), "f_exampleaaaaaaaaaaaaa");
            nppes::keep_rows(&path, date, w, rows);
        }

        fn put(&self, rel: &str, text: &str) {
            let p = self.root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }

        fn folder(&self) -> Folder {
            let loc = crate::store::Location::Dir(self.root.clone());
            Folder::open_at(&loc, Access::Read, Duration::from_secs(60)).unwrap()
        }

        /// Write the views, with the directory or on a machine without it:
        /// how many people's were made.
        fn sync(&self, directory: bool) -> usize {
            self.try_sync(directory).unwrap().people_rebuilt
        }

        fn try_sync(
            &self,
            directory: bool,
        ) -> Result<crate::viewfiles::Synced, pk_cli_core::CliError> {
            self.put(
                "Robin Example/index.json",
                &serde_json::to_string(&self.index).unwrap(),
            );
            self.put("Robin Example/marks.json", &self.marks.to_string());
            let d = self.directories();
            crate::viewfiles::sync_with(
                &self.folder(),
                false,
                None,
                directory.then_some(d.as_path()),
            )
        }

        fn view(&self, name: &str) -> Value {
            let v: crate::viewfiles::ViewsIndex = serde_json::from_str(
                &std::fs::read_to_string(self.root.join("views/views.json")).unwrap(),
            )
            .unwrap();
            match v.people[PERSON].files.get(name) {
                Some(md5) => {
                    let rel = format!(
                        "views/{PERSON}/{}",
                        crate::viewfiles::stored_name(name, md5)
                    );
                    serde_json::from_str(&std::fs::read_to_string(self.root.join(rel)).unwrap())
                        .unwrap()
                }
                None => Value::Null,
            }
        }

        /// Where a view is, as the last pass stored it.
        fn stored(&self, name: &str) -> PathBuf {
            let v: crate::viewfiles::ViewsIndex = serde_json::from_str(
                &std::fs::read_to_string(self.root.join("views/views.json")).unwrap(),
            )
            .unwrap();
            self.root.join(format!(
                "views/{PERSON}/{}",
                crate::viewfiles::stored_name(name, &v.people[PERSON].files[name])
            ))
        }

        fn state_path(&self) -> PathBuf {
            self.root.join("Robin Example/provider-state.json")
        }

        fn state(&self) -> Value {
            serde_json::from_str(&std::fs::read_to_string(self.state_path()).unwrap()).unwrap()
        }

        /// The states kept aside as not readable.
        fn unread(&self) -> Vec<Vec<u8>> {
            std::fs::read_dir(self.root.join("Robin Example"))
                .unwrap()
                .map(|e| e.unwrap().path())
                .filter(|p| p.to_string_lossy().contains("provider-state~unread-"))
                .map(|p| std::fs::read(p).unwrap())
                .collect()
        }

        /// What an answer judges private now.
        fn judged(&self) -> Value {
            let f = self.folder();
            let h: Household = serde_json::from_str(fixture!("household.json")).unwrap();
            let idx = f.index(&h.people[0]).unwrap();
            private::with_events(&f, &h, &h.people[0], &idx).unwrap().0
        }

        /// Whether a record is private, in the view and in an answer.
        fn private(&self, record: &str) -> bool {
            let (view, answer) = (self.view("private.json"), self.judged());
            assert_eq!(view, answer);
            view["records"].get(record).is_some()
        }

        fn add(&mut self, id: &str, provider: &str, npi: &str) {
            let mut r = self.index.records[0].clone();
            r.id = id.into();
            r.key = format!("claim|{id}");
            r.date = Some("2026-02-03".into());
            r.fields["provider"] = json!(provider);
            r.providers[0].npi = Some(npi.into());
            self.index.records.push(r);
        }

        fn mark(&mut self, item: &str, private: Option<bool>) {
            let marks = self.marks["private"].as_array_mut().unwrap();
            marks.retain(|m| m["item"] != item);
            if let Some(b) = private {
                marks.push(json!({"id": "o_markbaaaaaaaaaaaaaaa", "item": item,
                    "private": b, "at": "2026-01-03T00:00:00Z"}));
            }
        }

        fn labels(&self) -> Value {
            self.view(FILE)
        }
    }

    impl Drop for Case {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
            let _ = std::fs::remove_dir_all(self.directories());
        }
    }

    /// Every kind of label, where each is kept, and what is not labelled:
    /// two providers it can be (`D SPECIMEN`), papers of two practices
    /// (`P DUMMY`), a private paper for an open item (`L FILLER`), a service
    /// two fit (`r_therapy…`), and an item the household marked not private
    /// whose label is in a private area (`r_marked…`).
    #[test]
    fn the_file_is_as_the_contract_has_it() {
        let c = Case::new();
        assert_eq!(c.sync(true), 1);
        let want: Value = serde_json::from_str(fixture!("providers.json")).unwrap();
        let got = c.labels();
        assert_eq!(got, want, "{}", serde_json::to_string_pretty(&got).unwrap());
        // The label alone made this one private.
        assert_eq!(
            c.view("private.json")["records"]["r_talkaaaaaaaaaaaaaaaa"],
            json!({"area": "mental_health", "from": "provider"})
        );
        // Neither an open item nor the open part says anything of it.
        let open = json!([got["labels"], got["by_name"], got["by_item"]]).to_string();
        for s in [
            "Casey",
            "psychologist",
            "C EXAMPLE",
            "r_marked",
            "counseling",
        ] {
            assert!(!open.contains(s), "{s}");
        }
        // No NPI is anywhere in it. medrec keeps those beside it.
        assert!(!got.to_string().contains("00000000"));
        let want: Value = serde_json::from_str(fixture!("provider-state.json")).unwrap();
        let state = c.state();
        assert_eq!(
            state,
            want,
            "{}",
            serde_json::to_string_pretty(&state).unwrap()
        );
        assert_eq!(c.sync(true), 0);
    }

    /// What is private is the same for an answer as for the view, on any
    /// machine.
    #[test]
    fn an_answer_judges_as_the_views_do() {
        let c = Case::new();
        for directory in [true, false] {
            c.sync(directory);
            assert!(c.private("r_talkaaaaaaaaaaaaaaaa"));
        }
    }

    #[test]
    fn a_machine_without_the_directory_keeps_the_labels() {
        let mut c = Case::new();
        c.sync(true);
        let before = c.labels();
        assert_eq!(c.sync(false), 0);

        // Nothing is lost by a pass of its own, and what the papers say is
        // read again.
        c.add("r_newclaimaaaaaaaaaaaa", "SOMETHING NEW LLC", "0000000097");
        assert_eq!(c.sync(false), 1);
        assert_eq!(c.labels(), before);
        assert!(c.private("r_talkaaaaaaaaaaaaaaaa"));

        // A new record with a known NPI has its label, whatever it prints;
        // one with another NPI has none, even under a printed name that has.
        c.add("r_knownaaaaaaaaaaaaaaa", "SOMETHING ELSE LLC", "0000000022");
        c.add("r_otheraaaaaaaaaaaaaaa", "J SAMPLE", "0000000048");
        c.add("r_alsoaaaaaaaaaaaaaaaa", "SOMEONE NEW", "0000000014");
        c.sync(false);
        let now = c.labels();
        assert_eq!(
            now["by_item"]["r_knownaaaaaaaaaaaaaaa"]["SOMETHING ELSE LLC"],
            before["by_item"]["r_officevisitaaaaaaaaa"]["EXAMPLE PRIMARY CARE GROUP LLC"]
        );
        assert!(now["by_item"].get("r_otheraaaaaaaaaaaaaaa").is_none());
        assert!(now["by_name"].get("J SAMPLE").is_none());
        assert_eq!(
            now["by_item"]["r_samplevisitaaaaaaaaa"],
            before["by_item"]["r_samplevisitaaaaaaaaa"]
        );
        assert_eq!(now["directory"], before["directory"]);
        assert!(c.private("r_alsoaaaaaaaaaaaaaaaa"));

        // So does one whose source prints another name now.
        c.index.records[3].fields["provider"] = json!("CASEY E");
        c.sync(false);
        assert!(c.private("r_talkaaaaaaaaaaaaaaaa"));

        // What is private now decides again where each goes.
        c.mark("r_imagingaaaaaaaaaaaaa", Some(true));
        c.sync(false);
        let now = c.labels();
        assert!(now["by_item"].get("r_imagingaaaaaaaaaaaaa").is_none());
        assert_eq!(
            now["private"]["by_item"]["r_imagingaaaaaaaaaaaaa"]["A TESTPERSON"],
            before["by_item"]["r_imagingaaaaaaaaaaaaa"]["A TESTPERSON"]
        );
        c.mark("r_imagingaaaaaaaaaaaaa", None);
        c.sync(false);
        assert_eq!(
            c.labels()["by_item"]["r_imagingaaaaaaaaaaaaa"],
            before["by_item"]["r_imagingaaaaaaaaaaaaa"]
        );

        // The machine with the directory makes them again, once. An NPI it
        // does not have is still not another provider of that name.
        assert_eq!(c.sync(true), 1);
        assert_eq!(c.labels()["directory"], before["directory"]);
        assert!(c.labels()["by_item"]
            .get("r_otheraaaaaaaaaaaaaaa")
            .is_none());
        assert_eq!(c.sync(true), 0);
        assert_eq!(c.sync(false), 0);
    }

    /// A label nothing shows is still kept: an item marked not private, then
    /// left to the rules, is private by it again.
    #[test]
    fn a_label_withheld_from_an_open_item_is_not_lost() {
        let mut c = Case::new();
        c.index.records.retain(|r| r.id != "r_markedaaaaaaaaaaaaaa");
        c.sync(true);
        assert!(c.private("r_talkaaaaaaaaaaaaaaaa"));
        c.mark("r_talkaaaaaaaaaaaaaaaa", Some(false));
        c.sync(true);
        assert!(!c.private("r_talkaaaaaaaaaaaaaaaa"));
        assert!(!c.labels().to_string().contains("Casey"));
        c.sync(false);
        c.mark("r_talkaaaaaaaaaaaaaaaa", None);
        c.sync(false);
        assert!(c.private("r_talkaaaaaaaaaaaaaaaa"));
        assert!(c.labels()["private"].to_string().contains("Casey"));
    }

    /// The state is all that says which records a label made private. When
    /// it does not read, or is gone, nothing of the person's is open.
    #[test]
    fn a_state_that_does_not_read_holds_the_person_back() {
        type Damage = fn(&Case);
        let remove: Damage = |c| std::fs::remove_file(c.state_path()).unwrap();
        let cut: Damage = |c| {
            let text = std::fs::read_to_string(c.state_path()).unwrap();
            std::fs::write(c.state_path(), &text[..text.len() / 2]).unwrap();
        };
        let another: Damage = |c| {
            let text = std::fs::read_to_string(c.state_path()).unwrap();
            let text = text.replace(PERSON, "p_otheraaaaaaaaaaaaaaa");
            std::fs::write(c.state_path(), text).unwrap();
        };
        // As a medrec that knows another kind of evidence writes it.
        let evidence: Damage = |c| {
            let text = std::fs::read_to_string(c.state_path()).unwrap();
            let text = text.replace(r#""from": "npi""#, r#""from": "seal""#);
            std::fs::write(c.state_path(), text).unwrap();
        };
        for (n, damage) in [remove, cut, another, evidence].into_iter().enumerate() {
            let mut c = Case::new();
            c.sync(true);
            assert!(c.judged().get("labels_unreadable").is_none());
            damage(&c);
            let held = std::fs::read(c.state_path()).ok();
            for flag in ["labels_unreadable", "marks_unreadable"] {
                assert_eq!(c.judged()[flag], true, "{n}");
            }
            // A pass without the directory says so too, and the next.
            for day in ["2026-04-01", "2026-04-02"] {
                c.index.records[0].date = Some(day.into());
                assert_eq!(c.sync(false), 1, "{n}");
                let v = c.view("private.json");
                assert_eq!(v["labels_unreadable"], true, "{n}");
                assert_eq!(v["marks_unreadable"], true, "{n}");
                assert_eq!(c.judged(), v, "{n}");
                let l = c.labels();
                assert_eq!(l["labels"], json!({}), "{n}");
                assert!(!l.to_string().contains("sure"), "{n}");
                // It writes nothing over one that is there.
                if held.is_some() {
                    assert_eq!(std::fs::read(c.state_path()).ok(), held, "{n}");
                }
                assert_eq!(c.sync(false), 0, "{n}");
            }
            // One with it knows again, and what did not read stays beside it.
            assert!(c.unread().is_empty(), "{n}");
            assert_eq!(c.sync(true), 1);
            assert!(c.view("private.json").get("labels_unreadable").is_none());
            assert!(c.private("r_talkaaaaaaaaaaaaaaaa"));
            assert_eq!(c.sync(true), 0);
            assert_eq!(c.unread(), Vec::from_iter(held), "{n}");
        }
        // A later medrec's stays, whatever this one knows.
        let c = Case::new();
        c.sync(true);
        let text = std::fs::read_to_string(c.state_path()).unwrap();
        let text = text.replace(STATE_SCHEMA, "medrec-provider-state/v2");
        std::fs::write(c.state_path(), &text).unwrap();
        assert_eq!(c.sync(true), 1);
        assert_eq!(std::fs::read_to_string(c.state_path()).unwrap(), text);
        assert_eq!(c.view("private.json")["labels_unreadable"], true);
        assert_eq!(c.judged()["labels_unreadable"], true);
        assert_eq!(c.sync(true), 0);
        assert!(c.unread().is_empty());
        // A folder whose views never had one is not held back.
        let c = Case::new();
        c.sync(false);
        assert!(c.judged().get("labels_unreadable").is_none());
    }

    /// Only a directory asked of every NPI the records print ends it.
    #[test]
    fn a_directory_not_asked_of_every_npi_leaves_a_lost_state_lost() {
        let c = Case::new();
        c.sync(true);
        std::fs::write(c.state_path(), "{").unwrap();
        let mut narrow = Wanted::of([&c.index]);
        narrow.npis.remove("0000000014");
        let without: String = fixture!("directory.jsonl")
            .lines()
            .filter(|l| !l.contains("0000000014"))
            .map(|l| format!("{l}\n"))
            .collect();
        c.keep_of("2026-04-12", &narrow, &without);
        assert_eq!(c.sync(true), 1);
        assert_eq!(c.view("private.json")["labels_unreadable"], true);
        assert!(c.labels()["private"].to_string().contains("Jordan Sample"));
        assert_eq!(c.labels()["labels"], json!({}));
        assert_eq!(c.sync(true), 0);
        // Asked of it, and CMS's file has none: there is no more to know.
        c.keep_of("2026-05-10", &Wanted::of([&c.index]), &without);
        assert_eq!(c.sync(true), 1);
        assert!(c.view("private.json").get("labels_unreadable").is_none());
    }

    /// An earlier medrec saves an index without its providers, and
    /// `reindex` gives them back: what was private by one is again.
    #[test]
    fn an_index_without_providers_for_a_while_loses_no_label() {
        let mut c = Case::new();
        c.sync(true);
        let state = c.state();
        let whole = c.index.clone();
        for r in &mut c.index.records {
            r.providers.clear();
        }
        c.sync(false);
        assert_eq!(c.state()["by_npi"], state["by_npi"]);
        c.index = whole;
        c.sync(false);
        assert!(c.private("r_talkaaaaaaaaaaaaaaaa"));
        assert_eq!(c.state()["by_npi"], state["by_npi"]);
        assert_eq!(c.labels()["by_item"], {
            let want: Value = serde_json::from_str(fixture!("providers.json")).unwrap();
            want["by_item"].clone()
        });
    }

    /// The views are made from the state, not the state from the views.
    #[test]
    fn losing_the_views_loses_no_label() {
        type Damage = fn(&Case);
        let index: Damage = |c| c.put("views/views.json", "{");
        let all: Damage = |c| std::fs::remove_dir_all(c.root.join("views")).unwrap();
        // As a medrec before 0.9.6 leaves them.
        let earlier: Damage = |c| {
            let path = c.root.join("views/views.json");
            let mut v: Value =
                serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            v["medrec"] = "0.9.5".into();
            v.as_object_mut().unwrap().remove("label_salt");
            let p = v["people"][PERSON].as_object_mut().unwrap();
            p.remove("provider_state");
            p.remove("directory");
            p["files"].as_object_mut().unwrap().remove(FILE);
            std::fs::write(path, v.to_string()).unwrap();
        };
        for (n, damage) in [index, all, earlier].into_iter().enumerate() {
            let c = Case::new();
            c.sync(true);
            let state = c.state();
            damage(&c);
            assert!(
                c.judged()["records"]
                    .get("r_talkaaaaaaaaaaaaaaaa")
                    .is_some(),
                "{n}"
            );
            assert_eq!(c.sync(false), 1, "{n}");
            assert!(c.private("r_talkaaaaaaaaaaaaaaaa"), "{n}");
            assert_eq!(c.state(), state, "{n}");
            assert!(c.labels()["directory"].is_object(), "{n}");
        }
    }

    /// Another machine wrote the state during a pass, whether or not the
    /// pass writes one: no view of it is taken for current, and the next
    /// makes them from what is there.
    #[test]
    fn a_state_written_during_a_pass_ends_it() {
        for directory in [false, true] {
            let mut c = Case::new();
            c.sync(directory);
            let views = c.root.join("views/views.json");
            let before = std::fs::read(&views).unwrap();
            // Theirs is a sure psychologist's label on the visit's NPI.
            let mut theirs = match directory {
                true => c.state(),
                false => serde_json::from_str(fixture!("provider-state.json")).unwrap(),
            };
            theirs["by_npi"]["0000000006"] = theirs["by_npi"]["0000000014"].clone();
            let (at, text) = (c.state_path(), theirs.to_string());
            let theirs = text.clone();
            crate::viewfiles::tests::meanwhile(move || std::fs::write(at, text).unwrap());
            c.index.records[0].date = Some("2026-04-01".into());
            if directory {
                // This one writes a state as well.
                c.add("r_secondvisitaaaaaaaaa", "T PLACEHOLDER", "0000000055");
            }
            let err = c.try_sync(directory).unwrap_err();
            assert!(err.message().contains("another machine"), "{directory}");
            assert_eq!(std::fs::read(&views).unwrap(), before, "{directory}");
            assert_eq!(std::fs::read_to_string(c.state_path()).unwrap(), theirs);
            assert_eq!(c.sync(false), 1);
            assert!(c.private("r_samplevisitaaaaaaaaa"), "{directory}");
        }
    }

    /// A pass that was cut short after it wrote the state: the next makes
    /// the views it is ahead of.
    #[test]
    fn views_behind_the_state_are_made_again() {
        let c = Case::new();
        c.sync(true);
        let text = std::fs::read_to_string(c.state_path()).unwrap();
        std::fs::write(c.state_path(), text.replace("0101", "0199")).unwrap();
        assert_eq!(c.sync(false), 1);
        assert!(c.labels().to_string().contains("555-010-0199"));
        assert_eq!(c.sync(false), 0);
    }

    /// A view of another content than it was written with is made again, as
    /// a missing one is.
    #[test]
    fn a_view_that_is_not_as_written_is_made_again() {
        let mut c = Case::new();
        c.sync(true);
        let before = c.labels();
        std::fs::write(c.stored(FILE), "{}").unwrap();
        // The household file's change is what starts the pass.
        c.mark("r_imagingaaaaaaaaaaaaa", Some(true));
        c.mark("r_imagingaaaaaaaaaaaaa", None);
        let h = fixture!("household.json").replace("\"rev\": 1", "\"rev\": 2");
        c.put("household.json", &h);
        c.sync(true);
        assert_eq!(c.labels(), before);
    }

    #[test]
    fn another_months_file_makes_them_again_and_a_damaged_one_is_not_used() {
        let c = Case::new();
        c.sync(true);
        let w = Wanted::of([&c.index]);
        let rows = fixture!("directory.jsonl").replace("5550100101", "5550100199");
        c.keep_of("2026-04-12", &w, &rows);
        assert_eq!(c.sync(true), 1);
        assert!(c.labels().to_string().contains("555-010-0199"));
        assert!(!c.labels().to_string().contains("555-010-0101"));
        assert_eq!(c.labels()["directory"]["date"], "2026-04-12");

        let path = nppes::path(&c.directories(), "f_exampleaaaaaaaaaaaaa");
        let kept = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, kept.replace("5550100199", "5550100198")).unwrap();
        assert_eq!(c.sync(true), 0);
        assert!(c.labels().to_string().contains("555-010-0199"));
    }

    /// A provider that leaves the directory, or whom it was fetched before
    /// anyone printed, keeps what was said of them.
    #[test]
    fn a_directory_without_a_provider_takes_no_label_away() {
        let c = Case::new();
        c.sync(true);
        let before = c.labels();
        let w = Wanted::of([&c.index]);
        let rows: String = fixture!("directory.jsonl")
            .lines()
            .filter(|l| !l.contains("0000000014") && !l.contains("0000000022"))
            .map(|l| format!("{l}\n"))
            .collect();
        c.keep_of("2026-04-12", &w, &rows);
        assert_eq!(c.sync(true), 1);
        assert!(c.private("r_talkaaaaaaaaaaaaaaaa"));
        let now = c.labels();
        assert_eq!(now["by_item"], before["by_item"]);
        // Each phone is as of the file that gave it.
        let date = |npi_name: &str| {
            now["labels"]
                .as_object()
                .unwrap()
                .values()
                .find(|l| l["name"] == npi_name)
                .unwrap()["phone_from"]["date"]
                .clone()
        };
        assert_eq!(date("Example Primary Care"), "2026-03-08");
        assert_eq!(date("Jordan Sample, MD"), "2026-04-12");

        // A name the new file is not whole in keeps its match; one it is
        // whole in, and no longer has, does not.
        let mut narrow = Wanted::of([&c.index]);
        narrow.people.retain(|(l, _)| l != "TESTPERSON");
        let none = rows
            .lines()
            .filter(|l| !l.contains("TESTPERSON"))
            .map(|l| format!("{l}\n"))
            .collect::<String>();
        c.keep_of("2026-05-10", &narrow, &none);
        c.sync(true);
        assert!(c.labels()["by_item"]
            .get("r_imagingaaaaaaaaaaaaa")
            .is_some());
        c.keep_of("2026-06-14", &w, &none);
        c.sync(true);
        assert!(c.labels()["by_item"]
            .get("r_imagingaaaaaaaaaaaaa")
            .is_none());
    }

    /// Two machines, one with an older directory: what the newer one gave
    /// stands, and neither makes the other's views again for nothing.
    #[test]
    fn an_older_directory_adds_and_replaces_nothing() {
        let mut c = Case::new();
        let w = Wanted::of([&c.index]);
        let newer = fixture!("directory.jsonl").replace("5550100101", "5550100199");
        let older: String = fixture!("directory.jsonl")
            .lines()
            .filter(|l| !l.contains("0000000014"))
            .map(|l| format!("{l}\n"))
            .collect();
        c.keep_of("2026-04-12", &w, &newer);
        c.sync(true);
        let made = c.labels();
        c.keep_of("2026-03-08", &w, &older);
        assert_eq!(c.sync(true), 0);

        // The older one's own pass.
        c.add("r_newclaimaaaaaaaaaaaa", "D SPECIMEN", "0000000089");
        assert_eq!(c.sync(true), 1);
        let now = c.labels();
        assert!(c.private("r_talkaaaaaaaaaaaaaaaa"));
        assert_eq!(now["directory"], made["directory"]);
        assert!(now.to_string().contains("555-010-0199"));
        assert!(!now.to_string().contains("555-010-0101"));
        // An NPI the newer one was not asked of is labelled all the same.
        let id = now["by_item"]["r_newclaimaaaaaaaaaaaa"]["D SPECIMEN"].clone();
        let l = &now["labels"][id.as_str().unwrap()];
        assert_eq!(l["confidence"], "sure");
        assert_eq!(l["phone_from"]["date"], "2026-03-08");

        // The newer one's next pass fills in, then both rest.
        c.keep_of("2026-04-12", &w, &newer);
        assert_eq!(c.sync(true), 1);
        assert_eq!(c.sync(true), 0);
        c.keep_of("2026-03-08", &w, &older);
        assert_eq!(c.sync(true), 0);
    }

    /// Papers need no directory. Their evidence is theirs to lose.
    #[test]
    fn papers_label_with_no_directory() {
        let mut c = Case::new();
        c.sync(false);
        let v = c.labels();
        assert!(v.get("directory").is_none());
        assert_eq!(
            v["by_name"],
            json!({
                "M TEMPLATE": v["by_item"]["r_paperaloneaaaaaaaaaa"]["M TEMPLATE"],
                "R PLACEHOLDER": v["by_item"]["r_onepaperaaaaaaaaaaaa"]["R PLACEHOLDER"],
            })
        );
        let l = |k: &str| v["labels"][v["by_name"][k].as_str().unwrap()].clone();
        assert_eq!(
            l("M TEMPLATE"),
            json!({"practice": "Sample Health Partners", "confidence": "likely",
                "evidence": [{"from": "paper", "document": "d_summaryaaaaaaaaaaaaa"}]})
        );
        assert_eq!(l("R PLACEHOLDER")["phone"], "555-010-0150");
        assert_eq!(v["private"]["labels"], json!({}));

        // A person none of whose items has a label has no file.
        c.index.documents.clear();
        c.sync(false);
        assert!(c.labels().is_null());
    }

    #[test]
    fn only_names_and_states_the_kept_file_is_whole_in_are_matched() {
        let mut c = Case::new();
        // Printed since the directory was read: another of that name may
        // not be among the rows.
        for (id, name, state) in [
            ("r_newnameaaaaaaaaaaaaa", "J TESTPERSON", "FL"),
            ("r_newstateaaaaaaaaaaaa", "A TESTPERSON", "GA"),
        ] {
            let mut r = c.index.records[4].clone();
            assert_eq!(r.id, "r_imagingaaaaaaaaaaaaa");
            r.id = id.into();
            r.date = Some("2026-03-03".into());
            r.fields["provider"] = json!(name);
            r.providers[0].state = Some(state.into());
            c.index.records.push(r);
        }
        let rows = fixture!("directory.jsonl").replace(
            "\n",
            "\n",
        ) + "{\"npi\":\"0000000105\",\"last\":\"TESTPERSON\",\"first\":\"JAMIE\",\"state\":\"FL\",\"taxonomy\":\"2085R0202X\"}\n";
        let mut w = Wanted::of([&c.index]);
        w.people.retain(|p| p != &("TESTPERSON".to_string(), 'J'));
        w.states.retain(|s| s != "GA");
        c.keep(&w, &rows);
        c.sync(true);
        let v = c.labels();
        assert!(v["by_item"].get("r_imagingaaaaaaaaaaaaa").is_some());
        assert!(v["by_item"].get("r_newnameaaaaaaaaaaaaa").is_none());
        assert!(v["by_item"].get("r_newstateaaaaaaaaaaaa").is_none());
        // Once it is whole in them, the one name matches.
        c.keep_of("2026-04-12", &Wanted::of([&c.index]), &rows);
        c.sync(true);
        let v = c.labels();
        assert!(v["by_item"].get("r_newnameaaaaaaaaaaaaa").is_some());
        assert!(v["by_item"].get("r_newstateaaaaaaaaaaaa").is_none());
    }

    #[test]
    fn one_provider_has_one_id_in_a_folder_and_another_in_another() {
        let row: Provider =
            serde_json::from_str(fixture!("directory.jsonl").lines().next().unwrap()).unwrap();
        let l = sure(&row, "2026-03-08").unwrap();
        assert_eq!(l.id(SALT, "J SAMPLE"), l.id(SALT, "Jordan Sample"));
        assert_ne!(l.id(SALT, "J SAMPLE"), l.id("s_other", "J SAMPLE"));
        assert!(crate::ids::is_valid("v", &l.id(SALT, "J SAMPLE")));
        let likely = Label {
            confidence: Confidence::Likely,
            ..l.clone()
        };
        assert_ne!(likely.id(SALT, ""), l.id(SALT, ""));
        // With no name of its own it is the printed name's.
        let papers = Label {
            name: None,
            ..likely
        };
        assert_ne!(papers.id(SALT, "J SAMPLE"), papers.id(SALT, "K SAMPLE"));
    }

    #[test]
    fn the_salt_is_made_once_and_only_when_needed() {
        let mut c = Case::new();
        std::fs::remove_file(c.root.join("views/views.json")).unwrap();
        let documents = std::mem::take(&mut c.index.documents);
        let salt = |c: &Case| -> Value {
            serde_json::from_str::<Value>(
                &std::fs::read_to_string(c.root.join("views/views.json")).unwrap(),
            )
            .unwrap()["label_salt"]
                .clone()
        };
        c.sync(false);
        assert!(salt(&c).is_null());
        c.index.documents = documents;
        c.sync(false);
        let made = salt(&c);
        assert!(crate::ids::is_valid("s", made.as_str().unwrap()));
        c.sync(true);
        assert_eq!(salt(&c), made);
    }

    #[test]
    fn a_label_of_a_private_area_is_known_by_its_words() {
        let l = |name: &str, practice: &str, role: &str| {
            let some = |s: &str| (!s.is_empty()).then(|| s.to_string());
            Label {
                name: some(name),
                practice: some(practice),
                role: some(role),
                phone: None,
                phone_from: None,
                confidence: Confidence::Likely,
                evidence: Vec::new(),
            }
            .area()
        };
        assert_eq!(
            l("Casey Example", "", "psychologist"),
            Some("mental_health")
        );
        assert_eq!(l("Casey Example, LCSW", "", ""), Some("mental_health"));
        assert_eq!(
            l("Casey Example", "Example Counseling", ""),
            Some("mental_health")
        );
        assert_eq!(l("", "", "substance use counselor"), Some("substance_use"));
        assert_eq!(
            l("Casey Example", "", "fertility doctor"),
            Some("reproductive")
        );
        assert_eq!(l("Casey Example, MD", "Example Health", "OB-GYN"), None);
    }

    #[test]
    fn names_are_written_as_names_are() {
        let person = |last: &str, first: &str, credential: &str| {
            person_name(&Provider {
                last: last.into(),
                first: first.into(),
                credential: credential.into(),
                ..Provider::default()
            })
        };
        for (last, first, credential, want) in [
            ("SAMPLE", "JORDAN", "M.D.", "Jordan Sample, MD"),
            (
                "O'EXAMPLE-SAMPLE",
                "RILEY",
                "P.A.-C",
                "Riley O'Example-Sample, PA-C",
            ),
            ("MCSAMPLE", "DREW", "", "Drew McSample"),
            ("MCK", "DREW", "", "Drew Mck"),
            (
                "SAMPLE III",
                "DREW",
                "MD, PH.D., FACP",
                "Drew Sample III, MD, PhD",
            ),
            ("van Sample", "Drew", "whatever", "Drew van Sample"),
            ("NG", "SAM", "RN", "Sam Ng, RN"),
        ] {
            assert_eq!(person(last, first, credential).as_deref(), Some(want));
        }
        assert_eq!(person("", "", "MD"), None);
        let organization = |name: &str, business: &str| {
            organization_name(&Provider {
                organization: name.into(),
                business_name: business.into(),
                ..Provider::default()
            })
        };
        for (name, business, want) in [
            (
                "EXAMPLE PRIMARY CARE GROUP, LLC",
                "",
                "Example Primary Care Group",
            ),
            ("EXAMPLE HOLDINGS INC", "SAMPLE CLINIC", "Sample Clinic"),
            ("JORDAN A SAMPLE MD PA", "", "Jordan A Sample MD"),
            (
                "ST. SAMPLE'S HOSPITAL OF FL",
                "",
                "St. Sample's Hospital of FL",
            ),
            ("CVX PHARMACY", "", "CVX Pharmacy"),
            ("Example iCare", "", "Example iCare"),
        ] {
            assert_eq!(organization(name, business).as_deref(), Some(want));
        }
        for (digits, want) in [
            ("5550100101", Some("555-010-0101")),
            ("15550100101", Some("555-010-0101")),
            ("(555) 010-0101", Some("555-010-0101")),
            ("555010", None),
            ("", None),
        ] {
            assert_eq!(phone(digits).as_deref(), want, "{digits}");
        }
    }

    #[test]
    fn a_paper_prints_a_name_as_names_are_written() {
        let prints = |body: &str, name: &str| prints_name(body, &nppes::people(name));
        for body in [
            "Seen by Morgan Template, MD",
            "Seen by M. Template",
            "MORGAN A. TEMPLATE",
            "Template, Morgan",
            "TEMPLATE,\n MORGAN A",
        ] {
            assert!(prints(body, "M TEMPLATE"), "{body}");
            assert!(prints(body, "Morgan Template, MD"), "{body}");
        }
        for body in [
            "Seen by Robin Template",
            "Template, Robin",
            "make template",
            "the template morgan",
            "Morgan Placeholder Template",
            "Template Morgan",
        ] {
            assert!(!prints(body, "M TEMPLATE"), "{body}");
        }
        assert!(prints("Dr. Riley O'Example-Sample", "R O'EXAMPLE SAMPLE"));
        assert!(!prints("Al Li", "A LI"), "too short to look for");
    }

    #[test]
    fn a_contradicting_given_name_is_no_match() {
        let p: Provider =
            serde_json::from_str(fixture!("directory.jsonl").lines().next().unwrap()).unwrap();
        for text in [
            "J SAMPLE",
            "Jordan Sample, MD",
            "SAMPLE, JORDAN A",
            "J A SAMPLE",
            "Jord Sample",
        ] {
            assert!(given_names_agree(&p, text), "{text}");
        }
        for text in ["Jamie Sample", "SAMPLE, JAMIE", "J PLACEHOLDER SAMPLE"] {
            assert!(!given_names_agree(&p, text), "{text}");
        }
    }
}
