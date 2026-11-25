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
    /// What it says at its head.
    texts: Vec<String>,
    /// The clinician whose bill it is (N5), as the claims print them.
    people: BTreeSet<(String, char)>,
    /// Only by what a source gave, that the paper does not show.
    gave: bool,
}

impl Said {
    /// A paper's organization and its title, when either says an area. What
    /// a paper says inside is not read: it names others too.
    fn paper(d: &Document, gave: bool) -> Option<Said> {
        let texts: Vec<String> = [&d.organization, &d.title]
            .into_iter()
            .filter_map(|t| t.clone())
            .collect();
        let area = texts.iter().find_map(|t| private::place_area(t))?;
        Some(Said {
            area,
            texts,
            people: BTreeSet::new(),
            gave,
        })
    }

    /// The practice an event's bill says a clinician is of.
    fn alias(a: &Value) -> Option<Said> {
        Some(Said {
            area: private::place_area(a["organization"].as_str()?)?,
            texts: Vec::new(),
            people: nppes::people(a["clinician"].as_str()?),
            gave: false,
        })
    }

    /// Whether it names one of these people.
    fn names(&self, people: &BTreeSet<(String, char)>) -> bool {
        !self.people.is_disjoint(people) || self.texts.iter().any(|t| names(t, people))
    }
}

/// Whether a short text names one of these people, its words in either order.
fn names(text: &str, people: &BTreeSet<(String, char)>) -> bool {
    if labels::prints_name(text, people) {
        return true;
    }
    if events::says_organization(text) {
        return false;
    }
    let whole = nppes::readings(text).into_iter();
    whole
        .filter(|(_, given)| private::place_area(&given[0]).is_none())
        .filter_map(|(last, given)| Some((last, given[0].chars().next()?)))
        .any(|read| read.0.len() >= labels::SURNAME && people.contains(&read))
}

pub(crate) struct Clinicians(Vec<Said>);

impl Clinicians {
    /// Of the person's papers, and of their events' aliases.
    pub(crate) fn of(idx: &PersonIndex, events: &[&Value]) -> Clinicians {
        let mut each: Vec<Said> = Vec::new();
        for d in &idx.documents {
            each.extend(Said::paper(d, false));
            each.extend(given::papers_as_given(d).filter_map(|x| Said::paper(&x, true)));
        }
        let aliases = events.iter().filter_map(|e| e["aliases"].as_array());
        each.extend(aliases.flatten().filter_map(Said::alias));
        Clinicians(each)
    }

    /// The area of the clinician a claim prints, by a paper that names them:
    /// as both are shown, or also as a source gave either.
    pub(crate) fn area(&self, r: &Record, gave: bool) -> Option<&'static str> {
        if self.0.is_empty() || r.kind != "claim" {
            return None;
        }
        let of = |r: &Record| {
            let people = printed(r)?;
            let papers = self.0.iter().filter(|s| gave || !s.gave);
            papers
                .into_iter()
                .find(|s| s.names(&people))
                .map(|s| s.area)
        };
        let given = || given::as_given(r).find_map(|x| of(&x));
        of(r).or_else(|| gave.then(given).flatten())
    }
}

/// The people a claim's printed provider can be.
fn printed(r: &Record) -> Option<BTreeSet<(String, char)>> {
    let text = r.fields.get("provider").and_then(Value::as_str)?;
    let people = nppes::people(text);
    (!events::says_organization(text) && !people.is_empty()).then_some(people)
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

    fn area(d: Document, provider: &str) -> Option<&'static str> {
        let mut idx = PersonIndex::new("p_test");
        idx.documents = vec![d];
        Clinicians::of(&idx, &[]).area(&claim(provider), false)
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
            let events = json!({"aliases": [{"clinician": "K PLACEHOLDER",
                "organization": organization, "rule": "N5"}]});
            let idx = PersonIndex::new("p_test");
            Clinicians::of(&idx, &[&events]).area(&claim(provider), false)
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
        assert!(Clinicians::of(&idx, &[]).area(&r, false).is_some());
        r.kind = "result".into();
        assert!(Clinicians::of(&idx, &[]).area(&r, false).is_none());
    }

    /// What a paper said before still tells, and only where it is asked for.
    #[test]
    fn what_a_paper_said_before_still_tells() {
        let mut d = paper(Some("Example Health"), "Bill");
        d.given =
            serde_json::from_value(json!([{"organization": "Kim Placeholder, LCSW"}])).unwrap();
        let mut idx = PersonIndex::new("p_test");
        idx.documents = vec![d];
        let named = Clinicians::of(&idx, &[]);
        let r = claim("K PLACEHOLDER");
        assert_eq!(named.area(&r, false), None);
        assert_eq!(named.area(&r, true), Some("mental_health"));
    }
}
