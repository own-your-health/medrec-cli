//! Which of a person's sources a record or a document came from.
//!
//! A CLI, an organization and a FHIR base do not name a source: sources can
//! have all three alike and differ in their person, their patient or both. A
//! source is told apart by its person and by the patient its records were
//! imported as: the import run that filed a record says which. Where several
//! differently named sources are left, none is the one.

use std::collections::BTreeSet;

use crate::model::{ImportRun, PersonIndex, Provenance, Source};

fn base(b: Option<&str>) -> &str {
    b.unwrap_or("").trim_end_matches('/')
}

/// A person's own sources, and their import runs.
#[derive(Clone, Copy)]
pub struct Sources<'a> {
    person: &'a str,
    all: &'a [Source],
    runs: &'a [ImportRun],
}

impl<'a> Sources<'a> {
    /// Those of the index's person, out of the household's.
    pub fn of(sources: &'a [Source], idx: &'a PersonIndex) -> Sources<'a> {
        Sources {
            person: &idx.person,
            all: sources,
            runs: &idx.imports,
        }
    }

    /// The patient it was imported as: that of the import run that filed it,
    /// else of the one that read it. None for what no run did, and where runs
    /// of two patients could have.
    fn patient(&self, pv: &Provenance) -> Option<&'a str> {
        let cli = pv.source.cli.as_deref()?;
        let org = pv.source.organization_id.as_deref()?;
        let of = |is: &dyn Fn(&ImportRun) -> bool| -> BTreeSet<&'a str> {
            self.runs
                .iter()
                .filter(|r| r.cli == cli && r.organization == org && is(r))
                .map(|r| r.patient_id.as_str())
                .collect()
        };
        let filed = of(&|r| r.imported_at == pv.filed_at);
        let read = of(&|r| Some(&r.pulled_at) == pv.fetched_at.as_ref());
        let patients = match filed.len() {
            0 => read,
            1 => filed,
            _ => filed.intersection(&read).copied().collect(),
        };
        match patients.len() {
            1 => patients.first().copied(),
            _ => None,
        }
    }

    /// The person's own sources it can be from.
    fn can_be(&self, pv: &Provenance) -> Vec<&'a Source> {
        if pv.source.cli.is_none() || pv.source.organization_id.is_none() {
            return Vec::new();
        }
        let at = pv.source.fhir_base.as_deref();
        let patient = self.patient(pv);
        self.all
            .iter()
            .filter(|s| {
                s.person == self.person
                    && s.cli == pv.source.cli
                    && s.organization == pv.source.organization_id
                    && (at.is_none() || base(s.fhir_base.as_deref()) == base(at))
            })
            .filter(|s| patient.is_none() || s.patient_id.as_deref() == patient)
            .collect()
    }

    /// The source it came from, when only one can be.
    pub fn source(&self, pv: &Provenance) -> Option<&'a Source> {
        match self.can_be(pv).as_slice() {
            [s] => Some(s),
            _ => None,
        }
    }

    /// Its source's name, unless it is a records connection's own: the place
    /// of a visit whose records give none.
    pub fn place(&self, pv: &Provenance) -> Option<&'a str> {
        named(&self.can_be(pv)).filter(|n| !crate::plain::is_connection(n))
    }

    /// The place of what came from all of these: the one each gives. One
    /// that gives none is another place than the rest.
    pub fn place_of<'p>(&self, p: impl IntoIterator<Item = &'p Provenance>) -> Option<&'a str> {
        let mut places = p
            .into_iter()
            .filter(|pv| pv.source.cli.is_some())
            .map(|pv| self.place(pv));
        let first = places.next()??;
        places.all(|n| n == Some(first)).then_some(first)
    }

    /// The name it came through: its source's, else its CLI. Nothing else
    /// of the household's names it, so another person's practice never does.
    pub fn label(&self, pv: &Provenance) -> Option<String> {
        named(&self.can_be(pv))
            .map(str::to_string)
            .or_else(|| pv.source.cli.clone())
    }

    /// The same for what came from one practice: the CLI, organization, FHIR
    /// base and patient. Without a patient it equals only others without.
    pub fn key(&self, pv: &Provenance) -> String {
        let s = &pv.source;
        let patient = self
            .patient(pv)
            .or_else(|| self.source(pv)?.patient_id.as_deref());
        format!(
            "{}|{}|{}|{}",
            s.cli.as_deref().unwrap_or(""),
            s.organization_id.as_deref().unwrap_or(""),
            base(s.fhir_base.as_deref()),
            patient.unwrap_or("")
        )
    }

    /// [`Sources::key`] of what a record or document was first filed from.
    pub fn first_key(&self, p: &[Provenance]) -> Option<String> {
        Some(self.key(p.first()?))
    }
}

fn name_of(s: &Source) -> Option<&str> {
    s.organization_name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
}

/// The name of whichever of these sources it is: the one they all have.
/// Where they differ, or one has none, no name is known to be its own.
fn named<'a>(sources: &[&'a Source]) -> Option<&'a str> {
    let first = name_of(sources.first()?)?;
    sources
        .iter()
        .all(|s| name_of(s) == Some(first))
        .then_some(first)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Household;
    use serde_json::{json, Value};

    const BASE: &str = "https://records.example.test/fhir/r4";
    const ALEX: &str = "p_alexaaaaaaaaaaaaaaaa";
    const ROBIN: &str = "p_robinaaaaaaaaaaaaaaa";
    /// A source's name that names no place.
    const WIDE: &str = "Example Records (all practices)";

    /// Four sources alike in CLI, organization and base: every person with
    /// every patient of theirs.
    fn household() -> Household {
        let source = |id: &str, person: &str, patient: &str, name: &str| {
            json!({"id": id, "kind": "fhir", "person": person, "cli": "fakeprov",
                "organization": "shared", "organization_name": name,
                "fhir_base": format!("{BASE}//"), "patient_id": patient})
        };
        serde_json::from_value(json!({
            "schema": "medrec-household/v1", "folder_id": "f_exampleaaaaaaaaaaaaa", "format": 1, "rev": 1,
            "created_at": "2024-01-01T00:00:00Z", "people": [], "organizations": [
                {"id": "g_northaaaaaaaaaaaaaaa", "name": "Sample Clinic North", "fhir_base": BASE}],
            "sources": [
                source("s_robineastaaaaaaaaaaa", ROBIN, "east", "Sample Clinic East"),
                source("s_alexnorthaaaaaaaaaaa", ALEX, "north", "Sample Clinic North"),
                source("s_robinwestaaaaaaaaaaa", ROBIN, "west", "Sample Clinic West"),
                source("s_alexsouthaaaaaaaaaaa", ALEX, "south", "Sample Clinic South")],
        }))
        .unwrap()
    }

    /// The household with some of the person's own sources only, renamed.
    fn with_own(own: &[(&str, Option<&str>)]) -> Household {
        let mut h = household();
        h.sources.retain(|s| {
            s.person != ALEX || own.iter().any(|(p, _)| s.patient_id.as_deref() == Some(*p))
        });
        for (patient, name) in own {
            let s = h.sources.iter_mut();
            let mut s = s.filter(|s| s.person == ALEX && s.patient_id.as_deref() == Some(*patient));
            s.next().unwrap().organization_name = name.map(str::to_string);
        }
        h
    }

    /// Runs as `(imported_at, pulled_at, patient)`.
    fn index(runs: &[(&str, &str, &str)]) -> PersonIndex {
        let mut idx = PersonIndex::new(ALEX);
        idx.imports = runs
            .iter()
            .map(|(imported, pulled, patient)| {
                serde_json::from_value(json!({"id": "i_runaaaaaaaaaaaaaaaaa", "source_dir": "",
                    "stream_sha256": "", "stream_bytes": 0, "cli": "fakeprov", "cli_version": "0.0.1",
                    "organization": "shared", "patient_id": patient, "pulled_at": pulled,
                    "imported_at": imported, "counts": {}, "records_added": 0,
                    "records_updated": 0, "records_unchanged": 0, "resources_written": 0,
                    "resources_unchanged": 0, "documents_added": 0}))
                .unwrap()
            })
            .collect();
        idx
    }

    fn pv(filed: &str, fetched: Option<&str>) -> Provenance {
        let mut v = json!({"source": {"kind": "fhir", "cli": "fakeprov", "organization_id": "shared",
                "fhir_base": BASE}, "method": "fhir-api", "filed_at": filed, "filed_by": "medrec 0.9.6"});
        if let Some(f) = fetched {
            v["fetched_at"] = Value::from(f);
        }
        serde_json::from_value(v).unwrap()
    }

    // Times a run filed at, and times one read at.
    const FILED: &str = "2024-06-11T08:00:00Z";
    const FILED_LATER: &str = "2024-06-12T08:00:00Z";
    const READ: &str = "2024-06-11T07:00:00Z";
    const READ_LATER: &str = "2024-06-12T07:00:00Z";
    const NO_RUN: &str = "2024-06-01T00:00:00Z";

    /// Step 1 of the contract's "Which source".
    #[test]
    fn the_patient_is_the_runs_that_filed_it_else_the_runs_that_read_it() {
        let h = household();
        let two = [(FILED, READ, "north"), (FILED_LATER, READ_LATER, "south")];
        let at_once = [(FILED, READ, "north"), (FILED, READ_LATER, "south")];
        let read_by_another = [
            (FILED, READ, "north"),
            (FILED, READ, "south"),
            (FILED_LATER, READ_LATER, "south"),
        ];
        for (runs, filed, fetched, patient) in [
            // One patient filed it, whoever read it last.
            (&two[..], FILED, None, Some("north")),
            (&two, FILED_LATER, Some(READ), Some("south")),
            // Several did: the one that a run, any run, read it as.
            (&at_once, FILED, None, None),
            (&at_once, FILED, Some(READ_LATER), Some("south")),
            (&read_by_another, FILED, Some(READ_LATER), Some("south")),
            (&read_by_another, FILED, Some(READ), None),
            // None did: the one it was read as.
            (&two, NO_RUN, Some(READ_LATER), Some("south")),
            (&two, NO_RUN, None, None),
            (&[], FILED, Some(READ), None),
        ] {
            let idx = index(runs);
            let s = Sources::of(&h.sources, &idx);
            let pv = pv(filed, fetched);
            assert_eq!(s.patient(&pv), patient, "{runs:?} {filed} {fetched:?}");
            let place = patient.map(|p| match p {
                "north" => "Sample Clinic North",
                _ => "Sample Clinic South",
            });
            assert_eq!(s.place(&pv), place);
            assert_eq!(s.label(&pv).as_deref(), place.or(Some("fakeprov")));
        }
    }

    /// Step 2.
    #[test]
    fn its_sources_are_the_persons_own_with_its_key_and_patient() {
        let h = household();
        let idx = index(&[(FILED, READ, "south"), (FILED_LATER, READ, "east")]);
        let s = Sources::of(&h.sources, &idx);
        let own = pv(FILED, None);
        assert_eq!(s.source(&own).unwrap().id, "s_alexsouthaaaaaaaaaaa");
        // A patient none of the person's own sources has, another person's
        // though it is.
        let other = pv(FILED_LATER, None);
        assert!(s.source(&other).is_none());
        assert_eq!(s.label(&other).as_deref(), Some("fakeprov"));
        // Another base is another source's; with no base, any is its own.
        let mut at = pv(FILED, None);
        at.source.fhir_base = Some("https://other.example.test/fhir/r4".into());
        assert!(s.source(&at).is_none());
        at.source.fhir_base = None;
        assert_eq!(s.source(&at).unwrap().id, "s_alexsouthaaaaaaaaaaa");
        // What no CLI gave has a key, and no source.
        let mut scan = pv(NO_RUN, None);
        scan.source.cli = None;
        scan.source.organization_id = None;
        scan.source.fhir_base = None;
        assert_eq!(s.key(&scan), "|||");
        assert_eq!(s.label(&scan), None);
        assert!(s.source(&scan).is_none());
        // Nobody else's source names what is theirs.
        let none = with_own(&[]);
        let s = Sources::of(&none.sources, &idx);
        assert_eq!(s.place(&own), None);
        assert_eq!(s.label(&own).as_deref(), Some("fakeprov"));
    }

    /// Steps 3 and 4.
    #[test]
    fn its_name_is_the_one_its_sources_all_have() {
        let idx = index(&[]);
        let unknown = pv(NO_RUN, None);
        let north = Some("Sample Clinic North");
        for (own, name, place) in [
            (vec![("north", north)], north, north),
            (vec![("north", north), ("south", north)], north, north),
            (
                vec![("north", north), ("south", Some(" Sample Clinic North "))],
                north,
                north,
            ),
            (
                vec![("north", north), ("south", Some("Sample Clinic South"))],
                None,
                None,
            ),
            (vec![("north", north), ("south", None)], None, None),
            (vec![("north", None), ("south", north)], None, None),
            (vec![("north", north), ("south", Some(" "))], None, None),
            (vec![("north", north), ("south", Some(WIDE))], None, None),
            // A name that names no place is a label all the same.
            (vec![("north", Some(WIDE))], Some(WIDE), None),
            (vec![], None, None),
        ] {
            let h = with_own(&own);
            let s = Sources::of(&h.sources, &idx);
            assert_eq!(s.place(&unknown), place, "{own:?}");
            assert_eq!(
                s.label(&unknown).as_deref(),
                name.or(Some("fakeprov")),
                "{own:?}"
            );
            assert_eq!(s.source(&unknown).is_some(), own.len() == 1, "{own:?}");
        }
    }

    #[test]
    fn what_came_from_two_sources_has_one_place_only_if_they_agree() {
        let idx = index(&[(FILED, READ, "north"), (FILED_LATER, READ_LATER, "south")]);
        let (first, second) = (pv(FILED, None), pv(FILED_LATER, None));
        let unknown = pv(NO_RUN, None);
        let h = household();
        let s = Sources::of(&h.sources, &idx);
        assert_eq!(s.place_of([&first]), Some("Sample Clinic North"));
        assert_eq!(s.place_of([&first, &first]), s.place(&first));
        assert_ne!(s.key(&first), s.key(&second));
        for pair in [[&first, &second], [&first, &unknown], [&unknown, &first]] {
            assert_eq!(s.place_of(pair), None);
        }
        // One that names no place is another place than the rest.
        let wide = with_own(&[
            ("north", Some("Sample Clinic North")),
            ("south", Some(WIDE)),
        ]);
        let s = Sources::of(&wide.sources, &idx);
        for pair in [[&first, &second], [&second, &first]] {
            assert_eq!(s.place_of(pair), None);
        }
        // The same source, read again by another's run.
        let again = pv(FILED, Some(READ_LATER));
        assert_eq!(s.key(&again), s.key(&first));
    }
}
