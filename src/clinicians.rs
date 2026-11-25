//! The clinicians a person's papers name beside a practice or a credential
//! that says what they treat, and the claims that print no more of one than a
//! surname and an initial. A likely match: it only holds a claim back.

use std::collections::{BTreeMap, BTreeSet};

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

/// The practices of the bills in events, as a source gave a bill's or the
/// provider of its one clinician's claim, and neither shows.
fn given_practices(idx: &PersonIndex, events: &Value) -> Vec<Said> {
    let records: BTreeMap<&str, &Record> = idx.records.iter().map(|r| (r.id.as_str(), r)).collect();
    let papers: BTreeMap<&str, &Document> =
        idx.documents.iter().map(|d| (d.id.as_str(), d)).collect();
    let mut out: Vec<Said> = Vec::new();
    for e in events["events"].as_array().into_iter().flatten() {
        let members = e["members"].as_array().into_iter().flatten();
        let own: Vec<&Value> = members.filter(|m| m.get("person").is_none()).collect();
        let id = |m: &Value, of: &str| m[of].as_str().map(str::to_string);
        let claims: Vec<&Record> = own
            .iter()
            .filter_map(|m| records.get(id(m, "record")?.as_str()).copied())
            .filter(|r| r.kind == "claim")
            .collect();
        let bills: Vec<&Document> = own
            .iter()
            .filter(|m| m["rule"] == "D3")
            .filter_map(|m| papers.get(id(m, "document")?.as_str()).copied())
            .collect();
        if bills.is_empty() {
            continue;
        }
        let is = |text: &&str| events::is_clinician(text);
        let shown: Vec<(&str, &str)> = claims
            .iter()
            .filter_map(|r| Some((r.id.as_str(), provider(r).filter(is)?)))
            .collect();
        let mut clinicians: Vec<(String, bool)> = match shown.as_slice() {
            [(_, one)] => vec![(one.to_string(), false)],
            _ => Vec::new(),
        };
        for r in &claims {
            if shown.iter().any(|(other, _)| *other != r.id) {
                continue;
            }
            for x in given::as_given(r) {
                let text = provider(&x).filter(is).map(str::to_string);
                clinicians.extend(text.map(|t| (t, true)));
            }
        }
        for d in bills {
            let given = given::papers_as_given(d).map(|x| (x.organization, true));
            for (organization, gave) in
                std::iter::once((d.organization.clone(), false)).chain(given)
            {
                let Some(organization) = organization else {
                    continue;
                };
                let each = clinicians.iter().filter(|(_, given)| gave || *given);
                out.extend(each.filter_map(|(c, _)| Said::practice(c, &organization, true)));
            }
        }
    }
    out
}

pub(crate) struct Clinicians(Vec<Said>);

impl Clinicians {
    /// Of the person's papers, and of the practices (N5) their events learned
    /// clinicians to be of, as a paper is shown and as a source gave it.
    pub(crate) fn of<'a>(
        idx: &PersonIndex,
        practices: impl Iterator<Item = [&'a str; 3]>,
        events: &Value,
    ) -> Clinicians {
        let mut each: Vec<Said> = Vec::new();
        for d in &idx.documents {
            each.extend(Said::paper(d, false));
            each.extend(given::papers_as_given(d).flat_map(|x| Said::paper(&x, true)));
        }
        for [clinician, organization, paper] in practices {
            each.extend(Said::practice(clinician, organization, false));
            let of = idx.documents.iter().find(|d| d.id == paper);
            let given = of.into_iter().flat_map(given::papers_as_given);
            let given = given.filter_map(|x| x.organization);
            each.extend(given.filter_map(|o| Said::practice(clinician, &o, true)));
        }
        each.extend(given_practices(idx, events));
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
        Clinicians::of(idx, std::iter::empty(), &Value::Null)
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

    /// By the practice whose bill is of the clinician's visit.
    #[test]
    fn a_claim_is_of_the_practice_its_clinician_bills_for() {
        let of = |organization: &str, provider: &str| {
            let idx = PersonIndex::new("p_test");
            let learned = ["K PLACEHOLDER", organization, "d_aaaaaaaaaaaaaaaaaaaa"];
            let named = Clinicians::of(&idx, [learned].into_iter(), &Value::Null);
            named.area(&claim(provider), false)
        };
        let mind = Some("mental_health");
        assert_eq!(of("Example Counseling", "PLACEHOLDER, KIM"), mind);
        assert_eq!(of("Example Counseling", "J PLACEHOLDER"), None);
        assert_eq!(of("Example Pediatrics", "PLACEHOLDER, KIM"), None);
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
