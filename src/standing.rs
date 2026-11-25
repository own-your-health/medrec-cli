//! What is current among a person's conditions, allergies and medicines, by
//! what each of their sources says of it (docs/data-contract.md, "Standing").
//! No source's word wins over another's, and nothing here changes a record.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::model::{Person, PersonIndex, Provenance, Record, Source, Word};
use crate::sources::Sources;
use crate::summary::{day, series_key};

pub const SCHEMA: &str = "medrec-standing/v1";
pub const FILE: &str = "standing.json";

/// The kinds that have a standing.
pub const KINDS: &[&str] = &["condition", "allergy", "medication"];

/// What one word, or all of a record's together, says of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Is {
    Current,
    /// A medicine on hold.
    OnHold,
    /// An order never signed.
    Draft,
    Ended,
    /// A condition or an allergy it is said not to be.
    Refuted,
    /// A diagnosis given for a visit, on no problem list.
    Visit,
    /// Entered in error.
    Error,
}

/// One source's word.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Said {
    /// Its place in [`Standings::sources`].
    pub source: usize,
    /// The day its source was last read saying it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read: Option<String>,
    pub is: Is,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abatement: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_of: Option<String>,
}

/// A fill of the same medicine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Filled {
    pub record: String,
    pub date: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Standing {
    pub kind: String,
    /// Whatever the day: a medicine that is current is not one after `until`.
    pub is: Is,
    /// What a condition or an allergy is the same as another by: its code, or
    /// `~` and its folded name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<String>,
    /// The last day of a current medicine, where each word that says so has
    /// an end, and which end it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until_of: Option<String>,
    /// Its words differ.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub differs: bool,
    /// They differ after this day, by which one that says it is current has
    /// ended and another has not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub differs_after: Option<String>,
    /// The last fill of the medicine after `until`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filled: Option<Filled>,
    pub words: Vec<Said>,
}

/// A source that sends diagnoses of visits and no problem list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoProblemList {
    pub source: usize,
    /// Its diagnoses: it is named only where one of them may be shown.
    pub records: Vec<String>,
}

/// `standing.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Standings {
    pub schema: String,
    pub person: String,
    /// The names of the sources, each once by its place here: two can have
    /// one name.
    pub sources: Vec<Option<String>>,
    pub records: BTreeMap<String, Standing>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub no_problem_list: Vec<NoProblemList>,
}

/// A code as its letters, whatever its case and its spaces.
fn code(s: Option<&str>) -> String {
    let s = s.unwrap_or("").trim().to_lowercase();
    s.replace([' ', '_'], "-")
}

fn is_visit_diagnosis(categories: &[String]) -> bool {
    let has = |want: &str| categories.iter().any(|c| code(Some(c)) == want);
    has("encounter-diagnosis") && !has("problem-list-item")
}

/// What a word says by itself, whatever the day.
fn of_word(kind: &str, w: &Word) -> Is {
    let status = code(w.status.as_deref());
    let either = |s: &str| status == s || code(w.verification.as_deref()) == s;
    if either("entered-in-error") {
        return Is::Error;
    }
    if kind == "medication" {
        return match status.as_str() {
            "on-hold" => Is::OnHold,
            "draft" => Is::Draft,
            "not-taken" => Is::Ended,
            s if crate::views::is_ended(Some(s)) => Is::Ended,
            _ => Is::Current,
        };
    }
    if either("refuted") {
        return Is::Refuted;
    }
    let ended = match status.as_str() {
        "" => kind == "condition" && w.abatement.is_some(),
        s => crate::views::is_ended(Some(s)),
    };
    if ended {
        Is::Ended
    } else if kind == "condition" && is_visit_diagnosis(&w.categories) {
        Is::Visit
    } else {
        Is::Current
    }
}

/// The last day of a date as written: `2025-02` ends with its month.
fn last_day(end: &str) -> Option<String> {
    let d = day(Some(end))?;
    let mut parts = d.split('-').map(|n| n.parse::<u32>().ok());
    let year = parts.next()??;
    let month = match parts.next() {
        None => 12,
        Some(m) => m?,
    };
    let last = match month {
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        1..=12 => 31,
        _ => return None,
    };
    let of_month = match parts.next() {
        None => last,
        Some(d) => d.filter(|d| (1..=last).contains(d))?,
    };
    Some(format!("{year:04}-{month:02}-{of_month:02}"))
}

/// The day a word was last read at its source.
fn read_on(p: &Provenance) -> Option<String> {
    day(p.fetched_at.as_deref()).or_else(|| day(Some(&p.filed_at)))
}

fn is_fill(r: &Record) -> bool {
    r.kind == "medication" && r.fields.get("source") == Some(&"dispense".into())
}

/// Each word of a record with the provenance it is of. A record that keeps
/// none (a source's file is not kept, or an earlier medrec saved it) has its
/// own status as the word of its first.
fn words(r: &Record) -> Vec<(Word, &Provenance)> {
    let said: Vec<(Word, &Provenance)> = r
        .words
        .iter()
        .filter_map(|w| {
            let is = |p: &&Provenance| p.raw_path.as_ref() == Some(&w.raw_path);
            Some((w.clone(), r.provenance.iter().find(is)?))
        })
        .collect();
    if !said.is_empty() {
        return said;
    }
    let text = |k: &str| {
        let t = r.fields.get(k).and_then(|v| v.as_str());
        t.filter(|s| !s.is_empty()).map(str::to_string)
    };
    let categories = match r.fields.get("categories").and_then(|c| c.as_array()) {
        Some(a) => a
            .iter()
            .filter_map(|c| c.as_str().map(str::to_string))
            .collect(),
        None => text("category").into_iter().collect(),
    };
    let own = Word {
        raw_path: String::new(),
        status: r.status.clone(),
        verification: text("verification_status"),
        categories,
        abatement: text("abatement").or_else(|| text("abatement_text")),
        end: text("end"),
        end_of: text("end_of"),
        version: None,
    };
    r.provenance.first().map(|p| (own, p)).into_iter().collect()
}

impl Standings {
    pub fn of(sources: &[Source], p: &Person, idx: &PersonIndex) -> Standings {
        let from = Sources::of(sources, idx);
        let mut at: BTreeMap<String, usize> = BTreeMap::new();
        let mut names: Vec<Option<String>> = Vec::new();
        let mut records: BTreeMap<String, Standing> = BTreeMap::new();
        // Of each source: its visits' diagnoses, and whether it says anything
        // else of a condition.
        let mut diagnoses: BTreeMap<usize, Vec<String>> = BTreeMap::new();
        let mut lists: BTreeSet<usize> = BTreeSet::new();
        for r in &idx.records {
            if !KINDS.contains(&r.kind.as_str()) || is_fill(r) {
                continue;
            }
            let mut said: Vec<Said> = Vec::new();
            for (w, p) in words(r) {
                let source = *at.entry(from.key(p)).or_insert_with(|| {
                    names.push(from.label(p));
                    names.len() - 1
                });
                let is = of_word(&r.kind, &w);
                if r.kind == "condition" && is != Is::Error {
                    if is_visit_diagnosis(&w.categories) {
                        let of = diagnoses.entry(source).or_default();
                        if of.last() != Some(&r.id) {
                            of.push(r.id.clone());
                        }
                    } else {
                        lists.insert(source);
                    }
                }
                said.push(Said {
                    source,
                    read: read_on(p),
                    is,
                    status: w.status,
                    verification: w.verification,
                    abatement: w.abatement,
                    end: w.end,
                    end_of: w.end_of,
                });
            }
            let mut s = standing(&r.kind, said);
            if r.kind != "medication" {
                s.line = Some(series_key(r));
            }
            records.insert(r.id.clone(), s);
        }
        let mut fills: BTreeMap<String, Vec<Filled>> = BTreeMap::new();
        for r in idx.records.iter().filter(|r| is_fill(r)) {
            let given = matches!(r.status.as_deref(), None | Some("completed" | "unknown"));
            if let (true, Some(date)) = (given, day(r.date.as_deref())) {
                fills.entry(series_key(r)).or_default().push(Filled {
                    record: r.id.clone(),
                    date,
                });
            }
        }
        for r in &idx.records {
            let Some(s) = records.get_mut(&r.id) else {
                continue;
            };
            let Some(until) = &s.until else { continue };
            let of = fills.get(&series_key(r)).into_iter().flatten();
            s.filled = of
                .filter(|f| f.date > *until)
                .max_by(|a, b| (&a.date, &a.record).cmp(&(&b.date, &b.record)))
                .cloned();
        }
        Standings {
            schema: SCHEMA.into(),
            person: p.id.clone(),
            sources: names,
            records,
            no_problem_list: diagnoses
                .into_iter()
                .filter(|(source, _)| !lists.contains(source))
                .map(|(source, records)| NoProblemList { source, records })
                .collect(),
        }
    }
}

/// What a record's words say together.
fn standing(kind: &str, words: Vec<Said>) -> Standing {
    let all: BTreeSet<Is> = words.iter().map(|w| w.is).collect();
    let is = match () {
        _ if all.contains(&Is::Current) => Is::Current,
        _ if all.contains(&Is::OnHold) => Is::OnHold,
        _ if all.contains(&Is::Draft) => Is::Draft,
        _ if all.contains(&Is::Ended) => Is::Ended,
        _ if all.contains(&Is::Refuted) => Is::Refuted,
        _ if all.contains(&Is::Visit) => Is::Visit,
        _ => Is::Error,
    };
    let counted = |w: &&Said| !matches!(w.is, Is::Error | Is::Visit);
    let says: BTreeSet<Is> = words.iter().filter(counted).map(|w| w.is).collect();
    let sure: BTreeSet<String> = words
        .iter()
        .filter(counted)
        .filter_map(|w| w.verification.as_deref())
        .map(|v| code(Some(v)))
        .collect();
    let differs = says.len() > 1 || sure.len() > 1;

    let mut s = Standing {
        kind: kind.into(),
        is,
        line: None,
        until: None,
        until_of: None,
        differs,
        differs_after: None,
        filled: None,
        words,
    };
    if kind != "medication" || is != Is::Current {
        return s;
    }
    let ends: Vec<Option<(String, Option<String>)>> = s
        .words
        .iter()
        .filter(|w| w.is == Is::Current)
        .map(|w| Some((last_day(w.end.as_deref()?)?, w.end_of.clone())))
        .collect();
    let first = ends.iter().flatten().map(|(d, _)| d).min().cloned();
    let last = ends.iter().flatten().max_by(|a, b| a.0.cmp(&b.0)).cloned();
    let all_end = ends.iter().all(Option::is_some);
    if !differs && (!all_end || first.as_ref() != last.as_ref().map(|l| &l.0)) {
        s.differs_after = first;
    }
    if let (true, Some((day, of))) = (all_end, last) {
        s.until = Some(day);
        s.until_of = of;
    }
    s
}

/// One line of a current list: a condition or an allergy of one code or name,
/// or one medicine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Line {
    /// Its records, in the reader's order; the first gives its name.
    pub records: Vec<String>,
    /// Its records of other sources that say otherwise.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub against: Vec<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub differs: bool,
}

/// What is current of one kind, as a reader lists it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lists {
    pub current: Vec<Line>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub on_hold: Vec<Line>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ended_by_date: Vec<Line>,
    /// How many are entered in error, and not shown.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub in_error: usize,
    /// The sources that send no problem list.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub no_problem_list: Vec<usize>,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// Where a record is for a reader on a day.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum At {
    Current,
    OnHold,
    EndedByDate,
    /// In the list of all only.
    All,
    /// Asked for by id only.
    Error,
}

impl Standing {
    pub fn on(&self, today: &str) -> At {
        let past = |d: &Option<String>| d.as_deref().is_some_and(|d| d < today);
        match self.is {
            Is::Current if past(&self.until) => At::EndedByDate,
            Is::Current => At::Current,
            Is::OnHold => At::OnHold,
            Is::Error => At::Error,
            Is::Draft | Is::Ended | Is::Refuted | Is::Visit => At::All,
        }
    }

    /// Its own words differ on that day.
    pub fn differs_on(&self, today: &str) -> bool {
        self.differs || self.differs_after.as_deref().is_some_and(|d| d < today)
    }

    fn has(&self, source: usize, which: impl Fn(Is) -> bool) -> bool {
        self.words.iter().any(|w| w.source == source && which(w.is))
    }
}

impl Standings {
    /// The lists of one kind, of the records a reader may show, in its order.
    pub fn lists<'a>(
        &self,
        kind: &str,
        shown: impl IntoIterator<Item = &'a str>,
        today: &str,
    ) -> Lists {
        let shown: Vec<(&str, &Standing)> = shown
            .into_iter()
            .filter_map(|id| Some((id, self.records.get(id)?)))
            .filter(|(_, s)| s.kind == kind)
            .collect();
        let mut out = Lists::default();
        let mut lines: BTreeMap<&str, Vec<(&str, &Standing)>> = BTreeMap::new();
        for (id, s) in &shown {
            if let (Some(line), true) = (&s.line, s.is != Is::Error) {
                lines.entry(line).or_default().push((id, s));
            }
        }
        for (id, s) in &shown {
            let one = || Line {
                records: vec![id.to_string()],
                against: Vec::new(),
                differs: s.differs_on(today),
            };
            match (s.on(today), s.line.as_deref()) {
                (At::Error, _) => out.in_error += 1,
                (At::All, _) => {}
                (At::OnHold, _) => out.on_hold.push(one()),
                (At::EndedByDate, _) => out.ended_by_date.push(one()),
                (At::Current, None) => out.current.push(one()),
                (At::Current, Some(line)) => {
                    // At the first of its line, and once.
                    if let Some(of) = lines.remove(line) {
                        out.current.push(merged(&of, today));
                    }
                }
            }
        }
        if kind == "condition" {
            let is_shown = |id: &String| shown.iter().any(|(s, _)| s == id);
            let named = self.no_problem_list.iter();
            out.no_problem_list = named
                .filter(|n| n.records.iter().any(is_shown))
                .map(|n| n.source)
                .collect();
        }
        out
    }
}

/// One line of the records of one code or name.
fn merged(of: &[(&str, &Standing)], today: &str) -> Line {
    let current = |i: Is| i == Is::Current;
    let ended = |i: Is| matches!(i, Is::Ended | Is::Refuted);
    let sources: BTreeSet<usize> = of
        .iter()
        .flat_map(|(_, s)| s.words.iter().map(|w| w.source))
        .collect();
    // Sources that say it is not so, and nowhere that it is.
    let other: Vec<usize> = sources
        .into_iter()
        .filter(|n| of.iter().any(|(_, s)| s.has(*n, ended)))
        .filter(|n| !of.iter().any(|(_, s)| s.has(*n, current)))
        .collect();
    let is_current = |s: &Standing| s.on(today) == At::Current;
    let ids = |which: &dyn Fn(&Standing) -> bool| -> Vec<String> {
        of.iter()
            .filter(|(_, s)| which(s))
            .map(|(id, _)| id.to_string())
            .collect()
    };
    let records = ids(&is_current);
    let against = ids(&|s| !is_current(s) && other.iter().any(|n| s.has(*n, ended)));
    let differs =
        !against.is_empty() || of.iter().any(|(_, s)| is_current(s) && s.differs_on(today));
    Line {
        records,
        against,
        differs,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Household;
    use serde_json::{json, Value};

    macro_rules! fixture {
        ($f:literal) => {
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/standing/",
                $f
            ))
        };
    }

    const DAY: &str = "2024-06-15";

    fn of(index: Value) -> Standings {
        let h: Household = serde_json::from_str(fixture!("household.json")).unwrap();
        let idx: PersonIndex = serde_json::from_value(index).unwrap();
        Standings::of(&h.sources, &h.people[0], &idx)
    }

    fn fixture() -> Standings {
        of(serde_json::from_str(fixture!("index.json")).unwrap())
    }

    /// The fixture's record whose id starts so.
    fn one<'a>(s: &'a Standings, stem: &str) -> &'a Standing {
        let id = format!("r_{stem:a<20}");
        s.records.get(&id).unwrap_or_else(|| panic!("no {id}"))
    }

    fn at(stem: &str) -> At {
        one(&fixture(), stem).on(DAY)
    }

    #[test]
    fn the_fixture_gives_its_view() {
        let s = serde_json::to_value(fixture()).unwrap();
        let want: Value = serde_json::from_str(fixture!("standing.json")).unwrap();
        assert_eq!(s, want, "{}", serde_json::to_string_pretty(&s).unwrap());
    }

    #[test]
    fn a_reader_lists_them_by_the_rules() {
        let s = fixture();
        let v: Value = serde_json::from_str(fixture!("lists.json")).unwrap();
        for c in v["cases"].as_array().unwrap() {
            let shown: Vec<&str> = c["shown"]
                .as_array()
                .unwrap()
                .iter()
                .map(|i| i.as_str().unwrap())
                .collect();
            let got = s.lists(
                c["kind"].as_str().unwrap(),
                shown,
                c["day"].as_str().unwrap(),
            );
            assert_eq!(
                serde_json::to_value(got).unwrap(),
                c["lists"],
                "{}",
                c["why"]
            );
        }
    }

    // One test for each rule of "Standing" in docs/data-contract.md.

    #[test]
    fn a_record_is_current_while_any_source_says_so_and_differs() {
        let s = fixture();
        let r = one(&s, "bothsay");
        assert_eq!((r.on(DAY), r.differs_on(DAY)), (At::Current, true));
        let words: Vec<(usize, Option<&str>, Option<&str>)> = r
            .words
            .iter()
            .map(|w| (w.source, w.status.as_deref(), w.read.as_deref()))
            .collect();
        assert_eq!(
            words,
            [
                (0, Some("active"), Some("2024-05-02")),
                (1, Some("resolved"), Some("2024-05-03"))
            ]
        );
        assert_eq!(one(&s, "bothended").on(DAY), At::All);
    }

    #[test]
    fn a_visits_diagnosis_is_current_only_where_a_problem_list_has_it() {
        assert_eq!(at("visitonly"), At::All);
        let s = fixture();
        let l = s.lists(
            "condition",
            ["r_onlistvisitaaaaaaaaa", "r_onlistaaaaaaaaaaaaaa"],
            DAY,
        );
        assert_eq!(l.current.len(), 1);
        assert_eq!(
            (l.current[0].records.len(), l.current[0].differs),
            (1, false)
        );
    }

    #[test]
    fn a_source_with_no_problem_list_is_named_where_its_diagnosis_may_be_shown() {
        let s = fixture();
        let named = |shown: &[&str]| -> Vec<Option<String>> {
            let l = s.lists("condition", shown.iter().copied(), DAY);
            l.no_problem_list
                .iter()
                .map(|n| s.sources[*n].clone())
                .collect()
        };
        assert_eq!(
            named(&["r_visitonlyaaaaaaaaaaa"]),
            [Some("Example Walk-in".to_string())]
        );
        assert!(named(&["r_plainaaaaaaaaaaaaaaa"]).is_empty());
    }

    #[test]
    fn refuted_is_never_current_and_beside_an_active_word_differs() {
        assert_eq!(at("refuted"), At::All);
        assert_eq!(at("refutedallergy"), At::All);
        let s = fixture();
        let r = one(&s, "refutedbyone");
        assert_eq!((r.on(DAY), r.differs_on(DAY)), (At::Current, true));
    }

    #[test]
    fn entered_in_error_is_out_of_the_lists_and_counted_and_nothing_beside_another_word() {
        for stem in ["error", "errorallergy", "errormedicine"] {
            assert_eq!(at(stem), At::Error, "{stem}");
        }
        let s = fixture();
        let r = one(&s, "errorbeside");
        assert_eq!((r.on(DAY), r.differs_on(DAY)), (At::Current, false));
        let l = s.lists("allergy", ["r_errorallergyaaaaaaaa"], DAY);
        assert_eq!((l.current.len(), l.in_error), (0, 1));
    }

    #[test]
    fn what_is_not_sure_is_current_with_its_word() {
        let s = fixture();
        let r = one(&s, "provisional");
        assert_eq!(r.on(DAY), At::Current);
        assert_eq!(r.words[0].verification.as_deref(), Some("provisional"));
        let r = one(&s, "suretwice");
        assert_eq!((r.on(DAY), r.differs_on(DAY)), (At::Current, true));
    }

    #[test]
    fn a_condition_with_no_status_and_an_abatement_is_ended() {
        assert_eq!(at("abated"), At::All);
        // Not an allergy, whatever it says.
        let w = Word {
            abatement: Some("2020-01-02".into()),
            ..Word::default()
        };
        assert_eq!(of_word("allergy", &w), Is::Current);
    }

    #[test]
    fn a_medicine_is_current_through_its_last_day_and_then_ended_by_its_date() {
        let s = fixture();
        let r = one(&s, "ends");
        assert_eq!(
            (r.until.as_deref(), r.until_of.as_deref()),
            (Some("2024-03-31"), Some("prescription"))
        );
        assert_eq!(r.on("2024-03-31"), At::Current);
        assert_eq!(r.on("2024-04-01"), At::EndedByDate);
        assert_eq!(one(&s, "endsmonth").until.as_deref(), Some("2024-06-30"));
    }

    #[test]
    fn a_fill_after_the_end_is_a_fact_beside_it_and_changes_nothing() {
        let s = fixture();
        let r = one(&s, "ends");
        let f = r.filled.as_ref().unwrap();
        assert_eq!(
            (f.record.as_str(), f.date.as_str()),
            ("r_fillafteraaaaaaaaaaa", "2024-05-02")
        );
        assert_eq!(r.on(DAY), At::EndedByDate);
        for stem in ["fillbefore", "fillafter", "fillreturned"] {
            assert!(!s.records.contains_key(&format!("r_{stem:a<20}")), "{stem}");
        }
    }

    #[test]
    fn a_medicine_one_source_ends_is_current_by_the_other_and_differs_after() {
        let s = fixture();
        let r = one(&s, "endsatone");
        assert_eq!(
            (r.until.as_deref(), r.differs_after.as_deref()),
            (None, Some("2024-03-31"))
        );
        assert_eq!(
            (r.on("2024-03-31"), r.differs_on("2024-03-31")),
            (At::Current, false)
        );
        assert_eq!(
            (r.on("2024-04-01"), r.differs_on("2024-04-01")),
            (At::Current, true)
        );
        let r = one(&s, "endstwice");
        assert_eq!(
            (r.until.as_deref(), r.until_of.as_deref()),
            (Some("2024-06-30"), Some("taking"))
        );
    }

    #[test]
    fn unknown_is_current_with_its_word() {
        let s = fixture();
        let r = one(&s, "unknown");
        assert_eq!(
            (r.on(DAY), r.words[0].status.as_deref()),
            (At::Current, Some("unknown"))
        );
    }

    #[test]
    fn on_hold_is_a_group_of_its_own() {
        assert_eq!(at("onhold"), At::OnHold);
    }

    #[test]
    fn a_draft_is_in_the_list_of_all() {
        assert_eq!(at("draft"), At::All);
    }

    #[test]
    fn not_taken_is_not_current() {
        assert_eq!(at("nottaken"), At::All);
    }

    #[test]
    fn an_allergy_never_ends_by_a_date() {
        let s = fixture();
        assert_eq!(one(&s, "oldpollen").on("2099-01-01"), At::Current);
        assert_eq!(at("inactive"), At::All);
    }

    #[test]
    fn one_line_differs_by_another_source_and_not_by_the_same_ones_earlier_episode() {
        let s = fixture();
        let l = |a: &str, b: &str| {
            let ids = [format!("r_{a:a<20}"), format!("r_{b:a<20}")];
            s.lists("condition", ids.iter().map(String::as_str), DAY)
                .current
        };
        let rash = l("tworecords", "tworecordsother");
        assert_eq!(
            (rash.len(), rash[0].differs, rash[0].against.len()),
            (1, true, 1)
        );
        let cough = l("again", "againbefore");
        assert_eq!(
            (cough.len(), cough[0].differs, cough[0].against.len()),
            (1, false, 0)
        );
    }

    #[test]
    fn a_medicine_is_never_one_line_with_another() {
        let s = fixture();
        let l = s.lists(
            "medication",
            ["r_noendaaaaaaaaaaaaaaa", "r_noendtwiceaaaaaaaaaa"],
            DAY,
        );
        assert_eq!(l.current.len(), 2);
        assert!(one(&s, "noend").line.is_none());
    }

    #[test]
    fn uncoded_names_alike_are_one_line_and_each_source_is_named() {
        let s = fixture();
        let l = s.lists(
            "allergy",
            ["r_nutaaaaaaaaaaaaaaaaa", "r_nutagainaaaaaaaaaaaa"],
            DAY,
        );
        assert_eq!(l.current.len(), 1);
        let named: Vec<&str> = l.current[0]
            .records
            .iter()
            .flat_map(|id| &s.records[id].words)
            .map(|w| s.sources[w.source].as_deref().unwrap())
            .collect();
        assert_eq!(named, ["Example Health", "Sample Clinic East"]);
    }

    #[test]
    fn a_record_with_no_words_has_its_own_status() {
        let s = of(
            json!({"schema": SCHEMA.replace("standing", "index"), "person": "p_robinaaaaaaaaaaaaaaa",
            "rev": 1, "documents": [], "records": [
            {"id": "r_oneaaaaaaaaaaaaaaaaa", "kind": "condition", "key": "k1", "display": "Sample ache",
             "status": "resolved", "fields": {}, "provenance": [{"source": {"kind": "manual"}, "method": "manual",
             "filed_at": "2024-01-02T10:00:00Z", "filed_by": "medrec 0.9.9"}]},
            {"id": "r_twoaaaaaaaaaaaaaaaaa", "kind": "condition", "key": "k2", "display": "Sample itch",
             "fields": {"category": "encounter-diagnosis", "verification_status": "confirmed"},
             "provenance": [{"source": {"kind": "manual"}, "method": "manual",
             "filed_at": "2024-01-02T10:00:00Z", "filed_by": "medrec 0.9.9"}]}]}),
        );
        assert_eq!(one(&s, "one").on(DAY), At::All);
        let r = one(&s, "two");
        assert_eq!(
            (r.is, r.words[0].read.as_deref()),
            (Is::Visit, Some("2024-01-02"))
        );
    }

    #[test]
    fn the_last_day_of_a_date_as_written() {
        for (end, day) in [
            ("2024-02", Some("2024-02-29")),
            ("2023-02", Some("2023-02-28")),
            ("2100-02", Some("2100-02-28")),
            ("2024", Some("2024-12-31")),
            ("2024-04-05T10:00:00Z", Some("2024-04-05")),
            ("2024-13", None),
            ("2024-04-31", None),
            ("later", None),
        ] {
            assert_eq!(last_day(end).as_deref(), day, "{end}");
        }
    }
}
