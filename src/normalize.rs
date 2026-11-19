//! FHIR R4 resources to the index's normalized records (PLAN §4). Our own
//! code, by shape: it reads the elements US Core servers send and keeps the
//! rest in the raw layer, so a record can always be rebuilt from its raw
//! object.

use serde_json::{json, Map, Value};

use crate::model::{Code, Provenance, Record};

pub const LOINC: &str = "http://loinc.org";
pub const SNOMED: &str = "http://snomed.info/sct";
pub const ICD10: &str = "http://hl7.org/fhir/sid/icd-10-cm";
pub const RXNORM: &str = "http://www.nlm.nih.gov/research/umls/rxnorm";
pub const CVX: &str = "http://hl7.org/fhir/sid/cvx";
pub const NDC: &str = "http://hl7.org/fhir/sid/ndc";

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
            &["onsetDateTime", "onsetPeriod.start", "recordedDate"],
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
            &["onsetDateTime", "recordedDate"],
        ),
        "Immunization" => ("immunization", &[CVX], &["occurrenceDateTime"]),
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
    if let Some(w) = &when {
        fields.insert("when".into(), json!(w));
    }
    let status = match rt {
        "Condition" | "AllergyIntolerance" => r
            .get("clinicalStatus")
            .and_then(|c| codeable(c, &[]))
            .and_then(|c| c.code),
        _ => path_str(r, "status").map(str::to_string),
    };
    match rt {
        "Encounter" => {
            put(&mut fields, "class", r.get("class").map(coding_text));
            put_str(
                &mut fields,
                "location",
                path_str(r, "location.0.location.display"),
            );
            put_str(
                &mut fields,
                "practitioner",
                path_str(r, "participant.0.individual.display"),
            );
            put(
                &mut fields,
                "reason",
                first(r, "reasonCode").map(concept_text),
            );
            put_str(
                &mut fields,
                "service_provider",
                path_str(r, "serviceProvider.display"),
            );
            put(&mut fields, "period", r.get("period").cloned());
        }
        "Observation" => {
            put(&mut fields, "value", observation_value(r));
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
            put_str(&mut fields, "issued", path_str(r, "issued"));
        }
        "DiagnosticReport" => {
            put_str(&mut fields, "conclusion", path_str(r, "conclusion"));
            put(
                &mut fields,
                "category",
                first(r, "category").map(concept_text),
            );
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
            put(
                &mut fields,
                "verification_status",
                r.get("verificationStatus").map(concept_text),
            );
            put(
                &mut fields,
                "category",
                first(r, "category").map(concept_text),
            );
            put_str(&mut fields, "abatement", path_str(r, "abatementDateTime"));
        }
        "MedicationRequest" => {
            put_str(
                &mut fields,
                "dosage",
                path_str(r, "dosageInstruction.0.text"),
            );
            put_str(&mut fields, "prescriber", path_str(r, "requester.display"));
            put_str(&mut fields, "intent", path_str(r, "intent"));
            put(&mut fields, "source", Some(json!("request")));
        }
        "MedicationStatement" => {
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
            put_str(
                &mut fields,
                "pharmacy",
                path_str(r, "performer.0.actor.display")
                    .or_else(|| path_str(r, "location.display")),
            );
            put(&mut fields, "type", r.get("type").map(concept_text));
            put(&mut fields, "ndc", ndc(r.get("medicationCodeableConcept")));
            put(&mut fields, "source", Some(json!("dispense")));
        }
        "AllergyIntolerance" => {
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
            put_str(
                &mut fields,
                "performer",
                path_str(r, "performer.0.actor.display"),
            );
        }
        "Procedure" => {
            put_str(
                &mut fields,
                "performer",
                path_str(r, "performer.0.actor.display"),
            );
            put_str(&mut fields, "location", path_str(r, "location.display"));
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
            put_str(&mut fields, "provider", path_str(r, "provider.display"));
            put_str(&mut fields, "insurer", path_str(r, "insurer.display"));
            put_str(&mut fields, "outcome", path_str(r, "outcome"));
            put(
                &mut fields,
                "billable_period",
                r.get("billablePeriod").cloned(),
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
            put_str(&mut fields, "facility", path_str(r, "facility.display"));
            put(&mut fields, "items", eob_items(r));
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
    // A pharmacy claim is named for what was filled, not "Pharmacy".
    let filled = (rt == "ExplanationOfBenefit"
        && code.as_ref().and_then(|c| c.code.as_deref()) == Some("pharmacy"))
    .then(|| path(r, "item.0.productOrService").and_then(|c| codeable(c, &[RXNORM, NDC])))
    .flatten()
    .and_then(|c| c.display);
    let display = filled
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
        _ => merge_key(kind, code.as_ref(), &display, date.as_deref()),
    };
    Some(Record {
        id: crate::ids::new("r"),
        kind: kind.into(),
        key,
        date,
        code,
        display,
        status,
        fields,
        documents: Vec::new(),
        provenance: vec![provenance],
    })
}

fn default_display(kind: &str) -> &'static str {
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
        return Some(json!({
            "value": q.get("value"),
            "unit": q.get("unit").or_else(|| q.get("code")),
            "system": q.get("system"),
        }));
    }
    if let Some(s) = r.get("valueString") {
        return Some(s.clone());
    }
    if let Some(c) = r.get("valueCodeableConcept") {
        return Some(concept_text(c));
    }
    ["valueBoolean", "valueInteger", "valueDateTime"]
        .iter()
        .find_map(|k| r.get(*k).cloned())
}

fn range_text(rr: &Value) -> Value {
    if let Some(t) = rr.get("text") {
        return t.clone();
    }
    let side = |k: &str| {
        rr.get(k).map(|q| {
            let v = q.get("value").map(|v| v.to_string()).unwrap_or_default();
            let u = q.get("unit").and_then(Value::as_str).unwrap_or("");
            format!("{v} {u}").trim().to_string()
        })
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

/// Each claim line: its service code, dates and adjudication amounts.
fn eob_items(r: &Value) -> Option<Value> {
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
            json!({
                "sequence": it.get("sequence"),
                "service": it.get("productOrService").and_then(|c| codeable(c, &[])),
                "serviced": it.get("servicedDate").or_else(|| it.get("servicedPeriod")),
                "quantity": quantity(it.get("quantity")),
                "net": money(it.get("net")),
                "adjudication": adjudication,
            })
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

/// `a.b.0.c` into a resource.
pub fn path<'a>(r: &'a Value, p: &str) -> Option<&'a Value> {
    p.split('.')
        .try_fold(r, |v, seg| match seg.parse::<usize>() {
            Ok(i) => v.get(i),
            Err(_) => v.get(seg),
        })
}

pub fn path_str<'a>(r: &'a Value, p: &str) -> Option<&'a str> {
    path(r, p).and_then(Value::as_str)
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
        // Named for the fill.
        assert_eq!(rec.display, "Synthetic tablet 10 mg, bottle of 30");

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
}
