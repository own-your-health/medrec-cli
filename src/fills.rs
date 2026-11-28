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
use crate::standing::{is_fill, record_in_error, Filled};
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

/// The codes of the drug a claim is for.
fn of_claim(r: &Record) -> impl Iterator<Item = Drug> + '_ {
    let lines = r.fields.get("items").and_then(Value::as_array);
    let counted = lines
        .into_iter()
        .flatten()
        .filter(|l| l.get("counted") != Some(&Value::Bool(false)));
    let service = counted.filter_map(|l| l.get("service"));
    service.filter_map(|s| {
        let text = |k: &str| s.get(k).and_then(Value::as_str);
        coded(text("system"), text("code"))
    })
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
        if let (true, Some(day)) = (is_fill(r), day(r.date.as_deref())) {
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
        if skipped || record_in_error(r) {
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
            ("~sample syrup".into(), of(&[("uncoded", "2024-05-10")])),
        ]);
        assert_eq!(fills(&idx()), want);
    }

    /// A fill the pharmacy says it never gave is none of its own.
    #[test]
    fn a_claim_yields_to_the_pharmacys_own_record_of_its_day() {
        let mut idx = idx();
        let of = idx.records.iter_mut().find(|r| is_fill(r)).unwrap();
        of.status = Some("entered-in-error".into());
        let fills = fills(&idx);
        let of = &fills[&format!("{RXNORM}|RX-1")];
        assert!(of.iter().any(|f| f.0 == "dispensed"), "{of:?}");
        // Its package tells no medicine then.
        assert!(of.iter().all(|f| f.0 != "package"), "{of:?}");
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
