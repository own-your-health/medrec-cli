//! The vaccines a person's claims tell of (0.9.11 and later). A claim line
//! with a CVX code, or with a CPT code that CDC's table gives a CVX code, is
//! listed among the person's immunizations as from a claim. It stays its
//! claim: it makes no record, and has the claim's id, so what is private of
//! the claim is private of it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::{json, Map, Value};

use crate::model::{Code, Record};
use crate::standing::record_in_error;
use crate::summary::day;
use crate::tables::{CPT_CVX, CVX};

pub use crate::normalize::CVX as CVX_SYSTEM;

/// CVX codes that are no vaccine given ("no vaccine administered",
/// "unknown"), for a machine without CDC's CVX table.
const NO_VACCINE: &[u32] = &[998, 999];

/// What CDC's tables on this machine say of codes: of each CVX code whether it
/// is a vaccine, and each CPT code's CVX codes.
#[derive(Debug, Clone, Default)]
pub struct Codes {
    vaccine: Option<BTreeMap<u32, bool>>,
    of_cpt: Option<BTreeMap<String, Vec<String>>>,
}

impl Codes {
    /// From the tables on this machine, each when it is whole.
    pub fn here() -> Codes {
        crate::tables::dir().map_or_else(Codes::default, |d| Codes::load(&d))
    }

    pub fn load(dir: &Path) -> Codes {
        let vaccine = CVX.columns(dir, &[0, 5]).map(|rows| {
            let each = rows
                .into_iter()
                .filter_map(|r| Some((number(&r[0])?, r[1].eq_ignore_ascii_case("false"))));
            each.collect()
        });
        let of_cpt = CPT_CVX.columns(dir, &[0, 4]).map(|rows| {
            let mut m: BTreeMap<String, Vec<String>> = BTreeMap::new();
            for r in rows {
                m.entry(r[0].clone()).or_default().push(r[1].clone());
            }
            m
        });
        Codes { vaccine, of_cpt }
    }

    /// Invented tables: each CVX code with whether it is a vaccine, and each
    /// CPT code with its CVX codes.
    #[cfg(test)]
    pub fn invented(vaccine: &[(&str, bool)], of_cpt: &[(&str, &[&str])]) -> Codes {
        Codes {
            vaccine: Some(
                vaccine
                    .iter()
                    .map(|(c, v)| (number(c).unwrap(), *v))
                    .collect(),
            ),
            of_cpt: Some(
                of_cpt
                    .iter()
                    .map(|(c, x)| (c.to_string(), x.iter().map(|x| x.to_string()).collect()))
                    .collect(),
            ),
        }
    }

    /// The CVX codes of the vaccines a claim line names, as written. Its own
    /// CVX code is read with no table; a CPT code only with both, since only
    /// the CVX table tells a vaccine from an immune globulin.
    pub fn of_line(&self, line: &Value) -> Vec<String> {
        let service = line.get("service");
        let text = |k: &str| {
            service
                .and_then(|s| s.get(k))
                .and_then(Value::as_str)
                .map(str::trim)
        };
        let mut found: Vec<String> = Vec::new();
        found.extend(
            line.get("cvx")
                .and_then(Value::as_str)
                .map(|c| c.trim().to_string()),
        );
        if text("system") == Some(CVX_SYSTEM) {
            found.extend(text("code").map(str::to_string));
        }
        if found.is_empty() && crate::plain::is_cpt(text("system")) && self.vaccine.is_some() {
            let mapped = text("code").and_then(|c| self.of_cpt.as_ref()?.get(c));
            found.extend(mapped.into_iter().flatten().cloned());
        }
        found.retain(|c| self.is_vaccine(c));
        found.sort_by_key(|c| number(c));
        found.dedup_by_key(|c| number(c));
        found
    }

    fn is_vaccine(&self, cvx: &str) -> bool {
        let Some(n) = number(cvx) else {
            return false;
        };
        match &self.vaccine {
            Some(table) => table.get(&n) == Some(&true),
            None => !NO_VACCINE.contains(&n),
        }
    }
}

/// A CVX code's number: `03` and `3` are one code.
fn number(code: &str) -> Option<u32> {
    let c = code.trim();
    let digits = !c.is_empty() && c.len() <= 4 && c.bytes().all(|b| b.is_ascii_digit());
    digits.then(|| c.parse().ok()).flatten()
}

/// An immunization's CVX code, where it has one.
fn cvx_of(r: &Record) -> Option<u32> {
    let c = r.code.as_ref()?;
    (c.system.as_deref() == Some(CVX_SYSTEM)).then(|| number(c.code.as_deref()?))?
}

/// The day a claim line was given: its own, or its claim's.
fn line_day(line: &Value, claim: &Record) -> Option<String> {
    let own = match line.get("serviced") {
        Some(Value::String(d)) => Some(d.as_str()),
        Some(p) => p.get("start").and_then(Value::as_str),
        None => None,
    };
    day(own).or_else(|| day(claim.date.as_deref()))
}

/// The vaccines the claims of `records` tell of, as immunizations: each with
/// its claim's id and provenance, `fields.from_claim` true and `fields.cvx`
/// its CVX codes, and `code` the one of them where it has one. One is listed
/// for each vaccine and day that no immunization of `records` has, by its CVX
/// code, or by its kind of vaccine where that immunization has no CVX code.
pub fn of_claims(records: &[&Record], codes: &Codes) -> Vec<Record> {
    let shots = records
        .iter()
        .filter(|r| r.kind == "immunization" && !record_in_error(r));
    let mut seen: BTreeSet<(String, u32)> = BTreeSet::new();
    let mut kinds: BTreeSet<(String, &str)> = BTreeSet::new();
    for r in shots {
        let Some(d) = day(r.date.as_deref()) else {
            continue;
        };
        match cvx_of(r) {
            Some(n) => {
                seen.insert((d, n));
            }
            None => {
                if let Some(k) = crate::events::vaccine_family(&r.display) {
                    kinds.insert((d, k));
                }
            }
        }
    }
    let mut out = Vec::new();
    for r in records.iter().filter(|r| r.kind == "claim") {
        if !crate::fills::gave(r) {
            continue;
        }
        for line in crate::fills::counted_lines(r) {
            let cvx = codes.of_line(line);
            let Some(d) = line_day(line, r).filter(|_| !cvx.is_empty()) else {
                continue;
            };
            let numbers: Vec<u32> = cvx.iter().filter_map(|c| number(c)).collect();
            let text = line.pointer("/service/display").and_then(Value::as_str);
            let text = text
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .unwrap_or(&r.display);
            let filed = numbers.iter().any(|n| seen.contains(&(d.clone(), *n)))
                || crate::events::vaccine_family(text)
                    .is_some_and(|k| kinds.contains(&(d.clone(), k)));
            if filed {
                continue;
            }
            seen.extend(numbers.iter().map(|n| (d.clone(), *n)));
            let mut fields = Map::new();
            fields.insert("from_claim".into(), Value::Bool(true));
            fields.insert("cvx".into(), json!(cvx));
            let mut v = Record::clone(r);
            v.kind = "immunization".into();
            v.key = format!("{}|{}", r.key, line.get("sequence").unwrap_or(&Value::Null));
            v.date = Some(d);
            v.code = (cvx.len() == 1).then(|| Code {
                system: Some(CVX_SYSTEM.into()),
                code: Some(cvx[0].clone()),
                display: None,
            });
            v.display = text.to_string();
            v.status = Some("completed".into());
            v.fields = fields;
            v.providers.clear();
            out.push(v);
        }
    }
    out
}

/// Of `records`, the vaccines their claims tell of, by CDC's tables on this
/// machine.
pub fn listed(records: &[&Record]) -> Vec<Record> {
    of_claims(records, &Codes::here())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(v: Value) -> Record {
        serde_json::from_value(v).unwrap()
    }

    fn claim(id: &str, day: &str, lines: Value) -> Record {
        record(
            json!({"id": id, "kind": "claim", "key": format!("claim|{id}"), "date": day,
            "display": "Example Clinic · Example Health Plan", "status": "active",
            "fields": {"items": lines}, "provenance": []}),
        )
    }

    fn line(system: &str, code: &str) -> Value {
        json!({"sequence": 1, "service": {"system": system, "code": code, "display": "Sample vaccine"}})
    }

    fn shot(id: &str, day: &str, cvx: Option<&str>, display: &str) -> Record {
        let mut v = json!({"id": id, "kind": "immunization", "key": id, "date": day,
            "display": display, "status": "completed", "fields": {}, "provenance": []});
        if let Some(c) = cvx {
            v["code"] = json!({"system": CVX_SYSTEM, "code": c});
        }
        record(v)
    }

    const CPT: &str = "http://www.ama-assn.org/go/cpt";

    fn tables() -> Codes {
        Codes::invented(
            &[("900", true), ("901", false), ("902", true)],
            &[
                ("00011", &["900"]),
                ("00012", &["901"]),
                ("00013", &["900", "902"]),
            ],
        )
    }

    #[test]
    fn a_line_names_a_vaccine_by_its_cvx_code_or_cdcs_cpt_table() {
        let t = tables();
        assert_eq!(t.of_line(&line(CPT, "00011")), ["900"]);
        assert_eq!(t.of_line(&line(CPT, "00013")), ["900", "902"]);
        // An immune globulin, and a code CDC does not list.
        assert!(t.of_line(&line(CPT, "00012")).is_empty());
        assert!(t.of_line(&line(CPT, "00014")).is_empty());
        assert_eq!(t.of_line(&line(CVX_SYSTEM, "0900")), ["0900"]);
        let beside = json!({"service": {"system": CPT, "code": "00099"}, "cvx": "902"});
        assert_eq!(t.of_line(&beside), ["902"]);
        // With no tables, only a line's own CVX code, and never "unknown".
        let none = Codes::default();
        assert!(none.of_line(&line(CPT, "00011")).is_empty());
        assert_eq!(none.of_line(&line(CVX_SYSTEM, "900")), ["900"]);
        assert!(none.of_line(&line(CVX_SYSTEM, "999")).is_empty());
        // The CPT table alone tells no vaccine from a globulin.
        let cpt_only = Codes {
            vaccine: None,
            ..tables()
        };
        assert!(cpt_only.of_line(&line(CPT, "00011")).is_empty());
    }

    #[test]
    fn a_claims_vaccine_is_listed_once_where_no_record_has_it() {
        let t = tables();
        let records = [
            claim(
                "r_givenaaaaaaaaaaaaaaa",
                "2024-05-01",
                json!([line(CPT, "00011")]),
            ),
            // A record has that vaccine and day.
            claim(
                "r_filedaaaaaaaaaaaaaaa",
                "2024-05-02",
                json!([line(CPT, "00011")]),
            ),
            shot(
                "r_shotaaaaaaaaaaaaaaaa",
                "2024-05-02",
                Some("900"),
                "Sample vaccine",
            ),
            // A record of that day with no CVX code, of the same kind.
            claim(
                "r_kindaaaaaaaaaaaaaaaa",
                "2024-05-03",
                json!([{"service": {"system": CPT, "code": "00011", "display": "Influenza vaccine"}}]),
            ),
            shot("r_flushotaaaaaaaaaaaaa", "2024-05-03", None, "Flu shot"),
            // Another claim of the same vaccine and day.
            claim(
                "r_twiceaaaaaaaaaaaaaaa",
                "2024-05-01",
                json!([line(CPT, "00011")]),
            ),
            claim(
                "r_cancelaaaaaaaaaaaaaa",
                "2024-05-04",
                json!([line(CPT, "00011")]),
            ),
            claim(
                "r_leftoutaaaaaaaaaaaaa",
                "2024-05-05",
                json!([{"service": {"system": CPT, "code": "00011"}, "counted": false}]),
            ),
            claim(
                "r_denyaaaaaaaaaaaaaaaa",
                "2024-05-06",
                json!([{"service": {"system": CPT, "code": "00011"},
                "adjudication": [{"category": "Denial Reason", "amount": null, "reason": "Sample"}]}]),
            ),
            claim(
                "r_ownedaaaaaaaaaaaaaaa",
                "2024-05-07",
                json!([{"service": {"system": CPT, "code": "00099"}, "serviced": "2024-05-08"},
                       {"service": {"system": CPT, "code": "00011"}, "serviced": {"start": "2024-05-09"}}]),
            ),
        ];
        let mut records: Vec<Record> = records.into_iter().collect();
        records[6].status = Some("cancelled".into());
        let all: Vec<&Record> = records.iter().collect();
        let listed = of_claims(&all, &t);
        let got: Vec<(&str, Option<&str>, &str)> = listed
            .iter()
            .map(|v| (v.id.as_str(), v.date.as_deref(), v.display.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                (
                    "r_givenaaaaaaaaaaaaaaa",
                    Some("2024-05-01"),
                    "Sample vaccine"
                ),
                (
                    "r_ownedaaaaaaaaaaaaaaa",
                    Some("2024-05-09"),
                    "Example Clinic · Example Health Plan"
                ),
            ]
        );
        let v = &listed[0];
        assert_eq!(
            (v.kind.as_str(), v.status.as_deref()),
            ("immunization", Some("completed"))
        );
        assert_eq!(v.fields["from_claim"], true);
        assert_eq!(v.fields["cvx"], json!(["900"]));
        assert_eq!(v.code.as_ref().and_then(|c| c.code.as_deref()), Some("900"));
        // Two vaccines of one CPT code: both, and no one code.
        let both = claim(
            "r_bothaaaaaaaaaaaaaaaa",
            "2024-06-01",
            json!([line(CPT, "00013")]),
        );
        let listed = of_claims(&[&both], &t);
        assert_eq!(listed[0].fields["cvx"], json!(["900", "902"]));
        assert!(listed[0].code.is_none());
    }
}
