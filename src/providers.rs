//! Who a record's provider fields name, as far as its source says: the
//! National Provider Identifier and the state and city of the practitioner,
//! organization or place each field's reference points at.

use std::collections::BTreeSet;

use pk_cli_core::CliError;
use serde_json::{Map, Value};

use crate::model::ProviderRef;
use crate::normalize::{path, path_str};

/// The code system of a National Provider Identifier.
pub const NPI: &str = "http://hl7.org/fhir/sid/us-npi";

/// The resource types a provider reference points at.
pub const TYPES: &[&str] = &[
    "Practitioner",
    "PractitionerRole",
    "Organization",
    "Location",
];

/// Reads what a record's source holds.
pub trait Resolve {
    /// The source's FHIR base: an absolute reference to any other server is
    /// not followed, since an id is unique only on its own.
    fn base(&self) -> Option<&str>;
    fn resource(&mut self, resource_type: &str, id: &str) -> Result<Option<Value>, CliError>;
}

/// The fields that print a provider, of any kind, in the order a record
/// lists them.
pub const FIELDS: &[&str] = &[
    "facility",
    "location",
    "performer",
    "pharmacy",
    "practitioner",
    "prescriber",
    "provider",
    "service_provider",
];

/// What one source says of the providers a record prints.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Found {
    /// One entry per field its source says more than a name of, in
    /// [`FIELDS`] order.
    pub refs: Vec<ProviderRef>,
    /// Fields whose reference leads to something the source did not give.
    pub unresolved: BTreeSet<&'static str>,
}

/// The elements of `resource_type` that print each provider field: the first
/// with a display prints it.
fn references(resource_type: &str) -> &'static [(&'static str, &'static [&'static str])] {
    match resource_type {
        "Encounter" => &[
            ("practitioner", &["participant.0.individual"]),
            ("location", &["location.0.location"]),
            ("service_provider", &["serviceProvider"]),
        ],
        "MedicationRequest" => &[("prescriber", &["requester", "requester.agent"])],
        "MedicationDispense" => &[("pharmacy", &["performer.0.actor", "location"])],
        "Immunization" => &[("performer", &["performer.0.actor", "practitioner.0.actor"])],
        "Procedure" => &[
            ("performer", &["performer.0.actor"]),
            ("location", &["location"]),
        ],
        "ExplanationOfBenefit" => &[("provider", &["provider"]), ("facility", &["facility"])],
        _ => &[],
    }
}

/// The element that prints `field` of `r`, and what it prints.
fn printed_by<'a>(r: &'a Value, field: &str) -> Option<(&'a Value, &'a str)> {
    let ty = r.get("resourceType").and_then(Value::as_str)?;
    let (_, paths) = references(ty).iter().find(|(f, _)| *f == field)?;
    paths.iter().find_map(|p| {
        let at = path(r, p)?;
        Some((at, path_str(at, "display")?))
    })
}

/// The provider `field` of `r` prints.
pub fn printed<'a>(r: &'a Value, field: &str) -> Option<&'a str> {
    printed_by(r, field).map(|(_, text)| text)
}

/// What the source of `r` says of each provider `fields` prints as `r`
/// does.
pub fn of(
    r: &Value,
    fields: &Map<String, Value>,
    sources: &mut dyn Resolve,
) -> Result<Found, CliError> {
    let ty = r.get("resourceType").and_then(Value::as_str).unwrap_or("");
    let mut out = Found::default();
    for field in FIELDS.iter().copied() {
        let Some(text) = fields.get(field).and_then(Value::as_str) else {
            continue;
        };
        let facts = if ty == "MedicationDispense" && field == "prescriber" {
            match prescription(r, sources)? {
                Some(request) => match printed_by(&request, field) {
                    // A contained prescription's own references are the
                    // fill's.
                    Some((at, t)) if t == text => facts(at, r, sources, 0)?,
                    _ => continue,
                },
                None => Facts {
                    unresolved: true,
                    ..Facts::default()
                },
            }
        } else {
            match printed_by(r, field) {
                Some((at, t)) if t == text => facts(at, r, sources, 0)?,
                _ => continue,
            }
        };
        if facts.unresolved {
            out.unresolved.insert(field);
        }
        if facts.npi.is_some() || facts.place.is_some() {
            out.refs.push(ProviderRef {
                field: field.into(),
                npi: facts.npi,
                state: facts.place.as_ref().map(|p| p.state.clone()),
                city: facts.place.and_then(|p| p.city),
            });
        }
    }
    Ok(out)
}

impl Found {
    /// Fill what this lacks from another source of the same record.
    pub fn and(mut self, other: Found) -> Found {
        for r in other.refs {
            match self.refs.iter_mut().find(|s| s.field == r.field) {
                Some(s) => {
                    s.npi = s.npi.take().or(r.npi);
                    if s.state.is_none() {
                        (s.state, s.city) = (r.state, r.city);
                    }
                }
                None => self.refs.push(r),
            }
        }
        self.unresolved.extend(other.unresolved);
        self
    }

    /// What a record that said `old` of its providers, and printed
    /// `old_fields`, now says. A provider printed as before keeps what it had
    /// where this source says nothing and gives a reference it does not
    /// serve, and, where another source states the record too (`shared`),
    /// whatever it had, so that two sources do not write over each other.
    pub fn over(
        self,
        old: &[ProviderRef],
        old_fields: &Map<String, Value>,
        fields: &Map<String, Value>,
        shared: bool,
    ) -> Vec<ProviderRef> {
        FIELDS
            .iter()
            .filter_map(|f| {
                let text = fields.get(*f)?;
                let new = self.refs.iter().find(|r| r.field == *f);
                let had = old
                    .iter()
                    .find(|r| r.field == *f && old_fields.get(*f) == Some(text));
                let pick = |of: fn(&ProviderRef) -> Option<String>| {
                    let (new, had) = (new.and_then(of), had.and_then(of));
                    if shared {
                        had.or(new)
                    } else if self.unresolved.contains(f) {
                        new.or(had)
                    } else {
                        new
                    }
                };
                let (npi, state) = (pick(|r| r.npi.clone()), pick(|r| r.state.clone()));
                // The city of the address the state is of.
                let of = |r: Option<&ProviderRef>| {
                    let r = r.filter(|r| r.state.is_some() && r.state == state);
                    r.and_then(|r| r.city.clone())
                };
                let city = match shared {
                    true => of(had).or_else(|| of(new)),
                    false => of(new).or_else(|| of(had)),
                };
                (npi.is_some() || state.is_some()).then(|| ProviderRef {
                    field: f.to_string(),
                    npi,
                    state,
                    city,
                })
            })
            .collect()
    }
}

/// A fill's prescription: contained in it, or one its source serves.
fn prescription(r: &Value, sources: &mut dyn Resolve) -> Result<Option<Value>, CliError> {
    let Some(reference) = path_str(r, "authorizingPrescription.0.reference") else {
        return Ok(None);
    };
    if let Some(id) = reference.strip_prefix('#') {
        return Ok(contained(r, id).cloned());
    }
    match local(reference, sources.base()) {
        Some(("MedicationRequest", id)) => sources.resource("MedicationRequest", id),
        _ => Ok(None),
    }
}

/// What a reference leads to.
#[derive(Debug, Default)]
struct Facts {
    npi: Option<String>,
    place: Option<Place>,
    /// Something on the way could not be read.
    unresolved: bool,
}

/// Where an address is.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Place {
    state: String,
    city: Option<String>,
}

/// The NPI and address a reference leads to: its own identifier, then the
/// resource it points at, contained in `root` or held by the source.
fn facts(
    reference: &Value,
    root: &Value,
    sources: &mut dyn Resolve,
    depth: u8,
) -> Result<Facts, CliError> {
    let mut out = Facts {
        npi: reference.get("identifier").and_then(npi_of_one),
        ..Facts::default()
    };
    let v = match target(reference, root, sources)? {
        Target::None => return Ok(out),
        Target::Unread => {
            out.unresolved = true;
            return Ok(out);
        }
        Target::Read(v) => v,
    };
    out.npi = out.npi.or_else(|| identifiers(&v));
    out.place = address(&v);
    if depth >= 2 {
        return Ok(out);
    }
    match v["resourceType"].as_str() {
        // A person at a place: the NPI is the person's, and the address is
        // the place's before the person's.
        Some("PractitionerRole") => {
            let person = match v.get("practitioner") {
                Some(p) => facts(p, root, sources, depth + 1)?,
                None => Facts::default(),
            };
            out.npi = out.npi.or(person.npi);
            out.unresolved |= person.unresolved;
            for part in ["location.0", "organization"] {
                if out.place.is_none() {
                    if let Some(p) = path(&v, part) {
                        let at = facts(p, root, sources, depth + 1)?;
                        out.place = at.place;
                        out.unresolved |= at.unresolved;
                    }
                }
            }
            out.place = out.place.or(person.place);
        }
        Some("Location") if out.place.is_none() => {
            if let Some(o) = v.get("managingOrganization") {
                let at = facts(o, root, sources, depth + 1)?;
                out.place = at.place;
                out.unresolved |= at.unresolved;
            }
        }
        _ => {}
    }
    Ok(out)
}

enum Target {
    /// The reference points at nothing a provider is read from.
    None,
    /// It points at something the source did not give.
    Unread,
    Read(Value),
}

/// The resource a reference points at.
fn target(reference: &Value, root: &Value, sources: &mut dyn Resolve) -> Result<Target, CliError> {
    let Some(text) = reference.get("reference").and_then(Value::as_str) else {
        return Ok(Target::None);
    };
    let found = if let Some(id) = text.strip_prefix('#') {
        contained(root, id).cloned()
    } else {
        match local(text, sources.base()) {
            Some((ty, id)) if TYPES.contains(&ty) => sources.resource(ty, id)?,
            _ => return Ok(Target::None),
        }
    };
    Ok(found.map_or(Target::Unread, Target::Read))
}

fn contained<'a>(root: &'a Value, id: &str) -> Option<&'a Value> {
    root.get("contained")?
        .as_array()?
        .iter()
        .find(|c| c.get("id").and_then(Value::as_str) == Some(id))
}

/// `(type, id)` of a reference to the server at `base`: relative, or
/// absolute under it, with or without `/_history/<v>`.
fn local<'a>(reference: &'a str, base: Option<&str>) -> Option<(&'a str, &'a str)> {
    let rest = if reference.contains(':') {
        reference
            .strip_prefix(base?.trim_end_matches('/'))?
            .strip_prefix('/')?
    } else {
        reference
    };
    let mut parts = rest.split('/');
    let (ty, id) = (parts.next()?, parts.next()?);
    let end = matches!(
        (parts.next(), parts.next(), parts.next()),
        (None, _, _) | (Some("_history"), Some(_), None)
    );
    (end && !ty.is_empty() && !id.is_empty()).then_some((ty, id))
}

/// The first NPI among a resource's identifiers.
fn identifiers(v: &Value) -> Option<String> {
    v.get("identifier")?.as_array()?.iter().find_map(npi_of_one)
}

fn npi_of_one(id: &Value) -> Option<String> {
    let is_npi = id.get("system").and_then(Value::as_str) == Some(NPI)
        || path(id, "type.coding.0.code").and_then(Value::as_str) == Some("NPI");
    let value = id.get("value")?.as_str()?.trim();
    (is_npi && npi_holds(value)).then(|| value.to_string())
}

/// The first address with a state.
fn address(v: &Value) -> Option<Place> {
    let a = v.get("address")?;
    let one = |a: &Value| {
        let city = a.get("city").and_then(Value::as_str).map(str::trim);
        Some(Place {
            state: a.get("state").and_then(Value::as_str).and_then(state)?,
            city: city.filter(|c| !c.is_empty()).map(str::to_string),
        })
    };
    match a.as_array() {
        Some(list) => list.iter().find_map(one),
        None => one(a),
    }
}

/// Ten digits whose check digit holds. The check is Luhn over the prefix
/// 80840, which is what an NPI issued in the United States carries.
pub fn npi_holds(npi: &str) -> bool {
    if npi.len() != 10 || !npi.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    let mut sum = 0u32;
    for (i, b) in b"80840".iter().chain(npi.as_bytes()).rev().enumerate() {
        let d = u32::from(b - b'0');
        sum += if i % 2 == 1 {
            if d * 2 > 9 {
                d * 2 - 9
            } else {
                d * 2
            }
        } else {
            d
        };
    }
    sum.is_multiple_of(10)
}

/// A state as two letters, from its postal code or its name.
pub fn state(text: &str) -> Option<String> {
    let t = text.trim();
    if t.len() == 2 {
        let up = t.to_ascii_uppercase();
        return STATES.iter().any(|(c, _)| *c == up).then_some(up);
    }
    let name = t.to_ascii_lowercase();
    STATES
        .iter()
        .find(|(_, n)| *n == name)
        .map(|(c, _)| c.to_string())
}

const STATES: &[(&str, &str)] = &[
    ("AL", "alabama"),
    ("AK", "alaska"),
    ("AZ", "arizona"),
    ("AR", "arkansas"),
    ("CA", "california"),
    ("CO", "colorado"),
    ("CT", "connecticut"),
    ("DE", "delaware"),
    ("DC", "district of columbia"),
    ("FL", "florida"),
    ("GA", "georgia"),
    ("HI", "hawaii"),
    ("ID", "idaho"),
    ("IL", "illinois"),
    ("IN", "indiana"),
    ("IA", "iowa"),
    ("KS", "kansas"),
    ("KY", "kentucky"),
    ("LA", "louisiana"),
    ("ME", "maine"),
    ("MD", "maryland"),
    ("MA", "massachusetts"),
    ("MI", "michigan"),
    ("MN", "minnesota"),
    ("MS", "mississippi"),
    ("MO", "missouri"),
    ("MT", "montana"),
    ("NE", "nebraska"),
    ("NV", "nevada"),
    ("NH", "new hampshire"),
    ("NJ", "new jersey"),
    ("NM", "new mexico"),
    ("NY", "new york"),
    ("NC", "north carolina"),
    ("ND", "north dakota"),
    ("OH", "ohio"),
    ("OK", "oklahoma"),
    ("OR", "oregon"),
    ("PA", "pennsylvania"),
    ("RI", "rhode island"),
    ("SC", "south carolina"),
    ("SD", "south dakota"),
    ("TN", "tennessee"),
    ("TX", "texas"),
    ("UT", "utah"),
    ("VT", "vermont"),
    ("VA", "virginia"),
    ("WA", "washington"),
    ("WV", "west virginia"),
    ("WI", "wisconsin"),
    ("WY", "wyoming"),
    ("PR", "puerto rico"),
    ("VI", "virgin islands"),
    ("GU", "guam"),
    ("AS", "american samoa"),
    ("MP", "northern mariana islands"),
];

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// A source whose only resource is `Practitioner/a1`.
    struct One;
    impl Resolve for One {
        fn base(&self) -> Option<&str> {
            Some("https://fhir.example.org/r4/")
        }

        fn resource(&mut self, ty: &str, id: &str) -> Result<Option<Value>, CliError> {
            Ok((ty == "Practitioner" && id == "a1").then(|| {
                json!({
                    "resourceType": "Practitioner",
                    "identifier": [{"system": NPI, "value": "0000000006"}]
                })
            }))
        }
    }

    fn map(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    fn entry(field: &str, npi: Option<&str>, state: Option<&str>) -> ProviderRef {
        ProviderRef {
            field: field.into(),
            npi: npi.map(str::to_string),
            state: state.map(str::to_string),
            city: None,
        }
    }

    fn at(state: &str, city: &str) -> ProviderRef {
        ProviderRef {
            city: Some(city.into()),
            ..entry("performer", None, Some(state))
        }
    }

    /// A city is of the address the state is of.
    #[test]
    fn a_city_goes_with_its_state() {
        let of = |address: Value| {
            let r = json!({
                "resourceType": "Procedure",
                "performer": [{"actor": {"reference": "#who", "display": "Jordan Sample"}}],
                "contained": [{"resourceType": "Practitioner", "id": "who", "address": address}]
            });
            of(&r, &map(json!({"performer": "Jordan Sample"})), &mut One)
                .unwrap()
                .refs
        };
        assert_eq!(
            of(json!([{"city": "Sampleburg"}, {"state": "Florida", "city": " Exampleton "}])),
            [at("FL", "Exampleton")]
        );
        assert_eq!(
            of(json!([{"state": "FL", "city": " "}, {"state": "NY", "city": "Sampleburg"}])),
            [entry("performer", None, Some("FL"))]
        );
        assert!(of(json!({"city": "Exampleton"})).is_empty());

        // Through every way one source's word goes over another's.
        let fields = map(json!({"performer": "Jordan Sample"}));
        let says = |refs: Vec<ProviderRef>| Found {
            refs,
            unresolved: BTreeSet::new(),
        };
        let over = |new: &ProviderRef, had: &ProviderRef, shared: bool| -> ProviderRef {
            let had = std::slice::from_ref(had);
            let mut out = says(vec![new.clone()]).over(had, &fields, &fields, shared);
            assert_eq!(out.len(), 1);
            out.remove(0)
        };
        let (fl, ny) = (at("FL", "Exampleton"), at("NY", "Sampleburg"));
        let bare = entry("performer", None, Some("FL"));
        assert_eq!(over(&ny, &fl, false), ny);
        assert_eq!(over(&ny, &fl, true), fl);
        assert_eq!(over(&ny, &bare, true), bare);
        assert_eq!(over(&fl, &bare, true), fl);
        let npi = entry("performer", Some("0000000006"), None);
        let both = says(vec![npi.clone()]).and(says(vec![fl.clone()]));
        assert_eq!(
            both.refs,
            [ProviderRef {
                npi: npi.npi,
                ..fl.clone()
            }]
        );
        assert_eq!(says(vec![bare.clone()]).and(says(vec![fl])).refs, [bare]);
    }

    #[test]
    fn an_npi_holds_by_its_check_digit() {
        assert!(npi_holds("0000000006"));
        assert!(npi_holds("0000000014"));
        assert!(!npi_holds("0000000007"));
        assert!(!npi_holds("000000006"));
        assert!(!npi_holds("00000000060"));
        assert!(!npi_holds("00000000 6"));
        assert!(!npi_holds(""));
    }

    #[test]
    fn a_state_is_two_letters_from_its_code_or_its_name() {
        assert_eq!(state(" fl ").as_deref(), Some("FL"));
        assert_eq!(state("District of Columbia").as_deref(), Some("DC"));
        assert_eq!(state("PR").as_deref(), Some("PR"));
        assert_eq!(state("ZZ"), None);
        assert_eq!(state("Ontario"), None);
        assert_eq!(state(""), None);
    }

    /// Only what the fields print, as this resource prints it: a field that
    /// is not there or prints another provider adds nothing.
    #[test]
    fn a_reference_adds_only_what_its_source_says() {
        let r = json!({
            "resourceType": "Procedure",
            "performer": [{"actor": {"reference": "Practitioner/gone", "display": "Jordan Sample"}}],
            "location": {"reference": "#place", "display": "Example Annex"},
            "contained": [{"resourceType": "Location", "id": "place", "address": {"state": "NY"}}]
        });
        let of = |f: Value| of(&r, &map(f), &mut One).unwrap();
        let both = of(json!({"performer": "Jordan Sample", "location": "Example Annex"}));
        assert_eq!(both.refs, [entry("location", None, Some("NY"))]);
        assert_eq!(both.unresolved, BTreeSet::from(["performer"]));
        assert!(of(json!({"location": "Example Clinic"})).refs.is_empty());
        assert!(of(json!({})).refs.is_empty());
    }

    /// The element followed is the one that prints the name: an agent under
    /// a requester, and a place where the performer has no name.
    #[test]
    fn the_reference_followed_is_the_one_printed() {
        let rx = json!({
            "resourceType": "MedicationRequest",
            "requester": {"agent": {"reference": "Practitioner/a1", "display": "Jordan Sample"}}
        });
        assert_eq!(printed(&rx, "prescriber"), Some("Jordan Sample"));
        let fields = map(json!({"prescriber": "Jordan Sample"}));
        assert_eq!(
            of(&rx, &fields, &mut One).unwrap().refs,
            [entry("prescriber", Some("0000000006"), None)]
        );
        let fill = json!({
            "resourceType": "MedicationDispense",
            "performer": [{"actor": {"reference": "Practitioner/a1"}}],
            "location": {"display": "Sample Pharmacy"}
        });
        assert_eq!(printed(&fill, "pharmacy"), Some("Sample Pharmacy"));
        let fields = map(json!({"pharmacy": "Sample Pharmacy"}));
        assert!(of(&fill, &fields, &mut One).unwrap().refs.is_empty());
    }

    /// An id is unique only on its own server.
    #[test]
    fn an_absolute_reference_is_followed_only_on_its_sources_server() {
        let at = |reference: &str| {
            let r = json!({
                "resourceType": "Procedure",
                "performer": [{"actor": {"reference": reference, "display": "Jordan Sample"}}]
            });
            of(&r, &map(json!({"performer": "Jordan Sample"})), &mut One)
                .unwrap()
                .refs
                .len()
        };
        assert_eq!(at("Practitioner/a1"), 1);
        assert_eq!(at("Practitioner/a1/_history/2"), 1);
        assert_eq!(at("https://fhir.example.org/r4/Practitioner/a1"), 1);
        assert_eq!(
            at("https://fhir.example.org/r4/Practitioner/a1/_history/2"),
            1
        );
        for other in [
            "https://other.example.org/r4/Practitioner/a1",
            "https://fhir.example.org/r4x/Practitioner/a1",
            "https://fhir.example.org/Practitioner/a1",
            "urn:uuid:a1",
            "Practitioner/a1/extra",
            "Practitioner/",
            "a1",
            "",
        ] {
            assert_eq!(at(other), 0, "{other}");
        }
    }

    #[test]
    fn a_record_keeps_what_a_source_cannot_resolve_and_what_another_says() {
        let fields = map(json!({"performer": "Jordan Sample", "location": "Example Annex"}));
        let old = [
            entry("location", None, Some("NY")),
            entry("performer", Some("0000000006"), Some("FL")),
        ];
        let says = |refs: Vec<ProviderRef>, unresolved: &[&'static str]| Found {
            refs,
            unresolved: unresolved.iter().copied().collect(),
        };
        // Its one source says nothing any more.
        assert!(says(vec![], &[])
            .over(&old, &fields, &fields, false)
            .is_empty());
        // It could not read the performer.
        assert_eq!(
            says(vec![], &["performer"]).over(&old, &fields, &fields, false),
            [old[1].clone()]
        );
        // What it says now wins, whatever else it could not read.
        assert_eq!(
            says(
                vec![entry("performer", Some("0000000014"), None)],
                &["performer"]
            )
            .over(&old, &fields, &fields, false),
            [entry("performer", Some("0000000014"), Some("FL"))]
        );
        // A source that cannot resolve a provider does not keep another's
        // correction out when every source is read again.
        let again = says(vec![entry("performer", Some("0000000014"), None)], &[])
            .and(says(vec![], &["performer"]));
        assert_eq!(
            again.over(&old, &fields, &fields, false),
            [entry("performer", Some("0000000014"), Some("FL"))]
        );
        // It says less, and another source stands behind the record.
        assert_eq!(
            says(vec![entry("performer", Some("0000000014"), None)], &[])
                .over(&old, &fields, &fields, true),
            old
        );
        // It prints another performer.
        let now = map(json!({"performer": "Casey Example", "location": "Example Annex"}));
        assert_eq!(
            says(vec![], &["performer"]).over(&old, &fields, &now, true),
            [old[0].clone()]
        );
        // The first source to say something stands.
        let both = says(vec![entry("performer", Some("0000000006"), None)], &[]).and(says(
            vec![
                entry("performer", Some("0000000014"), Some("FL")),
                entry("location", None, Some("NY")),
            ],
            &[],
        ));
        assert_eq!(both.over(&[], &fields, &fields, false), old);
    }

    /// A reference's own identifier counts whether or not what it leads to
    /// is served.
    #[test]
    fn an_identifier_beside_a_missing_reference_is_read() {
        let r = json!({
            "resourceType": "ExplanationOfBenefit",
            "provider": {
                "reference": "Organization/gone",
                "identifier": {"system": NPI, "value": "0000000014"},
                "display": "Example Primary Care Group"
            }
        });
        let fields = map(json!({"provider": "Example Primary Care Group"}));
        let found = of(&r, &fields, &mut One).unwrap();
        assert_eq!(found.unresolved, BTreeSet::from(["provider"]));
        let old = [entry("provider", Some("0000000006"), Some("FL"))];
        assert_eq!(
            found.over(&old, &fields, &fields, false),
            [entry("provider", Some("0000000014"), Some("FL"))]
        );
    }

    #[test]
    fn every_field_a_kind_prints_is_one_a_record_lists() {
        for ty in [
            "Encounter",
            "MedicationRequest",
            "MedicationDispense",
            "Immunization",
            "Procedure",
            "ExplanationOfBenefit",
        ] {
            assert!(!references(ty).is_empty());
            for (f, _) in references(ty) {
                assert!(FIELDS.contains(f), "{ty} {f}");
            }
        }
        assert!(FIELDS.is_sorted());
    }
}
