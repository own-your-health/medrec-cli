//! What a paper's text says on the lines it labels: when to come back (a
//! follow-up), and the drugs it prescribes with their directions (0.9.12).
//! Read by rules alone, from the text intake kept, of the papers of care: not
//! a bill, a receipt, an explanation of benefits or an insurance card, whose
//! statement [`crate::bill`] reads.
//!
//! A line is read only where it starts with its label. A private line of the
//! text ([`crate::parts::of_text`]) is never read, and nothing is joined
//! across one. What a field says is then judged on its own by the rules of
//! what is private ([`crate::private`]'s parts): such a field's lines are
//! among its paper's private lines, so a reader that leaves those out leaves
//! the field out with them. The words of a follow-up and a drug's directions
//! are the only text kept, each a labeled field. A drug is listed under its
//! paper alone, "from a paper": never in a list of medicines, never with a
//! status, and one with the person's medicine only by an equal RxNorm code or
//! NDC ([`medicine`]).

use std::collections::BTreeSet;
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::{Document, PersonIndex, PersonText, Record};
use crate::normalize::{NDC, RXNORM};
use crate::parts::{self, Place};

/// The most of a follow-up's words, or of a drug's directions, kept, in
/// characters.
const WORDS: usize = 200;
/// The most of a drug's name kept.
const NAME: usize = 120;
/// How many lines after its drug's a line of its directions or codes can be.
const NEAR: usize = 3;

/// What a paper's labeled lines say.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fields {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub follow_up: Vec<FollowUp>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub drugs: Vec<Drug>,
}

impl Fields {
    pub fn is_empty(&self) -> bool {
        self.follow_up.is_empty() && self.drugs.is_empty()
    }

    /// Without those on any of these lines.
    pub fn without(&self, places: &[Place]) -> Fields {
        let keep = |lines: &[Place]| !left_out(lines, places);
        Fields {
            follow_up: self
                .follow_up
                .iter()
                .filter(|x| keep(&x.lines))
                .cloned()
                .collect(),
            drugs: self
                .drugs
                .iter()
                .filter(|x| keep(&x.lines))
                .cloned()
                .collect(),
        }
    }
}

/// How a follow-up's date was told.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Dated {
    /// The line prints it whole.
    Printed,
    /// A time after the paper's own date ("in 2 weeks"); of a range ("2 to 3
    /// weeks"), its earliest day.
    Relative,
    /// Printed without a year: its first day on or after the paper's date.
    Resolved,
}

/// When a paper says to come back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FollowUp {
    /// The line's words after its label, as printed.
    pub words: String,
    /// `YYYY-MM-DD`, where the words tell a day before any condition ("as
    /// needed", "if symptoms persist"); none where they tell none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dated: Option<Dated>,
    /// The places and clinicians the words name, as printed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub with: Vec<String>,
    /// Its lines' places in the text (`[start, length]` in bytes).
    pub lines: Vec<Place>,
}

/// A drug a paper prescribes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Drug {
    /// As printed.
    pub drug: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directions: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rxnorm: Option<String>,
    /// Eleven digits where its hyphens tell them, else as printed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ndc: Option<String>,
    pub lines: Vec<Place>,
}

/// Whether a paper of this type is read: a paper of care, or one of no type.
pub fn reads(doc_type: Option<&str>) -> bool {
    !matches!(
        doc_type,
        Some("bill" | "receipt" | "eob" | "insurance_card")
    )
}

/// Give a paper what its text says, where it is read.
pub fn annotate(doc: &mut Document, text: &str) {
    doc.fields = of(doc, Some(text));
}

/// What a paper's text says, where its type is read.
pub(crate) fn of(doc: &Document, text: Option<&str>) -> Option<Fields> {
    let text = text.filter(|_| reads(doc.doc_type.as_deref()))?;
    Some(read(text, doc.date.as_deref())).filter(|f| !f.is_empty())
}

/// Every paper read again from its stored text, as this medrec reads it.
/// Returns how many changed.
pub fn backfill(index: &mut PersonIndex, texts: &PersonText) -> usize {
    let mut changed = 0;
    for d in &mut index.documents {
        let now = of(d, texts.texts.get(&d.id).map(String::as_str));
        if d.fields != now {
            d.fields = now;
            changed += 1;
        }
    }
    changed
}

/// What a text's labeled lines say; relative days and days without a year
/// from `date`, the paper's own.
pub fn read(text: &str, date: Option<&str>) -> Fields {
    let lines = parts::lines(text);
    let held: BTreeSet<usize> = parts::of_text(text).iter().map(|p| p[0]).collect();
    let body = |p: &Place| unbulleted(&text[p[0]..p[0] + p[1]]);
    let readable = |i: usize| lines.get(i).filter(|p| !held.contains(&p[0]));
    // A label alone on its line heads its value on the next.
    let value = |i: usize, rest: &str| -> Option<(String, Vec<Place>)> {
        let rest = clean(rest);
        if !rest.is_empty() {
            return Some((rest, vec![lines[i]]));
        }
        let next = readable(i + 1)?;
        let n = body(next);
        (!n.is_empty() && !labelled(n)).then(|| (clean(n), vec![lines[i], *next]))
    };
    let mut out = Fields::default();
    // The drug the lines after it can give directions or codes of: its place
    // in `out.drugs`, and its line.
    let mut open: Option<(usize, usize)> = None;
    let mut i = 0;
    while i < lines.len() {
        if readable(i).is_none() {
            open = None;
            i += 1;
            continue;
        }
        let line = body(&lines[i]);
        let mut used = 1;
        if let Some(rest) = follow_up_label(line) {
            if let Some((words, at)) = value(i, rest) {
                used = at.len();
                out.follow_up.push(follow_up(&words, at, date));
            }
            open = None;
        } else if let Some(rest) = drug_label(line) {
            if let Some((name, at)) = value(i, rest).filter(|(n, _)| a_drug(n)) {
                used = at.len();
                let mut d = Drug {
                    drug: clip(&name, NAME),
                    directions: None,
                    rxnorm: None,
                    ndc: None,
                    lines: at,
                };
                codes(&name, &mut d);
                out.drugs.push(d);
                open = Some((out.drugs.len() - 1, i + used - 1));
            }
        } else if let Some((n, at)) = open.filter(|(_, at)| i - at <= NEAR) {
            let d = &mut out.drugs[n];
            if let Some(rest) = directions_label(line).filter(|_| d.directions.is_none()) {
                if let Some((words, places)) = value(i, rest) {
                    used = places.len();
                    codes(&words, d);
                    d.directions = Some(words);
                    d.lines.extend(places);
                    open = Some((n, at.max(i + used - 1)));
                }
            } else if CODE_LINE.is_match(line) && codes(line, d) {
                d.lines.push(lines[i]);
            }
        }
        i += used;
    }
    out
}

/// A line without the bullet or number of a list it is in.
fn unbulleted(line: &str) -> &str {
    static LIST: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^\s*(?:[0-9]{1,2}[.)]\s+|[-*•·◦▪>–]+\s*)*").unwrap());
    let at = LIST.find(line).map_or(0, |m| m.end());
    line[at..].trim()
}

/// Words as kept: without the punctuation that ends a label, on one line, cut
/// short.
fn clean(s: &str) -> String {
    let s = s.trim_start_matches(|c: char| c.is_whitespace() || ":-–—.".contains(c));
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    clip(&s, WORDS)
}

fn clip(s: &str, most: usize) -> String {
    match s.char_indices().nth(most) {
        Some((at, _)) => s[..at].trim_end().to_string(),
        None => s.to_string(),
    }
}

/// Whether a line starts with any label this reads.
fn labelled(line: &str) -> bool {
    follow_up_label(line).is_some()
        || drug_label(line).is_some()
        || directions_label(line).is_some()
        || CODE_LINE.is_match(line)
}

/// The rest of a follow-up's line: after "Follow up", "Return" or "Next
/// appointment". A "Return" is one marked as a visit, by "visit",
/// "appointment", a colon or a dash, or where the words after it tell when or
/// why; never a return to work, school or what one does.
fn follow_up_label(line: &str) -> Option<&str> {
    static FOLLOW: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)^(?:follow[ -]*up|f/u|next (?:appointment|appt\.?|visit))(?: (?:appointment|appt\.?|visit|care|instructions?))?(?:\s*[:–—-]|\s+|$)").unwrap()
    });
    static RETURN: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)^return(?: (visit|appointment|appt\.?))?(?:\s*[:–—-]|\s+|$)").unwrap()
    });
    static WHEN: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)^(?:to (?:clinic|the clinic|office|the office|see|this)|in|on|at|for|if|as|within|after|precautions|when|sooner|prn)\b").unwrap()
    });
    // A return to what one does; "school physical" or "regular checkup" is a
    // visit.
    static ACTIVITY: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)^(?:to\s+(?:work|school|class|duty|sports?|play|daycare|day\s*care|(?:normal|full|usual|regular)\b|(?:all\s+)?activit)|(?:work|school|class|duty|sports?|play|daycare|day\s*care)\b|(?:normal|full|usual|regular)\s+(?:activit|duty\b|work\b|school\b|diet\b|routine\b)|(?:all\s+)?activit)").unwrap()
    });
    static VISIT: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)^\S+\s+(?:physical|visit|appointment|appt|check|checkup|exam|clearance)\b")
            .unwrap()
    });
    if let Some(m) = FOLLOW.find(line) {
        return Some(&line[m.end()..]);
    }
    let c = RETURN.captures(line)?;
    let rest = &line[c.get(0)?.end()..];
    let said = rest.trim_start_matches(|c: char| c.is_whitespace() || ":-–—".contains(c));
    if ACTIVITY.is_match(said) && !VISIT.is_match(said) {
        return None;
    }
    let marked = c.get(1).is_some()
        || line[..c.get(0)?.end()]
            .trim_end()
            .ends_with([':', '-', '–', '—']);
    (marked || WHEN.is_match(rest.trim_start())).then_some(rest)
}

/// The rest of a drug's line: after "Rx", "Medication", "Medicine", "Drug",
/// "Prescribed" or "Prescription".
fn drug_label(line: &str) -> Option<&str> {
    static LABEL: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)^(?:rx|medication(?: name)?|medicine|drug(?: name)?|prescribed|prescription)\s*:|^rx\s+").unwrap()
    });
    LABEL.find(line).map(|m| &line[m.end()..])
}

/// Whether what follows a drug's label names one: words, not a number, a
/// count or a word that there is none.
fn a_drug(name: &str) -> bool {
    const NOT: &[&str] = &[
        "no", "number", "num", "date", "only", "count", "qty", "quantity", "none", "n", "na",
        "see", "nka", "nkda", "refill", "refills", "bin", "pcn", "group", "id",
    ];
    let first = name
        .split(|c: char| !c.is_alphanumeric())
        .next()
        .unwrap_or_default();
    let starts = name.chars().next();
    starts.is_some_and(|c| c.is_alphabetic()) && !NOT.contains(&first.to_ascii_lowercase().as_str())
}

/// The rest of a line of directions: after "Sig" or "Directions", or the
/// whole of one that starts "Take".
fn directions_label(line: &str) -> Option<&str> {
    static LABEL: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)^(?:sig\b\.?\s*:?|directions(?: for use)?\s*:)").unwrap()
    });
    static TAKE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^take\s+\S").unwrap());
    if let Some(m) = LABEL.find(line) {
        return Some(&line[m.end()..]);
    }
    TAKE.is_match(line).then_some(line)
}

static CODE_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^(?:ndc|rxnorm|rxcui)\b").unwrap());

/// Give a drug the codes a text of it prints. Whether it printed any.
fn codes(text: &str, d: &mut Drug) -> bool {
    static NDC_AT: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"(?i)\bndc\s*(?:#|:|no\.?)?\s*([0-9]{4,5}-[0-9]{3,4}-[0-9]{1,2}|[0-9]{10,11})\b",
        )
        .unwrap()
    });
    static RXCUI_AT: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)\b(?:rxnorm|rxcui)(?:\s*(?:#|:|code))*\s*:?\s*([0-9]{1,8})\b").unwrap()
    });
    let mut any = false;
    if let Some(c) = NDC_AT.captures(text) {
        d.ndc = d.ndc.take().or_else(|| crate::fills::ndc(&c[1]));
        any = true;
    }
    if let Some(c) = RXCUI_AT.captures(text) {
        d.rxnorm = d.rxnorm.take().or_else(|| Some(c[1].to_string()));
        any = true;
    }
    any
}

// ---------------------------------------------------------------- follow-ups

const MONTHS: &str = r"jan(?:uary)?|feb(?:ruary)?|mar(?:ch)?|apr(?:il)?|may|june?|july?|aug(?:ust)?|sept?(?:ember)?|oct(?:ober)?|nov(?:ember)?|dec(?:ember)?";
const COUNTS: &str = r"[0-9]{1,3}|one|two|three|four|five|six|seven|eight|nine|ten|eleven|twelve";

fn follow_up(words: &str, lines: Vec<Place>, date: Option<&str>) -> FollowUp {
    // What comes after a condition tells no day: "sooner if symptoms worsen".
    static CONDITION: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)\b(?:if|as needed|as necessary|prn|p\.r\.n|unless|should|when needed)\b")
            .unwrap()
    });
    let head = &words[..CONDITION.find(words).map_or(words.len(), |m| m.start())];
    let paper = date.and_then(civil);
    let (date, dated) = match day_in(head, paper) {
        Some((day, how)) => (Some(day), Some(how)),
        None => (None, None),
    };
    FollowUp {
        words: words.to_string(),
        date,
        dated,
        with: names(words),
        lines,
    }
}

type Civil = (i64, i64, i64);

fn civil(s: &str) -> Option<Civil> {
    let s = s.get(..10)?;
    let n = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    (s.as_bytes().get(4) == Some(&b'-')).then_some(())?;
    valid((n(0..4)?, n(5..7)?, n(8..10)?))
}

fn valid((y, m, d): Civil) -> Option<Civil> {
    let ok = (1900..=2200).contains(&y)
        && (1..=12).contains(&m)
        && d >= 1
        && d <= crate::refresh::time::days_in(y, m);
    ok.then_some((y, m, d))
}

fn iso((y, m, d): Civil) -> String {
    format!("{y:04}-{m:02}-{d:02}")
}

fn month(s: &str) -> Option<i64> {
    let s = s.to_ascii_lowercase();
    let names = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    names
        .iter()
        .position(|n| s.starts_with(n))
        .map(|i| i as i64 + 1)
}

fn count(s: &str) -> Option<i64> {
    let words = [
        "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten", "eleven",
        "twelve",
    ];
    let s = s.to_ascii_lowercase();
    s.parse()
        .ok()
        .or_else(|| words.iter().position(|w| *w == s).map(|i| i as i64 + 1))
        .filter(|n| *n > 0)
}

fn year(s: Option<regex::Match>) -> Option<Option<i64>> {
    match s {
        None => Some(None),
        Some(m) => {
            let y: i64 = m.as_str().parse().ok()?;
            Some(Some(if m.as_str().len() == 2 { 2000 + y } else { y }))
        }
    }
}

/// The first day the words tell, and how: printed whole, after the paper's
/// date, or printed without a year.
fn day_in(head: &str, paper: Option<Civil>) -> Option<(String, Dated)> {
    static ISO: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\b([0-9]{4})-([0-9]{1,2})-([0-9]{1,2})\b").unwrap());
    static US: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\b([0-9]{1,2})/([0-9]{1,2})(?:/([0-9]{4}|[0-9]{2}))?\b").unwrap()
    });
    static MONTH_DAY: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(&format!(
            r"(?i)\b({MONTHS})\.?\s+([0-9]{{1,2}})(?:st|nd|rd|th)?\b(?:,?\s*([0-9]{{4}})\b)?"
        ))
        .unwrap()
    });
    static DAY_MONTH: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(&format!(r"(?i)\b([0-9]{{1,2}})(?:st|nd|rd|th)?\s+(?:of\s+)?({MONTHS})\b\.?(?:,?\s*([0-9]{{4}})\b)?")).unwrap()
    });
    // A time from the paper's date only where it is one: at the start of
    // the words or after "in", "within" or "after", and not a time something
    // lasts or an age ("after 10 days of antibiotics", "6 month well visit").
    static AFTER: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(&format!(r"(?i)(?:^|\b(?:in|within|after)\s+)(?:(?:about|approximately|approx\.?|around|roughly|another|~)\s*)?(?:(a|an)\s+|({COUNTS})(?:\s*(?:-|–|to|or)\s*(?:{COUNTS}))?\s*)(days?|wks?|weeks?|mos?|months?|yrs?|years?)\b")).unwrap()
    });
    static LASTS: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)^[\s-]*(?:of|old|before|ago|prior|earlier|well|check|visit|exam|physical|course|supply|post|from now on)\b").unwrap()
    });
    // Each day told, by where it starts.
    let mut told: Vec<(usize, Option<(String, Dated)>)> = Vec::new();
    let whole = |at: Civil| Some((iso(valid(at)?), Dated::Printed));
    let after_paper = |m: i64, d: i64| -> Option<(String, Dated)> {
        let p = paper?;
        let this = valid((p.0, m, d)).filter(|c| *c >= p);
        let day = this.or_else(|| (1..=8).find_map(|k| valid((p.0 + k, m, d))))?;
        Some((iso(day), Dated::Resolved))
    };
    let dated = |y: Option<i64>, m: i64, d: i64| match y {
        Some(y) => whole((y, m, d)),
        None => after_paper(m, d),
    };
    for c in ISO.captures_iter(head) {
        let n = |i: usize| c[i].parse::<i64>().ok();
        told.push((c.get(0)?.start(), (|| whole((n(1)?, n(2)?, n(3)?)))()));
    }
    for c in US.captures_iter(head) {
        let n = |i: usize| c[i].parse::<i64>().ok();
        let at = c.get(0)?.start();
        let day = (|| dated(year(c.get(3))?, n(1)?, n(2)?))();
        told.push((at, day));
    }
    for (re, m_at, d_at) in [(&*MONTH_DAY, 1, 2), (&*DAY_MONTH, 2, 1)] {
        for c in re.captures_iter(head) {
            let at = c.get(0)?.start();
            let day = (|| dated(year(c.get(3))?, month(&c[m_at])?, c[d_at].parse().ok()?))();
            told.push((at, day));
        }
    }
    for c in AFTER.captures_iter(head) {
        let at = c.get(0)?.start();
        if LASTS.is_match(&head[c.get(0)?.end()..]) {
            continue;
        }
        let n = match (c.get(1), c.get(2)) {
            (Some(_), _) => Some(1),
            (None, Some(n)) => count(n.as_str()),
            (None, None) => None,
        };
        let unit = c[3].to_ascii_lowercase();
        let day = (|| {
            let (n, p) = (n?, paper?);
            let day = match unit.chars().next()? {
                'd' => add_days(p, n),
                'w' => add_days(p, 7 * n),
                'm' => add_months(p, n),
                _ => add_months(p, 12 * n),
            }?;
            Some((iso(day), Dated::Relative))
        })();
        told.push((at, day));
    }
    // A day printed whole, on or after the paper's own, is what the paper
    // says; else the first told. One before it is what the follow-up is of.
    told.sort_by_key(|(at, _)| *at);
    let from = paper.map(iso);
    let printed = told.iter().find_map(|(_, d)| {
        let after = |day: &String| from.as_ref().is_none_or(|p| day >= p);
        d.clone()
            .filter(|(day, how)| *how == Dated::Printed && after(day))
    });
    printed.or_else(|| told.into_iter().find_map(|(_, day)| day))
}

fn add_days(p: Civil, n: i64) -> Option<Civil> {
    let day = crate::events::day(&iso(p))? + n;
    civil(&crate::events::iso(day))
}

fn add_months((y, m, d): Civil, n: i64) -> Option<Civil> {
    let months = y * 12 + (m - 1) + n;
    let (y, m) = (months.div_euclid(12), months.rem_euclid(12) + 1);
    valid((y, m, d.min(crate::refresh::time::days_in(y, m))))
}

/// The places and clinicians words name after "with", "see" or "at", as
/// printed: words that start with a capital, up to a word of when or why. A
/// comma ends one and may start another ("with Dr. Pat Sample, Placeholder
/// Clinic"); a credential after it is no name ("Kim Placeholder, LCSW").
fn names(words: &str) -> Vec<String> {
    static AFTER: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(&format!(r"\b(?i:with|see|at)\s+({RUN})")).unwrap());
    let mut out = Vec::new();
    for c in AFTER.captures_iter(words) {
        if let Some(m) = c.get(1) {
            run_names(m.as_str(), &mut out);
        }
    }
    out
}

/// Every name words may print, wherever it stands, for the privacy judge,
/// which matches a name whole: each run of words that start with a capital,
/// cut at a word of when or why, a month and a comma, and every part of each
/// run of up to `PART` words that neither starts nor ends with a join.
pub(crate) fn spans(words: &str) -> Vec<String> {
    const PART: usize = 8;
    static WORD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\S+").unwrap());
    let mut runs: Vec<Vec<&str>> = vec![Vec::new()];
    for w in WORD.find_iter(words) {
        let raw = w
            .as_str()
            .trim_start_matches(|c: char| !c.is_alphanumeric() && c != '&');
        let word = raw.trim_end_matches([',', ';', ':', ')']);
        let b = bare(word);
        let capital = word.chars().next().is_some_and(char::is_uppercase);
        let stop = !INITIAL.is_match(word) && (STOP.contains(&b.as_str()) || MONTH.is_match(&b));
        if stop || !(capital || JOIN.contains(&b.as_str())) || word.is_empty() {
            runs.push(Vec::new());
            continue;
        }
        if let Some(run) = runs.last_mut() {
            run.push(word);
        }
        if raw.len() != word.len() {
            runs.push(Vec::new());
        }
    }
    let joins = |w: &&str| JOIN.contains(&bare(w).as_str());
    let mut out = Vec::new();
    for run in &runs {
        for i in 0..run.len() {
            for j in i + 1..=run.len().min(i + PART) {
                let part = &run[i..j];
                if part.first().is_some_and(joins) || part.last().is_some_and(joins) {
                    continue;
                }
                let name = part.join(" ");
                let title = matches!(bare(&name).as_str(), "dr" | "doctor");
                if !title && !out.contains(&name) {
                    out.push(name);
                }
            }
        }
    }
    out
}

const STOP: &[&str] = &[
    "in",
    "on",
    "at",
    "for",
    "within",
    "after",
    "if",
    "as",
    "to",
    "by",
    "next",
    "about",
    "around",
    "approximately",
    "approx",
    "your",
    "my",
    "our",
    "a",
    "an",
    "noon",
    "prn",
    "sooner",
    "or",
    "when",
    "before",
    "this",
    "that",
];
const JOIN: &[&str] = &["&", "and", "of", "the"];
static MONTH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!(r"(?i)^(?:{MONTHS})$")).unwrap());
static INITIAL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Z]\.,?$").unwrap());

fn bare(w: &str) -> String {
    w.trim_matches(|c: char| !c.is_alphanumeric() && c != '&')
        .to_ascii_lowercase()
}

/// Words that start with a capital, with the joins between them, and a comma
/// between two.
const RUN: &str = r"(?:[A-Z][\w'’.&-]*,?|&|and|of|the)(?:\s+(?:[A-Z][\w'’.&-]*,?|&|and|of|the))*";

/// The names in a run of words, into `out`: each to the first word of when or
/// why, or to a comma.
fn run_names(run: &str, out: &mut Vec<String>) {
    static WORD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\S+").unwrap());
    static CREDENTIAL: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^(?:[A-Z]{2,5}(?:-[A-Z]{1,2})?|Ph\.?D\.?|M\.D\.|D\.O\.|Psy\.?D\.?)[,;:.]?$")
            .unwrap()
    });
    let mut names: Vec<Vec<regex::Match>> = vec![Vec::new()];
    for w in WORD.find_iter(run) {
        let raw = w.as_str();
        let b = bare(raw);
        // An initial is never the article it spells.
        if !INITIAL.is_match(raw) && (STOP.contains(&b.as_str()) || MONTH.is_match(&b)) {
            break;
        }
        // A credential after a comma is the name's before it, and no name.
        let after_comma = names.len() > 1 && names.last().is_some_and(Vec::is_empty);
        if after_comma && CREDENTIAL.is_match(raw) {
            continue;
        }
        if let Some(last) = names.last_mut() {
            last.push(w);
        }
        if raw.ends_with([',', ';', ':']) {
            names.push(Vec::new());
        }
    }
    let joins = |w: &regex::Match| JOIN.contains(&bare(w.as_str()).as_str());
    for mut kept in names {
        while kept.first().is_some_and(joins) {
            kept.remove(0);
        }
        while kept.last().is_some_and(joins) {
            kept.pop();
        }
        let (Some(first), Some(last)) = (kept.first(), kept.last()) else {
            continue;
        };
        let mut name = run[first.start()..last.end()].trim_end_matches([',', ';', ':']);
        // A full stop ends a sentence, not an initial or a title.
        if name.ends_with('.') && last.as_str().trim_end_matches('.').chars().count() > 3 {
            name = &name[..name.len() - 1];
        }
        let title = matches!(bare(name).as_str(), "dr" | "doctor");
        let name = clip(name, NAME);
        if !title && !out.contains(&name) {
            out.push(name);
        }
    }
}

// ---------------------------------------------------------------- medicines

/// The person's medicine a paper's drug is, by an equal RxNorm code or NDC:
/// never by its name. One entered in error is none.
pub fn medicine<'a>(drug: &Drug, records: &'a [Record]) -> Option<&'a Record> {
    medicines(drug, records).next()
}

/// Every medicine of the person's a paper's drug is, as `medicine`.
pub fn medicines<'a>(drug: &Drug, records: &'a [Record]) -> impl Iterator<Item = &'a Record> {
    let ndc = |s: Option<&str>| s.and_then(crate::fills::ndc);
    let drug = Drug {
        lines: Vec::new(),
        ..drug.clone()
    };
    records
        .iter()
        .filter(|r| r.kind == "medication" && !crate::standing::record_in_error(r))
        .filter(move |r| {
            let code = r.code.as_ref();
            let system = code.and_then(|c| c.system.as_deref());
            let c = code.and_then(|c| c.code.as_deref()).map(str::trim);
            let rx = system == Some(RXNORM) && c.is_some() && drug.rxnorm.as_deref() == c;
            let package = r.fields.get("ndc").and_then(Value::as_str);
            let by_ndc = drug.ndc.is_some()
                && ((system == Some(NDC) && ndc(c) == drug.ndc) || ndc(package) == drug.ndc);
            rx || by_ndc
        })
}

/// A paper's fields as the views and `medrec documents` give them: each drug
/// with the id of the person's medicine of an equal code (`medicine`).
pub fn shown(f: &Fields, records: &[Record]) -> Value {
    let mut v = serde_json::to_value(f).unwrap_or(Value::Null);
    for (i, d) in f.drugs.iter().enumerate() {
        let at = format!("/drugs/{i}");
        if let (Some(r), Some(x)) = (medicine(d, records), v.pointer_mut(&at)) {
            x["medicine"] = r.id.clone().into();
        }
    }
    v
}

/// The diagnosis codes of an area a text prints, by ICD-10's shape.
pub(crate) fn code_area(text: &str) -> Option<&'static str> {
    static ICD10: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\b([A-TV-Z][0-9][0-9AB](?:\.[0-9A-TV-Z]{1,4})?)\b").unwrap());
    ICD10
        .captures_iter(text)
        .find_map(|c| crate::private::icd10_area(&c[1]))
}

/// Whether a field's lines are among a paper's private places.
pub fn left_out(lines: &[Place], places: &[Place]) -> bool {
    lines.iter().any(|l| places.contains(l))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DATE: Option<&str> = Some("2027-01-10");

    fn one(text: &str) -> FollowUp {
        let f = read(text, DATE);
        assert_eq!(f.follow_up.len(), 1, "{text}: {f:?}");
        f.follow_up[0].clone()
    }

    fn day(text: &str) -> (Option<String>, Option<Dated>) {
        let f = one(text);
        (f.date, f.dated)
    }

    fn on(d: &str, how: Dated) -> (Option<String>, Option<Dated>) {
        (Some(d.to_string()), Some(how))
    }

    #[test]
    fn a_follow_up_is_read_from_its_labeled_line_alone() {
        let f = one("Visit summary\nFollow up in 2 weeks.\nWe talked about sleep.");
        assert_eq!(f.words, "in 2 weeks.");
        assert_eq!(f.lines, vec![[14, 21]]);
        for text in [
            "Follow-up: 2 weeks",
            "FOLLOW UP IN 2 WEEKS",
            "f/u 2 wks",
            "- Return in 2 weeks",
            "1. Return to clinic in 2 weeks",
            "Next appointment: in 2 weeks",
            "Return visit: 2 weeks",
            "Return visit: regular checkup in 2 weeks",
            "Return visit: full physical in 2 weeks",
            "Return appointment: school physical in 2 weeks",
            "Return: workup results in 2 weeks",
            "Follow up in about 2 weeks",
            "Follow up in approximately 2 weeks",
            "Follow up within approx. 2 weeks",
        ] {
            assert_eq!(day(text), on("2027-01-24", Dated::Relative), "{text}");
        }
        for text in [
            "Return to work in 2 weeks",
            "Return: to work on 3/4/2027",
            "Return - to school Monday",
            "Return visit: to daycare in 2 days",
            "Return to normal activities in 1 week",
            "Return to full duty Monday",
            "Return this part with your payment",
            "Please follow the plan",
            "We will follow up",
            "Return address: 1 Placeholder Way",
        ] {
            assert!(read(text, DATE).follow_up.is_empty(), "{text}");
        }
    }

    #[test]
    fn a_label_alone_heads_the_words_on_the_next_line() {
        let f = one("Follow-up instructions:\nSee Dr. Pat Sample in 3 days\n");
        assert_eq!(f.words, "See Dr. Pat Sample in 3 days");
        assert_eq!(f.lines.len(), 2);
        assert_eq!(f.date.as_deref(), Some("2027-01-13"));
        assert_eq!(f.with, vec!["Dr. Pat Sample"]);
    }

    #[test]
    fn a_day_is_printed_relative_or_resolved() {
        assert_eq!(
            day("Follow up on 02/03/2027"),
            on("2027-02-03", Dated::Printed)
        );
        assert_eq!(day("Follow up on 2/3/27"), on("2027-02-03", Dated::Printed));
        assert_eq!(
            day("Next appointment: 2027-02-03"),
            on("2027-02-03", Dated::Printed)
        );
        assert_eq!(
            day("Follow up February 3, 2027"),
            on("2027-02-03", Dated::Printed)
        );
        assert_eq!(
            day("Follow up 3 Feb 2027"),
            on("2027-02-03", Dated::Printed)
        );
        // Without a year: its first day on or after the paper's.
        assert_eq!(day("Return on Feb 3"), on("2027-02-03", Dated::Resolved));
        assert_eq!(day("Return on 1/5"), on("2028-01-05", Dated::Resolved));
        assert_eq!(
            day("Return on January 10th"),
            on("2027-01-10", Dated::Resolved)
        );
        // Relative: days, weeks, months and years; a range's earliest day.
        assert_eq!(
            day("Follow up in 10 days"),
            on("2027-01-20", Dated::Relative)
        );
        assert_eq!(
            day("Follow up in 2-3 weeks"),
            on("2027-01-24", Dated::Relative)
        );
        assert_eq!(
            day("Follow up in two to three weeks"),
            on("2027-01-24", Dated::Relative)
        );
        assert_eq!(
            day("Follow up in a month"),
            on("2027-02-10", Dated::Relative)
        );
        assert_eq!(day("Return in 6 months"), on("2027-07-10", Dated::Relative));
        assert_eq!(day("Return in 1 year"), on("2028-01-10", Dated::Relative));
        // The first day the words tell; a day printed whole before any other.
        assert_eq!(
            day("Follow up 2/3/2027 or in 6 weeks"),
            on("2027-02-03", Dated::Printed)
        );
        assert_eq!(
            day("Follow up after 10 days of antibiotics, on 3/4/2027"),
            on("2027-03-04", Dated::Printed)
        );
        assert_eq!(
            day("Next visit: 6 month well-child check on 3/4/2027"),
            on("2027-03-04", Dated::Printed)
        );
        assert_eq!(
            day("Follow up within 3 days"),
            on("2027-01-13", Dated::Relative)
        );
        // A day before the paper's is what the follow-up is of.
        assert_eq!(
            day("Follow up in 2 weeks to review labs from 1/5/2027"),
            on("2027-01-24", Dated::Relative)
        );
        assert_eq!(
            day("Follow up on 1/5/2027"),
            on("2027-01-05", Dated::Printed)
        );
        assert_eq!(
            day("Follow up after 1 week"),
            on("2027-01-17", Dated::Relative)
        );
    }

    #[test]
    fn a_time_something_lasts_or_an_age_is_no_day() {
        for text in [
            "Follow up for 3 days of fever",
            "Next appointment: 12 year well visit",
            "Follow up 2 days before your procedure",
            "Follow up after 10 days of antibiotics",
            "Next visit: 6 month check",
            "Follow up for your 2 month shots",
        ] {
            assert_eq!(day(text), (None, None), "{text}");
        }
    }

    #[test]
    fn a_month_later_is_the_months_last_day_where_it_has_no_such_day() {
        let f = read("Follow up in 1 month", Some("2027-01-31"));
        assert_eq!(f.follow_up[0].date.as_deref(), Some("2027-02-28"));
        let f = read("Return on Feb 29", Some("2027-03-01"));
        assert_eq!(f.follow_up[0].date.as_deref(), Some("2028-02-29"));
    }

    #[test]
    fn a_condition_keeps_its_words_with_no_day() {
        for text in [
            "Follow up as needed",
            "Return if symptoms persist",
            "Return if symptoms persist beyond 3 days",
            "Follow up PRN",
        ] {
            let f = one(text);
            assert_eq!((f.date, f.dated), (None, None), "{text}");
            assert!(!f.words.is_empty());
        }
        // A day before the condition is the follow-up's.
        assert_eq!(
            day("Follow up in 1 week, sooner if symptoms worsen"),
            on("2027-01-17", Dated::Relative)
        );
    }

    #[test]
    fn a_paper_with_no_date_tells_only_days_printed_whole() {
        let f = read(
            "Follow up in 2 weeks\nReturn on Feb 3\nNext visit 2/3/2027",
            None,
        );
        let days: Vec<Option<&str>> = f.follow_up.iter().map(|f| f.date.as_deref()).collect();
        assert_eq!(days, vec![None, None, Some("2027-02-03")]);
        assert_eq!(f.follow_up[2].dated, Some(Dated::Printed));
    }

    #[test]
    fn the_places_and_clinicians_a_follow_up_names_are_kept_as_printed() {
        let with = |t: &str| one(t).with;
        assert_eq!(
            with("Follow up with Dr. Pat Sample in 2 weeks"),
            vec!["Dr. Pat Sample"]
        );
        assert_eq!(
            with("FOLLOW UP WITH DR SAMPLE IN 2 WEEKS"),
            vec!["DR SAMPLE"]
        );
        assert_eq!(
            with("Return to see Q. Ample, NP at Placeholder Family Clinic on Feb 3"),
            vec!["Q. Ample", "Placeholder Family Clinic"]
        );
        assert_eq!(
            with("Follow up with Placeholder Pines Clinic."),
            vec!["Placeholder Pines Clinic"]
        );
        // An initial is never an article; a comma starts another name, and a
        // credential after one is none.
        assert_eq!(
            with("Follow up with Dr. A. Sample in 2 weeks"),
            vec!["Dr. A. Sample"]
        );
        assert_eq!(
            with("Follow up with Pat A. Sample in 2 weeks"),
            vec!["Pat A. Sample"]
        );
        assert_eq!(
            with("Follow up with Dr. Pat Sample, Placeholder Counseling Center"),
            vec!["Dr. Pat Sample", "Placeholder Counseling Center"]
        );
        assert_eq!(
            with("Follow up with Kim Placeholder, PA-C, in 2 weeks"),
            vec!["Kim Placeholder"]
        );
        assert!(with("Follow up with your doctor in 1 week").is_empty());
        assert!(with("Return at 10:30 AM on Feb 3").is_empty());
        assert!(with("Follow up with Dr. in 2 weeks").is_empty());
    }

    #[test]
    fn every_name_a_follow_up_prints_is_a_span_wherever_it_stands() {
        let has = |words: &str, name: &str| spans(words).iter().any(|s| s == name);
        for (words, name) in [
            (
                "Placeholder Counseling Center, 3/4/2027",
                "Placeholder Counseling Center",
            ),
            ("in 2 weeks: Kim Placeholder, LCSW", "Kim Placeholder"),
            ("Call Sample Group in 2 weeks", "Sample Group"),
            ("At Sample Group", "Sample Group"),
            ("TO SAMPLE GROUP", "SAMPLE GROUP"),
            ("in 2 weeks: June Placeholder", "Placeholder"),
            ("Contact Sample Group Tuesday", "Sample Group"),
            ("with Dr. A. Sample (Sample Group)", "Dr. A. Sample"),
            ("with Dr. A. Sample (Sample Group)", "Sample Group"),
        ] {
            assert!(has(words, name), "{words}: {:?}", spans(words));
        }
        assert!(spans("in 2 weeks as needed").is_empty());
        assert!(!has("with Dr. in 2 weeks", "Dr."));
        assert!(!has("Sample of the Group", "of the Group"));
    }

    #[test]
    fn a_drug_is_read_with_its_directions_and_codes() {
        let text = "Prescription\nRx: Exampleprofen 200 mg tablet\nSig: 1 tab by mouth twice daily\nQty: 30\nNDC 00000-0001-01\nRefills: 0\n";
        let f = read(text, DATE);
        assert_eq!(f.drugs.len(), 1);
        let d = &f.drugs[0];
        assert_eq!(d.drug, "Exampleprofen 200 mg tablet");
        assert_eq!(d.directions.as_deref(), Some("1 tab by mouth twice daily"));
        assert_eq!(d.ndc.as_deref(), Some("00000000101"));
        assert_eq!(d.lines.len(), 3);
        let f = read(
            "Medication: Sampleomycin 250 mg capsule (RxNorm 0000345)\nTake 1 capsule every 8 hours for 7 days\n",
            DATE,
        );
        let d = &f.drugs[0];
        assert_eq!(d.rxnorm.as_deref(), Some("0000345"));
        assert_eq!(
            d.directions.as_deref(),
            Some("Take 1 capsule every 8 hours for 7 days")
        );
        // Directions alone, far from a drug, or a count after its label, are none.
        let f = read(
            "Sig: 1 tab daily\nRx #: 0000234\nRx: none\nMedication:\n\nDirections: rest",
            DATE,
        );
        assert!(f.drugs.is_empty(), "{f:?}");
        let f = read(
            "Rx: Exampleprofen 200 mg\na\nb\nc\nd\nSig: 1 tab daily",
            DATE,
        );
        assert_eq!(f.drugs[0].directions, None);
        // Two drugs, each with its own directions.
        let f = read(
            "Rx: Exampleprofen 200 mg\nSig: 1 tab daily\nRx: Sampleomycin 250 mg\nDirections:\n1 cap twice daily",
            DATE,
        );
        let both: Vec<Option<&str>> = f.drugs.iter().map(|d| d.directions.as_deref()).collect();
        assert_eq!(both, vec![Some("1 tab daily"), Some("1 cap twice daily")]);
        assert_eq!(f.drugs[1].lines.len(), 3);
    }

    #[test]
    fn a_private_line_is_never_read_nor_read_across() {
        // A test of an area's name, which the rule holds as a line of the text.
        let text = "Follow up in 2 weeks for HIV test results\nRx: Exampleprofen 200 mg\nFollow up in 1 week";
        let f = read(text, DATE);
        assert_eq!(f.follow_up.len(), 1);
        assert_eq!(f.follow_up[0].words, "in 1 week");
        // The directions after a private line are no drug's.
        let text = "Rx: Exampleprofen 200 mg\nHIV\nSig: 1 tab daily";
        assert_eq!(read(text, DATE).drugs[0].directions, None);
        let text = "Follow up:\nHIV clinic in 2 weeks";
        assert!(read(text, DATE).follow_up.is_empty());
    }

    #[test]
    fn bills_and_cards_are_not_read() {
        for t in ["bill", "receipt", "eob", "insurance_card"] {
            assert!(!reads(Some(t)), "{t}");
        }
        for t in [
            None,
            Some("visit_summary"),
            Some("discharge_summary"),
            Some("prescription"),
            Some("other"),
        ] {
            assert!(reads(t), "{t:?}");
        }
    }

    #[test]
    fn a_drug_is_the_persons_medicine_only_by_an_equal_code() {
        let med = |code: serde_json::Value, fields: serde_json::Value| -> Record {
            serde_json::from_value(serde_json::json!({
                "id": "r_paperaaaaaaaaaaaaaaaaaa", "kind": "medication", "key": "medication|x",
                "display": "Exampleprofen 200 mg tablet", "status": "active", "code": code,
                "fields": fields, "provenance": []
            }))
            .unwrap()
        };
        let drug = |rxnorm: Option<&str>, ndc: Option<&str>| Drug {
            drug: "Exampleprofen 200 mg tablet".into(),
            directions: None,
            rxnorm: rxnorm.map(str::to_string),
            ndc: ndc.map(str::to_string),
            lines: Vec::new(),
        };
        let rx = [med(
            serde_json::json!({"system": RXNORM, "code": "0000345"}),
            serde_json::json!({}),
        )];
        assert!(medicine(&drug(Some("0000345"), None), &rx).is_some());
        assert!(medicine(&drug(Some("0000999"), None), &rx).is_none());
        // Its name alone is no tie.
        assert!(medicine(&drug(None, None), &rx).is_none());
        let by_ndc = [med(
            serde_json::json!({"system": NDC, "code": "00000-0001-01"}),
            serde_json::json!({}),
        )];
        assert!(medicine(&drug(None, Some("00000000101")), &by_ndc).is_some());
        let package = [med(
            serde_json::json!(null),
            serde_json::json!({"ndc": "0-0001-01"}),
        )];
        assert!(medicine(&drug(None, Some("00000000101")), &package).is_some());
    }

    #[test]
    fn a_diagnosis_code_of_an_area_is_told() {
        assert!(code_area("Follow up for F32.9 in 2 weeks").is_some());
        assert!(code_area("Follow up for J06.9 in 2 weeks").is_none());
        assert!(code_area("Vitamin D3 1000 units").is_none());
    }
}
