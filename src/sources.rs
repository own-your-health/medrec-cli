//! Which of a person's sources a record or a document came from.
//!
//! One CLI, organization and FHIR base can serve several practices and
//! several people (an aggregator's connection does), so those three alone
//! name no practice. A source is told apart by its person and by the patient
//! its records were imported as: the import run that filed a record says
//! which. Where several differently named sources are left, none is the one.

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
/// Where they differ, no name is known to be its own; a connection's name,
/// or none, differs from a practice's.
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

    /// One connection of two practices, for two people: Alex at both, Robin
    /// at one under the same key, whose source no record came from is last.
    fn household() -> Household {
        let source = |id: &str, person: &str, patient: &str, name: &str| {
            json!({"id": id, "kind": "fhir", "person": person, "cli": "athena",
                "organization": "all", "organization_name": name,
                "fhir_base": format!("{BASE}/"), "patient_id": patient})
        };
        serde_json::from_value(json!({
            "schema": "medrec-household/v1", "folder_id": "f_exampleaaaaaaaaaaaaa", "format": 1, "rev": 1,
            "created_at": "2026-01-01T00:00:00Z", "people": [], "organizations": [
                {"id": "g_firstaaaaaaaaaaaaaaa", "name": "Example Eye Center", "fhir_base": BASE},
                {"id": "g_secondaaaaaaaaaaaaaa", "name": "Sample Skin Studio", "fhir_base": BASE}],
            "sources": [
                source("s_alexeyeaaaaaaaaaaaaa", ALEX, "eye-1", "Example Eye Center"),
                source("s_alexstudioaaaaaaaaaa", ALEX, "studio-1", "Sample Skin Studio"),
                source("s_robinlastaaaaaaaaaaa", ROBIN, "last-1", "Placeholder Hearing Clinic")],
        }))
        .unwrap()
    }

    fn index(runs: &[(&str, &str)]) -> PersonIndex {
        let mut idx = PersonIndex::new(ALEX);
        idx.imports = runs
            .iter()
            .map(|(at, patient)| {
                serde_json::from_value(json!({"id": "i_runaaaaaaaaaaaaaaaaa", "source_dir": "",
                    "stream_sha256": "", "stream_bytes": 0, "cli": "athena", "cli_version": "0.6.0",
                    "organization": "all", "patient_id": patient, "pulled_at": format!("{at}:01Z"),
                    "imported_at": format!("{at}:30Z"), "counts": {}, "records_added": 0,
                    "records_updated": 0, "records_unchanged": 0, "resources_written": 0,
                    "resources_unchanged": 0, "documents_added": 0}))
                .unwrap()
            })
            .collect();
        idx
    }

    fn pv(filed: &str, fetched: Option<&str>) -> Provenance {
        let mut v = json!({"source": {"kind": "fhir", "cli": "athena", "organization_id": "all",
                "fhir_base": BASE}, "method": "fhir-api", "filed_at": filed, "filed_by": "medrec 0.9.6"});
        if let Some(f) = fetched {
            v["fetched_at"] = Value::from(f);
        }
        serde_json::from_value(v).unwrap()
    }

    const EYE: &str = "2026-02-03T10:00";
    const STUDIO: &str = "2026-02-04T10:00";

    #[test]
    fn the_run_that_filed_it_says_which_practice() {
        let h = household();
        let idx = index(&[(EYE, "eye-1"), (STUDIO, "studio-1")]);
        let s = Sources::of(&h.sources, &idx);
        let (eye, studio) = (
            pv("2026-02-03T10:00:30Z", None),
            pv("2026-02-04T10:00:30Z", None),
        );
        assert_eq!(s.place(&eye), Some("Example Eye Center"));
        assert_eq!(s.label(&studio).as_deref(), Some("Sample Skin Studio"));
        assert_eq!(s.source(&studio).unwrap().id, "s_alexstudioaaaaaaaaaa");
        assert_ne!(s.key(&eye), s.key(&studio));
        // One record from both has no place.
        assert_eq!(s.place_of([&eye, &studio]), None);
        assert_eq!(s.place_of([&studio]), s.place(&studio));
        assert_eq!(s.place_of([&studio, &studio]), s.place(&studio));
        // Nor has one from a practice and from the connection itself, or from
        // a source not known, whichever comes first.
        let unknown = pv("2026-01-01T10:00:30Z", None);
        let mut h = household();
        h.sources[0].organization_name = Some("athenahealth (all practices)".into());
        let named = Sources::of(&h.sources, &idx);
        for (s, other) in [(&named, &eye), (&s, &unknown)] {
            assert_eq!(s.place(other), None);
            assert_eq!(s.place_of([&studio, other]), None);
            assert_eq!(s.place_of([other, &studio]), None);
        }
        // Read again later, by whichever run: the one that filed it says.
        let again = pv("2026-02-03T10:00:30Z", Some("2026-02-04T10:00:01Z"));
        assert_eq!(s.place(&again), Some("Example Eye Center"));
        assert_eq!(s.key(&again), s.key(&eye));
        // Filed by none the index holds, read by one.
        let read = pv("2026-01-01T10:00:30Z", Some("2026-02-04T10:00:01Z"));
        assert_eq!(s.place(&read), Some("Sample Skin Studio"));
    }

    #[test]
    fn two_names_are_no_name() {
        let h = household();
        let idx = index(&[]);
        let s = Sources::of(&h.sources, &idx);
        let unknown = pv("2026-02-03T10:00:30Z", None);
        assert_eq!(s.place(&unknown), None);
        assert_eq!(s.source(&unknown).map(|s| &s.id), None);
        // No practice's name, and not the first organization's at the base.
        assert_eq!(s.label(&unknown).as_deref(), Some("athena"));
        // Two runs of two patients in one second say neither, but for one
        // that read it.
        let idx = index(&[(EYE, "eye-1"), (EYE, "studio-1")]);
        let s = Sources::of(&h.sources, &idx);
        assert_eq!(s.place(&unknown), None);
        let mut idx = idx;
        idx.imports[1].pulled_at = "2026-02-03T10:00:09Z".into();
        let s = Sources::of(&h.sources, &idx);
        let read = pv("2026-02-03T10:00:30Z", Some("2026-02-03T10:00:09Z"));
        assert_eq!(s.place(&read), Some("Sample Skin Studio"));
        // Nor does one of them being the connection's own name, or none.
        for other in [Some("athenahealth (all practices)"), None] {
            let mut h = household();
            h.sources[1].organization_name = other.map(str::to_string);
            let idx = index(&[]);
            let s = Sources::of(&h.sources, &idx);
            assert_eq!(s.place(&unknown), None, "{other:?}");
            assert_eq!(s.label(&unknown).as_deref(), Some("athena"), "{other:?}");
        }
        // One name twice is the name.
        let mut h = household();
        h.sources[1].organization_name = Some(" Example Eye Center ".into());
        let idx = index(&[]);
        let s = Sources::of(&h.sources, &idx);
        assert_eq!(s.place(&unknown), Some("Example Eye Center"));
        assert!(s.source(&unknown).is_none());
    }

    #[test]
    fn only_the_persons_own_sources_name_what_is_theirs() {
        let mut h = household();
        // Alex's one source here has no name of a practice yet; Robin's has.
        h.sources.remove(1);
        h.sources[0].organization_name = Some("athenahealth (all practices)".into());
        let idx = index(&[]);
        let s = Sources::of(&h.sources, &idx);
        let one = pv("2026-02-03T10:00:30Z", None);
        assert_eq!(s.source(&one).unwrap().id, "s_alexeyeaaaaaaaaaaaaa");
        assert_eq!(s.place(&one), None);
        assert_eq!(
            s.label(&one).as_deref(),
            Some("athenahealth (all practices)")
        );
        // None of their own under the key: no one else's.
        h.sources.remove(0);
        let s = Sources::of(&h.sources, &idx);
        assert_eq!(s.place(&one), None);
        assert_eq!(s.label(&one).as_deref(), Some("athena"));
        // Nor the one a run of another patient filed.
        let h = household();
        let idx = index(&[(EYE, "gone-1")]);
        let s = Sources::of(&h.sources, &idx);
        assert_eq!(s.place(&one), None);
        assert_eq!(s.label(&one).as_deref(), Some("athena"));
    }

    #[test]
    fn what_no_cli_gave_has_a_key_and_no_source() {
        let h = household();
        let idx = index(&[]);
        let s = Sources::of(&h.sources, &idx);
        let mut scan = pv("2026-02-03T10:00:30Z", None);
        scan.source.cli = None;
        scan.source.organization_id = None;
        scan.source.fhir_base = None;
        assert_eq!(s.key(&scan), "|||");
        assert_eq!(s.label(&scan), None);
        assert!(s.source(&scan).is_none());
    }
}
