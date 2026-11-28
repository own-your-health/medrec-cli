//! The vaccines a person's claims tell of (0.9.11 and later). A claim line
//! with a CVX code, or with a CPT code that CDC's table gives a CVX code, is
//! listed among the person's immunizations as from a claim. It stays its
//! claim: it makes no record, and has the claim's id, so what is private of
//! the claim is private of it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::{json, Map, Value};

use crate::model::{Code, Record};
use crate::parts::Parts;
use crate::standing::record_in_error;
use crate::summary::day;
use crate::tables::{CPT_CVX, CVX};

pub use crate::normalize::CVX as CVX_SYSTEM;

/// CVX codes that are no vaccine given ("no vaccine administered",
/// "unknown"), with CDC's CVX table or without it.
const NO_VACCINE: &[u32] = &[998, 999];

/// What CDC's tables on this machine say of codes: of each CVX code whether it
/// is a vaccine, and each CPT code's CVX codes.
#[derive(Debug, Clone, Default)]
pub struct Codes {
    vaccine: Option<BTreeMap<u32, bool>>,
    of_cpt: Option<BTreeMap<String, Vec<String>>>,
    /// `<id> <SHA-256>` of both tables, as a view records them; none where
    /// either is not here.
    identity: Option<String>,
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
        let ids: Vec<String> = [&CVX, &CPT_CVX]
            .iter()
            .filter_map(|t| t.identity(dir))
            .collect();
        // One table alone names no vaccine by a CPT code, so it is not a set a
        // pass makes views again by.
        let identity = (ids.len() == 2).then(|| ids.join(", "));
        Codes {
            vaccine,
            of_cpt,
            identity,
        }
    }

    /// The tables these codes are of, `<id> <SHA-256>` each, comma-separated;
    /// none without both.
    pub fn identity(&self) -> Option<&str> {
        self.identity.as_deref()
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
            identity: Some("invented".into()),
        }
    }

    /// The CVX codes of the vaccines a claim line names, as written, by any
    /// code of its service, first or not (`codes`). Its own CVX code is read
    /// with no table, and is none only where CDC's CVX table marks it no
    /// vaccine. A CPT code is read only where the line has no CVX code, with
    /// both tables, and only to a CVX code the CVX table lists as a vaccine,
    /// since only it tells a vaccine from an immune globulin. 998 and 999 are
    /// never one.
    pub fn of_line(&self, line: &Value) -> Vec<String> {
        let codes = crate::claims::service_codes(line);
        let codes: Vec<(&str, &str)> = codes.into_iter().map(|(s, c)| (s, c.trim())).collect();
        let own: Vec<String> = codes
            .iter()
            .filter(|(system, code)| *system == CVX_SYSTEM && !code.is_empty())
            .map(|(_, code)| code.to_string())
            .collect();
        let given = |c: &String| number(c).filter(|n| !NO_VACCINE.contains(n));
        let table = self.vaccine.as_ref();
        let mut found: Vec<String> = match (&own[..], table, &self.of_cpt) {
            ([], Some(table), Some(of_cpt)) => {
                let cpt = codes
                    .iter()
                    .filter(|(system, _)| crate::plain::is_cpt(Some(system)));
                let mapped = cpt.filter_map(|(_, code)| of_cpt.get(*code)).flatten();
                let listed = |c: &&String| given(c).is_some_and(|n| table.get(&n) == Some(&true));
                mapped.filter(listed).cloned().collect()
            }
            _ => {
                let marked = |n: u32| table.is_some_and(|t| t.get(&n) == Some(&false));
                let is = |c: &String| given(c).is_some_and(|n| !marked(n));
                own.into_iter().filter(is).collect()
            }
        };
        found.sort_by_key(|c| number(c));
        found.dedup_by_key(|c| number(c));
        found
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
/// code, or by its kind of vaccine where that immunization has no CVX code;
/// one entered in error or not given (a refusal) is none. No line `held`
/// leaves out of its claim (`private.json`'s `parts`) tells of one.
pub fn of_claims(records: &[&Record], codes: &Codes, held: &Parts) -> Vec<Record> {
    let given = |r: &&&Record| r.status.as_deref() != Some("not-done");
    let shots = records
        .iter()
        .filter(|r| r.kind == "immunization" && !record_in_error(r))
        .filter(given);
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
        // Its name where a line has none, as it is shown without what is held.
        let shown = held.record(r);
        let lines = crate::fills::counted_lines(r);
        for (_, line) in lines.filter(|(i, _)| !held.leaves_out(&r.id, "items", *i)) {
            let cvx = codes.of_line(line);
            let Some(d) = line_day(line, r).filter(|_| !cvx.is_empty()) else {
                continue;
            };
            let numbers: Vec<u32> = cvx.iter().filter_map(|c| number(c)).collect();
            let text = line.pointer("/service/display").and_then(Value::as_str);
            let text = text
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .unwrap_or(&shown.display);
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
/// machine, for the person at its terminal: the whole of each claim.
pub fn listed(records: &[&Record]) -> Vec<Record> {
    of_claims(records, &Codes::here(), &Parts::default())
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
                ("00015", &["950"]),
            ],
        )
    }

    #[test]
    fn a_line_names_a_vaccine_by_its_cvx_code_or_cdcs_cpt_table() {
        let t = tables();
        assert_eq!(t.of_line(&line(CPT, "00011")), ["900"]);
        assert_eq!(t.of_line(&line(CPT, "00013")), ["900", "902"]);
        // An immune globulin, a code CDC does not list, and one it gives a
        // CVX code its CVX table does not.
        assert!(t.of_line(&line(CPT, "00012")).is_empty());
        assert!(t.of_line(&line(CPT, "00014")).is_empty());
        assert!(t.of_line(&line(CPT, "00015")).is_empty());
        assert_eq!(t.of_line(&line(CVX_SYSTEM, "0900")), ["0900"]);
        // A line's own CVX code that the table does not list, newer than it,
        // is still one; one it marks no vaccine is none, as 998 and 999 are
        // whatever a table says of them.
        assert_eq!(t.of_line(&line(CVX_SYSTEM, "950")), ["950"]);
        assert!(t.of_line(&line(CVX_SYSTEM, "901")).is_empty());
        let says = Codes::invented(&[("998", true), ("999", true)], &[("00016", &["998"])]);
        for l in [
            line(CVX_SYSTEM, "998"),
            line(CVX_SYSTEM, "999"),
            line(CPT, "00016"),
        ] {
            assert!(says.of_line(&l).is_empty(), "{l}");
            assert!(t.of_line(&l).is_empty(), "{l}");
        }
        // A CVX code after a code of the payer's: its own, before any table's.
        let beside = json!({"service": {"system": CPT, "code": "00011"}, "codes": [
            {"system": CPT, "code": "00011"}, {"system": CVX_SYSTEM, "code": "902"}]});
        assert_eq!(t.of_line(&beside), ["902"]);
        let after = json!({"service": {"system": "http://example.invalid/item", "code": "I-1"},
            "codes": [{"system": "http://example.invalid/item", "code": "I-1"},
            {"system": CPT, "code": "00013"}]});
        assert_eq!(t.of_line(&after), ["900", "902"]);
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
        // A refusal of that vaccine on a claim's day is no dose of it, nor is
        // one entered in error.
        let refused = |id: &str, day: &str, status: &str| {
            let mut r = shot(id, day, Some("900"), "Sample vaccine");
            r.status = Some(status.into());
            r
        };
        records.push(claim(
            "r_refusedaaaaaaaaaaaaa",
            "2024-05-10",
            json!([line(CPT, "00011")]),
        ));
        records.push(refused("r_notdoneaaaaaaaaaaaaa", "2024-05-10", "not-done"));
        records.push(refused(
            "r_wrongaaaaaaaaaaaaaaa",
            "2024-05-10",
            "entered-in-error",
        ));
        let all: Vec<&Record> = records.iter().collect();
        let listed = of_claims(&all, &t, &Parts::default());
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
                (
                    "r_refusedaaaaaaaaaaaaa",
                    Some("2024-05-10"),
                    "Sample vaccine"
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
        let listed = of_claims(&[&both], &t, &Parts::default());
        assert_eq!(listed[0].fields["cvx"], json!(["900", "902"]));
        assert!(listed[0].code.is_none());
    }

    /// A line `private.json` leaves out of its claim tells of no vaccine,
    /// though its claim is open: the vaccine would show its name and day.
    #[test]
    fn a_private_line_of_an_open_claim_tells_of_no_vaccine() {
        let t = tables();
        let mixed = claim(
            "r_mixedaaaaaaaaaaaaaaa",
            "2024-07-01",
            json!([
                {"sequence": 1, "service": {"system": CPT, "code": "00011", "display": "Sample vaccine"}},
                {"sequence": 2, "service": {"system": CVX_SYSTEM, "code": "902", "display": "Sample vaccine B"}}
            ]),
        );
        let all = [&mixed];
        assert_eq!(of_claims(&all, &t, &Parts::default()).len(), 2);
        let private = json!({"parts": {"r_mixedaaaaaaaaaaaaaaa": {"lines": [1]}}});
        let held = Parts::of(&private);
        let listed = of_claims(&all, &t, &held);
        let shown: Vec<&str> = listed.iter().map(|v| v.display.as_str()).collect();
        assert_eq!(shown, ["Sample vaccine"]);
        assert_eq!(listed[0].fields["cvx"], json!(["900"]));
        // Another claim's parts leave none of its lines out.
        let other = Parts::of(&json!({"parts": {"r_otheraaaaaaaaaaaaaaa": {"lines": [0, 1]}}}));
        assert_eq!(of_claims(&all, &t, &other).len(), 2);

        // A vaccine line with no name of its own takes the claim's as it is
        // shown without the private line, never the name that line gave it.
        let mut untitled = claim(
            "r_untitledaaaaaaaaaaaa",
            "2024-07-02",
            json!([
                {"sequence": 1, "service": {"system": CPT, "code": "00099", "display": "Sample counseling"},
                 "payer_notes": ["Psychotherapy is paid apart"]},
                {"sequence": 2, "service": {"system": CVX_SYSTEM, "code": "900"}}
            ]),
        );
        untitled.display = "Sample counseling · Example Clinic".into();
        let private = json!({"parts": {"r_untitledaaaaaaaaaaaa": {"lines": [0]}}});
        let held = Parts::of(&private);
        let listed = of_claims(&[&untitled], &t, &held);
        assert_eq!(listed.len(), 1);
        assert!(
            !listed[0].display.contains("counseling"),
            "{}",
            listed[0].display
        );
        assert_eq!(listed[0].display, held.record(&untitled).display);
        let whole = of_claims(&[&untitled], &t, &Parts::default());
        assert_eq!(whole[0].display, untitled.display);
    }

    /// Views record the tables only as a pair: one alone names no vaccine by
    /// a CPT code.
    #[test]
    fn one_table_alone_is_no_set_of_tables() {
        let dir = std::env::temp_dir().join(format!("medrec-vaccine-set-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vaccines");
        let keep = |t: &crate::tables::Table, file: &str| {
            let path = fixtures.join(file);
            let sha = crate::hash::sha256_hex(&std::fs::read(&path).unwrap());
            crate::tables::fetch(t, &dir, Some(&path), Some(&sha), true).unwrap();
            sha
        };
        assert_eq!(Codes::load(&dir).identity(), None);
        let cvx = keep(&CVX, "cvx.txt");
        let alone = Codes::load(&dir);
        assert!(alone.vaccine.is_some());
        assert_eq!(alone.identity(), None);
        let cpt = keep(&CPT_CVX, "cpt.txt");
        let both = format!("cvx {cvx}, cpt-cvx {cpt}");
        assert_eq!(Codes::load(&dir).identity(), Some(both.as_str()));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
