//! The clinicians a person's papers name beside a practice or a credential
//! that says what they treat, and the claims that print no more of one than a
//! surname and an initial. A likely match: it only holds a claim back.

use std::collections::BTreeSet;

use serde_json::Value;

use crate::model::{Document, PersonIndex, Record};
use crate::{events, given, labels, nppes, private};

/// Who a paper says is of an area.
struct Said {
    area: &'static str,
    /// What it says at its head: its organization, or its title.
    text: String,
    /// Whether that is the organization, and says the area itself.
    organization: bool,
    /// The clinician whose bill it is (N5), as the claims print them.
    people: BTreeSet<(String, char)>,
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
                people: BTreeSet::new(),
                gave,
            })
        };
        let each = [
            said(&d.organization, of, true),
            said(&d.title, of.or(titled), false),
        ];
        each.into_iter().flatten().collect()
    }

    /// The practice a bill says a clinician is of.
    fn practice(clinician: &str, organization: &str, gave: bool) -> Option<Said> {
        Some(Said {
            area: private::place_area(organization)?,
            text: String::new(),
            organization: false,
            people: nppes::people(clinician),
            gave,
        })
    }

    /// Whether it names one of these people. A name that may be a place's is
    /// looked for in an organization alone.
    fn names(&self, printed: &Printed) -> bool {
        let people = &printed.people;
        if !printed.a_person && !self.organization {
            return false;
        }
        let known = |of: &(String, char)| people.iter().any(|p| same_person(of, p));
        self.people.iter().any(known) || names(&self.text, people)
    }
}

/// The least of a surname that one cut short keeps: a payer cuts a name to 20
/// characters.
const CUT: usize = 8;

fn same_surname(a: &str, b: &str) -> bool {
    let cut = |a: &str, b: &str| a.len() >= CUT && b.starts_with(a);
    a.len() > 1 && (a == b || cut(a, b) || cut(b, a))
}

fn same_person(a: &(String, char), b: &(String, char)) -> bool {
    a.1 == b.1 && same_surname(&a.0, &b.0)
}

/// The people a claim's provider can be.
struct Printed {
    people: BTreeSet<(String, char)>,
    /// Whether the text is a person's: by an initial, a degree, or a comma
    /// after the surname. `Kim Placeholder` can be a place.
    a_person: bool,
}

impl Printed {
    fn of(r: &Record) -> Option<Printed> {
        let text = provider(r)?;
        let people = nppes::people(text);
        if events::says_organization(text) || people.is_empty() {
            return None;
        }
        let mut words = text.split(|c: char| !c.is_alphanumeric());
        let initial = words.any(|w| w.chars().count() == 1);
        // `PA` is a practice's as often as a degree.
        let degree = |w: &str| w != "PA" && nppes::around_a_name(w);
        let mut words = text.split(|c: char| c.is_whitespace() || c == ',');
        let a_person = initial
            || text.contains(',')
            || labels::a_person("provider", text)
            || words.any(|w| degree(&nppes::fold(w)));
        Some(Printed { people, a_person })
    }
}

fn provider(r: &Record) -> Option<&str> {
    r.fields.get("provider").and_then(Value::as_str)
}

/// Whether a short text names one of these people, its words in either order.
fn names(text: &str, people: &BTreeSet<(String, char)>) -> bool {
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

/// Whether a claim is of a day: its billable period, else its date.
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

/// The practice that a paper of a visit's day (one D3 joins, above) says it is,
/// for each clinician a claim of that day prints, as each is shown and as a
/// source gave it. Every one of them: a day with two visits holds both back.
fn practices(idx: &PersonIndex) -> Vec<Said> {
    let is = |text: &&str| events::is_clinician(text);
    let claims: Vec<(&Record, Vec<(String, bool)>)> = idx
        .records
        .iter()
        .filter(|r| r.kind == "claim")
        .map(|r| {
            let shown = provider(r).filter(is).map(|t| (t.to_string(), false));
            let given = given::as_given(r)
                .filter_map(|x| provider(&x).filter(is).map(|t| (t.to_string(), true)));
            (r, shown.into_iter().chain(given).collect())
        })
        .collect();
    let mut out = Vec::new();
    for d in &idx.documents {
        let ty = d.doc_type.as_deref().unwrap_or("other");
        let day = d.date.as_deref().and_then(|t| t.get(..10));
        let (true, Some(day)) = (events::of_the_days_visit(ty), day) else {
            continue;
        };
        let of_day: Vec<&(&Record, Vec<(String, bool)>)> =
            claims.iter().filter(|(r, _)| of_day(r, day)).collect();
        if of_day.is_empty() {
            continue;
        }
        let given = given::papers_as_given(d).map(|x| (x.organization, true));
        for (organization, paper_gave) in
            std::iter::once((d.organization.clone(), false)).chain(given)
        {
            let Some(organization) = organization else {
                continue;
            };
            let pharmacy = organization.to_lowercase().contains("pharmacy");
            if pharmacy || crate::plain::is_connection(&organization) {
                continue;
            }
            for (_, printed) in &of_day {
                for (text, gave) in printed {
                    out.extend(Said::practice(text, &organization, paper_gave || *gave));
                }
            }
        }
    }
    out
}

pub(crate) struct Clinicians(Vec<Said>);

impl Clinicians {
    /// Of the person's own papers, and of the practices those of a visit's day
    /// say its clinicians are of, as each is shown and as a source gave it.
    pub(crate) fn of(idx: &PersonIndex) -> Clinicians {
        let mut each: Vec<Said> = Vec::new();
        for d in &idx.documents {
            each.extend(Said::paper(d, false));
            each.extend(given::papers_as_given(d).flat_map(|x| Said::paper(&x, true)));
        }
        each.extend(practices(idx));
        Clinicians(each)
    }

    /// The area of the clinician a claim prints, by a paper that names them:
    /// as both are shown, or also as a source gave either.
    pub(crate) fn area(&self, r: &Record, gave: bool) -> Option<&'static str> {
        if self.0.is_empty() || r.kind != "claim" {
            return None;
        }
        let of = |r: &Record| {
            let printed = Printed::of(r)?;
            let mut papers = self.0.iter().filter(|s| gave || !s.gave);
            papers.find(|s| s.names(&printed)).map(|s| s.area)
        };
        let given = || given::as_given(r).find_map(|x| of(&x));
        of(r).or_else(|| gave.then(given).flatten())
    }
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
        Clinicians::of(idx)
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

    /// By the practice whose paper is of a day the clinician's claim is of:
    /// each clinician of that day's, whatever the events make of them.
    #[test]
    fn a_claim_is_of_the_practice_a_paper_of_its_day_is() {
        let dated = |id: &str, provider: &str, day: &str| {
            let mut r = claim(provider);
            r.id = id.into();
            r.date = Some(day.into());
            r
        };
        let of = |organization: &str, ty: &str, day: &str, provider: &str| {
            let mut d = paper(Some(organization), "Statement");
            (d.doc_type, d.date) = (Some(ty.into()), Some("2024-10-01".into()));
            let mut idx = PersonIndex::new("p_test");
            idx.documents = vec![d];
            idx.records = vec![
                dated("r_billedaaaaaaaaaaaaaa", "K PLACEHOLDER", day),
                dated("r_sameaaaaaaaaaaaaaaaa", "J SAMPLE", "2024-10-01"),
            ];
            named(&idx).area(&claim(provider), false)
        };
        let mind = Some("mental_health");
        let counseling = "Example Counseling";
        assert_eq!(
            of(counseling, "bill", "2024-10-01", "PLACEHOLDER, KIM"),
            mind
        );
        assert_eq!(
            of(counseling, "visit_summary", "2024-10-01", "PLACEHOLDER, K"),
            mind
        );
        // The other clinician of that day is held back too.
        assert_eq!(of(counseling, "bill", "2024-10-01", "SAMPLE, JO"), mind);
        assert_eq!(
            of(counseling, "bill", "2024-10-02", "PLACEHOLDER, KIM"),
            None
        );
        assert_eq!(of(counseling, "bill", "2024-10-01", "J PLACEHOLDER"), None);
        assert_eq!(
            of(counseling, "prescription", "2024-10-01", "K PLACEHOLDER"),
            None
        );
        assert_eq!(
            of("Example Pediatrics", "bill", "2024-10-01", "K PLACEHOLDER"),
            None
        );
        let pharmacy = "Example Counseling Pharmacy";
        assert_eq!(of(pharmacy, "receipt", "2024-10-01", "K PLACEHOLDER"), None);
    }

    /// Nothing but a claim: what else prints a name is judged by its own words.
    #[test]
    fn it_is_claims_only() {
        let mut idx = PersonIndex::new("p_test");
        idx.documents = vec![paper(Some("Kim Placeholder, LCSW"), "Bill")];
        let mut r = claim("K PLACEHOLDER");
        assert!(named(&idx).area(&r, false).is_some());
        r.kind = "result".into();
        assert!(named(&idx).area(&r, false).is_none());
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
