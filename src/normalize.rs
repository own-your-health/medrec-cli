//! FHIR R4 resources to the index's normalized records (PLAN §4). Our own
//! code, by shape: it reads the elements US Core servers send and keeps the
//! rest in the raw layer, so a record can always be rebuilt from its raw
//! object.

use std::collections::BTreeSet;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::{json, Map, Value};

use crate::claims;
use crate::model::{Code, Provenance, Record, Word};
use crate::providers::printed;
use crate::views::value_text;

pub const LOINC: &str = "http://loinc.org";
pub const SNOMED: &str = "http://snomed.info/sct";
pub const ICD10: &str = "http://hl7.org/fhir/sid/icd-10-cm";
pub const RXNORM: &str = "http://www.nlm.nih.gov/research/umls/rxnorm";
pub const CVX: &str = "http://hl7.org/fhir/sid/cvx";
/// The field of a result that holds its parts.
pub const COMPONENTS: &str = "components";
/// The two parts of a blood pressure, by their LOINC codes.
const SYSTOLIC: &str = "8480-6";
const DIASTOLIC: &str = "8462-4";
pub const NDC: &str = "http://hl7.org/fhir/sid/ndc";

/// Each kind's `fields`, as data-contract.md's table lists them: a reader
/// may meet any of these, and no other. A claim's `visit` and `results` are
/// the links `claims` makes.
pub const FIELDS: &[(&str, &[&str])] = &[
    (
        "encounter",
        &[
            "when",
            "class",
            "location",
            "practitioner",
            "reason",
            "service_provider",
            "period",
        ],
    ),
    (
        "result",
        &[
            "when",
            "value",
            COMPONENTS,
            "reference_range",
            "interpretation",
            "category",
            "categories",
            "issued",
            crate::panels::MEMBERS,
            crate::about::FIELD,
        ],
    ),
    (
        "report",
        &[
            "when",
            "conclusion",
            "category",
            "categories",
            "results",
            "issued",
            crate::panels::MEMBERS,
        ],
    ),
    (
        "condition",
        &[
            "when",
            "onset",
            "recorded",
            "verification_status",
            "category",
            "categories",
            "abatement",
            "abatement_text",
        ],
    ),
    (
        "medication",
        &[
            "when",
            "start",
            "end",
            "end_of",
            "dosage",
            "prescriber",
            "intent",
            "categories",
            "status_reason",
            "reported",
            "quantity",
            "days_supply",
            "pharmacy",
            "type",
            "ndc",
            "source",
        ],
    ),
    (
        "allergy",
        &[
            "when",
            "onset",
            "recorded",
            "verification_status",
            "criticality",
            "reaction",
        ],
    ),
    (
        "immunization",
        &["when", "lot_number", "site", "route", "performer"],
    ),
    ("procedure", &["when", "performer", "location"]),
    (
        "coverage",
        &[
            "when",
            "payer",
            "period",
            "relationship",
            "plan",
            "group",
            "subscriber_id",
        ],
    ),
    (
        "claim",
        &[
            "when",
            "provider",
            "insurer",
            "facility",
            "outcome",
            "use",
            "type",
            "category",
            "billable_period",
            "benefit_period",
            "total",
            "payment",
            "amounts",
            "items",
            "payer_notes",
            "diagnoses",
            "benefit_balance",
            "days_supply",
            "refill_number",
            "refills_authorized",
            "brand_generic",
            "visit",
            "results",
        ],
    ),
];

/// The record a resource becomes, if it is one medrec indexes.
pub fn record(r: &Value, provenance: Provenance) -> Option<Record> {
    let rt = r.get("resourceType")?.as_str()?;
    let (kind, prefer, date_paths): (&str, &[&str], &[&str]) = match rt {
        "Encounter" => ("encounter", &[], &["period.start"]),
        "Observation" => (
            "result",
            &[LOINC],
            &[
                "effectiveDateTime",
                "effectivePeriod.start",
                "effectiveInstant",
                "issued",
            ],
        ),
        "DiagnosticReport" => (
            "report",
            &[LOINC],
            &["effectiveDateTime", "effectivePeriod.start", "issued"],
        ),
        "Condition" => (
            "condition",
            &[ICD10, SNOMED],
            &[
                "onsetDateTime",
                "onsetPeriod.start",
                "recordedDate",
                "assertedDate",
            ],
        ),
        "MedicationRequest" => ("medication", &[RXNORM], &["authoredOn"]),
        "MedicationDispense" => (
            "medication",
            &[RXNORM, NDC],
            &["whenHandedOver", "whenPrepared"],
        ),
        "MedicationStatement" => (
            "medication",
            &[RXNORM],
            &["effectiveDateTime", "effectivePeriod.start", "dateAsserted"],
        ),
        "AllergyIntolerance" => (
            "allergy",
            &[SNOMED, RXNORM],
            &["onsetDateTime", "recordedDate", "assertedDate"],
        ),
        "Immunization" => ("immunization", &[CVX], &["occurrenceDateTime", "date"]),
        "Procedure" => (
            "procedure",
            &[SNOMED],
            &["performedDateTime", "performedPeriod.start"],
        ),
        "Coverage" => ("coverage", &[], &["period.start"]),
        "ExplanationOfBenefit" => ("claim", &[], &["billablePeriod.start", "created"]),
        _ => return None,
    };
    let code_el = match rt {
        "Encounter" => first(r, "type"),
        "MedicationRequest" | "MedicationStatement" | "MedicationDispense" => {
            r.get("medicationCodeableConcept")
        }
        "Immunization" => r.get("vaccineCode"),
        "Coverage" => r.get("type"),
        "ExplanationOfBenefit" => r.get("type"),
        _ => r.get("code"),
    };
    let code = code_el.and_then(|c| codeable(c, prefer));
    let when = date_paths
        .iter()
        .find_map(|p| path_str(r, p))
        .map(str::to_string);
    let mut fields = Map::new();
    let mut absent = Vec::new();
    if let Some(w) = &when {
        fields.insert("when".into(), json!(w));
    }
    let status = match rt {
        "Condition" | "AllergyIntolerance" => r.get("clinicalStatus").and_then(status_code),
        // STU3 marks a refusal with `notGiven` (R4: `status: not-done`).
        "Immunization" if r.get("notGiven") == Some(&json!(true)) => Some("not-done".into()),
        _ => path_str(r, "status").map(str::to_string),
    };
    match rt {
        "Encounter" => {
            put(&mut fields, "class", r.get("class").map(coding_text));
            put_str(&mut fields, "location", printed(r, "location"));
            put_str(&mut fields, "practitioner", printed(r, "practitioner"));
            put(
                &mut fields,
                "reason",
                first(r, "reasonCode").map(concept_text),
            );
            put_str(
                &mut fields,
                "service_provider",
                printed(r, "service_provider"),
            );
            put(&mut fields, "period", r.get("period").cloned());
        }
        "Observation" => {
            let (parts, without_value) = components(r);
            let value = observation_value(r);
            let own = value.as_ref().is_some_and(|v| !value_text(v).is_empty());
            let value = if own || parts.is_empty() {
                value
            } else {
                reading(&parts)
            };
            put(&mut fields, "value", value);
            if !parts.is_empty() {
                let parts: Vec<Value> = parts.into_iter().map(Value::Object).collect();
                fields.insert(COMPONENTS.into(), parts.into());
            }
            absent = without_value;
            put(
                &mut fields,
                "reference_range",
                first(r, "referenceRange").map(range_text),
            );
            put(
                &mut fields,
                "interpretation",
                first(r, "interpretation").map(concept_text),
            );
            put(
                &mut fields,
                "category",
                first(r, "category").map(concept_text),
            );
            put(&mut fields, "categories", categories(r));
            put_str(&mut fields, "issued", path_str(r, "issued"));
            put_str(&mut fields, crate::about::FIELD, about(r));
        }
        "DiagnosticReport" => {
            put_str(&mut fields, "conclusion", path_str(r, "conclusion"));
            put(
                &mut fields,
                "category",
                first_or_one(r, "category").map(concept_text),
            );
            put(&mut fields, "categories", categories(r));
            put(
                &mut fields,
                "results",
                r.get("result")
                    .and_then(Value::as_array)
                    .map(|a| json!(a.len())),
            );
            put_str(&mut fields, "issued", path_str(r, "issued"));
        }
        "Condition" => {
            stated(&mut fields, r);
            put(
                &mut fields,
                "category",
                first(r, "category").map(concept_text),
            );
            put(&mut fields, "categories", categories(r));
            put_str(&mut fields, "abatement", path_str(r, "abatementDateTime"));
            if !fields.contains_key("abatement") {
                put(&mut fields, "abatement_text", when_text(r, "abatement"));
            }
        }
        "MedicationRequest" => {
            let (start, end) = dosage_bounds(r);
            let valid = |k: &str| path_str(r, &format!("dispenseRequest.validityPeriod.{k}"));
            put_str(&mut fields, "start", start.or_else(|| valid("start")));
            ends(
                &mut fields,
                [(end, "dosage"), (valid("end"), "prescription")],
            );
            put(&mut fields, "categories", categories(r));
            put(
                &mut fields,
                "status_reason",
                r.get("statusReason").map(concept_text),
            );
            let reported = r.get("reportedBoolean") == Some(&json!(true))
                || r.get("reportedReference").is_some();
            put(&mut fields, "reported", reported.then(|| json!(true)));
            put_str(
                &mut fields,
                "dosage",
                path_str(r, "dosageInstruction.0.text"),
            );
            put_str(&mut fields, "prescriber", printed(r, "prescriber"));
            put_str(&mut fields, "intent", path_str(r, "intent"));
            put(&mut fields, "source", Some(json!("request")));
        }
        "MedicationStatement" => {
            put_str(&mut fields, "start", path_str(r, "effectivePeriod.start"));
            ends(
                &mut fields,
                [(path_str(r, "effectivePeriod.end"), "taking")],
            );
            put(&mut fields, "categories", categories(r));
            put(
                &mut fields,
                "status_reason",
                first(r, "statusReason").map(concept_text),
            );
            put_str(&mut fields, "dosage", path_str(r, "dosage.0.text"));
            put(&mut fields, "source", Some(json!("statement")));
        }
        "MedicationDispense" => {
            put_str(
                &mut fields,
                "dosage",
                path_str(r, "dosageInstruction.0.text"),
            );
            put(&mut fields, "quantity", quantity(r.get("quantity")));
            put(
                &mut fields,
                "days_supply",
                path(r, "daysSupply.value").cloned(),
            );
            put_str(&mut fields, "pharmacy", printed(r, "pharmacy"));
            put(&mut fields, "type", r.get("type").map(concept_text));
            put(&mut fields, "ndc", ndc(r.get("medicationCodeableConcept")));
            put_str(&mut fields, "prescriber", contained_prescriber(r));
            put(&mut fields, "source", Some(json!("dispense")));
        }
        "AllergyIntolerance" => {
            stated(&mut fields, r);
            put_str(&mut fields, "criticality", path_str(r, "criticality"));
            let reactions: Vec<Value> = r
                .get("reaction")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .flat_map(|rx| {
                    rx.get("manifestation")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default()
                })
                .map(|m| concept_text(&m))
                .collect();
            if !reactions.is_empty() {
                fields.insert("reaction".into(), Value::Array(reactions));
            }
        }
        "Immunization" => {
            put_str(&mut fields, "lot_number", path_str(r, "lotNumber"));
            put(&mut fields, "site", r.get("site").map(concept_text));
            put(&mut fields, "route", r.get("route").map(concept_text));
            put_str(&mut fields, "performer", printed(r, "performer"));
        }
        "Procedure" => {
            put_str(&mut fields, "performer", printed(r, "performer"));
            put_str(&mut fields, "location", printed(r, "location"));
        }
        "Coverage" => {
            put_str(&mut fields, "payer", path_str(r, "payor.0.display"));
            put(&mut fields, "period", r.get("period").cloned());
            put_str(
                &mut fields,
                "relationship",
                r.get("relationship")
                    .and_then(|c| codeable(c, &[]))
                    .and_then(|c| c.code)
                    .as_deref(),
            );
            for class in r
                .get("class")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let ty = class
                    .get("type")
                    .and_then(|t| codeable(t, &[]))
                    .and_then(|c| c.code);
                let value = class.get("name").or_else(|| class.get("value")).cloned();
                match ty.as_deref() {
                    Some("plan") => put(&mut fields, "plan", value),
                    Some("group") => put(&mut fields, "group", class.get("value").cloned()),
                    _ => {}
                }
            }
            put_str(&mut fields, "subscriber_id", path_str(r, "subscriberId"));
        }
        "ExplanationOfBenefit" => {
            put_str(&mut fields, "provider", printed(r, "provider"));
            put_str(&mut fields, "insurer", path_str(r, "insurer.display"));
            put_str(&mut fields, "outcome", path_str(r, "outcome"));
            put(
                &mut fields,
                "billable_period",
                r.get("billablePeriod").cloned(),
            );
            put(
                &mut fields,
                "benefit_period",
                r.get("benefitPeriod").cloned(),
            );
            let totals: Vec<Value> = r
                .get("total")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(|t| {
                    json!({
                        "category": t.get("category").map(concept_text),
                        "amount": money(t.get("amount")),
                    })
                })
                .collect();
            if !totals.is_empty() {
                fields.insert("total".into(), Value::Array(totals));
            }
            put(&mut fields, "payment", money(path(r, "payment.amount")));
            put(&mut fields, "type", r.get("type").map(concept_text));
            put_str(&mut fields, "use", path_str(r, "use"));
            put_str(&mut fields, "facility", printed(r, "facility"));
            let pharmacy = is_pharmacy(r);
            let mut items = eob_items(r, pharmacy);
            let totals = claims::amounts(eob_entries(r.get("total")));
            // The provider's own marks decide; with none, the totals do.
            if let Some(Value::Array(list)) = items
                .as_mut()
                .filter(|l| l.as_array().is_some_and(|l| l.iter().all(claims::counted)))
            {
                for i in claims::left_out(list, &totals) {
                    if let Some(o) = list[i].as_object_mut() {
                        o.insert("counted".into(), Value::Bool(false));
                    }
                }
            }
            let lines = items
                .as_ref()
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            put_str(
                &mut fields,
                "category",
                claims::claim_category(lines, pharmacy),
            );
            let amounts = claims::claim_amounts(totals, lines, money(path(r, "payment.amount")));
            if !amounts.is_empty() {
                fields.insert("amounts".into(), Value::Object(amounts));
            }
            put(&mut fields, "items", items);
            put(&mut fields, "payer_notes", claim_notes(r));
            put(&mut fields, "diagnoses", eob_diagnoses(r));
            // A pharmacy claim (CARIN's Pharmacy EOB) carries the fill on
            // `supportingInfo`.
            for (key, code) in [
                ("days_supply", "dayssupply"),
                ("refill_number", "refillnum"),
                ("refills_authorized", "refillsauthorized"),
            ] {
                put(&mut fields, key, supporting_value(r, code));
            }
            put(
                &mut fields,
                "brand_generic",
                supporting(r, "brandgenericindicator")
                    .and_then(|s| s.get("code"))
                    .map(concept_text),
            );
            // CARIN carries plan-year accumulators (deductible and
            // out-of-pocket used and remaining) on the EOB.
            put(&mut fields, "benefit_balance", benefit_balance(r));
        }
        _ => {}
    }
    // A claim is named for what it paid for and who: a fill for its drug,
    // other claims for their lines' services, else their category.
    let claim_title = (rt == "ExplanationOfBenefit")
        .then(|| claim_title(r, &fields))
        .flatten();
    let display = claim_title
        .or_else(|| code.as_ref().and_then(|c| c.display.clone()))
        .or_else(|| match rt {
            "MedicationRequest" | "MedicationStatement" | "MedicationDispense" => {
                path_str(r, "medicationReference.display").map(str::to_string)
            }
            "Encounter" => r
                .get("class")
                .map(coding_text)
                .and_then(|v| v.as_str().map(str::to_string)),
            "Coverage" => path_str(r, "payor.0.display").map(str::to_string),
            "ExplanationOfBenefit" => path_str(r, "provider.display").map(str::to_string),
            _ => None,
        })
        .unwrap_or_else(|| default_display(kind).into());
    let date = when
        .as_deref()
        .map(|w| w.chars().take(10).collect::<String>());
    // A fill and a claim are each one event, not a fact two sources state:
    // keyed by what names that event, so two of them on a day (two fills of
    // a drug, two visits) never fold into one record and lose their
    // quantities and amounts, and a fill never folds into a prescription.
    let key = match rt {
        "MedicationDispense" => format!(
            "medication-dispense|{}|{}",
            identity(r, &provenance),
            date.as_deref().unwrap_or("")
        ),
        "ExplanationOfBenefit" => format!(
            "claim|{}|{}",
            identity(r, &provenance),
            date.as_deref().unwrap_or("")
        ),
        // A test of somebody else is none of the person's own.
        "Observation" if fields.contains_key(crate::about::FIELD) => crate::about::key(
            &merge_key(kind, code.as_ref(), &display, date.as_deref()),
            Some(&focus(r)),
        ),
        _ => merge_key(kind, code.as_ref(), &display, date.as_deref()),
    };
    debug_assert!(
        FIELDS
            .iter()
            .any(|(k, known)| *k == kind && fields.keys().all(|f| known.contains(&f.as_str()))),
        "a {kind} field is not in FIELDS: {:?}",
        fields.keys().collect::<Vec<_>>()
    );
    let words = word(r, rt, status.as_deref(), &fields, &provenance);
    let lists = crate::panels::lists(r, &provenance);
    Some(Record {
        id: crate::ids::new("r"),
        kind: kind.into(),
        key,
        date,
        code,
        display,
        status,
        fields,
        providers: Vec::new(),
        words: words.into_iter().collect(),
        errors: Vec::new(),
        fields_from: None,
        given: Vec::new(),
        absent,
        lists,
        listed_by: Vec::new(),
        documents: Vec::new(),
        provenance: vec![provenance],
    })
}

/// What the source of a record says of it, when its raw file is kept: its
/// status alone, but of a condition, an allergy and a prescription.
fn word(
    r: &Value,
    resource_type: &str,
    status: Option<&str>,
    fields: &Map<String, Value>,
    provenance: &Provenance,
) -> Option<Word> {
    let raw_path = provenance.raw_path.clone()?;
    if !WORD_TYPES.contains(&resource_type) {
        return Some(Word {
            raw_path,
            status: status.map(str::to_string),
            ..Word::default()
        });
    }
    let text = |k: &str| fields.get(k).and_then(Value::as_str).map(str::to_string);
    Some(Word {
        raw_path,
        status: status.map(str::to_string),
        verification: r.get("verificationStatus").and_then(status_code),
        categories: fields
            .get("categories")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|c| c.as_str().map(str::to_string))
            .collect(),
        abatement: text("abatement").or_else(|| text("abatement_text")),
        end: text("end"),
        end_of: text("end_of"),
        ..Word::default()
    })
}

/// The resources whose word a record always keeps.
pub const WORD_TYPES: &[&str] = &[
    "Condition",
    "AllergyIntolerance",
    "MedicationRequest",
    "MedicationStatement",
];

/// What a condition and an allergy say alike: when it began, when it was
/// written down, and how sure they are.
fn stated(fields: &mut Map<String, Value>, r: &Value) {
    put(fields, "onset", when_text(r, "onset"));
    put_str(
        fields,
        "recorded",
        path_str(r, "recordedDate").or_else(|| path_str(r, "assertedDate")),
    );
    put(
        fields,
        "verification_status",
        r.get("verificationStatus").map(code_or_concept_text),
    );
}

/// A FHIR `onset[x]` or `abatement[x]` as text: a date as written, a period
/// and a range as their ends, an age with its unit, the source's own words,
/// and `yes` for one it only says there was.
fn when_text(r: &Value, name: &str) -> Option<Value> {
    let at = |suffix: &str| r.get(format!("{name}{suffix}")).filter(|v| !v.is_null());
    let between = |from: Option<String>, to: Option<String>| match (from, to) {
        (Some(a), Some(b)) => Some(format!("{a} – {b}")),
        (Some(a), None) => Some(format!("from {a}")),
        (None, Some(b)) => Some(format!("until {b}")),
        (None, None) => None,
    };
    let text = |v: &Value| v.as_str().filter(|s| !s.is_empty()).map(str::to_string);
    let side = |v: &Value, k: &str| v.get(k).and_then(text);
    at("DateTime")
        .and_then(text)
        .or_else(|| at("Period").and_then(|p| between(side(p, "start"), side(p, "end"))))
        .or_else(|| at("Age").and_then(age_text))
        .or_else(|| {
            let range = at("Range")?;
            let age = |k: &str| range.get(k).and_then(age_text);
            between(age("low"), age("high"))
        })
        .or_else(|| at("String").and_then(text))
        .or_else(|| (at("Boolean") == Some(&json!(true))).then(|| "yes".to_string()))
        .map(Value::String)
}

/// A quantity of time with its unit: `12 years`.
fn age_text(q: &Value) -> Option<String> {
    let value = match q.get("value")? {
        Value::Number(n) => n.to_string(),
        Value::String(s) if !s.is_empty() => s.clone(),
        _ => return None,
    };
    let s = |k: &str| q.get(k).and_then(Value::as_str);
    let unit = match s("code") {
        Some("a") => "years",
        Some("mo") => "months",
        Some("wk") => "weeks",
        Some("d") => "days",
        Some("h") => "hours",
        code => s("unit").or(code).unwrap_or(""),
    };
    Some(format!("{value} {unit}").trim().to_string())
}

/// What an Observation's `focus` refers to, in order.
fn focus(r: &Value) -> String {
    let all = r.get("focus").and_then(Value::as_array);
    let of = all.into_iter().flatten();
    let of = of.filter_map(|f| f.get("reference")?.as_str());
    of.collect::<Vec<_>>().join(" ")
}

/// The person an Observation's `focus` says it is about, where that is not its
/// subject.
fn about(r: &Value) -> Option<&'static str> {
    let id = |v: &Value| {
        let of = v.get("reference")?.as_str()?;
        Some(of.rsplit('/').next().unwrap_or(of).to_string())
    };
    let subject = r.get("subject").and_then(id);
    r.get("focus")?.as_array()?.iter().find_map(|f| {
        let of = id(f);
        let contained = of.as_deref().and_then(|of| {
            let all = r.get("contained").and_then(Value::as_array)?;
            let of = of.strip_prefix('#')?;
            all.iter().find(|c| c["id"] == of)
        });
        let reference = f.get("reference").and_then(Value::as_str).unwrap_or("");
        let is = |kind: &str| {
            let named = f.get("type").and_then(Value::as_str) == Some(kind);
            let held = contained.is_some_and(|c| c["resourceType"] == kind);
            named || held || reference.rsplit('/').nth(1) == Some(kind)
        };
        let other = is("RelatedPerson") || (is("Patient") && of.is_some() && of != subject);
        if !other {
            return None;
        }
        let relation = contained.and_then(|c| c.get("relationship")).map(all_words);
        let display = f.get("display").and_then(Value::as_str).unwrap_or("");
        let said = format!("{display} {}", relation.unwrap_or_default());
        Some(match PARENT.is_match(&said) {
            true => crate::about::PARENT,
            false => crate::about::ANOTHER,
        })
    })
}

static PARENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:mother|father|parent|N?MTH|N?FTH|PRN|NPRN|GESTM)\b").unwrap()
});

/// Every string in a value, in order.
fn all_words(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().map(all_words).collect::<Vec<_>>().join(" "),
        Value::Object(o) => o.values().map(all_words).collect::<Vec<_>>().join(" "),
        _ => String::new(),
    }
}

/// The code of each category, else its text, in the systems' own codes before
/// a vendor's.
fn categories(r: &Value) -> Option<Value> {
    const PREFER: &[&str] = &[
        "http://terminology.hl7.org/CodeSystem/condition-category",
        "http://hl7.org/fhir/us/core/CodeSystem/condition-category",
        "http://hl7.org/fhir/condition-category",
        "http://terminology.hl7.org/CodeSystem/medicationrequest-category",
        "http://terminology.hl7.org/CodeSystem/medication-statement-category",
        "http://terminology.hl7.org/CodeSystem/observation-category",
        "http://hl7.org/fhir/observation-category",
        "http://terminology.hl7.org/CodeSystem/v2-0074",
        "http://hl7.org/fhir/v2/0074",
    ];
    let list: Vec<&Value> = match r.get("category")? {
        Value::Array(a) => a.iter().collect(),
        one => vec![one],
    };
    let mut out: Vec<String> = Vec::new();
    for c in list {
        let code = match c {
            Value::String(s) => Some(s.clone()),
            _ => codeable(c, PREFER).and_then(|c| c.code.or(c.display)),
        };
        if let Some(code) = code.filter(|s| !s.is_empty() && !out.contains(s)) {
            out.push(code);
        }
    }
    (!out.is_empty()).then(|| json!(out))
}

/// The first start of a prescription's dosages, and the last end when each
/// that has a period has an end: by their days, since times as written do not
/// sort.
fn dosage_bounds(r: &Value) -> (Option<&str>, Option<&str>) {
    let periods: Vec<&Value> = r
        .get("dosageInstruction")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|d| path(d, "timing.repeat.boundsPeriod"))
        .collect();
    let day = |s: &&str| s.get(..10).unwrap_or(s).to_string();
    let starts = periods.iter().filter_map(|p| path_str(p, "start"));
    let ends: Option<Vec<&str>> = periods.iter().map(|p| path_str(p, "end")).collect();
    (
        starts.min_by_key(day),
        ends.and_then(|e| e.into_iter().max_by_key(day)),
    )
}

/// The first end a medicine's source gives, and what of it ends.
fn ends<const N: usize>(fields: &mut Map<String, Value>, of: [(Option<&str>, &str); N]) {
    if let Some((end, what)) = of.into_iter().find_map(|(e, w)| Some((e?, w))) {
        put_str(fields, "end", Some(end));
        put_str(fields, "end_of", Some(what));
    }
}

pub(crate) fn default_display(kind: &str) -> &'static str {
    match kind {
        "encounter" => "Visit",
        "result" => "Result",
        "report" => "Report",
        "condition" => "Condition",
        "medication" => "Medication",
        "allergy" => "Allergy",
        "immunization" => "Immunization",
        "procedure" => "Procedure",
        "coverage" => "Coverage",
        "claim" => "Claim",
        _ => "Record",
    }
}

/// Records that describe the same fact share a key: kind, the coded
/// concept (or its folded text), and the date.
pub fn merge_key(kind: &str, code: Option<&Code>, display: &str, date: Option<&str>) -> String {
    let concept = match code {
        Some(Code {
            system: Some(s),
            code: Some(c),
            ..
        }) => format!("{s}|{c}"),
        _ => crate::intake::classify::fold(display),
    };
    format!("{kind}|{concept}|{}", date.unwrap_or(""))
}

/// A `CodeableConcept` as one code: the first coding in a preferred
/// system, else the first coding; the concept's text as display when the
/// coding has none.
pub fn codeable(c: &Value, prefer: &[&str]) -> Option<Code> {
    let codings: Vec<&Value> = c
        .get("coding")
        .and_then(Value::as_array)
        .map(|a| a.iter().collect())
        .unwrap_or_default();
    let chosen = prefer
        .iter()
        .find_map(|p| {
            codings
                .iter()
                .find(|cd| cd.get("system").and_then(Value::as_str) == Some(p))
        })
        .or_else(|| codings.first())
        .copied();
    let text = c.get("text").and_then(Value::as_str);
    if chosen.is_none() && text.is_none() {
        return None;
    }
    let s = |k: &str| {
        chosen
            .and_then(|cd| cd.get(k))
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    Some(Code {
        system: s("system"),
        code: s("code"),
        display: s("display").or_else(|| text.map(str::to_string)),
    })
}

fn concept_text(c: &Value) -> Value {
    codeable(c, &[])
        .and_then(|cd| cd.display.or(cd.code))
        .map(Value::String)
        .unwrap_or(Value::Null)
}

fn coding_text(c: &Value) -> Value {
    c.get("display")
        .or_else(|| c.get("code"))
        .cloned()
        .unwrap_or(Value::Null)
}

fn observation_value(r: &Value) -> Option<Value> {
    if let Some(q) = r.get("valueQuantity") {
        let mut v = measured(q)?;
        v["system"] = q.get("system").cloned().unwrap_or_default();
        return Some(v);
    }
    if let Some(of) = r.get("valueRange") {
        return Some(range_text(of)).filter(|v| !v.is_null());
    }
    if let Some(of) = r.get("valueRatio") {
        let side = |k: &str| {
            let text = of.get(k).and_then(measured);
            text.map(|q| value_text(&q)).filter(|t| !t.is_empty())
        };
        let (n, d) = (side("numerator")?, side("denominator")?);
        return Some(format!("{n}:{d}").into());
    }
    if let Some(s) = r.get("valueString") {
        return Some(s.clone());
    }
    if let Some(c) = r.get("valueCodeableConcept") {
        return Some(concept_text(c));
    }
    ["valueBoolean", "valueInteger", "valueDateTime", "valueTime"]
        .iter()
        .find_map(|k| r.get(*k).cloned())
}

/// A number with its unit, and what says it is a bound: below or above what
/// could be measured is not the number itself.
fn measured(q: &Value) -> Option<Value> {
    let mut v = quantity(Some(q)).filter(|v| !v["value"].is_null())?;
    let than = path_str(q, "comparator");
    if let Some(c) = than.filter(|c| ["<", "<=", ">=", ">"].contains(c)) {
        v["comparator"] = c.into();
    }
    Some(v)
}

/// The parts of a reading that have a value, each as a result has its own, and
/// the codes of those that have none.
fn components(r: &Value) -> (Vec<Map<String, Value>>, Vec<Code>) {
    let parts = r.get("component").and_then(Value::as_array);
    let (mut out, mut absent) = (Vec::new(), Vec::new());
    for c in parts.into_iter().flatten() {
        let code = c.get("code").and_then(|c| codeable(c, &[LOINC]));
        let Some(code) = code.filter(|c| c.display.is_some() || c.code.is_some()) else {
            continue;
        };
        let value = observation_value(c).filter(|v| !value_text(v).is_empty());
        let Some(value) = value else {
            absent.push(code);
            continue;
        };
        let mut part = Map::new();
        put(&mut part, "code", serde_json::to_value(code).ok());
        part.insert("value".into(), value);
        put(
            &mut part,
            "reference_range",
            first(c, "referenceRange").map(range_text),
        );
        put(
            &mut part,
            "interpretation",
            first_or_one(c, "interpretation").map(concept_text),
        );
        out.push(part);
    }
    (out, absent)
}

/// What a reading of parts says as one: the two of a blood pressure, told by
/// their codes alone and in one unit, then each other by its name.
fn reading(parts: &[Map<String, Value>]) -> Option<Value> {
    let of = |code: &str| {
        let mut its = parts
            .iter()
            .filter(|p| p["code"]["system"] == LOINC && p["code"]["code"] == code);
        let one = its.next().filter(|_| its.next().is_none())?;
        let v = &one["value"];
        let exact = v.get("comparator").is_none();
        let n = v.get("value").filter(|_| exact)?.as_number()?;
        Some((n.to_string(), v.get("unit")?.as_str()?.to_string(), one))
    };
    let pressure = match (of(SYSTOLIC), of(DIASTOLIC)) {
        (Some((s, unit, a)), Some((d, other, b))) if unit == other => {
            Some((format!("{s}/{d}"), unit, [a, b]))
        }
        _ => None,
    };
    let its = |p: &Map<String, Value>| {
        pressure
            .as_ref()
            .is_some_and(|(_, _, of)| of.iter().any(|x| std::ptr::eq(*x, p)))
    };
    let others: Vec<String> = parts
        .iter()
        .filter(|p| !its(p))
        .map(|p| {
            let name = p["code"]["display"].as_str().or(p["code"]["code"].as_str());
            let value = value_text(&p["value"]);
            format!("{} {value}", name.unwrap_or_default())
                .trim()
                .to_string()
        })
        .collect();
    match pressure {
        Some((value, unit, _)) if others.is_empty() => Some(json!({"value": value, "unit": unit})),
        Some((value, unit, _)) => Some(format!("{value} {unit}; {}", others.join("; ")).into()),
        None => (!others.is_empty()).then(|| others.join("; ").into()),
    }
}

fn range_text(rr: &Value) -> Value {
    if let Some(t) = rr.get("text") {
        return t.clone();
    }
    let side = |k: &str| {
        let q = rr.get(k)?;
        let u = q.get("unit").and_then(Value::as_str).unwrap_or("");
        Some(format!("{} {u}", q.get("value")?).trim().to_string())
    };
    match (side("low"), side("high")) {
        (Some(l), Some(h)) => json!(format!("{l} – {h}")),
        (Some(l), None) => json!(format!("≥ {l}")),
        (None, Some(h)) => json!(format!("≤ {h}")),
        (None, None) => Value::Null,
    }
}

/// FHIR `Money` as the family's `{amount: "12.34", currency}`, amount a
/// string decimal as the source wrote it.
fn money(m: Option<&Value>) -> Option<Value> {
    let m = m?;
    let amount = m.get("value")?;
    let text = match amount {
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        _ => return None,
    };
    Some(
        json!({ "amount": text, "currency": m.get("currency").and_then(Value::as_str).unwrap_or("USD") }),
    )
}

/// What names one event (a fill, a claim): its first identifier
/// (`system|value`); else its id, scoped to the source that served it,
/// since an id is unique only on its own server; else a hash of the
/// resource itself.
fn identity(r: &Value, prov: &Provenance) -> String {
    if let Some(v) = path_str(r, "identifier.0.value").filter(|v| !v.is_empty()) {
        return format!("{}|{v}", path_str(r, "identifier.0.system").unwrap_or(""));
    }
    if let Some(id) = path_str(r, "id").filter(|v| !v.is_empty()) {
        let s = &prov.source;
        return format!(
            "id|{}|{}|{}|{id}",
            s.fhir_base.as_deref().unwrap_or(""),
            s.cli.as_deref().unwrap_or(""),
            s.organization_id.as_deref().unwrap_or("")
        );
    }
    format!("sha256|{}", crate::version::value_digest(r))
}

/// A `Quantity` as `{value, unit}`, the unit falling back to its code.
fn quantity(q: Option<&Value>) -> Option<Value> {
    let q = q?;
    Some(json!({
        "value": q.get("value")?,
        "unit": q.get("unit").or_else(|| q.get("code")),
    }))
}

/// The NDC among a concept's codings, when the preferred code is another.
fn ndc(c: Option<&Value>) -> Option<Value> {
    c?.get("coding")?
        .as_array()?
        .iter()
        .find(|k| k.get("system").and_then(Value::as_str) == Some(NDC))
        .and_then(|k| k.get("code").cloned())
}

/// An EOB's `supportingInfo` entry whose category has `code`.
fn supporting<'a>(r: &'a Value, code: &str) -> Option<&'a Value> {
    r.get("supportingInfo")?.as_array()?.iter().find(|s| {
        s.get("category")
            .and_then(|c| c.get("coding"))
            .and_then(Value::as_array)
            .is_some_and(|cs| {
                cs.iter()
                    .any(|k| k.get("code").and_then(Value::as_str) == Some(code))
            })
    })
}

fn supporting_value(r: &Value, code: &str) -> Option<Value> {
    let s = supporting(r, code)?;
    s.get("valueQuantity")
        .and_then(|q| q.get("value"))
        .or_else(|| s.get("valueInteger"))
        .or_else(|| s.get("valueString"))
        .cloned()
}

/// A pharmacy claim: of type `pharmacy` (CARIN's Pharmacy EOB).
fn is_pharmacy(r: &Value) -> bool {
    r.get("type")
        .and_then(|t| t.get("coding"))
        .and_then(Value::as_array)
        .is_some_and(|cs| {
            cs.iter()
                .any(|k| k.get("code").and_then(Value::as_str) == Some("pharmacy"))
        })
}

/// An EOB's or a line's `total` or `adjudication` entries as (raw
/// category, money) pairs, for `claims::amounts`.
fn eob_entries(list: Option<&Value>) -> impl Iterator<Item = (&Value, Option<Value>)> {
    list.and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|e| Some((e.get("category")?, money(e.get("amount")))))
}

/// A claim's title (`claims::title`): a fill by its drug (RxNorm or NDC
/// display), other claims by the services of the lines its totals count.
fn claim_title(r: &Value, fields: &Map<String, Value>) -> Option<String> {
    let lines = r
        .get("item")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let prefer: &[&str] = if is_pharmacy(r) { &[RXNORM, NDC] } else { &[] };
    // `items` holds one entry per line, in order.
    let shaped = fields.get("items").and_then(Value::as_array);
    let names: Vec<String> = lines
        .iter()
        .enumerate()
        .filter(|(i, _)| shaped.and_then(|s| s.get(*i)).is_none_or(claims::counted))
        .filter_map(|(_, l)| l.get("productOrService").and_then(|c| codeable(c, prefer)))
        .filter_map(|c| c.display)
        .collect();
    claims::title(
        names.iter().map(String::as_str),
        fields.get("category").and_then(Value::as_str),
        fields.get("provider").and_then(Value::as_str),
    )
}

/// A claim's diagnoses: `[{sequence, code}]`, ICD-10 preferred.
fn eob_diagnoses(r: &Value) -> Option<Value> {
    let list: Vec<Value> = r
        .get("diagnosis")
        .and_then(Value::as_array)?
        .iter()
        .filter_map(|d| {
            let code = codeable(d.get("diagnosisCodeableConcept")?, &[ICD10])?;
            Some(json!({"sequence": d.get("sequence"), "code": code}))
        })
        .collect();
    (!list.is_empty()).then(|| Value::Array(list))
}

/// An EOB's `processNote` texts by their `number`.
fn process_notes(r: &Value) -> Vec<(Option<u64>, &str)> {
    r.get("processNote")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|n| {
            let text = n.get("text").and_then(Value::as_str)?.trim();
            (!text.is_empty()).then(|| (n.get("number").and_then(Value::as_u64), text))
        })
        .collect()
}

/// The `noteNumber`s a line points at.
fn note_numbers(item: &Value) -> Vec<u64> {
    item.get("noteNumber")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_u64)
        .collect()
}

/// The claim's processing notes that no line points at, as the payer wrote
/// them; each line carries its own (`eob_items`).
fn claim_notes(r: &Value) -> Option<Value> {
    let used: BTreeSet<u64> = r
        .get("item")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .flat_map(note_numbers)
        .collect();
    let notes: Vec<Value> = process_notes(r)
        .into_iter()
        .filter(|(n, _)| n.is_none_or(|n| !used.contains(&n)))
        .map(|(_, t)| t.into())
        .collect();
    (!notes.is_empty()).then(|| Value::Array(notes))
}

/// Whether a provider CLI of this project marked an EOB line as left out of
/// the claim's totals: an adjudication whose category is `not-counted` in
/// its own `…/fhir/adjudication` code system (uhc-cli 0.3.6 and later).
fn marked_not_counted(item: &Value) -> bool {
    item.get("adjudication")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|a| a.pointer("/category/coding").and_then(Value::as_array))
        .flatten()
        .any(|c| {
            c["code"] == "not-counted"
                && c["system"].as_str().is_some_and(|s| {
                    s.starts_with("https://github.com/own-your-health/")
                        && s.ends_with("/fhir/adjudication")
                })
        })
}

/// Each claim line: its service and revenue codes, category, dates and
/// amounts, with its adjudication entries and processing notes as the payer
/// wrote them.
fn eob_items(r: &Value, pharmacy: bool) -> Option<Value> {
    let notes = process_notes(r);
    let items: Vec<Value> = r
        .get("item")
        .and_then(Value::as_array)?
        .iter()
        .map(|it| {
            let adjudication: Vec<Value> = it
                .get("adjudication")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(|a| {
                    json!({
                        "category": a.get("category").map(concept_text),
                        "amount": money(a.get("amount")),
                        "reason": a.get("reason").map(concept_text),
                    })
                })
                .collect();
            let amounts = claims::amounts(eob_entries(it.get("adjudication")));
            let mut line = json!({
                "sequence": it.get("sequence"),
                "category": claims::item_category(it, pharmacy),
                "amounts": (!amounts.is_empty()).then_some(amounts),
                "service": it.get("productOrService").and_then(|c| codeable(c, &[])),
                "serviced": it.get("servicedDate").or_else(|| it.get("servicedPeriod")),
                "quantity": quantity(it.get("quantity")),
                "net": money(it.get("net")),
                "adjudication": adjudication,
            });
            if marked_not_counted(it) {
                line["counted"] = Value::Bool(false);
            }
            // A vaccine's CVX code, where one coding of its service is one,
            // first or not (0.9.11 and later).
            let codings = it
                .pointer("/productOrService/coding")
                .and_then(Value::as_array);
            let cvx = codings.into_iter().flatten().find(|c| c["system"] == CVX);
            if let Some(code) = cvx
                .and_then(|c| c["code"].as_str())
                .filter(|c| !c.trim().is_empty())
            {
                line["cvx"] = code.trim().into();
            }
            let texts: Vec<Value> = note_numbers(it)
                .into_iter()
                .filter_map(|k| notes.iter().find(|(n, _)| *n == Some(k)))
                .map(|(_, t)| (*t).into())
                .collect();
            if !texts.is_empty() {
                line["payer_notes"] = Value::Array(texts);
            }
            // A facility line's revenue code (NUBC: 045x is the emergency
            // room), beside or instead of its service code.
            if let Some(rev) = it.get("revenue").and_then(|c| codeable(c, &[])) {
                line["revenue"] = json!(rev);
            }
            line
        })
        .collect();
    (!items.is_empty()).then(|| Value::Array(items))
}

fn benefit_balance(r: &Value) -> Option<Value> {
    let balances: Vec<Value> = r
        .get("benefitBalance")
        .and_then(Value::as_array)?
        .iter()
        .map(|b| {
            let financial: Vec<Value> = b
                .get("financial")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(|f| {
                    json!({
                        "type": f.get("type").map(concept_text),
                        "allowed": money(f.get("allowedMoney")),
                        "used": money(f.get("usedMoney")),
                    })
                })
                .collect();
            json!({
                "category": b.get("category").map(concept_text),
                "network": b.get("network").map(concept_text),
                "unit": b.get("unit").map(concept_text),
                "term": b.get("term").map(concept_text),
                "financial": financial,
            })
        })
        .collect();
    (!balances.is_empty()).then(|| Value::Array(balances))
}

fn first<'a>(r: &'a Value, key: &str) -> Option<&'a Value> {
    r.get(key).and_then(Value::as_array).and_then(|a| a.first())
}

/// The first of an array, or a single value where STU3 has one
/// (`DiagnosticReport.category`).
fn first_or_one<'a>(r: &'a Value, key: &str) -> Option<&'a Value> {
    match r.get(key) {
        Some(Value::Array(a)) => a.first(),
        other => other,
    }
}

/// A fill's prescriber: the `requester` of the contained MedicationRequest
/// its `authorizingPrescription` points at (medical-common DESIGN).
fn contained_prescriber(r: &Value) -> Option<&str> {
    let id = path_str(r, "authorizingPrescription.0.reference")?.strip_prefix('#')?;
    let request = r.get("contained")?.as_array()?.iter().find(|c| {
        c.get("resourceType").and_then(Value::as_str) == Some("MedicationRequest")
            && c.get("id").and_then(Value::as_str) == Some(id)
    })?;
    printed(request, "prescriber")
}

/// A status served as a CodeableConcept (R4) or a plain code (STU3).
fn status_code(c: &Value) -> Option<String> {
    match c {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        _ => codeable(c, &[]).and_then(|c| c.code),
    }
}

/// [`concept_text`], or a plain code as it is (STU3).
fn code_or_concept_text(c: &Value) -> Value {
    match c {
        Value::String(s) if !s.is_empty() => json!(s),
        _ => concept_text(c),
    }
}

/// `a.b.0.c` into a resource.
pub fn path<'a>(r: &'a Value, p: &str) -> Option<&'a Value> {
    p.split('.')
        .try_fold(r, |v, seg| match seg.parse::<usize>() {
            Ok(i) => v.get(i),
            Err(_) => v.get(seg),
        })
}

/// The string at `p`. An empty one is absent: FHIR allows no empty strings,
/// and some servers send them for missing values.
pub fn path_str<'a>(r: &'a Value, p: &str) -> Option<&'a str> {
    path(r, p).and_then(Value::as_str).filter(|s| !s.is_empty())
}

fn put(m: &mut Map<String, Value>, k: &str, v: Option<Value>) {
    if let Some(v) = v.filter(|v| !v.is_null()) {
        m.insert(k.into(), v);
    }
}

fn put_str(m: &mut Map<String, Value>, k: &str, v: Option<&str>) {
    if let Some(v) = v.filter(|v| !v.is_empty()) {
        m.insert(k.into(), json!(v));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// data-contract.md's table of fields is FIELDS, row for row.
    #[test]
    fn the_contract_lists_each_kinds_fields() {
        let doc = include_str!("../docs/data-contract.md");
        let table = doc
            .split_once("**Fields by kind.**")
            .expect("the table")
            .1
            .split_once("\n\n")
            .unwrap()
            .1;
        let rows: Vec<(String, String)> = table
            .lines()
            .take_while(|l| l.starts_with('|'))
            .skip(2)
            .map(|l| {
                let cell: Vec<&str> = l.split('|').map(str::trim).collect();
                let name = |c: &str| c.trim_matches('`').to_string();
                (name(cell[1]), name(cell[2]))
            })
            .collect();
        let want: Vec<(String, String)> = FIELDS
            .iter()
            .flat_map(|(k, fs)| fs.iter().map(move |f| (k.to_string(), f.to_string())))
            .collect();
        assert_eq!(rows, want);
    }

    use crate::model::{filed_by, ProvenanceSource};

    fn prov() -> Provenance {
        Provenance {
            source: ProvenanceSource {
                kind: "fhir".into(),
                cli: Some("fakemed".into()),
                cli_version: None,
                organization_id: None,
                fhir_base: None,
            },
            method: "fhir-api".into(),
            source_resource: None,
            fetched_at: None,
            filed_at: "2026-11-12T00:00:00Z".into(),
            raw_path: None,
            classified_by: None,
            derived_from: None,
            filed_by: filed_by(),
        }
    }

    const DISPENSE: &str = include_str!("../tests/fixtures/fhir/medication-dispense.json");
    const PHARMACY_EOB: &str = include_str!("../tests/fixtures/fhir/pharmacy-eob.json");
    const LEFT_OUT_LINE_EOB: &str = include_str!("../tests/fixtures/fhir/left-out-line-eob.json");
    const STU3: &str = include_str!("../tests/fixtures/fhir/stu3.json");
    const STATUS: &str = include_str!("../tests/fixtures/fhir/status.json");
    const READINGS: &str = include_str!("../tests/fixtures/fhir/readings.json");
    const DISPENSE_CONTAINED: &str =
        include_str!("../tests/fixtures/fhir/medication-dispense-contained.json");

    #[test]
    fn a_fill_names_the_prescriber_of_its_contained_request() {
        let with: Value = serde_json::from_str(DISPENSE_CONTAINED).unwrap();
        let rec = record(&with, prov()).unwrap();
        assert_eq!(rec.fields["prescriber"], json!("Example Prescriber"));
        // A prescription referenced elsewhere names no prescriber here.
        let without: Value = serde_json::from_str(DISPENSE).unwrap();
        assert!(record(&without, prov())
            .unwrap()
            .fields
            .get("prescriber")
            .is_none());
    }

    #[test]
    fn a_status_is_read_in_every_form_a_source_gives() {
        let all: Vec<Value> = serde_json::from_str(STATUS).unwrap();
        let rec = |id: &str| {
            let r = all.iter().find(|r| r["id"] == id).unwrap();
            let mut p = prov();
            p.raw_path = Some(format!("{id}.json"));
            record(r, p).unwrap()
        };
        let fields = |id: &str, of: &[&str]| -> Value {
            let r = rec(id);
            let some = of.iter().filter_map(|k| {
                let v = r.fields.get(*k)?;
                Some((k.to_string(), v.clone()))
            });
            Value::Object(some.collect())
        };
        const CONDITION: &[&str] = &[
            "onset",
            "recorded",
            "verification_status",
            "categories",
            "abatement",
            "abatement_text",
        ];
        assert_eq!(
            fields("cond-period", CONDITION),
            json!({"onset": "2019-04 – 2019-06", "recorded": "2020-01-15",
                "verification_status": "Confirmed",
                "categories": ["problem-list-item", "health-concern", "Example words"],
                "abatement_text": "until 2021-02-03"})
        );
        assert_eq!(
            fields("cond-age", CONDITION),
            json!({"onset": "12 years", "recorded": "2020-01-15",
                "categories": ["encounter-diagnosis"], "abatement_text": "14 years – 15 years"})
        );
        assert_eq!(
            fields("cond-words", CONDITION),
            json!({"onset": "some years ago", "recorded": "2018-03-04",
                "verification_status": "entered-in-error",
                "categories": ["problem-list-item"], "abatement_text": "yes"})
        );
        assert_eq!(
            fields("allergy-refuted", CONDITION),
            json!({"onset": "5 years", "recorded": "2017-05-06", "verification_status": "refuted"})
        );
        // Who wrote it down is no field: a provider a record prints is one its
        // privacy is judged by.
        for r in ["cond-period", "cond-age", "allergy-refuted"] {
            let text = serde_json::to_string(&rec(r).fields).unwrap();
            assert!(!text.contains("Example Recorder") && !text.contains("Example Asserter"));
        }
        let mut dated = all[0].clone();
        dated["abatementDateTime"] = json!("2021-02-03");
        let dated = record(&dated, prov()).unwrap().fields;
        assert_eq!(dated["abatement"], "2021-02-03");
        assert!(!dated.contains_key("abatement_text"));
        const MEDICINE: &[&str] = &[
            "start",
            "end",
            "end_of",
            "categories",
            "status_reason",
            "reported",
        ];
        assert_eq!(
            fields("med-dosage", MEDICINE),
            json!({"start": "2024-01-10", "end": "2024-03-11T09:30:00Z", "end_of": "dosage",
                "categories": ["community"], "status_reason": "Sample reason", "reported": true})
        );
        // Written with and without a time, the days decide.
        let mut mixed = all[4].clone();
        let bounds = |m: &mut Value, i: usize, start: &str, end: &str| {
            m["dosageInstruction"][i]["timing"]["repeat"]["boundsPeriod"] =
                json!({"start": start, "end": end});
        };
        bounds(&mut mixed, 0, "2024-01-10T23:00:00-05:00", "2024-03-11");
        bounds(&mut mixed, 1, "2024-01-11", "2024-03-10T23:30:00-05:00");
        let mixed = record(&mixed, prov()).unwrap().fields;
        assert_eq!(mixed["start"], "2024-01-10T23:00:00-05:00");
        assert_eq!(mixed["end"], "2024-03-11");
        // One dosage goes on: the prescription's own end is the one there is.
        assert_eq!(
            fields("med-prescription", MEDICINE),
            json!({"start": "2024-01-10", "end": "2025-01-01", "end_of": "prescription"})
        );
        assert_eq!(
            fields("med-taking", MEDICINE),
            json!({"start": "2023-01-02", "end": "2023-04-05", "end_of": "taking",
                "categories": ["patientspecified"], "status_reason": "Sample reason"})
        );
        // What its source says, by code, when its raw file is kept.
        let words = |id: &str| serde_json::to_value(rec(id).words).unwrap();
        assert_eq!(
            words("cond-period"),
            json!([{"raw_path": "cond-period.json", "status": "resolved",
                "verification": "confirmed", "abatement": "until 2021-02-03",
                "categories": ["problem-list-item", "health-concern", "Example words"]}])
        );
        assert_eq!(
            words("med-taking"),
            json!([{"raw_path": "med-taking.json", "status": "active", "end": "2023-04-05",
                "end_of": "taking", "categories": ["patientspecified"]}])
        );
        assert!(record(&all[0], prov()).unwrap().words.is_empty());
        let fill: Value = serde_json::from_str(DISPENSE).unwrap();
        let mut p = prov();
        p.raw_path = Some("fill.json".into());
        // A fill's is its status alone, as any other kind's.
        assert_eq!(
            serde_json::to_value(record(&fill, p).unwrap().words).unwrap(),
            json!([{"raw_path": "fill.json", "status": "completed"}])
        );
    }

    #[test]
    fn stu3_shapes_read_as_their_r4_counterparts() {
        let all: Vec<Value> = serde_json::from_str(STU3).unwrap();
        let rec = |id: &str| {
            let r = all.iter().find(|r| r["id"] == id).unwrap();
            record(r, prov()).unwrap()
        };
        let cond = rec("cond-stu3");
        assert_eq!(cond.status.as_deref(), Some("active"));
        assert_eq!(cond.fields["verification_status"], json!("confirmed"));
        // An empty onset is absent; the asserted date stands in.
        assert_eq!(cond.date.as_deref(), Some("2026-02-10"));
        let allergy = rec("allergy-stu3");
        assert_eq!(allergy.status.as_deref(), Some("active"));
        assert_eq!(allergy.date.as_deref(), Some("2026-02-11"));
        let given = rec("imm-stu3-given");
        assert_eq!(given.status.as_deref(), Some("completed"));
        assert_eq!(given.date.as_deref(), Some("2026-01-15"));
        assert_eq!(given.fields["performer"], json!("Example Nurse"));
        assert_eq!(rec("imm-stu3-refused").status.as_deref(), Some("not-done"));
        let med = rec("medreq-stu3");
        assert_eq!(med.fields["prescriber"], json!("Example Prescriber"));
        assert_eq!(med.date.as_deref(), Some("2026-02-01"));
        let report = rec("report-stu3");
        assert_eq!(report.fields["category"], json!("Laboratory"));
        assert_eq!(report.date.as_deref(), Some("2026-02-20"));
    }

    #[test]
    fn a_dispense_is_a_medication_fill_keyed_apart_from_a_prescription() {
        let r: Value = serde_json::from_str(DISPENSE).unwrap();
        let rec = record(&r, prov()).unwrap();
        assert_eq!(rec.kind, "medication");
        let code = rec.code.as_ref().unwrap();
        assert_eq!(code.system.as_deref(), Some(RXNORM));
        assert_eq!(code.code.as_deref(), Some("314076"));
        // Handed over, not prepared, is when the fill happened.
        assert_eq!(rec.date.as_deref(), Some("2026-03-10"));
        assert_eq!(rec.status.as_deref(), Some("completed"));
        let f = &rec.fields;
        assert_eq!(f["source"], "dispense");
        assert_eq!(f["quantity"], json!({"value": 30, "unit": "TAB"}));
        assert_eq!(f["days_supply"], 30);
        assert_eq!(f["pharmacy"], "Example Mail Pharmacy");
        assert_eq!(f["ndc"], "00000-0001-01");
        assert_eq!(f["dosage"], "Take 1 tablet by mouth once daily");
        assert_eq!(f["type"], "Refill");
        assert_eq!(
            crate::views::record_detail(&rec).as_deref(),
            Some("dispensed 30 TAB, 30 days")
        );

        // Another fill of the drug that day (a partial fill and its
        // completion) is another record.
        let mut second = r.clone();
        second["id"] = json!("md-0002");
        assert_ne!(record(&second, prov()).unwrap().key, rec.key);

        // A prescription of the same drug that day is another fact.
        let mut req = json!({
            "resourceType": "MedicationRequest",
            "id": "mr-0001",
            "status": "active",
            "intent": "order",
            "authoredOn": "2026-03-10",
        });
        req["medicationCodeableConcept"] = r["medicationCodeableConcept"].clone();
        let prescribed = record(&req, prov()).unwrap();
        assert_eq!(prescribed.kind, "medication");
        assert_ne!(prescribed.key, rec.key);

        // Coded by NDC alone, the NDC is the code.
        let mut ndc_only = r.clone();
        ndc_only["medicationCodeableConcept"]["coding"]
            .as_array_mut()
            .unwrap()
            .retain(|c| c["system"] == NDC);
        let rec = record(&ndc_only, prov()).unwrap();
        assert_eq!(rec.code.unwrap().system.as_deref(), Some(NDC));
        // With no handover, the date is when it was prepared.
        let mut prepared = r.clone();
        prepared.as_object_mut().unwrap().remove("whenHandedOver");
        assert_eq!(
            record(&prepared, prov()).unwrap().date.as_deref(),
            Some("2026-03-09")
        );
    }

    #[test]
    fn a_line_its_provider_marks_not_counted_is_left_out_whatever_the_totals_say() {
        let marker = |system: &str| {
            json!({"category": {"coding": [{"system": system, "code": "not-counted",
                "display": "Not counted in the claim's totals"}]}})
        };
        let mut r: Value = serde_json::from_str(LEFT_OUT_LINE_EOB).unwrap();
        r["item"][3]["adjudication"]
            .as_array_mut()
            .unwrap()
            .push(marker(
                "https://github.com/own-your-health/uhc-cli/fhir/adjudication",
            ));
        let counted = |r: &Value| -> Vec<bool> {
            let rec = record(r, prov()).unwrap();
            let items = rec.fields["items"].as_array().unwrap().clone();
            items.iter().map(|i| i.get("counted").is_none()).collect()
        };
        // The provider's mark decides: the line the totals would leave out
        // stays counted.
        assert_eq!(counted(&r), [true, true, true, false]);

        // A mark in another code system is the payer's words, not a mark.
        let mut r: Value = serde_json::from_str(LEFT_OUT_LINE_EOB).unwrap();
        r["item"][3]["adjudication"]
            .as_array_mut()
            .unwrap()
            .push(marker("https://example.org/fhir/adjudication"));
        assert_eq!(counted(&r), [true, true, false, true]);
    }

    #[test]
    fn a_line_the_totals_leave_out_is_marked_and_named_by_nothing() {
        let r: Value = serde_json::from_str(LEFT_OUT_LINE_EOB).unwrap();
        let rec = record(&r, prov()).unwrap();
        let f = &rec.fields;
        let items = f["items"].as_array().unwrap();
        let counted: Vec<bool> = items.iter().map(|i| i["counted"] != false).collect();
        assert_eq!(counted, [true, true, false, true]);
        // Its amounts stay as the payer wrote them; the claim's are the
        // totals.
        assert_eq!(items[2]["amounts"]["you_owe"]["amount"], "60.0");
        assert_eq!(f["amounts"]["you_owe"]["amount"], "42.0");
        assert_eq!(f["amounts"]["billed"]["amount"], "390.0");
        // A name no total gives sums the counted lines only.
        assert_eq!(f["amounts"]["not_covered"], Value::Null);
        assert_eq!(f["amounts"]["coinsurance"]["amount"], "42.00");
        // The title names what the counted lines paid for.
        let title = "Exercise session, Hands-on session and 1 more · Sample Rehab Center";
        assert_eq!(rec.display, title);
        assert_eq!(f["category"], "mixed");
        // Each line keeps the processing notes it points at; the claim
        // keeps those no line points at.
        assert_eq!(
            items[1]["payer_notes"],
            json!(["K4 -- A sample note about the session limit."])
        );
        assert!(items[0].get("payer_notes").is_none());
        assert_eq!(items[0]["revenue"]["code"], "0420");
        assert!(items[3].get("revenue").is_none());
        assert_eq!(
            f["payer_notes"],
            json!(["P7 -- A sample note about the whole claim."])
        );

        // Without the left-out line its claim reads the same, nothing marked.
        let mut whole = r.clone();
        whole["item"].as_array_mut().unwrap().remove(2);
        let rec = record(&whole, prov()).unwrap();
        let items = rec.fields["items"].as_array().unwrap();
        assert!(items.iter().all(|i| i.get("counted").is_none()));
        assert_eq!(rec.display, title);
    }

    #[test]
    fn a_pharmacy_claim_keeps_its_fill_details() {
        let r: Value = serde_json::from_str(PHARMACY_EOB).unwrap();
        let rec = record(&r, prov()).unwrap();
        assert_eq!(rec.kind, "claim");
        let f = &rec.fields;
        assert_eq!(f["type"], "Pharmacy");
        assert_eq!(f["days_supply"], 30);
        assert_eq!(f["refill_number"], 2);
        assert_eq!(f["refills_authorized"], 5);
        assert_eq!(f["brand_generic"], "Generic");
        let item = &f["items"][0];
        assert_eq!(item["service"]["code"], "00000-0001-01");
        assert_eq!(item["quantity"], json!({"value": 30, "unit": "TAB"}));
        assert_eq!(f["payment"]["amount"], "32.5");
        // Named for the fill and the pharmacy.
        assert_eq!(
            rec.display,
            "Synthetic tablet 10 mg, bottle of 30 · Example Mail Pharmacy"
        );
        assert_eq!(f["category"], "pharmacy");
        assert_eq!(f["items"][0]["category"], "pharmacy");

        // Two claims of a type on one day are two records; the same claim
        // from another source, by its identifier, is one.
        let mut other = r.clone();
        other["id"] = json!("eob-rx-0002");
        assert_ne!(record(&other, prov()).unwrap().key, rec.key);
        // An id names a claim only on its own source.
        let mut elsewhere = prov();
        elsewhere.source.cli = Some("otherrx".into());
        assert_ne!(record(&r, elsewhere).unwrap().key, rec.key);
        // With neither identifier nor id, the claim's content names it.
        let mut bare = r.clone();
        bare.as_object_mut().unwrap().remove("id");
        let mut bare_other = bare.clone();
        bare_other["payment"]["amount"]["value"] = json!(12);
        let (k1, k2) = (
            record(&bare, prov()).unwrap().key,
            record(&bare_other, prov()).unwrap().key,
        );
        assert!(k1.contains("|sha256|"), "{k1}");
        assert_ne!(k1, k2);
        let mut numbered = r.clone();
        numbered["identifier"] =
            json!([{"system": "https://payer.example.org/claim", "value": "CLAIM-0001"}]);
        let mut again = numbered.clone();
        again["id"] = json!("eob-rx-9999");
        assert_eq!(
            record(&numbered, prov()).unwrap().key,
            record(&again, prov()).unwrap().key
        );
    }

    #[test]
    fn an_observation_keeps_loinc_value_text_and_range() {
        let r: Value = serde_json::from_str(
            r#"{"resourceType":"Observation","id":"o1","status":"final",
                "code":{"coding":[{"system":"urn:other","code":"x"},{"system":"http://loinc.org","code":"718-7","display":"Hemoglobin"}]},
                "effectiveDateTime":"2026-03-04T09:00:00Z",
                "valueQuantity":{"value":12.10,"unit":"g/dL"},
                "referenceRange":[{"low":{"value":11,"unit":"g/dL"},"high":{"value":15,"unit":"g/dL"}}]}"#,
        )
        .unwrap();
        let rec = record(&r, prov()).unwrap();
        assert_eq!(rec.kind, "result");
        assert_eq!(rec.code.as_ref().unwrap().code.as_deref(), Some("718-7"));
        assert_eq!(rec.date.as_deref(), Some("2026-03-04"));
        assert_eq!(rec.fields["value"]["value"].to_string(), "12.10");
        assert_eq!(rec.fields["reference_range"], "11 g/dL – 15 g/dL");
        assert_eq!(rec.key, "result|http://loinc.org|718-7|2026-03-04");
    }

    #[test]
    fn an_immunization_and_a_condition_use_their_code_systems() {
        let imm: Value = serde_json::from_str(
            r#"{"resourceType":"Immunization","id":"i1","status":"completed",
                "vaccineCode":{"coding":[{"system":"http://hl7.org/fhir/sid/cvx","code":"08","display":"Hep B, adolescent or pediatric"}]},
                "occurrenceDateTime":"2026-01-16","lotNumber":"LOT000"}"#,
        )
        .unwrap();
        let rec = record(&imm, prov()).unwrap();
        assert_eq!(
            rec.key,
            "immunization|http://hl7.org/fhir/sid/cvx|08|2026-01-16"
        );
        assert_eq!(rec.fields["lot_number"], "LOT000");
        let cond: Value = serde_json::from_str(
            r#"{"resourceType":"Condition","id":"c1",
                "clinicalStatus":{"coding":[{"code":"active"}]},
                "code":{"coding":[{"system":"http://snomed.info/sct","code":"1"},{"system":"http://hl7.org/fhir/sid/icd-10-cm","code":"J45.909","display":"Asthma"}]},
                "onsetDateTime":"2020-05-01"}"#,
        )
        .unwrap();
        let c = record(&cond, prov()).unwrap();
        assert_eq!(c.status.as_deref(), Some("active"));
        assert_eq!(c.code.unwrap().code.as_deref(), Some("J45.909"));
    }

    #[test]
    fn a_claim_keeps_money_as_string_decimals() {
        let eob: Value = serde_json::from_str(
            r#"{"resourceType":"ExplanationOfBenefit","id":"e1","status":"active","outcome":"complete",
                "provider":{"display":"Example Clinic"},"billablePeriod":{"start":"2026-02-01"},
                "total":[{"category":{"coding":[{"code":"submitted"}]},"amount":{"value":120.50,"currency":"USD"}}],
                "payment":{"amount":{"value":96.40,"currency":"USD"}}}"#,
        )
        .unwrap();
        let rec = record(&eob, prov()).unwrap();
        assert_eq!(rec.kind, "claim");
        assert_eq!(rec.display, "Example Clinic");
        assert_eq!(rec.fields["payment"]["amount"], "96.40");
        assert_eq!(rec.fields["total"][0]["amount"]["amount"], "120.50");
    }

    #[test]
    fn resources_medrec_does_not_index_become_nothing() {
        for rt in [
            "Patient",
            "Practitioner",
            "Binary",
            "DocumentReference",
            "Bundle",
        ] {
            assert!(record(&json!({"resourceType": rt, "id": "x"}), prov()).is_none());
        }
    }

    /// A result's value, with its parts, as each shape of it says.
    #[test]
    fn a_reading_is_its_value_or_what_its_parts_say() {
        let cases: Vec<Value> = serde_json::from_str(READINGS).unwrap();
        assert!(cases.len() > 10);
        for case in &cases {
            let of = case["of"].as_str().unwrap();
            let r = record(&case["resource"], prov()).unwrap();
            let (value, unit) = crate::summary::reading(&r);
            assert_eq!(json!(value), case["value"], "{of}");
            assert_eq!(json!(unit), case["unit"], "{of}");
            assert_eq!(json!(crate::views::record_detail(&r)), case["says"], "{of}");
            let parts = r.fields.get(COMPONENTS).and_then(Value::as_array);
            let parts = parts.cloned().unwrap_or_default();
            assert_eq!(json!(parts.len()), case["parts"], "{of}");
            let absent = r.absent.iter().map(|c| c.display.as_deref());
            let absent: Vec<Option<&str>> = absent.collect();
            assert_eq!(json!(absent), case["absent"], "{of}");
            assert!(
                parts.iter().all(|p| p["code"]["display"].is_string()),
                "{of}"
            );
            let categories = r.fields.get("categories").cloned().unwrap_or_default();
            assert_eq!(categories, case["categories"], "{of}");
        }
        let first = record(&cases[0]["resource"], prov()).unwrap();
        let parts = &first.fields[COMPONENTS];
        assert_eq!(parts[0]["reference_range"], "90 – 120");
        assert_eq!(parts[0]["interpretation"], "High");
        assert_eq!(parts[1]["value"]["value"], 76);
    }
}
