//! The fills that a payer's pharmacy claims tell of (docs/data-contract.md,
//! "Standing", `filled`). A claim stays one record: it is a fill of the
//! medicine it names, and no medicine. One that the pharmacy's own records
//! have on its day is theirs.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::events::SKIPPED_STATUS;
use crate::intake::classify;
use crate::model::{PersonIndex, Record};
use crate::normalize::{NDC, RXNORM};
use crate::standing::{gave_fill, record_in_error, Filled};
use crate::summary::{day, series_key};

/// What a drug is told by.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Drug {
    RxNorm(String),
    Ndc(String),
    /// Its folded name.
    Name(String),
}

/// An NDC as eleven digits: five of the labeler, four of the product and two
/// of the package. Ten with no hyphens can be one of three, and are as they are.
fn ndc(text: &str) -> Option<String> {
    let digits = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
    let parts: Vec<&str> = text.trim().split('-').collect();
    if !parts.iter().all(|p| digits(p)) {
        return None;
    }
    match parts.as_slice() {
        [all] if matches!(all.len(), 10 | 11) => Some(all.to_string()),
        [l, p, k] if l.len() <= 5 && p.len() <= 4 && k.len() <= 2 => {
            Some(format!("{l:0>5}{p:0>4}{k:0>2}"))
        }
        _ => None,
    }
}

fn coded(system: Option<&str>, code: Option<&str>) -> Option<Drug> {
    let code = code.map(str::trim).filter(|c| !c.is_empty())?;
    match system? {
        RXNORM => Some(Drug::RxNorm(code.to_string())),
        NDC => ndc(code).map(Drug::Ndc),
        _ => None,
    }
}

fn named(name: &str) -> Option<Drug> {
    Some(classify::fold(name))
        .filter(|n| !n.is_empty())
        .map(Drug::Name)
}

/// What a medicine is told by: its codes and its name.
fn of_medicine(r: &Record) -> impl Iterator<Item = Drug> + '_ {
    let code = r.code.as_ref();
    let code = code.and_then(|c| coded(c.system.as_deref(), c.code.as_deref()));
    let package = r.fields.get("ndc").and_then(Value::as_str);
    let package = package.and_then(|n| coded(Some(NDC), Some(n)));
    code.into_iter().chain(package).chain(named(&r.display))
}

/// The lines a claim's totals count.
fn counted(r: &Record) -> impl Iterator<Item = &Value> + '_ {
    let lines = r.fields.get("items").and_then(Value::as_array);
    lines
        .into_iter()
        .flatten()
        .filter(|l| l.get("counted") != Some(&Value::Bool(false)))
}

/// A claim the plan denied: each line its totals count gives a reason it was
/// denied, or a pharmacy's that it was rejected (CARIN's `denialreason` and
/// `rejectreason`, by their code or their name).
fn denied(r: &Record) -> bool {
    let says = |l: &Value| {
        let of = l.get("adjudication").and_then(Value::as_array);
        let categories = of
            .into_iter()
            .flatten()
            .filter_map(|a| a["category"].as_str());
        categories.map(classify::fold).any(|c| {
            let c = c.replace(' ', "");
            c == "denialreason" || c == "rejectreason"
        })
    };
    let mut lines = counted(r).peekable();
    lines.peek().is_some() && lines.all(says)
}

/// Whether the household paid any of a claim: the member's share of it, or
/// of a line its totals count, is more than nothing.
fn paid_some(r: &Record) -> bool {
    let more = |a: &Value| {
        let share = crate::claims::member_share(a);
        let units = share.and_then(|(v, _)| crate::claims::units(v["amount"].as_str()?));
        units.is_some_and(|u| u > 0)
    };
    let mut lines = counted(r).filter_map(|l| l.get("amounts"));
    r.fields.get("amounts").is_some_and(more) || lines.any(more)
}

/// The codes of the drug a claim is for: each code of a line its totals
/// count, first or not.
fn of_claim(r: &Record) -> impl Iterator<Item = Drug> + '_ {
    let codes = counted(r).flat_map(crate::claims::service_codes);
    codes.filter_map(|(system, code)| coded(Some(system), Some(code)))
}

/// The fills that claims tell of, by the line of the medicine (`series_key`)
/// each is of: by an equal code, else by an equal name.
pub fn of_claims(idx: &PersonIndex) -> BTreeMap<String, Vec<Filled>> {
    let mut lines: BTreeMap<Drug, BTreeSet<String>> = BTreeMap::new();
    let mut dispensed: BTreeSet<(String, String)> = BTreeSet::new();
    for r in idx.records.iter().filter(|r| r.kind == "medication") {
        if record_in_error(r) {
            continue;
        }
        let line = series_key(r);
        for drug in of_medicine(r) {
            lines.entry(drug).or_default().insert(line.clone());
        }
        if let (true, Some(day)) = (gave_fill(r), day(r.date.as_deref())) {
            dispensed.insert((line, day));
        }
    }
    let mut out: BTreeMap<String, Vec<Filled>> = BTreeMap::new();
    for r in idx.records.iter().filter(|r| r.kind == "claim") {
        let skipped = r.status.as_deref();
        let skipped = skipped.is_some_and(|s| SKIPPED_STATUS.contains(&s));
        let (Some(name), Some(day)) = (crate::private::dispensed(r), day(r.date.as_deref())) else {
            continue;
        };
        // A reversal is cancelled; a denial the household paid none of gave
        // nothing.
        if skipped || record_in_error(r) || (denied(r) && !paid_some(r)) {
            continue;
        }
        let mut of: BTreeSet<&String> = of_claim(r)
            .filter_map(|drug| lines.get(&drug))
            .flatten()
            .collect();
        if of.is_empty() {
            let by_name = named(name).and_then(|drug| lines.get(&drug));
            of.extend(by_name.into_iter().flatten());
        }
        for line in of {
            if !dispensed.contains(&(line.clone(), day.clone())) {
                out.entry(line.clone()).or_default().push(Filled {
                    record: r.id.clone(),
                    date: day.clone(),
                    from_claim: true,
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::standing::is_fill;

    fn idx() -> PersonIndex {
        let text = include_str!("../tests/fixtures/fills/index.json");
        serde_json::from_str(text).unwrap()
    }

    /// Each line's, by the stem of the claim's id, with its day.
    fn fills(idx: &PersonIndex) -> BTreeMap<String, Vec<(String, String)>> {
        let stem = |id: &str| id[2..].trim_end_matches('a').to_string();
        let each = |f: Filled| {
            assert!(f.from_claim);
            (stem(&f.record), f.date)
        };
        let all = of_claims(idx).into_iter();
        all.map(|(line, f)| (line, f.into_iter().map(each).collect()))
            .collect()
    }

    #[test]
    fn a_claim_is_a_fill_of_the_medicine_it_names() {
        let line = |code: &str| format!("{RXNORM}|{code}");
        let of = |list: &[(&str, &str)]| -> Vec<(String, String)> {
            let each = list.iter();
            each.map(|(a, b)| (a.to_string(), b.to_string())).collect()
        };
        let want = BTreeMap::from([
            (
                line("RX-1"),
                of(&[
                    ("package", "2024-06-07"),
                    ("coded", "2024-05-06"),
                    ("early", "2024-03-04"),
                ]),
            ),
            (line("RX-2"), of(&[("named", "2024-05-07")])),
            (line("RX-3"), of(&[("codefirst", "2024-05-09")])),
            (
                line("RX-4"),
                of(&[
                    ("partly", "2024-05-14"),
                    ("deniedpaid", "2024-05-13"),
                    ("claimpaid", "2024-05-12"),
                    ("copayonly", "2024-05-11"),
                ]),
            ),
            (line("RX-5"), of(&[("notgiven", "2024-05-20")])),
            ("~sample syrup".into(), of(&[("uncoded", "2024-05-10")])),
        ]);
        assert_eq!(fills(&idx()), want);
    }

    /// A fill the pharmacy says it never gave is none of its own.
    #[test]
    fn a_claim_yields_to_the_pharmacys_own_record_of_its_day() {
        let mut idx = idx();
        let mut of = idx.records.iter_mut();
        let of = of
            .find(|r| is_fill(r) && r.id.starts_with("r_capsulefill"))
            .unwrap();
        of.status = Some("entered-in-error".into());
        let fills = fills(&idx);
        let of = &fills[&format!("{RXNORM}|RX-1")];
        assert!(of.iter().any(|f| f.0 == "dispensed"), "{of:?}");
        // Its package tells no medicine then.
        assert!(of.iter().all(|f| f.0 != "package"), "{of:?}");
    }

    /// A payer can give a drug's RxNorm code or NDC after a code of its own,
    /// and normalizing keeps every one of them.
    #[test]
    fn each_code_of_a_line_tells_its_drug_whichever_is_first() {
        use crate::model::{Provenance, ProvenanceSource};
        let eob: Value = serde_json::from_str(include_str!(
            "../tests/fixtures/fhir/pharmacy-eob-codings.json"
        ))
        .unwrap();
        let provenance = Provenance {
            source: ProvenanceSource {
                kind: "fhir".into(),
                cli: Some("fakeprov".into()),
                cli_version: None,
                organization_id: None,
                fhir_base: None,
            },
            method: "fhir-api".into(),
            source_resource: None,
            fetched_at: None,
            filed_at: "2026-03-11T00:00:00Z".into(),
            raw_path: None,
            classified_by: None,
            derived_from: None,
            filed_by: crate::model::filed_by(),
        };
        let claim = crate::normalize::record(&eob, provenance).unwrap();
        let line = &claim.fields["items"][0];
        assert_eq!(
            line["service"]["system"],
            "http://example.invalid/pharmacy-item"
        );
        assert_eq!(line["codes"].as_array().unwrap().len(), 3, "{line}");
        let codes: Vec<Drug> = of_claim(&claim).collect();
        assert_eq!(
            codes,
            [Drug::Ndc("00000000601".into()), Drug::RxNorm("RX-6".into())]
        );
        // So it is a fill of the medicine of that code, under another name.
        let mut idx = idx();
        idx.records.push(claim);
        let fills = fills(&idx);
        let of = &fills[&format!("{RXNORM}|RX-6")];
        assert_eq!(of.len(), 1, "{of:?}");
        assert_eq!(of[0].1, "2026-03-10");
        // A line of one coding has no `codes`.
        let one = idx
            .records
            .iter()
            .find(|r| r.id.starts_with("r_coded"))
            .unwrap();
        assert!(one.fields["items"][0].get("codes").is_none());
    }

    #[test]
    fn an_ndc_is_the_same_in_each_of_its_forms() {
        for (a, b) in [
            ("0000-0001-01", "00000000101"),
            ("00000-001-01", "00000000101"),
            ("00000-0001-1", "00000000101"),
            ("00000-0001-01", "00000000101"),
        ] {
            assert_eq!(ndc(a), ndc(b), "{a}");
        }
        // Ten digits can be any of three.
        assert_eq!(ndc("0000000101").as_deref(), Some("0000000101"));
        for other in ["", "-", "0000", "00000-0001", "000000-0001-01", "Sample"] {
            assert_eq!(ndc(other), None, "{other}");
        }
    }
}
