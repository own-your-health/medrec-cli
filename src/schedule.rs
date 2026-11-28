//! A person's vaccine schedule (0.9.12 and later), by CDC's CDSi supporting
//! data: of each vaccine group, the doses of its routine series at the ages
//! the schedule recommends them, on the dates those ages fall on for the
//! person, and beside them the doses on file. It says nothing of what is due:
//! no dose on file is compared with a dose of the schedule.
//!
//! A routine series is each antigen's standard series that CDC chooses by
//! default, one for each of its series groups. CDC's series hold the doses of
//! catch-up too, so a dose is listed only where CDC's own conditional skips
//! (Logic Specification 4.6, Tables 6-6 to 6-11, of a forecast) keep it when
//! every dose before it is given at its earliest recommended age. Dates follow
//! CDC's rules (CALCDT-1 to 6): the years, then the months, then the days, and
//! a date that does not exist moves forward to the first of the next month.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Map, Value};

use crate::model::{Code, Record};
use crate::tables::{self, Table, CDSI};
use crate::xml::{self, Element};

pub type Day = i64;

/// The schedules medrec reads, in the order their doses are shown.
pub const SOURCES: &[&Table] = &[&CDSI];

pub const SCHEMA: &str = "medrec-schedule/v1";

/// A person's view of it.
pub const FILE: &str = "schedule.json";

/// The most one file of the archive may be.
const FILE_MAX: u64 = 8 << 20;

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

fn day(iso: &str) -> Option<Day> {
    let d = iso.get(..10)?;
    crate::refresh::time::parse(&format!("{d}T00:00:00Z")).map(|s| s.div_euclid(86_400))
}

pub fn iso(d: Day) -> String {
    crate::refresh::time::fmt(d * 86_400)[..10].to_string()
}

fn ymd(d: Day) -> (i64, i64, i64) {
    let s = iso(d);
    let n = |r: std::ops::Range<usize>| s[r].parse().unwrap_or(0);
    (n(0..4), n(5..7), n(8..10))
}

/// The day `d` of month `m` (counted past 12 into the next years) of year
/// `y`, or, when that is not a real date, the first of the next month
/// (CALCDT-5).
fn on(y: i64, m: i64, d: i64) -> Option<Day> {
    let t = y * 12 + (m - 1);
    let (y, m) = (t.div_euclid(12), t.rem_euclid(12) + 1);
    if !(1..=9998).contains(&y) {
        return None;
    }
    day(&format!("{y:04}-{m:02}-{d:02}")).or_else(|| on(y, m + 1, 1))
}

/// "Mar 15, 2027".
pub fn us_day(iso: &str) -> String {
    let (Some(y), Some(m), Some(d)) = (iso.get(..4), iso.get(5..7), iso.get(8..10)) else {
        return iso.to_string();
    };
    match (m.parse::<usize>(), d.parse::<u32>()) {
        (Ok(m @ 1..=12), Ok(d)) => format!("{} {d}, {y}", &MONTHS[m - 1][..3]),
        _ => iso.to_string(),
    }
}

/// An age or an interval as CDC writes one: "12 months - 4 days".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Age {
    years: i64,
    months: i64,
    days: i64,
}

impl Age {
    pub fn parse(s: &str) -> Option<Age> {
        let mut a = Age::default();
        let mut words = s.split_whitespace().peekable();
        let mut first = true;
        while words.peek().is_some() {
            let sign = match first {
                true => 1,
                false => match words.next()? {
                    "+" => 1,
                    "-" => -1,
                    _ => return None,
                },
            };
            first = false;
            let n = sign * words.next()?.parse::<i64>().ok()?;
            match words.next()?.to_ascii_lowercase().trim_end_matches('s') {
                "year" => a.years += n,
                "month" => a.months += n,
                "week" => a.days += 7 * n,
                "day" => a.days += n,
                _ => return None,
            }
        }
        (!first).then_some(a)
    }

    /// The day this long after `d` (CALCDT-1 to 6).
    pub fn after(&self, d: Day) -> Option<Day> {
        let (y, m, dd) = ymd(d);
        let (y, m, dd) = ymd(on(y + self.years, m, dd)?);
        Some(on(y, m + self.months, dd)? + self.days)
    }
}

/// What a CVX code or a vaccine counts for: an antigen, between ages.
type Between = (String, Option<Age>, Option<Age>);

/// What one table says.
#[derive(Debug, Clone)]
pub struct Schedules {
    /// Its version, from its folder's name.
    pub version: String,
    /// Its date: its release notes', else its newest file's.
    pub date: String,
    /// How many series its antigen files have.
    pub series: usize,
    routine: Vec<Series>,
    /// Of each CVX code, the antigens it counts for, between ages.
    cvx: BTreeMap<u32, Vec<Between>>,
    /// Of each antigen, its vaccine group.
    groups: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
struct Series {
    antigen: String,
    name: String,
    group: String,
    doses: Vec<Dose>,
}

#[derive(Debug, Clone)]
struct Dose {
    number: String,
    /// The earliest recommended age, and its words.
    earliest: Option<(Age, String)>,
    recurring: bool,
    season: Option<(Day, Day)>,
    /// The preferable vaccines' CVX codes, between ages.
    vaccines: Vec<Between>,
    /// The conditional skips of a forecast.
    skips: Vec<Skip>,
}

#[derive(Debug, Clone)]
struct Skip {
    all: bool,
    sets: Vec<Set>,
}

#[derive(Debug, Clone)]
struct Set {
    all: bool,
    from: Option<Day>,
    until: Option<Day>,
    conditions: Vec<Condition>,
}

#[derive(Debug, Clone)]
enum Condition {
    Age {
        begin: Option<Age>,
        end: Option<Age>,
    },
    Interval(Age),
    Count {
        types: Vec<String>,
        begin: Option<Age>,
        end: Option<Age>,
        start: Option<Day>,
        end_date: Option<Day>,
        count: usize,
        logic: Ordering,
    },
    /// One medrec does not read, such as a completed series: it keeps the dose.
    Other,
}

/// The text at `path`, read by `f`: `Some(None)` where it is absent or
/// empty, `None` where it does not read.
fn opt<T>(e: &Element, path: &str, f: impl Fn(&str) -> Option<T>) -> Option<Option<T>> {
    match e.text_at(path) {
        None => Some(None),
        Some(t) => f(t).map(Some),
    }
}

/// `YYYYMMDD`.
fn compact(s: &str) -> Option<Day> {
    if s.len() != 8 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    day(&format!("{}-{}-{}", &s[..4], &s[4..6], &s[6..]))
}

fn condition(c: &Element) -> Option<Condition> {
    let kind = c
        .text_at("conditionType")
        .unwrap_or_default()
        .to_ascii_lowercase();
    let begin = opt(c, "beginAge", Age::parse)?;
    let end = opt(c, "endAge", Age::parse)?;
    Some(match kind.as_str() {
        "age" => Condition::Age { begin, end },
        "interval" => match opt(c, "interval", Age::parse)? {
            Some(i) => Condition::Interval(i),
            None => Condition::Other,
        },
        k if k.starts_with("vaccine count by") => {
            let logic = match c.text_at("doseCountLogic").map(str::to_ascii_lowercase) {
                Some(l) if l == "greater than" => Ordering::Greater,
                Some(l) if l == "less than" => Ordering::Less,
                Some(l) if l == "equal to" || l == "equal" => Ordering::Equal,
                _ => return Some(Condition::Other),
            };
            let types = c.text_at("vaccineTypes").unwrap_or_default();
            Condition::Count {
                types: types
                    .split(';')
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                    .map(String::from)
                    .collect(),
                begin,
                end,
                start: opt(c, "startDate", compact)?,
                end_date: opt(c, "endDate", compact)?,
                count: c.text_at("doseCount")?.parse().ok()?,
                logic,
            }
        }
        _ => Condition::Other,
    })
}

fn dose(d: &Element) -> Option<Dose> {
    let ages: Vec<&Element> = d.all("age").collect();
    // The age in effect: the one with no cessation date, else the last.
    let age = ages
        .iter()
        .find(|a| a.text_at("cessationDate").is_none())
        .or(ages.last());
    let earliest = match age.and_then(|a| a.text_at("earliestRecAge")) {
        Some(t) => Some((Age::parse(t)?, t.to_string())),
        None => None,
    };
    let season = match d.child("seasonalRecommendation") {
        Some(s) => match (opt(s, "startDate", compact)?, opt(s, "endDate", compact)?) {
            (Some(a), Some(b)) => Some((a, b)),
            _ => None,
        },
        None => None,
    };
    let mut vaccines = Vec::new();
    for v in d.all("preferableVaccine") {
        let Some(cvx) = v.text_at("cvx") else {
            continue;
        };
        let begin = opt(v, "beginAge", Age::parse)?;
        vaccines.push((cvx.to_string(), begin, opt(v, "endAge", Age::parse)?));
    }
    let mut skips = Vec::new();
    for s in d.all("conditionalSkip") {
        if !matches!(s.text_at("context"), Some("Forecast" | "Both")) {
            continue;
        }
        let mut sets = Vec::new();
        for set in s.all("set") {
            sets.push(Set {
                all: set.text_at("conditionLogic") == Some("AND"),
                from: opt(set, "effectiveDate", compact)?,
                until: opt(set, "cessationDate", compact)?,
                conditions: set.all("condition").map(condition).collect::<Option<_>>()?,
            });
        }
        skips.push(Skip {
            all: s.text_at("setLogic") == Some("AND"),
            sets,
        });
    }
    Some(Dose {
        number: d.text_at("doseNumber")?.to_string(),
        earliest,
        recurring: d.text_at("recurringDose") == Some("Yes"),
        season,
        vaccines,
        skips,
    })
}

/// The day of the first date a document's text gives, as "October 9, 2026".
fn written_day(text: &str) -> Option<String> {
    // A document's runs can split "8" from ", 2026".
    let re = regex::Regex::new(r"\b([A-Z][a-z]+) (\d{1,2}) ?, (\d{4})\b").ok()?;
    let found = re.captures_iter(text).find_map(|c| {
        let m = MONTHS.iter().position(|m| *m == &c[1])? + 1;
        let d = format!("{}-{m:02}-{:02}", &c[3], c[2].parse::<u32>().ok()?);
        day(&d).map(|_| d)
    });
    found
}

impl Schedules {
    /// The archive's schedules, when every file of them reads.
    pub fn read(zip: &[u8]) -> Option<Schedules> {
        let entries = tables::entries(zip)?;
        let unpack = |e: &tables::Entry| (e.size <= FILE_MAX).then(|| tables::unpack(zip, e))?;
        let file = |e: &tables::Entry| xml::read(std::str::from_utf8(&unpack(e)?).ok()?);
        let main = entries
            .iter()
            .find(|e| e.name.ends_with("/XML/ScheduleSupportingData.xml"))?;
        let top = main.name.split('/').next()?.to_string();
        let version = top.strip_prefix("Version ")?.split_whitespace().next()?;
        if !version.chars().all(|c| c.is_ascii_digit() || c == '.') || version.is_empty() {
            return None;
        }
        let schedule = file(main)?;
        let mut cvx = BTreeMap::new();
        for m in schedule.child("cvxToAntigenMap")?.all("cvxMap") {
            let code: u32 = m.text_at("cvx")?.parse().ok()?;
            let mut of = Vec::new();
            for a in m.all("association") {
                let begin = opt(a, "associationBeginAge", Age::parse)?;
                let end = opt(a, "associationEndAge", Age::parse)?;
                of.push((a.text_at("antigen")?.to_string(), begin, end));
            }
            cvx.insert(code, of);
        }
        let mut groups = BTreeMap::new();
        for g in schedule
            .child("vaccineGroupToAntigenMap")?
            .all("vaccineGroupMap")
        {
            let name = g.text_at("name")?.trim().to_string();
            for a in g.all("antigen").filter(|a| !a.text.is_empty()) {
                groups.insert(a.text.clone(), name.clone());
            }
        }
        let prefix = format!("{top}/XML/AntigenSupportingData");
        let files: Vec<_> = entries
            .iter()
            .filter(|e| e.name.starts_with(&prefix) && e.name.ends_with(".xml"))
            .collect();
        if files.is_empty() || cvx.is_empty() {
            return None;
        }
        let (mut series, mut routine) = (0, Vec::new());
        for f in &files {
            let root = file(f)?;
            if root.name != "antigenSupportingData" {
                return None;
            }
            for s in root.all("series") {
                series += 1;
                let doses = s.all("seriesDose").map(dose).collect::<Option<Vec<_>>>()?;
                let standard = s.text_at("seriesType") == Some("Standard");
                if !standard || s.text_at("selectSeries/defaultSeries") != Some("Yes") {
                    continue;
                }
                let antigen = s.text_at("targetDisease")?.to_string();
                let group = s.text_at("vaccineGroup")?.trim().to_string();
                groups
                    .entry(antigen.clone())
                    .or_insert_with(|| group.clone());
                routine.push(Series {
                    antigen,
                    name: s.text_at("seriesName")?.to_string(),
                    group,
                    doses,
                });
            }
        }
        let notes = entries
            .iter()
            .find(|e| e.name == format!("{top}/Release Notes.docx"))
            .and_then(|e| {
                let docx = unpack(e)?;
                let doc = tables::entries(&docx)?
                    .into_iter()
                    .find(|e| e.name == "word/document.xml")?;
                let body = String::from_utf8(tables::unpack(&docx, &doc)?).ok()?;
                let text = regex::Regex::new(r"<[^>]*>").ok()?.replace_all(&body, " ");
                written_day(&text.split_whitespace().collect::<Vec<_>>().join(" "))
            });
        let newest = files
            .iter()
            .chain([&main])
            .filter_map(|e| e.day.clone())
            .max();
        Some(Schedules {
            version: version.to_string(),
            date: notes.or(newest)?,
            series,
            routine,
            cvx,
            groups,
        })
    }

    /// The schedules of `t` on this machine, when it may be used.
    pub fn here(t: &Table) -> Option<Schedules> {
        Schedules::read(&t.archive(&tables::dir()?)?)
    }
}

/// One dose of a routine series, as it falls for a person.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Planned {
    /// CDC's number of it.
    number: String,
    age: Option<String>,
    day: Option<Day>,
    recurring: bool,
    season: Option<(Day, Day)>,
}

fn between(birth: Day, begin: Option<Age>, end: Option<Age>, d: Day) -> bool {
    let from = begin.and_then(|a| a.after(birth)).unwrap_or(Day::MIN);
    let until = end.and_then(|a| a.after(birth)).unwrap_or(Day::MAX);
    from <= d && d < until
}

/// Whether `c` is met on `reference`, the doses before having been given on
/// the days in `given`, each with the CVX codes it may be (Tables 6-6 to
/// 6-9); none where medrec does not read it.
fn met(c: &Condition, birth: Day, reference: Day, given: &[(Day, Vec<String>)]) -> Option<bool> {
    Some(match c {
        Condition::Age { begin, end } => between(birth, *begin, *end, reference),
        Condition::Interval(i) => match given.last() {
            Some((d, _)) => reference >= i.after(*d)?,
            None => false,
        },
        Condition::Count {
            types,
            begin,
            end,
            start,
            end_date,
            count,
            logic,
        } => {
            let n = given
                .iter()
                .filter(|(d, cvx)| {
                    (types.is_empty() || cvx.iter().any(|c| types.contains(c)))
                        && between(birth, *begin, *end, *d)
                        && start.is_none_or(|s| s <= *d)
                        && end_date.is_none_or(|e| *d < e)
                })
                .count();
            n.cmp(count) == *logic
        }
        Condition::Other => return None,
    })
}

/// Whether CDC's skips of a forecast skip `dose` on `reference` (Tables 6-10
/// and 6-11). A set out of effect that day counts for nothing; a condition
/// medrec does not read keeps the dose.
fn skipped(dose: &Dose, birth: Day, reference: Day, given: &[(Day, Vec<String>)]) -> bool {
    for skip in &dose.skips {
        let mut sets = Vec::new();
        for set in &skip.sets {
            let effective =
                set.from.is_none_or(|f| f <= reference) && set.until.is_none_or(|u| reference <= u);
            if !effective {
                continue;
            }
            let met: Option<Vec<bool>> = set
                .conditions
                .iter()
                .map(|c| met(c, birth, reference, given))
                .collect();
            let Some(met) = met.filter(|m| !m.is_empty()) else {
                return false;
            };
            sets.push(match set.all {
                true => met.iter().all(|m| *m),
                false => met.iter().any(|m| *m),
            });
        }
        if sets.is_empty() {
            continue;
        }
        return match skip.all {
            true => sets.iter().all(|s| *s),
            false => sets.iter().any(|s| *s),
        };
    }
    false
}

/// The doses of `s` a person born on `birth` is given on the routine path.
fn routine(s: &Series, birth: Day) -> Vec<Planned> {
    let mut given: Vec<(Day, Vec<String>)> = Vec::new();
    let mut out = Vec::new();
    for dose in &s.doses {
        let day = dose.earliest.as_ref().and_then(|(a, _)| a.after(birth));
        // A dose of no age of its own comes after the one before it.
        let reference = day.or(given.last().map(|g| g.0)).unwrap_or(birth);
        if skipped(dose, birth, reference, &given) {
            continue;
        }
        let cvx = dose
            .vaccines
            .iter()
            .filter(|(_, b, e)| between(birth, *b, *e, reference))
            .map(|(c, _, _)| c.clone())
            .collect();
        given.push((reference, cvx));
        out.push(Planned {
            number: dose.number.clone(),
            age: dose.earliest.as_ref().map(|(_, t)| t.clone()),
            day,
            recurring: dose.recurring,
            season: dose.season,
        });
    }
    out
}

/// A dose on file: an immunization, or a vaccine a claim tells of.
#[derive(Debug, Clone)]
pub struct OnFile {
    pub id: String,
    pub date: Option<String>,
    pub cvx: Vec<u32>,
    pub display: String,
    pub from_claim: bool,
}

fn cvx_number(c: &str) -> Option<u32> {
    c.trim().parse().ok()
}

impl OnFile {
    /// Of an immunization or a vaccine from a claim; none of one entered in
    /// error or not given.
    pub fn of(r: &Record) -> Option<OnFile> {
        if r.kind != "immunization"
            || crate::standing::record_in_error(r)
            || r.status
                .as_deref()
                .is_some_and(|s| s.eq_ignore_ascii_case("not-done"))
        {
            return None;
        }
        let from_claim = r.fields.get("from_claim").and_then(Value::as_bool) == Some(true);
        let mut cvx = BTreeSet::new();
        if let Some(Code {
            system: Some(system),
            code: Some(code),
            ..
        }) = &r.code
        {
            if system == crate::normalize::CVX {
                cvx.extend(cvx_number(code));
            }
        }
        for c in r
            .fields
            .get("cvx")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            cvx.extend(c.as_str().and_then(cvx_number));
        }
        Some(OnFile {
            id: r.id.clone(),
            date: r.date.as_ref().and_then(|d| d.get(..10)).map(String::from),
            cvx: cvx.into_iter().collect(),
            display: r.display.clone(),
            from_claim,
        })
    }

    fn json(&self) -> Value {
        let mut m = Map::new();
        m.insert("id".into(), self.id.clone().into());
        m.insert("date".into(), self.date.clone().into());
        m.insert("cvx".into(), json!(self.cvx));
        m.insert("display".into(), self.display.clone().into());
        if self.from_claim {
            m.insert("from_claim".into(), true.into());
        }
        Value::Object(m)
    }
}

/// One line of a group: a list of doses, the antigens and series it is the
/// routine of, and the tables that give it.
struct Line<'a> {
    antigens: BTreeSet<String>,
    names: Vec<String>,
    doses: Vec<Planned>,
    sources: Vec<&'a Table>,
}

fn source_json(t: &Table, s: &Schedules) -> Value {
    json!({"id": t.id, "version": s.version, "date": s.date})
}

/// A person's schedule: `SCHEMA`'s document. `earliest` is the day of their
/// earliest record of any kind; a dose before it is `before_records`.
pub fn of_person(
    person: &str,
    birth: Option<&str>,
    tables: &[(&Table, &Schedules)],
    on_file: &[OnFile],
    earliest: Option<&str>,
) -> Value {
    let sources: Vec<Value> = tables
        .iter()
        .map(|(t, s)| {
            json!({"id": t.id, "title": t.title, "publisher": t.publisher, "version": s.version,
                "date": s.date, "url": t.url})
        })
        .collect();
    let Some(birth) = birth.and_then(day) else {
        return json!({"schema": SCHEMA, "person": person, "birth_date": null,
            "sources": sources, "groups": [], "not_placed": []});
    };
    let earliest = earliest.and_then(day);
    let mut groups: BTreeMap<String, Vec<Line>> = BTreeMap::new();
    for (t, s) in tables {
        for series in &s.routine {
            let doses = routine(series, birth);
            if doses.is_empty() {
                continue;
            }
            let lines = groups.entry(series.group.clone()).or_default();
            match lines.iter_mut().find(|l| l.doses == doses) {
                Some(l) => {
                    l.antigens.insert(series.antigen.clone());
                    if !l.names.contains(&series.name) {
                        l.names.push(series.name.clone());
                    }
                    if !l.sources.iter().any(|x| x.id == t.id) {
                        l.sources.push(t);
                    }
                }
                None => lines.push(Line {
                    antigens: [series.antigen.clone()].into(),
                    names: vec![series.name.clone()],
                    doses,
                    sources: vec![t],
                }),
            }
        }
    }
    // The doses on file, under each group their CVX codes count for.
    let mut filed: BTreeMap<String, Vec<&OnFile>> = BTreeMap::new();
    let mut not_placed = Vec::new();
    for f in on_file {
        let on = f.date.as_deref().and_then(day);
        let mut placed = BTreeSet::new();
        for (_, s) in tables {
            for code in &f.cvx {
                for (antigen, b, e) in s.cvx.get(code).into_iter().flatten() {
                    if on.is_none_or(|d| between(birth, *b, *e, d)) {
                        placed.extend(s.groups.get(antigen).cloned());
                    }
                }
            }
        }
        if placed.is_empty() {
            let mut v = f.json();
            let why = match f.cvx.is_empty() {
                true => "no CVX code",
                false => "a CVX code the schedule does not place",
            };
            v["why"] = why.into();
            not_placed.push(v);
        }
        for g in placed {
            filed.entry(g).or_default().push(f);
        }
    }
    let by = |s: &(&Table, &Schedules)| s.0.id;
    let schedules: BTreeMap<&str, &Schedules> = tables.iter().map(|s| (by(s), s.1)).collect();
    let mut out: Vec<(Option<Day>, String, Value)> = Vec::new();
    let names: BTreeSet<String> = groups.keys().chain(filed.keys()).cloned().collect();
    for name in names {
        let mut lines = groups.remove(&name).unwrap_or_default();
        lines.sort_by_key(|l| (l.doses.iter().find_map(|d| d.day), l.names.clone()));
        let first = lines
            .iter()
            .flat_map(|l| &l.doses)
            .filter_map(|d| d.day)
            .min();
        let antigens: BTreeSet<&String> = lines.iter().flat_map(|l| &l.antigens).collect();
        let lines: Vec<Value> = lines
            .iter()
            .map(|l| {
                let sources: Vec<Value> = l
                    .sources
                    .iter()
                    .map(|t| source_json(t, schedules[t.id]))
                    .collect();
                // A dose of no age of its own falls after the one before it.
                let mut last = None;
                let doses: Vec<Value> = l
                    .doses
                    .iter()
                    .enumerate()
                    .map(|(i, d)| {
                        let on = d.day.or(last);
                        last = on;
                        let mut m = Map::new();
                        m.insert("dose".into(), (i + 1).into());
                        m.insert("series_dose".into(), d.number.clone().into());
                        m.insert("age".into(), d.age.clone().into());
                        m.insert("date".into(), d.day.map(iso).into());
                        if d.day.is_none() && i > 0 {
                            m.insert("after".into(), i.into());
                        }
                        let before = matches!((on, earliest), (Some(d), Some(e)) if d < e);
                        m.insert("before_records".into(), before.into());
                        if d.recurring {
                            m.insert("recurring".into(), true.into());
                        }
                        if let Some((a, b)) = d.season {
                            m.insert("season".into(), json!({"from": iso(a), "to": iso(b)}));
                        }
                        m.insert("sources".into(), json!(sources));
                        Value::Object(m)
                    })
                    .collect();
                json!({"antigens": l.antigens, "series": l.names, "doses": doses})
            })
            .collect();
        let mut on: Vec<&&OnFile> = filed.get(&name).into_iter().flatten().collect();
        on.sort_by(|a, b| (&a.date, &a.id).cmp(&(&b.date, &b.id)));
        let v = json!({"group": name, "antigens": antigens, "lines": lines,
            "on_file": on.iter().map(|f| f.json()).collect::<Vec<_>>()});
        out.push((first, name, v));
    }
    out.sort_by(|a, b| {
        let key = |x: &(Option<Day>, String, Value)| (x.0.is_none(), x.0, x.1.clone());
        key(a).cmp(&key(b))
    });
    json!({"schema": SCHEMA, "person": person, "birth_date": iso(birth),
        "sources": sources, "groups": out.into_iter().map(|x| x.2).collect::<Vec<_>>(),
        "not_placed": not_placed})
}

/// A person's schedule from their index, by the schedules in `codes`. With
/// `private` (`private.json`), as in the views: no dose on file that is
/// private, nor a vaccine of a claim line it holds back, and the earliest
/// record of those that are open.
pub fn of_index(
    p: &crate::model::Person,
    idx: &crate::model::PersonIndex,
    codes: &crate::vaccines::Codes,
    private: Option<&Value>,
) -> Value {
    let is_private = |kind: &str, id: &str| private.is_some_and(|v| v[kind].get(id).is_some());
    let held = private.map(crate::parts::Parts::of).unwrap_or_default();
    let all: Vec<&Record> = idx.records.iter().collect();
    let claimed = crate::vaccines::of_claims(&all, codes, &held);
    let on_file: Vec<OnFile> = idx
        .records
        .iter()
        .chain(&claimed)
        .filter(|r| !is_private("records", &r.id))
        .filter_map(OnFile::of)
        .collect();
    let records = idx
        .records
        .iter()
        .filter(|r| !is_private("records", &r.id))
        .filter_map(|r| r.date.as_deref());
    let documents = idx
        .documents
        .iter()
        .filter(|d| !is_private("documents", &d.id))
        .filter_map(|d| d.date.as_deref());
    let earliest = records
        .chain(documents)
        .filter_map(|d| d.get(..10).filter(|d| day(d).is_some()))
        .min();
    of_person(
        &p.id,
        p.birth_date.as_deref(),
        &codes.schedules(),
        &on_file,
        earliest,
    )
}

/// The words of one table beside a dose: "CDC CDSi 4.66, Oct 9, 2026".
fn said_by(s: &Value) -> String {
    let id = s["id"].as_str().unwrap_or_default();
    let name = match id {
        "cdsi" => "CDC CDSi",
        other => other,
    };
    let date = s["date"].as_str().map(us_day).unwrap_or_default();
    format!(
        "{name} {}, {date}",
        s["version"].as_str().unwrap_or_default()
    )
}

fn sources_said(d: &Value) -> String {
    let all: Vec<String> = d["sources"]
        .as_array()
        .into_iter()
        .flatten()
        .map(said_by)
        .collect();
    all.join("; ")
}

/// A group's heading: "MMR (Measles, Mumps, Rubella)".
fn heading(g: &Value) -> String {
    let antigens: Vec<&str> = g["antigens"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let group = g["group"].as_str().unwrap_or_default();
    match antigens.as_slice() {
        [] => group.to_string(),
        [one] if *one == group => group.to_string(),
        _ => format!("{group} ({})", antigens.join(", ")),
    }
}

/// A group's lines: of each of its series, the doses before the person's
/// records begin folded into one, then each other dose with its table; then
/// the doses on file. A group of more than one series names each.
fn group_lines(g: &Value) -> Vec<String> {
    let mut out = Vec::new();
    let lines = g["lines"].as_array().cloned().unwrap_or_default();
    for l in &lines {
        let label = (lines.len() > 1).then(|| {
            let names: Vec<&str> = l["series"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            format!("{}: ", names.join("; "))
        });
        let label = label.unwrap_or_default();
        let doses = l["doses"].as_array().cloned().unwrap_or_default();
        let before: Vec<&Value> = doses
            .iter()
            .filter(|d| d["before_records"] == true)
            .collect();
        if !before.is_empty() {
            let ages: Vec<String> = before
                .iter()
                .map(|d| match (d["age"].as_str(), d["after"].as_u64()) {
                    (Some(age), _) => format!("dose {} at {age}", d["dose"]),
                    (None, Some(n)) => format!("dose {} after dose {n}", d["dose"]),
                    (None, None) => format!("dose {}", d["dose"]),
                })
                .collect();
            out.push(format!(
                "{label}Before these records begin: {} ({})",
                ages.join(", "),
                sources_said(before[0])
            ));
        }
        for d in doses.iter().filter(|d| d["before_records"] != true) {
            let mut s = format!("{label}Dose {}: ", d["dose"]);
            match (d["age"].as_str(), d["date"].as_str()) {
                (Some(a), Some(day)) => s.push_str(&format!("recommended at {a}: {}", us_day(day))),
                _ => match d["after"].as_u64() {
                    Some(n) => s.push_str(&format!("after dose {n}, at no age of its own")),
                    None => s.push_str("at no age of its own"),
                },
            }
            if d["recurring"] == true {
                s.push_str(", then again");
            }
            if let Some(season) = d["season"].as_object() {
                let day = |k: &str| season[k].as_str().map(us_day).unwrap_or_default();
                s.push_str(&format!(", in the season {} to {}", day("from"), day("to")));
            }
            s.push_str(&format!(" ({})", sources_said(d)));
            out.push(s);
        }
    }
    for f in g["on_file"].as_array().into_iter().flatten() {
        out.push(format!("On file: {}", on_file_said(f)));
    }
    out
}

/// The schedule's opening line.
fn opening(v: &Value, name: &str) -> Option<String> {
    let birth = v["birth_date"].as_str()?;
    let sources: Vec<String> = v["sources"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|s| {
            format!(
                "{} ({})",
                said_by(s),
                s["title"].as_str().unwrap_or_default()
            )
        })
        .collect();
    Some(format!(
        "{name}, born {}: the recommended doses by age, from {}. The doses on file are listed \
         beside them; no dose is matched to one.",
        us_day(birth),
        sources.join("; ")
    ))
}

/// Each group of `of_person`'s document as a row: its heading, the date of
/// its first dose after the person's records begin, and its lines joined.
pub fn rows(v: &Value) -> Vec<(String, Option<String>, String)> {
    let mut out = Vec::new();
    for g in v["groups"].as_array().into_iter().flatten() {
        let lines = g["lines"].as_array().into_iter().flatten();
        let first = lines
            .flat_map(|l| l["doses"].as_array().into_iter().flatten())
            .filter(|d| d["before_records"] != true)
            .find_map(|d| d["date"].as_str().map(String::from));
        out.push((heading(g), first, group_lines(g).join("; ")));
    }
    let not_placed: Vec<String> = v["not_placed"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|f| {
            format!(
                "{} ({})",
                on_file_said(f),
                f["why"].as_str().unwrap_or_default()
            )
        })
        .collect();
    if !not_placed.is_empty() {
        out.push(("On file, in no group".into(), None, not_placed.join("; ")));
    }
    out
}

/// `of_person`'s document as lines of text: each group's heading, then its
/// lines.
pub fn text(v: &Value, name: &str) -> String {
    let Some(open) = opening(v, name) else {
        return format!("{name}: no birth date on file, so no schedule\n");
    };
    let mut out = format!("{open}\n");
    for g in v["groups"].as_array().into_iter().flatten() {
        out.push_str(&format!("\n{}\n", heading(g)));
        for l in group_lines(g) {
            out.push_str(&format!("  {l}\n"));
        }
    }
    let not_placed = v["not_placed"].as_array().cloned().unwrap_or_default();
    if !not_placed.is_empty() {
        out.push_str("\nOn file, in no group\n");
        for f in &not_placed {
            let why = f["why"].as_str().unwrap_or_default();
            out.push_str(&format!("  {} ({why})\n", on_file_said(f)));
        }
    }
    out
}

fn on_file_said(f: &Value) -> String {
    let day = f["date"]
        .as_str()
        .map(us_day)
        .unwrap_or_else(|| "no date".into());
    let codes: Vec<String> = f["cvx"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|c| format!("CVX {c}"))
        .collect();
    let mut s = format!("{day}, {}", f["display"].as_str().unwrap_or_default());
    if !codes.is_empty() {
        s.push_str(&format!(" ({})", codes.join(", ")));
    }
    if f["from_claim"] == true {
        s.push_str(", from a claim");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> Day {
        day(s).unwrap()
    }

    fn after(from: &str, age: &str) -> String {
        iso(Age::parse(age).unwrap().after(d(from)).unwrap())
    }

    #[test]
    fn ages_are_added_by_cdcs_date_rules() {
        // Logic Specification 4.6, Table 3-6.
        assert_eq!(after("2000-01-01", "3 years"), "2003-01-01");
        assert_eq!(after("2000-01-01", "6 months"), "2000-07-01");
        assert_eq!(after("2000-11-01", "6 months"), "2001-05-01");
        assert_eq!(after("2000-01-01", "3 days"), "2000-01-04");
        assert_eq!(after("2000-01-01", "3 weeks"), "2000-01-22");
        assert_eq!(after("2000-02-01", "5 weeks"), "2000-03-07");
        assert_eq!(after("2001-02-01", "5 weeks"), "2001-03-08");
        assert_eq!(after("2000-03-31", "6 months"), "2000-10-01");
        assert_eq!(after("2000-08-31", "6 months"), "2001-03-01");
        assert_eq!(after("2000-01-31", "6 months - 4 days"), "2000-07-27");
        assert_eq!(after("2000-02-29", "1 year"), "2001-03-01");
        assert_eq!(after("2026-03-10", "16 months + 4 weeks"), "2027-08-07");
        assert_eq!(after("2026-03-10", "0 days"), "2026-03-10");
        for bad in [
            "",
            "12",
            "12 moons",
            "12 months -",
            "12 months 4 days",
            "- 4 days",
        ] {
            assert_eq!(Age::parse(bad), None, "{bad}");
        }
    }

    fn invented() -> Schedules {
        Schedules::read(&crate::testdata::cdsi_archive()).unwrap()
    }

    /// A dose's number, CDC's number of it, its age and its date.
    fn doses(line: &Value) -> Vec<(u64, String, Option<String>, Option<String>)> {
        let all = line["doses"].as_array().unwrap();
        all.iter()
            .map(|d| {
                let s = |k: &str| d[k].as_str().map(String::from);
                (
                    d["dose"].as_u64().unwrap(),
                    s("series_dose").unwrap(),
                    s("age"),
                    s("date"),
                )
            })
            .collect()
    }

    fn on(n: &str, date: &str, cvx: &[u32], from_claim: bool) -> OnFile {
        OnFile {
            id: format!("r_{n}{}", "a".repeat(20))[..22].to_string(),
            date: (!date.is_empty()).then(|| date.to_string()),
            cvx: cvx.to_vec(),
            display: format!("Sample vaccine {n}"),
            from_claim,
        }
    }

    const BIRTH: &str = "2027-01-15";

    #[test]
    fn an_archive_is_read_by_its_folder_files_and_release_notes() {
        let s = invented();
        assert_eq!((s.version.as_str(), s.date.as_str()), ("0.1", "2026-09-08"));
        // A series that is not the default, and one of a risk, are no routine.
        assert_eq!((s.series, s.routine.len()), (6, 4));
        assert_eq!(s.groups["Samplepox B"], "Samplepox");
        assert_eq!(s.cvx[&952].len(), 2);
        // Without its release notes, its newest file's day.
        let mut z = crate::testdata::cdsi_archive();
        let notes = b"Release Notes.docx";
        while let Some(at) = z.windows(notes.len()).position(|w| w == notes) {
            z[at] = b'X';
        }
        assert_eq!(Schedules::read(&z).unwrap().date, "2026-09-01");
        // An archive with no schedule file, or one that does not read, is none.
        assert!(Schedules::read(b"PK no zip").is_none());
        let broken = crate::testdata::zip_of(&[(
            "Version 0.1 - 508/XML/ScheduleSupportingData.xml",
            b"<scheduleSupportingData>",
            (2026, 9, 1),
        )]);
        assert!(Schedules::read(&broken).is_none());
    }

    #[test]
    fn the_routine_path_keeps_the_doses_cdcs_skips_keep() {
        let s = invented();
        let v = of_person("p_x", Some(BIRTH), &[(&CDSI, &s)], &[], None);
        let groups = v["groups"].as_array().unwrap();
        let names: Vec<&str> = groups
            .iter()
            .map(|g| g["group"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["Examplitis", "Samplepox"]);
        let ex = &groups[0]["lines"][0];
        let o = |s: &str| Some(s.to_string());
        assert_eq!(
            doses(ex),
            vec![
                (1, "Dose 1".into(), o("2 months"), o("2027-03-15")),
                // A skip of a condition medrec does not read keeps it.
                (2, "Dose 2".into(), o("4 months"), o("2027-05-15")),
                // A skip of evaluation alone keeps it.
                (3, "Dose 3".into(), o("12 months - 4 days"), o("2028-01-11")),
                // Dose 4 is skipped: more than two given. The age in effect,
                // and a set out of effect skips nothing.
                (4, "Dose 5".into(), o("4 years"), o("2031-01-15")),
                (5, "Dose 6".into(), None, None),
                (6, "Dose 7".into(), o("11 years"), o("2038-01-15")),
            ]
        );
        assert_eq!(ex["doses"][4]["after"], 4);
        assert_eq!(ex["doses"][5]["recurring"], true);
        assert!(ex["doses"][0].get("recurring").is_none());
        assert_eq!(
            ex["doses"][0]["sources"],
            json!([{"id": "cdsi", "version": "0.1", "date": "2026-09-08"}])
        );
        assert_eq!(v["sources"][0]["title"], CDSI.title);
    }

    #[test]
    fn antigens_alike_share_a_line_and_a_season_is_given() {
        let s = invented();
        let v = of_person("p_x", Some(BIRTH), &[(&CDSI, &s)], &[], None);
        let pox = &v["groups"][1];
        assert_eq!(
            pox["antigens"],
            json!(["Samplepox A", "Samplepox B", "Samplepox C"])
        );
        let lines = pox["lines"].as_array().unwrap();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["antigens"], json!(["Samplepox C"]));
        let seasonal = &lines[0]["doses"][0];
        assert_eq!(seasonal["date"], "2027-07-15");
        assert_eq!(
            seasonal["season"],
            json!({"from": "2027-08-01", "to": "2028-06-30"})
        );
        assert_eq!(lines[1]["antigens"], json!(["Samplepox A", "Samplepox B"]));
        assert_eq!(
            lines[1]["series"],
            json!(["Samplepox A 2-dose series", "Samplepox B 2-dose series"])
        );
        assert_eq!(doses(&lines[1]).len(), 2);
    }

    #[test]
    fn two_tables_are_each_named_and_both_shown_where_they_differ() {
        const OTHER: Table = Table {
            id: "other",
            title: "Another schedule",
            publisher: "Example Society",
            version: "1",
            date: "2026-01-01",
            url: "https://example.test/other.zip",
            sha256: "0",
            bytes: 0,
            form: tables::Form::Archive,
            rows: 0,
        };
        let s = invented();
        let v = of_person("p_x", Some(BIRTH), &[(&CDSI, &s), (&OTHER, &s)], &[], None);
        let ex = &v["groups"][0]["lines"];
        assert_eq!(ex.as_array().unwrap().len(), 1);
        let ids: Vec<&str> = ex[0]["doses"][0]["sources"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["cdsi", "other"]);
        let mut fewer = s.clone();
        for series in &mut fewer.routine {
            series.doses.truncate(1);
        }
        let v = of_person(
            "p_x",
            Some(BIRTH),
            &[(&CDSI, &s), (&OTHER, &fewer)],
            &[],
            None,
        );
        let ex = v["groups"][0]["lines"].as_array().unwrap();
        assert_eq!(ex.len(), 2);
        let only = |l: &Value| l["doses"][0]["sources"][0]["id"].clone();
        assert_eq!(
            (only(&ex[0]), ex[0]["doses"].as_array().unwrap().len()),
            (json!("cdsi"), 6)
        );
        assert_eq!(
            (only(&ex[1]), ex[1]["doses"].as_array().unwrap().len()),
            (json!("other"), 1)
        );
        assert_eq!(v["sources"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn the_doses_on_file_go_under_the_groups_their_codes_count_for() {
        let s = invented();
        let filed = [
            on("one", "2027-03-20", &[951], false),
            on("claim", "2028-01-20", &[952], true),
            on("young", "2028-02-01", &[954], false),
            // CVX 954 counts for Samplepox A under 2 years alone.
            on("old", "2032-02-01", &[954], false),
            on("unknown", "2028-02-01", &[999], false),
            on("none", "", &[], false),
        ];
        let v = of_person("p_x", Some(BIRTH), &[(&CDSI, &s)], &filed, None);
        let ids = |g: &Value| -> Vec<String> {
            let all = g["on_file"].as_array().unwrap();
            all.iter()
                .map(|f| f["display"].as_str().unwrap().to_string())
                .collect()
        };
        assert_eq!(ids(&v["groups"][0]), ["Sample vaccine one"]);
        assert_eq!(
            ids(&v["groups"][1]),
            ["Sample vaccine claim", "Sample vaccine young"]
        );
        assert_eq!(v["groups"][1]["on_file"][0]["from_claim"], true);
        assert!(v["groups"][0]["on_file"][0].get("from_claim").is_none());
        let why: Vec<(&str, &str)> = v["not_placed"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| (f["display"].as_str().unwrap(), f["why"].as_str().unwrap()))
            .collect();
        assert_eq!(
            why,
            [
                (
                    "Sample vaccine old",
                    "a CVX code the schedule does not place"
                ),
                (
                    "Sample vaccine unknown",
                    "a CVX code the schedule does not place"
                ),
                ("Sample vaccine none", "no CVX code"),
            ]
        );
        // No dose is matched to one on file, nor said to be due.
        let all = v.to_string();
        for word in ["due", "overdue", "missed", "matched"] {
            assert!(!all.contains(word), "{word}");
        }
    }

    #[test]
    fn a_dose_before_the_records_begin_is_marked_and_folded() {
        let s = invented();
        let v = of_person("p_x", Some(BIRTH), &[(&CDSI, &s)], &[], Some("2027-06-01"));
        let before: Vec<bool> = v["groups"][0]["lines"][0]["doses"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["before_records"].as_bool().unwrap())
            .collect();
        assert_eq!(before, [true, true, false, false, false, false]);
        let words = text(&v, "Robin");
        let lines: Vec<&str> = words.lines().collect();
        assert!(lines[0].starts_with("Robin, born Jan 15, 2027: the recommended doses by age, from CDC CDSi 0.1, Sep 8, 2026"), "{words}");
        let at = lines.iter().position(|l| *l == "Examplitis").unwrap();
        assert_eq!(
            &lines[at + 1..at + 6],
            [
                "  Before these records begin: dose 1 at 2 months, dose 2 at 4 months (CDC CDSi 0.1, Sep 8, 2026)",
                "  Dose 3: recommended at 12 months - 4 days: Jan 11, 2028 (CDC CDSi 0.1, Sep 8, 2026)",
                "  Dose 4: recommended at 4 years: Jan 15, 2031 (CDC CDSi 0.1, Sep 8, 2026)",
                "  Dose 5: after dose 4, at no age of its own (CDC CDSi 0.1, Sep 8, 2026)",
                "  Dose 6: recommended at 11 years: Jan 15, 2038, then again (CDC CDSi 0.1, Sep 8, 2026)",
            ]
        );
        assert!(words.contains("\nSamplepox (Samplepox A, Samplepox B, Samplepox C)\n"));
        assert!(words.contains("  Samplepox C seasonal series: Dose 1: recommended at 6 months: Jul 15, 2027, then again, in the season Aug 1, 2027 to Jun 30, 2028 (CDC CDSi 0.1, Sep 8, 2026)\n"), "{words}");
        // An adult's childhood doses fold whole, a dose of no age with the
        // one before it.
        let v = of_person(
            "p_y",
            Some("1980-05-31"),
            &[(&CDSI, &s)],
            &[],
            Some("2015-01-01"),
        );
        let adult = text(&v, "Alex");
        assert!(adult.contains("  Before these records begin: dose 1 at 2 months, dose 2 at 4 months, dose 3 at 12 months - 4 days, dose 4 at 4 years, dose 5 after dose 4, dose 6 at 11 years (CDC CDSi 0.1, Sep 8, 2026)\n"), "{adult}");
        assert!(!adult.contains("  Dose "), "{adult}");
        // Rows, as an answer gives them: a group's lines joined, and the
        // first date after the records begin.
        let rows = rows(&of_person(
            "p_x",
            Some(BIRTH),
            &[(&CDSI, &s)],
            &[],
            Some("2027-06-01"),
        ));
        assert_eq!(rows[0].0, "Examplitis");
        assert_eq!(rows[0].1.as_deref(), Some("2028-01-11"));
        assert!(rows[0]
            .2
            .starts_with("Before these records begin: dose 1 at 2 months"));
        // No birth date, no schedule.
        let v = of_person("p_z", None, &[(&CDSI, &s)], &[], None);
        assert_eq!(v["groups"], json!([]));
        assert_eq!(
            text(&v, "Sam"),
            "Sam: no birth date on file, so no schedule\n"
        );
    }

    #[test]
    fn a_private_dose_is_no_dose_on_file_and_sets_no_beginning() {
        let p: crate::model::Person =
            serde_json::from_value(json!({"id": "p_robinaaaaaaaaaaaaaaa",
            "name": "Robin Example", "folder": "Robin Example", "birth_date": BIRTH,
            "created_at": "2027-01-16T10:00:00Z"}))
            .unwrap();
        let immunization = |id: &str, date: &str, cvx: &str| -> Record {
            serde_json::from_value(json!({"id": id, "kind": "immunization", "key": id,
                "date": date, "display": "Sample vaccine", "status": "completed",
                "code": {"system": crate::normalize::CVX, "code": cvx},
                "fields": {}, "provenance": []}))
            .unwrap()
        };
        let mut idx = crate::model::PersonIndex::new(&p.id);
        idx.records = vec![
            immunization("r_openaaaaaaaaaaaaaaaa", "2027-06-01", "951"),
            immunization("r_heldaaaaaaaaaaaaaaaa", "2027-03-20", "952"),
        ];
        let codes = crate::vaccines::Codes::default().with_schedules(vec![(&CDSI, invented())]);
        let private = json!({"records": {"r_heldaaaaaaaaaaaaaaaa": {"area": "sample"}},
            "documents": {}, "parts": {}});
        let v = of_index(&p, &idx, &codes, Some(&private));
        let all = v.to_string();
        assert!(!all.contains("r_heldaaaaaaaaaaaaaaaa"), "{all}");
        assert_eq!(v["groups"][0]["on_file"][0]["id"], "r_openaaaaaaaaaaaaaaaa");
        // The earliest record is the open one's: dose 2 is not before it.
        let first = &v["groups"][0]["lines"][0]["doses"];
        assert_eq!(first[1]["before_records"], true);
        assert_eq!(first[2]["before_records"], false);
        let whole = of_index(&p, &idx, &codes, None);
        assert_eq!(
            whole["groups"][1]["on_file"][0]["id"],
            "r_heldaaaaaaaaaaaaaaaa"
        );
        let first = &whole["groups"][0]["lines"][0]["doses"];
        assert_eq!(first[1]["before_records"], false);
    }

    #[test]
    fn a_day_reads_as_people_write_it() {
        assert_eq!(us_day("2027-03-15"), "Mar 15, 2027");
        assert_eq!(us_day("2026-10-09"), "Oct 9, 2026");
        assert_eq!(
            written_day("Release Notes 4.0 October 9, 2026 Overview"),
            Some("2026-10-09".into())
        );
        assert_eq!(
            written_day("Released September 8 , 2026"),
            Some("2026-09-08".into())
        );
        assert_eq!(written_day("Smarch 9, 2026"), None);
        assert_eq!(written_day("February 30, 2026"), None);
    }
}
