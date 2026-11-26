//! The clinicians a person's papers name beside a practice or a credential
//! that says what they treat, the days of a paper from a practice of an area
//! and the clinicians linked to one, and the items that print one by surname
//! and first initial: claims, visits, prescriptions, procedures and results. A
//! likely match: it only holds an item back, and the household's word at the
//! Mac opens a clinician (`Opened`).

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::model::{Document, PersonIndex, Record};
use crate::{events, given, labels, nppes, private};

/// A clinician as items print them: the surname in capitals, and the first
/// initial.
pub(crate) type Person = (String, char);

/// Who a paper says is of an area.
struct Said {
    area: &'static str,
    /// What it says at its head: its organization, or its title.
    text: String,
    /// Whether that is the organization, and says the area itself.
    organization: bool,
    /// Only by what a source gave, that the paper does not show.
    gave: bool,
}

impl Said {
    /// A paper's organization, where it says an area, and its title, where it
    /// or the organization does. What a paper says inside is not read: it
    /// names others too.
    fn paper(d: &Document, gave: bool) -> Vec<Said> {
        let area = |t: &Option<String>| private::place_area(t.as_deref()?);
        let (of, titled) = (area(&d.organization), area(&d.title));
        let said = |text: &Option<String>, area: Option<&'static str>, organization: bool| {
            Some(Said {
                area: area?,
                text: text.clone()?,
                organization,
                gave,
            })
        };
        let each = [
            said(&d.organization, of, true),
            said(&d.title, of.or(titled), false),
        ];
        each.into_iter().flatten().collect()
    }

    /// Whether it names one of these people. A name that may be a place's is
    /// looked for in an organization alone.
    fn names(&self, printed: &Printed) -> bool {
        let people = &printed.people;
        if !printed.a_person && !self.organization {
            return false;
        }
        names(&self.text, people)
    }
}

/// The least of a surname that one cut short keeps: a payer cuts a name to 20
/// characters.
const CUT: usize = 8;

fn same_surname(a: &str, b: &str) -> bool {
    let cut = |a: &str, b: &str| a.len() >= CUT && b.starts_with(a);
    a.len() > 1 && (a == b || cut(a, b) || cut(b, a))
}

fn same_person(a: &Person, b: &Person) -> bool {
    a.1 == b.1 && same_surname(&a.0, &b.0)
}

/// The field each kind prints its clinician in.
pub(crate) fn printing(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "claim" => "provider",
        "encounter" => "practitioner",
        "medication" => "prescriber",
        "procedure" | "result" | "report" => "performer",
        _ => return None,
    })
}

/// The people an item's clinician can be.
#[derive(Clone)]
struct Printed {
    /// As the item prints it.
    text: String,
    people: BTreeSet<Person>,
    /// The first way it reads: the last word the surname, or the words before
    /// a comma.
    first: Option<Person>,
    /// Whether the text is a person's: by an initial, a degree, or a comma
    /// after the surname, or as a visit prints its clinician and a medicine its
    /// prescriber. `Kim Placeholder` on a claim can be a place.
    a_person: bool,
}

impl Printed {
    fn of(r: &Record) -> Option<Printed> {
        let field = printing(&r.kind)?;
        let text = r.fields.get(field).and_then(Value::as_str)?;
        let people = nppes::people(text);
        if events::says_organization(text) || people.is_empty() {
            return None;
        }
        let mut words = text.split(|c: char| !c.is_alphanumeric());
        let initial = words.any(|w| w.chars().count() == 1);
        // `PA` is a practice's as often as a degree.
        let degree = |w: &str| w != "PA" && nppes::around_a_name(w);
        let mut words = text.split(|c: char| c.is_whitespace() || c == ',');
        // A performer can be a place, as a claim's provider can.
        let person_field = if field == "performer" {
            "provider"
        } else {
            field
        };
        let a_person = initial
            || text.contains(',')
            || labels::a_person(person_field, text)
            || words.any(|w| degree(&nppes::fold(w)));
        let first = nppes::readings(text).into_iter().find_map(|(last, given)| {
            let person = nppes::person(&last, &given[0])?;
            people.contains(&person).then_some(person)
        });
        let text = text.to_string();
        Some(Printed {
            text,
            people,
            first,
            a_person,
        })
    }

    /// Without the clinicians the household opened, as the rule matches them.
    fn but(mut self, opened: &[Person]) -> Option<Printed> {
        self.people.retain(|p| !is_open(opened, p));
        (!self.people.is_empty()).then_some(self)
    }
}

/// Whether the household opened this clinician.
fn is_open(opened: &[Person], p: &Person) -> bool {
    opened.iter().any(|o| same_person(o, p))
}

/// Whether a short text names one of these people, its words in either order.
fn names(text: &str, people: &BTreeSet<Person>) -> bool {
    if labels::prints(text, people, &same_surname) {
        return true;
    }
    if events::says_organization(text) {
        return false;
    }
    let whole = nppes::readings(text).into_iter();
    whole
        .filter(|(_, given)| private::place_area(&given[0]).is_none())
        .filter_map(|(last, given)| Some((last, given[0].chars().next()?)))
        .any(|read| people.iter().any(|p| same_person(&read, p)))
}

/// Whether a claim or a visit is of a day: a claim's billable period, else
/// its date.
fn of_day(r: &Record, day: &str) -> bool {
    fn ten(v: Option<&str>) -> Option<&str> {
        v?.get(..10)
    }
    let period = r.fields.get("billable_period");
    let side = |k: &str| ten(period.and_then(|p| p.get(k)).and_then(Value::as_str));
    let start = side("start").or(ten(r.date.as_deref()));
    let end = side("end").or(start);
    matches!((start, end), (Some(s), Some(e)) if s <= day && day <= e)
}

/// The clinician a claim prints, where it is a person's: the people they can
/// be.
fn of_claim(r: &Record) -> Option<BTreeSet<Person>> {
    claim_printing(r).map(|p| p.people)
}

/// What a claim prints of its clinician, where it is a person's.
fn claim_printing(r: &Record) -> Option<Printed> {
    if r.kind != "claim" {
        return None;
    }
    Printed::of(r).filter(|p| p.a_person)
}

/// The days of a paper from a practice of an area, and the clinicians linked
/// to one.
#[derive(Default)]
struct Days {
    /// Each day of a paper that D3 (above) can join to a visit, from a practice
    /// whose organization says an area, and that area.
    days: Vec<(String, &'static str)>,
    /// The clinicians that two or more such days of an area have a claim of,
    /// or one of which has no other clinician's claim.
    linked: Vec<(Person, &'static str)>,
}

impl Days {
    /// From the person's own papers and claims, those in error included: as
    /// shown, or also as a source gave each (`gave`).
    fn of(idx: &PersonIndex, gave: bool) -> Days {
        // Each claim with the people it prints, and whether every version of it
        // prints one.
        let claims: Vec<(&Record, BTreeSet<Person>, bool)> = idx
            .records
            .iter()
            .filter(|r| r.kind == "claim")
            .map(|r| {
                let each: Vec<Option<BTreeSet<Person>>> =
                    versions(r, gave).map(|x| of_claim(&x)).collect();
                let every = each.iter().all(Option::is_some);
                (r, each.into_iter().flatten().flatten().collect(), every)
            })
            .collect();
        let mut out = Days::default();
        let mut on: Vec<(Person, &str, &'static str)> = Vec::new();
        for d in &idx.documents {
            let ty = d.doc_type.as_deref().unwrap_or("other");
            let day = d.date.as_deref().and_then(|t| t.get(..10));
            let (true, Some(day)) = (events::of_the_days_visit(ty), day) else {
                continue;
            };
            let of_day: Vec<&(&Record, BTreeSet<Person>, bool)> =
                claims.iter().filter(|(r, ..)| of_day(r, day)).collect();
            if of_day.is_empty() {
                continue;
            }
            let given = given::papers_as_given(d).filter(|_| gave);
            let organizations = std::iter::once(d.organization.clone())
                .chain(given.map(|x| x.organization))
                .flatten();
            for organization in organizations {
                let pharmacy = organization.to_lowercase().contains("pharmacy");
                if pharmacy || crate::plain::is_connection(&organization) {
                    continue;
                }
                let Some(area) = private::place_area(&organization) else {
                    continue;
                };
                out.days.push((day.to_string(), area));
                for (_, people, _) in &of_day {
                    on.extend(people.iter().map(|p| (p.clone(), day, area)));
                }
                // The only clinician with a claim that day. Any claim that
                // prints one, as shown or as given, offers them, and only a
                // claim each version of which prints one rules them out: so
                // that what a source gave holds all that is shown does.
                let ruling: Vec<&BTreeSet<Person>> = of_day
                    .iter()
                    .filter(|(_, people, every)| *every && !people.is_empty())
                    .map(|(_, people, _)| people)
                    .collect();
                let in_each = |p: &&Person| {
                    let has = |c: &&BTreeSet<Person>| c.iter().any(|q| same_person(p, q));
                    ruling.iter().all(has)
                };
                let offered = of_day.iter().flat_map(|(_, people, _)| people);
                let only: Vec<&Person> = offered.filter(in_each).collect();
                out.linked
                    .extend(only.into_iter().map(|p| (p.clone(), area)));
            }
        }
        for (p, _, area) in &on {
            let days: BTreeSet<&str> = on
                .iter()
                .filter(|(q, _, of)| same_person(p, q) && of == area)
                .map(|(_, day, _)| *day)
                .collect();
            if days.len() >= 2 {
                out.linked.push((p.clone(), *area));
            }
        }
        out
    }

    /// The area of a day an item is of.
    fn of_day(&self, r: &Record) -> Option<&'static str> {
        let mut days = self.days.iter();
        days.find(|(day, _)| of_day(r, day)).map(|(_, area)| *area)
    }

    /// The area of a clinician linked to one.
    fn linked(&self, people: &BTreeSet<Person>) -> Option<&'static str> {
        let mut linked = self.linked.iter();
        let found = linked.find(|(p, _)| people.iter().any(|q| same_person(p, q)));
        found.map(|(_, area)| *area)
    }
}

/// An item as it is shown, and as each source gave it where `gave`.
fn versions(r: &Record, gave: bool) -> impl Iterator<Item = Record> + '_ {
    let given = given::as_given(r).filter(move |_| gave);
    std::iter::once(r.clone()).chain(given)
}

/// The household's word that a clinician of the person's is of no private area
/// (`marks.json` `clinicians`): none of their items is private by this rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Opened {
    pub id: String,
    /// The clinician as the record shows them, where it does; absent where
    /// only a source gave them (`given`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub printed: Option<String>,
    /// Only a source gave the record this clinician: no item shows them.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub given: bool,
    /// The surnames and first initials that name them: those that tie the
    /// record, or the first that the name reads as.
    pub people: Vec<Reading>,
    /// The record it was said of.
    pub item: String,
    pub at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
    /// What a later medrec writes here, kept as it is.
    #[serde(flatten)]
    pub other: Map<String, Value>,
}

/// A surname, in capitals, and a first initial.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reading {
    pub surname: String,
    pub initial: String,
}

impl Opened {
    pub(crate) fn people(&self) -> impl Iterator<Item = Person> + '_ {
        self.people.iter().filter_map(|r| {
            let mut initial = r.initial.chars();
            match (initial.next(), initial.next()) {
                (Some(c), None) => Some((r.surname.clone(), c)),
                _ => None,
            }
        })
    }

    /// Whether it is of a clinician this record prints.
    pub(crate) fn of(&self, r: &Record) -> bool {
        let printed = printed(r);
        self.people()
            .any(|o| printed.iter().any(|p| same_person(&o, p)))
    }
}

/// What ties an item's clinician to an area, the surest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Learned {
    /// A paper names them.
    Paper,
    /// They are linked to the practice of a paper of a day.
    Linked,
    /// The item is of the day of such a paper, and no more.
    Day,
}

impl Learned {
    /// What `private.json` says of it: nothing of a paper.
    pub(crate) fn word(self) -> Option<&'static str> {
        match self {
            Learned::Paper => None,
            Learned::Linked => Some("linked"),
            Learned::Day => Some("day"),
        }
    }
}

/// Why an item is private by its clinician.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Named {
    pub area: &'static str,
    pub learned: Learned,
}

pub(crate) struct Clinicians {
    said: Vec<Said>,
    /// As shown, and also as a source gave it.
    days: [Days; 2],
    opened: Vec<Person>,
}

impl Clinicians {
    /// Of the person's own papers, and of the days of a paper from a practice,
    /// as each is shown and as a source gave it, without those the household
    /// opened.
    pub(crate) fn of(idx: &PersonIndex, opened: &[Opened]) -> Clinicians {
        let mut said: Vec<Said> = Vec::new();
        for d in &idx.documents {
            said.extend(Said::paper(d, false));
            said.extend(given::papers_as_given(d).flat_map(|x| Said::paper(&x, true)));
        }
        Clinicians {
            said,
            days: [Days::of(idx, false), Days::of(idx, true)],
            opened: opened.iter().flat_map(Opened::people).collect(),
        }
    }

    /// What ties these people to an area: a paper before a link.
    fn by(&self, printed: &Printed, gave: bool) -> Option<Named> {
        let mut said = self.said.iter().filter(|s| gave || !s.gave);
        if let Some(s) = said.find(|s| s.names(printed)) {
            let learned = Learned::Paper;
            return Some(Named {
                area: s.area,
                learned,
            });
        }
        let area = self.days[usize::from(gave)].linked(&printed.people);
        let area = area.filter(|_| printed.a_person)?;
        let learned = Learned::Linked;
        Some(Named { area, learned })
    }

    /// The area of the day a claim or a visit is of, where it prints a
    /// clinician the household did not open.
    fn of_day(&self, r: &Record, gave: bool) -> Option<Named> {
        let area = self.days[usize::from(gave)].of_day(r)?;
        // Opened by the first way the name reads, which the mark keeps.
        let mut printing = versions(r, gave).filter_map(|x| claim_printing(&x));
        let open = |p: &Printed| p.first.as_ref().is_some_and(|f| is_open(&self.opened, f));
        let prints = printing.any(|p| !open(&p));
        let learned = Learned::Day;
        prints.then_some(Named { area, learned })
    }

    /// The area of the clinician an item prints, by a paper that names them, a
    /// link to the practice of a paper of a day, or that day: as all are shown,
    /// or also as a source gave any (`gave`). The surest of them.
    pub(crate) fn named(&self, r: &Record, gave: bool) -> Option<Named> {
        let by = versions(r, gave).filter_map(|x| {
            let printed = Printed::of(&x)?.but(&self.opened)?;
            self.by(&printed, gave)
        });
        let all: Vec<Named> = by.chain(self.of_day(r, gave)).collect();
        all.into_iter().min_by_key(|n| n.learned)
    }

    /// The area alone.
    #[cfg(test)]
    pub(crate) fn area(&self, r: &Record, gave: bool) -> Option<&'static str> {
        self.named(r, gave).map(|n| n.area)
    }

    /// The clinicians an item prints, as shown or as a source gave it, that
    /// hold it back, each as printed with the readings that do: whom opening
    /// it opens. A day holds a claim by any reading, and the first is kept.
    /// Keyed by the text as printed, with whether the record shows it.
    pub(crate) fn tied(&self, r: &Record) -> BTreeMap<String, (bool, BTreeSet<Person>)> {
        let mut out: BTreeMap<String, (bool, BTreeSet<Person>)> = BTreeMap::new();
        let by_day = self.of_day(r, true).is_some();
        let shown = Printed::of(r).map(|p| p.text);
        for x in versions(r, true) {
            let Some(printed) = Printed::of(&x).and_then(|p| p.but(&self.opened)) else {
                continue;
            };
            let mut tie: BTreeSet<Person> = BTreeSet::new();
            for one in &printed.people {
                let alone = Printed {
                    people: BTreeSet::from([one.clone()]),
                    ..printed.clone()
                };
                if self.by(&alone, true).is_some() {
                    tie.insert(one.clone());
                }
            }
            if let Some(first) = claim_printing(&x).and_then(|p| p.first).filter(|_| by_day) {
                tie.insert(first);
            }
            if !tie.is_empty() {
                let is_shown = shown.as_ref() == Some(&printed.text);
                let entry = out.entry(printed.text.clone()).or_default();
                entry.0 |= is_shown;
                entry.1.extend(tie);
            }
        }
        out
    }
}

/// Every clinician an item can print, as shown or as a source gave it.
pub(crate) fn printed(r: &Record) -> BTreeSet<Person> {
    let each = versions(r, true).filter_map(|x| Printed::of(&x));
    each.flat_map(|p| p.people).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn paper(organization: Option<&str>, title: &str) -> Document {
        let mut v = json!({"id": "d_aaaaaaaaaaaaaaaaaaaa", "path": "x.pdf", "sha256": "0",
            "md5": "0", "size": 1, "media_type": "application/pdf", "title": title,
            "filed_at": "2024-01-02T10:00:00Z", "origin": {"kind": "file", "action": "copied"},
            "provenance": {"source": {"kind": "scan"}, "method": "file",
                "filed_at": "2024-01-02T10:00:00Z", "filed_by": "medrec"}});
        v["organization"] = organization.into();
        serde_json::from_value(v).unwrap()
    }

    fn claim(provider: &str) -> Record {
        serde_json::from_value(json!({"id": "r_aaaaaaaaaaaaaaaaaaaa", "kind": "claim",
            "key": "a", "display": "Office visit", "fields": {"provider": provider},
            "provenance": []}))
        .unwrap()
    }

    fn named(idx: &PersonIndex) -> Clinicians {
        Clinicians::of(idx, &[])
    }

    fn area(d: Document, provider: &str) -> Option<&'static str> {
        let mut idx = PersonIndex::new("p_test");
        idx.documents = vec![d];
        named(&idx).area(&claim(provider), false)
    }

    /// By its organization or its title, whichever says what it treats.
    #[test]
    fn a_claim_is_of_the_clinician_a_paper_names() {
        let mind = Some("mental_health");
        let by = |organization: &str, title: &str, provider: &str| {
            area(paper(Some(organization), title), provider)
        };
        for provider in [
            "K PLACEHOLDER",
            "PLACEHOLDER K",
            "PLACEHOLDER, KIM",
            "K. Placeholder",
        ] {
            assert_eq!(
                by("Kim Placeholder, LCSW", "Bill", provider),
                mind,
                "{provider}"
            );
        }
        for organization in [
            "PLACEHOLDER KIM LCSW",
            "Placeholder, Kim A, PsyD",
            "Kim A. Placeholder LMHC",
        ] {
            assert_eq!(
                by(organization, "Bill", "K PLACEHOLDER"),
                mind,
                "{organization}"
            );
        }
        let practice = "Example Counseling";
        assert_eq!(
            by(practice, "Visit with Kim Placeholder", "PLACEHOLDER K"),
            mind
        );
        assert_eq!(
            by("Example Health", "Kim Placeholder, LCSW", "PLACEHOLDER K"),
            mind
        );
        let use_of = Some("substance_use");
        assert_eq!(by("Kim Placeholder, CADC", "Bill", "PLACEHOLDER K"), use_of);
        // A name with no initial, degree or comma can be a place's.
        assert_eq!(by("Kim Placeholder, LCSW", "Bill", "Kim Placeholder"), mind);
        assert_eq!(
            by(practice, "Visit with Kim Placeholder", "Kim Placeholder"),
            None
        );
        assert_eq!(
            by(
                "Example Pediatrics",
                "Psychotherapy letter",
                "Example Pediatrics"
            ),
            None
        );
        assert_eq!(
            by(
                "Kim Placeholder, MD",
                "Psychotherapy letter",
                "K PLACEHOLDER"
            ),
            None
        );
        // Another surname, another initial, and no initial at all.
        for provider in ["K SAMPLE", "J PLACEHOLDER", "PLACEHOLDER", "K P"] {
            assert_eq!(
                by("Kim Placeholder, LCSW", "Bill", provider),
                None,
                "{provider}"
            );
        }
        // A practice of that surname, and one that says nothing.
        assert_eq!(by("Placeholder Counseling", "Bill", "C PLACEHOLDER"), None);
        assert_eq!(by("Kim Placeholder, MD", "Bill", "K PLACEHOLDER"), None);
        assert_eq!(
            by(
                "Kim Placeholder, LCSW",
                "Bill",
                "K Placeholder Medical Group"
            ),
            None
        );
        assert_eq!(area(paper(None, "Bill"), "K PLACEHOLDER"), None);
    }

    /// The person's items of days, each `(id, kind, who, day)`.
    fn items(each: &[(&str, &str, &str, &str)]) -> Vec<Record> {
        let item = |(id, kind, who, day): &(&str, &str, &str, &str)| {
            let mut r = claim(who);
            (r.id, r.kind, r.date) = (id.to_string(), kind.to_string(), Some(day.to_string()));
            if *kind == "encounter" {
                r.fields = json!({"practitioner": who}).as_object().unwrap().clone();
            }
            r
        };
        each.iter().map(item).collect()
    }

    /// Papers of days: `(organization, type, day)`.
    fn dated(each: &[(&str, &str, &str)]) -> Vec<Document> {
        let one = |(organization, ty, day): &(&str, &str, &str)| {
            let mut d = paper(Some(organization), "Statement");
            (d.doc_type, d.date) = (Some(ty.to_string()), Some(day.to_string()));
            d
        };
        each.iter().map(one).collect()
    }

    fn of(records: Vec<Record>, papers: Vec<Document>, r: &Record) -> Option<Named> {
        let mut idx = PersonIndex::new("p_test");
        (idx.records, idx.documents) = (records, papers);
        named(&idx).named(r, false)
    }

    const COUNSELING: &str = "Example Counseling";
    const MIND: &str = "mental_health";

    /// The day of a practice's paper holds each clinician's claim of that day.
    /// A clinician is linked to the practice, and so their items of other days,
    /// by two such days, or by one with no other clinician's claim.
    #[test]
    fn a_practices_paper_holds_its_day_and_a_clinician_it_links() {
        let day = |n: u8| format!("2024-10-0{n}");
        let (d1, d2, d9) = (day(1), day(2), day(9));
        let linked = Some(Named {
            area: MIND,
            learned: Learned::Linked,
        });
        let of_day = Some(Named {
            area: MIND,
            learned: Learned::Day,
        });
        let other = |who: &str| items(&[("r_otherdayaaaaaaaaaaaa", "claim", who, &d9)]).remove(0);
        // The only clinician of the day.
        let alone = items(&[("r_billedaaaaaaaaaaaaaa", "claim", "K PLACEHOLDER", &d1)]);
        let paper = || dated(&[(COUNSELING, "bill", &d1)]);
        assert_eq!(
            of(alone.clone(), paper(), &other("PLACEHOLDER, KIM")),
            linked
        );
        assert_eq!(of(alone.clone(), paper(), &other("J PLACEHOLDER")), None);
        for (organization, ty) in [
            ("Example Pediatrics", "bill"),
            (COUNSELING, "prescription"),
            ("Example Counseling Pharmacy", "receipt"),
        ] {
            let papers = dated(&[(organization, ty, &d1)]);
            assert_eq!(
                of(alone.clone(), papers, &other("K PLACEHOLDER")),
                None,
                "{organization}"
            );
        }
        assert_eq!(
            of(
                alone.clone(),
                dated(&[(COUNSELING, "bill", &d2)]),
                &other("K PLACEHOLDER")
            ),
            None
        );
        // Two clinicians of one day: their claims of that day alone.
        let two = items(&[
            ("r_billedaaaaaaaaaaaaaa", "claim", "K PLACEHOLDER", &d1),
            ("r_sameaaaaaaaaaaaaaaaa", "claim", "J SAMPLE", &d1),
        ]);
        assert_eq!(of(two.clone(), paper(), &two[1]), of_day);
        assert_eq!(of(two.clone(), paper(), &other("K PLACEHOLDER")), None);
        assert_eq!(of(two.clone(), paper(), &other("SAMPLE, JO")), None);
        // A visit of the day is no claim: the day holds it not, and it is no
        // other clinician's claim.
        let visit = items(&[
            ("r_billedaaaaaaaaaaaaaa", "claim", "K PLACEHOLDER", &d1),
            ("r_visitaaaaaaaaaaaaaaa", "encounter", "Jo Sample", &d1),
        ]);
        assert_eq!(of(visit.clone(), paper(), &visit[1]), None);
        assert_eq!(of(visit.clone(), paper(), &other("K PLACEHOLDER")), linked);
        // Two days the clinician shares link them both.
        let mut shared = two.clone();
        shared.extend(items(&[
            ("r_billedbaaaaaaaaaaaaa", "claim", "K PLACEHOLDER", &d2),
            ("r_samebaaaaaaaaaaaaaaa", "claim", "SAMPLE, JO", &d2),
        ]));
        let papers = || {
            dated(&[
                (COUNSELING, "bill", &d1),
                (COUNSELING, "visit_summary", &d2),
            ])
        };
        assert_eq!(of(shared.clone(), papers(), &other("SAMPLE, J")), linked);
        assert_eq!(
            of(shared.clone(), papers(), &other("K PLACEHOLDER")),
            linked
        );
        // Two days of two areas link no one.
        let two_areas = dated(&[
            (COUNSELING, "bill", &d1),
            ("Example Fertility Clinic", "bill", &d2),
        ]);
        assert_eq!(of(shared.clone(), two_areas, &other("SAMPLE, J")), None);
        // A paper that names them is surer than a link.
        let mut named_too = papers();
        named_too.extend(dated(&[("Kim Placeholder, LCSW", "letter", &d9)]));
        let by_paper = Some(Named {
            area: MIND,
            learned: Learned::Paper,
        });
        assert_eq!(of(shared, named_too, &other("K PLACEHOLDER")), by_paper);
    }

    fn mark(printed: &str, people: &[(&str, &str)], item: &str) -> Opened {
        let reading = |(surname, initial): &(&str, &str)| Reading {
            surname: surname.to_string(),
            initial: initial.to_string(),
        };
        Opened {
            id: "o_openedaaaaaaaaaaaaaa".into(),
            printed: Some(printed.into()),
            given: false,
            people: people.iter().map(reading).collect(),
            item: item.into(),
            at: "2024-12-01T00:00:00Z".into(),
            by: None,
            other: Map::new(),
        }
    }

    /// What the household opened holds nothing back: on the day, by a link or
    /// by a paper, whatever the form of the name, one mark for each clinician.
    #[test]
    fn a_clinician_the_household_opened_is_held_by_nothing() {
        let d1 = "2024-10-01";
        let alone = items(&[("r_billedaaaaaaaaaaaaaa", "claim", "K PLACEHOLDER", d1)]);
        let mut papers = dated(&[(COUNSELING, "bill", d1)]);
        papers.push(paper(Some("Kim Placeholder, LCSW"), "Bill"));
        let mut idx = PersonIndex::new("p_test");
        (idx.records, idx.documents) = (alone.clone(), papers);
        let k = || BTreeSet::from([("PLACEHOLDER".to_string(), 'K')]);
        let tied = named(&idx).tied(&alone[0]);
        assert_eq!(
            tied,
            BTreeMap::from([("K PLACEHOLDER".to_string(), (true, k()))])
        );
        // A payer's cut of the surname is held as the whole one is.
        let cut = claim("K PLACEHOL");
        assert!(named(&idx).named(&cut, true).is_some());
        let opened = mark("K PLACEHOLDER", &[("PLACEHOLDER", "K")], &alone[0].id);
        let open = Clinicians::of(&idx, std::slice::from_ref(&opened));
        for r in [&alone[0], &claim("PLACEHOLDER, KIM"), &cut] {
            assert_eq!(open.named(r, true), None, "{r:?}");
        }
        assert!(opened.of(&cut));
        // Another of that surname is not.
        assert!(open.named(&claim("PLACEHOLDER, JO"), true).is_none());
        let mut idx2 = idx.clone();
        idx2.documents
            .push(paper(Some("Jo Placeholder, LCSW"), "Bill"));
        let open = Clinicians::of(&idx2, std::slice::from_ref(&opened));
        assert!(open.named(&claim("PLACEHOLDER, JO"), true).is_some());
        assert_eq!(printed(&alone[0]), k());
    }

    /// A claim a day holds by a name spelled out is opened by one mark, of the
    /// first way the name reads, and that opens no one else of that day.
    #[test]
    fn a_name_a_day_holds_is_opened_by_the_way_it_reads_first() {
        let d1 = "2024-10-01";
        let two = items(&[
            ("r_billedaaaaaaaaaaaaaa", "claim", "K PLACEHOLDER", d1),
            ("r_sameaaaaaaaaaaaaaaaa", "claim", "Jo Sample MD", d1),
        ]);
        let mut idx = PersonIndex::new("p_test");
        (idx.records, idx.documents) = (two.clone(), dated(&[(COUNSELING, "bill", d1)]));
        let first = BTreeSet::from([("SAMPLE".to_string(), 'J')]);
        let tied = named(&idx).tied(&two[1]);
        assert_eq!(
            tied,
            BTreeMap::from([("Jo Sample MD".to_string(), (true, first))])
        );
        let opened = mark("Jo Sample MD", &[("SAMPLE", "J")], &two[1].id);
        let open = Clinicians::of(&idx, &[opened]);
        assert_eq!(open.named(&two[1], true), None);
        let day = Some(Named {
            area: MIND,
            learned: Learned::Day,
        });
        assert_eq!(open.named(&two[0], true), day);
    }

    /// What a source gave of a claim of the day holds all that is shown does,
    /// and more: a claim that printed a place, then or now, ends no link.
    #[test]
    fn what_a_claim_was_given_ends_no_link() {
        let (d1, d9) = ("2024-10-01", "2024-10-09");
        let linked = Some(Named {
            area: MIND,
            learned: Learned::Linked,
        });
        let with = |shown: &str, gave: &str| {
            let mut all = items(&[
                ("r_billedaaaaaaaaaaaaaa", "claim", "K PLACEHOLDER", d1),
                ("r_sameaaaaaaaaaaaaaaaa", "claim", shown, d1),
                ("r_otherdayaaaaaaaaaaaa", "claim", "PLACEHOLDER, KIM", d9),
            ]);
            let mut fields = Map::new();
            fields.insert("provider".into(), gave.into());
            all[1].given = vec![given::Given {
                fields,
                ..Default::default()
            }];
            let mut idx = PersonIndex::new("p_test");
            (idx.records, idx.documents) = (all.clone(), dated(&[(COUNSELING, "bill", d1)]));
            let c = named(&idx);
            (c.named(&all[2], false), c.named(&all[2], true))
        };
        // A place now, another clinician before.
        assert_eq!(with("Example Walk-in", "J SAMPLE"), (linked, linked));
        // A clinician only a source gave is tied as not shown.
        let mut gone = claim("Example Walk-in");
        let mut fields = Map::new();
        fields.insert("provider".into(), "PLACEHOLDER, KIM".into());
        gone.given = vec![given::Given {
            fields,
            ..Default::default()
        }];
        let mut idx = PersonIndex::new("p_test");
        idx.documents = vec![paper(Some("Kim Placeholder, LCSW"), "Bill")];
        let k = BTreeSet::from([("PLACEHOLDER".to_string(), 'K')]);
        let tied = BTreeMap::from([("PLACEHOLDER, KIM".to_string(), (false, k))]);
        assert_eq!(named(&idx).tied(&gone), tied);
        // Another clinician now, a place before.
        assert_eq!(with("J SAMPLE", "Example Walk-in"), (None, linked));
    }

    /// Each kind that prints a clinician: a visit's and a prescriber need no
    /// initial, as a claim's does.
    #[test]
    fn visits_prescriptions_procedures_and_results_print_one_too() {
        let mut idx = PersonIndex::new("p_test");
        idx.documents = vec![paper(Some(COUNSELING), "Visit with Kim Placeholder")];
        let clinicians = named(&idx);
        let one = |kind: &str, field: &str, who: &str| {
            let mut r = claim("unused");
            r.kind = kind.into();
            r.fields = json!({ field: who }).as_object().unwrap().clone();
            clinicians.area(&r, false)
        };
        let mind = Some(MIND);
        assert_eq!(one("encounter", "practitioner", "Kim Placeholder"), mind);
        assert_eq!(one("medication", "prescriber", "Kim Placeholder"), mind);
        assert_eq!(one("procedure", "performer", "K Placeholder"), mind);
        assert_eq!(one("result", "performer", "PLACEHOLDER, KIM"), mind);
        assert_eq!(one("report", "performer", "K. Placeholder"), mind);
        // A claim's provider and a performer can be a place's.
        assert_eq!(one("claim", "provider", "Kim Placeholder"), None);
        assert_eq!(one("procedure", "performer", "Kim Placeholder"), None);
        // An immunization prints a performer, and is judged by no clinician.
        assert_eq!(one("immunization", "performer", "K Placeholder"), None);
        assert_eq!(one("encounter", "location", "K Placeholder"), None);
    }

    /// What a paper said before still tells, and only where it is asked for.
    #[test]
    fn what_a_paper_said_before_still_tells() {
        let mut d = paper(Some("Example Health"), "Bill");
        d.given =
            serde_json::from_value(json!([{"organization": "Kim Placeholder, LCSW"}])).unwrap();
        let mut idx = PersonIndex::new("p_test");
        idx.documents = vec![d];
        let named = named(&idx);
        let r = claim("K PLACEHOLDER");
        assert_eq!(named.area(&r, false), None);
        assert_eq!(named.area(&r, true), Some("mental_health"));
    }

    /// So does what a claim printed before.
    #[test]
    fn what_a_claim_printed_before_still_tells() {
        let mut idx = PersonIndex::new("p_test");
        idx.documents = vec![paper(Some("Kim Placeholder, LCSW"), "Bill")];
        let mut r = claim("Example Health Group");
        r.given =
            serde_json::from_value(json!([{"fields": {"provider": "K PLACEHOLDER"}}])).unwrap();
        assert_eq!(named(&idx).area(&r, false), None);
        assert_eq!(named(&idx).area(&r, true), Some("mental_health"));
    }

    /// A surname of two letters, one that a payer cut short, and a degree that
    /// says the text is a person's.
    #[test]
    fn a_short_a_cut_and_a_titled_name_are_a_clinicians() {
        let mind = Some("mental_health");
        for provider in ["K OX", "OX K", "OX, KIM"] {
            for organization in ["Kim Ox, LCSW", "OX KIM LCSW", "Ox, Kim, LCSW"] {
                let d = paper(Some(organization), "Bill");
                assert_eq!(area(d, provider), mind, "{provider}, {organization}");
            }
            let d = paper(Some("Example Counseling"), "Visit with Kim Ox");
            assert_eq!(area(d, provider), mind, "{provider}");
        }
        for provider in ["K PLACEHOLD", "PLACEHOLDE, K", "PLACEHOLD K"] {
            let d = paper(Some("Kim Placeholder, LCSW"), "Bill");
            assert_eq!(area(d, provider), mind, "{provider}");
            let d = paper(Some("Example Counseling"), "Visit with Kim Placeholder");
            assert_eq!(area(d, provider), mind, "{provider}");
        }
        for provider in ["K PLACE", "J PLACEHOLD", "K PLACEHOLDS"] {
            let d = paper(Some("Kim Placeholder, LCSW"), "Bill");
            assert_eq!(area(d, provider), None, "{provider}");
        }
        let titled = || paper(Some("Example Counseling"), "Visit with Kim Placeholder");
        assert_eq!(area(titled(), "Kim Placeholder MSW"), mind);
        assert_eq!(area(titled(), "Kim Placeholder PA"), None);
    }
}
