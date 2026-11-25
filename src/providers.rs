//! Who a record's provider fields name, as far as its source says: the
//! National Provider Identifier and the state of the practitioner,
//! organization or place each field's reference points at.

use pk_cli_core::CliError;
use serde_json::{Map, Value};

use crate::model::ProviderRef;
use crate::normalize::path;

/// The code system of a National Provider Identifier.
pub const NPI: &str = "http://hl7.org/fhir/sid/us-npi";

/// The resource types a provider reference points at.
pub const TYPES: &[&str] = &[
    "Practitioner",
    "PractitionerRole",
    "Organization",
    "Location",
];

/// Reads a resource its source holds, by type and id.
pub trait Resolve {
    fn resource(&mut self, resource_type: &str, id: &str) -> Result<Option<Value>, CliError>;
}

/// What the source says of each provider `fields` print: one entry per field
/// that names one and whose reference gives an NPI or a state.
pub fn of(
    r: &Value,
    fields: &Map<String, Value>,
    sources: &mut dyn Resolve,
) -> Result<Vec<ProviderRef>, CliError> {
    let mut out = Vec::new();
    for (field, refs) in references(r) {
        if !fields.contains_key(field) {
            continue;
        }
        let Some(target) = refs.iter().find_map(|p| path(r, p)) else {
            continue;
        };
        let (npi, state) = facts(target, r, sources, 0)?;
        if npi.is_some() || state.is_some() {
            out.push(ProviderRef {
                field: field.into(),
                npi,
                state,
            });
        }
    }
    if let Some((npi, state)) = fill_prescriber(r, fields, sources)? {
        out.push(ProviderRef {
            field: "prescriber".into(),
            npi,
            state,
        });
    }
    Ok(out)
}

/// The fields that print a provider, of any kind.
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

/// Whether `stated`, as one source states it, prints the providers `fields`
/// prints. A fill's prescriber is left out where its source names it only in
/// the prescription.
pub fn prints_the_same(stated: &Map<String, Value>, fields: &Map<String, Value>) -> bool {
    FIELDS.iter().all(|f| {
        stated.get(*f) == fields.get(*f) || (*f == "prescriber" && !stated.contains_key(*f))
    })
}

/// The fields of each kind that print a provider, with the elements that
/// refer to it, the first present one counting.
fn references(r: &Value) -> Vec<(&'static str, &'static [&'static str])> {
    match r.get("resourceType").and_then(Value::as_str).unwrap_or("") {
        "Encounter" => vec![
            ("practitioner", &["participant.0.individual"]),
            ("location", &["location.0.location"]),
            ("service_provider", &["serviceProvider"]),
        ],
        "MedicationRequest" => vec![("prescriber", &["requester", "requester.agent"])],
        "MedicationDispense" => vec![("pharmacy", &["performer.0.actor", "location"])],
        "Immunization" => vec![("performer", &["performer.0.actor", "practitioner.0.actor"])],
        "Procedure" => vec![
            ("performer", &["performer.0.actor"]),
            ("location", &["location"]),
        ],
        "ExplanationOfBenefit" => vec![("provider", &["provider"]), ("facility", &["facility"])],
        _ => Vec::new(),
    }
}

/// A fill's prescriber is its prescription's `requester`: contained in the
/// fill, or the one its source serves.
fn fill_prescriber(
    r: &Value,
    fields: &Map<String, Value>,
    sources: &mut dyn Resolve,
) -> Result<Option<Facts>, CliError> {
    if r.get("resourceType").and_then(Value::as_str) != Some("MedicationDispense")
        || !fields.contains_key("prescriber")
    {
        return Ok(None);
    }
    let Some(reference) = path(r, "authorizingPrescription.0.reference").and_then(Value::as_str)
    else {
        return Ok(None);
    };
    let request = match reference.strip_prefix('#') {
        Some(id) => contained(r, id).cloned(),
        None => match split(reference) {
            Some(("MedicationRequest", id)) => sources.resource("MedicationRequest", id)?,
            _ => None,
        },
    };
    let Some(request) = request else {
        return Ok(None);
    };
    let Some(target) = path(&request, "requester").or_else(|| path(&request, "requester.agent"))
    else {
        return Ok(None);
    };
    // A contained prescription's own references are the fill's.
    let found = facts(target, r, sources, 0)?;
    Ok((found.0.is_some() || found.1.is_some()).then_some(found))
}

/// A provider's NPI and its state, each when the source gives it.
type Facts = (Option<String>, Option<String>);

/// The NPI and state a reference leads to: its own identifier, then the
/// resource it points at, contained in `root` or held by the source.
fn facts(
    reference: &Value,
    root: &Value,
    sources: &mut dyn Resolve,
    depth: u8,
) -> Result<Facts, CliError> {
    let own = reference.get("identifier").and_then(npi_of_one);
    let Some(v) = target(reference, root, sources)? else {
        return Ok((own, None));
    };
    let mut npi = own.or_else(|| identifiers(&v));
    let mut state = address_state(&v);
    if depth >= 2 {
        return Ok((npi, state));
    }
    match v["resourceType"].as_str() {
        // A person at a place: the NPI is the person's, and the state is the
        // place's before the person's.
        Some("PractitionerRole") => {
            let person = match v.get("practitioner") {
                Some(p) => facts(p, root, sources, depth + 1)?,
                None => Facts::default(),
            };
            npi = npi.or(person.0);
            for part in ["location.0", "organization"] {
                if state.is_none() {
                    if let Some(p) = path(&v, part) {
                        state = facts(p, root, sources, depth + 1)?.1;
                    }
                }
            }
            state = state.or(person.1);
        }
        Some("Location") if state.is_none() => {
            if let Some(o) = v.get("managingOrganization") {
                state = facts(o, root, sources, depth + 1)?.1;
            }
        }
        _ => {}
    }
    Ok((npi, state))
}

/// The resource a reference points at.
fn target(
    reference: &Value,
    root: &Value,
    sources: &mut dyn Resolve,
) -> Result<Option<Value>, CliError> {
    let Some(text) = reference.get("reference").and_then(Value::as_str) else {
        return Ok(None);
    };
    if let Some(id) = text.strip_prefix('#') {
        return Ok(contained(root, id).cloned());
    }
    match split(text) {
        Some((ty, id)) if TYPES.contains(&ty) => sources.resource(ty, id),
        _ => Ok(None),
    }
}

fn contained<'a>(root: &'a Value, id: &str) -> Option<&'a Value> {
    root.get("contained")?
        .as_array()?
        .iter()
        .find(|c| c.get("id").and_then(Value::as_str) == Some(id))
}

/// `Type/id` from a reference, relative or absolute, with or without
/// `/_history/<v>`.
pub fn split(reference: &str) -> Option<(&str, &str)> {
    let mut parts: Vec<&str> = reference.split('/').collect();
    if let Some(h) = parts.iter().position(|p| *p == "_history") {
        parts.truncate(h);
    }
    let id = parts.pop().filter(|s| !s.is_empty())?;
    let ty = parts.pop().filter(|s| !s.is_empty())?;
    Some((ty, id))
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

/// The first address's state.
fn address_state(v: &Value) -> Option<String> {
    let a = v.get("address")?;
    let one = |a: &Value| a.get("state").and_then(Value::as_str).and_then(state);
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

    struct None_;
    impl Resolve for None_ {
        fn resource(&mut self, _: &str, _: &str) -> Result<Option<Value>, CliError> {
            Ok(None)
        }
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

    /// Only what the fields print, and only where the source says more than
    /// a name: an unresolved reference and a field that is not there add
    /// nothing.
    #[test]
    fn a_reference_adds_only_what_its_source_says() {
        let r = json!({
            "resourceType": "Procedure",
            "performer": [{"actor": {"reference": "Practitioner/gone", "display": "Jordan Sample"}}],
            "location": {"reference": "#place"},
            "contained": [{"resourceType": "Location", "id": "place", "address": {"state": "NY"}}]
        });
        let fields = json!({"performer": "Jordan Sample", "location": "Example Annex"});
        let of = |f: &Value| of(&r, f.as_object().unwrap(), &mut None_).unwrap();
        assert_eq!(
            of(&fields),
            [ProviderRef {
                field: "location".into(),
                npi: None,
                state: Some("NY".into())
            }]
        );
        assert!(of(&json!({"performer": "Jordan Sample"})).is_empty());
    }

    /// The fields compared are all the fields references print, and a fill's
    /// prescriber is only in its record.
    #[test]
    fn the_source_that_wrote_the_fields_prints_the_same_providers() {
        for ty in [
            "Encounter",
            "MedicationRequest",
            "MedicationDispense",
            "Immunization",
            "Procedure",
            "ExplanationOfBenefit",
        ] {
            for (f, _) in references(&json!({"resourceType": ty})) {
                assert!(FIELDS.contains(&f), "{ty} {f}");
            }
        }
        let same =
            |a: Value, b: Value| prints_the_same(a.as_object().unwrap(), b.as_object().unwrap());
        assert!(same(
            json!({"pharmacy": "Sample Pharmacy", "quantity": 1}),
            json!({"pharmacy": "Sample Pharmacy", "prescriber": "Casey Example", "quantity": 2})
        ));
        assert!(!same(
            json!({"pharmacy": "Sample Pharmacy"}),
            json!({"pharmacy": "Example Pharmacy"})
        ));
        assert!(!same(
            json!({"prescriber": "Casey Example"}),
            json!({"prescriber": "Jordan Sample"})
        ));
        assert!(!same(json!({"location": "Example Annex"}), json!({})));
    }

    #[test]
    fn a_reference_is_split_whatever_its_shape() {
        assert_eq!(split("Practitioner/a1"), Some(("Practitioner", "a1")));
        assert_eq!(
            split("https://fhir.example.org/r4/Practitioner/a1/_history/2"),
            Some(("Practitioner", "a1"))
        );
        assert_eq!(split("a1"), None);
        assert_eq!(split("Practitioner/"), None);
        assert_eq!(split(""), None);
    }
}
