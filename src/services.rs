//! Which area a service is of: a claim's line or a procedure, by its CPT or
//! HCPCS number, by a revenue code and by the words of its name
//! (docs/data-contract.md, "Private"). The numbers alone: no name of one is
//! here.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use crate::claims;
use crate::model::Record;
use crate::private::{self, MENTAL_HEALTH, REPRODUCTIVE, SEXUAL_HEALTH, SUBSTANCE_USE};

/// CPT and HCPCS numbers, inclusive.
const NUMBERS: [(&str, &str); 4] = [
    (
        SUBSTANCE_USE,
        "80305-80307 80320-80377 99406-99409 \
         1000F 1031F-1036F 3016F 4000F-4001F 4004F 4290F-4293F 4303F-4306F 4320F \
         G0029-G0030 G0137 G0396-G0397 G0436-G0437 G0442-G0443 G0480-G0483 G0659 G2011 \
         G2067-G2080 G2086-G2088 G2213 G2215-G2216 G9016 G9275-G9276 G9458-G9460 \
         H0001-H0030 H0047-H0050 H2034-H2036 \
         J0570-J0578 J2315 Q9991-Q9992 S0109 S9453 S9475 T1006-T1007 T1012",
    ),
    (
        MENTAL_HEALTH,
        "90785-90899 96127 96130-96139 96146 96156-96159 96161 96164-96171 99484 99492-99494 \
         0545F 1040F 1220F 2060F 3085F 3088F-3093F 3351F-3354F 3725F 4060F-4067F \
         G0017-G0018 G0176-G0177 G0323 G0410-G0411 G0444 G0502-G0504 G0507 G0512 G0544 \
         G0552-G0554 G0560 G2214 G8431-G8433 G8510-G8511 G9717 \
         H0031-H0046 H2010-H2033 H2040-H2041 \
         J0400-J0402 J1630-J1631 J1942-J1944 J2358 J2426-J2427 J2794 J2798-J2799 J2801 J3486 \
         S0201 S9480 S9482-S9485",
    ),
    (
        SEXUAL_HEALTH,
        "86592-86593 86689 86696 86701-86703 86780 87110 87270 87320 87389-87391 87490-87492 \
         87534-87539 87590-87592 87660-87661 87806 87808 87810 87850 \
         0353U 0402U 3511F-3512F \
         G0011-G0013 G0432-G0433 G0435 G0445 G0475 J0739 J0741 J0750-J0751 Q0516-Q0520",
    ),
    (
        REPRODUCTIVE,
        "11976 55250 55870 57170 58300-58301 58321-58323 58600-58615 58670-58671 58970-58976 \
         59100-59151 59812-59857 76948 81025 82166 84702-84704 89250-89398 \
         A4261 A4264 A4266-A4269 G0027 H1010 J7294-J7298 J7300-J7307 \
         S0190-S0191 S0199 S2260-S2267 S4011-S4042 S4981 S4989 S4993",
    ),
];

/// Revenue codes, by number.
const REVENUE: [(&str, &str); 2] = [
    (
        MENTAL_HEALTH,
        "114 124 134 144 154 204 513 900-943 946-949 961 1000-1001 1003-1006",
    ),
    (SUBSTANCE_USE, "116 126 136 146 156 944-945 1002"),
];

/// A number as a range can hold it: what stands before its digits or after
/// them, and the digits.
type Number = (char, u32);

fn number(code: &str) -> Option<Number> {
    let code = code.trim();
    let bytes = code.as_bytes();
    if bytes.len() != 5 || !code.is_ascii() {
        return None;
    }
    let digits = |s: &str| {
        s.bytes()
            .all(|b| b.is_ascii_digit())
            .then(|| s.parse().ok())?
    };
    let (first, last) = (bytes[0], bytes[4]);
    if first.is_ascii_alphabetic() {
        return Some((first.to_ascii_uppercase() as char, digits(&code[1..])?));
    }
    // A Category II code's letter comes last, as a laboratory's own does.
    match last.to_ascii_uppercase() {
        b'F' => Some(('f', digits(&code[..4])?)),
        b'U' => Some(('u', digits(&code[..4])?)),
        _ => Some((' ', digits(code)?)),
    }
}

type Ranges = Vec<(Number, Number, &'static str)>;

static RANGES: LazyLock<Ranges> = LazyLock::new(|| ranges(&NUMBERS, number));
static REVENUES: LazyLock<Ranges> = LazyLock::new(|| ranges(&REVENUE, revenue));

fn ranges(table: &[(&'static str, &'static str)], of: fn(&str) -> Option<Number>) -> Ranges {
    let one = |area, text: &str| {
        let (a, z) = text.split_once('-').unwrap_or((text, text));
        Some((of(a)?, of(z)?, area))
    };
    let each = table.iter().map(|(area, all)| (*area, *all));
    each.flat_map(|(area, all)| all.split_whitespace().filter_map(move |t| one(area, t)))
        .collect()
}

fn revenue(code: &str) -> Option<Number> {
    let code = code.trim();
    let digits = (3..=4).contains(&code.len()) && code.bytes().all(|b| b.is_ascii_digit());
    digits.then(|| Some((' ', code.parse().ok()?)))?
}

fn within(ranges: &Ranges, n: Number) -> Option<&'static str> {
    let holds = |(a, z, _): &&(Number, Number, &str)| a.0 == n.0 && (a.1..=z.1).contains(&n.1);
    ranges.iter().find(holds).map(|(.., area)| *area)
}

/// The area of a CPT or HCPCS number.
pub fn by_number(code: &str) -> Option<&'static str> {
    within(&RANGES, number(code)?)
}

/// Whether a code of this system can be a CPT or HCPCS number: a line's bare
/// one can, and a medicine's or SNOMED's is none.
fn is_service(system: Option<&str>) -> bool {
    const OTHERS: [&str; 5] = ["ndc", "rxnorm", "snomed", "loinc", "icd-10"];
    system.is_none_or(|s| {
        let s = s.to_ascii_lowercase();
        !OTHERS.iter().any(|o| s.contains(o))
    })
}

/// The area of a coded service: its number's, else what its name says.
pub fn coded(system: Option<&str>, code: Option<&str>, name: Option<&str>) -> Option<&'static str> {
    let numbered = code.filter(|_| is_service(system)).and_then(by_number);
    numbered.or_else(|| private::words(name?))
}

/// The area of a claim's line: of its service, its revenue code, or what the
/// payer notes of it.
pub fn line(l: &Value) -> Option<&'static str> {
    let text = |k: &str, of: &str| l.get(k)?.get(of)?.as_str();
    let service = |of| text("service", of);
    let notes = l.get("payer_notes").and_then(Value::as_array);
    let notes = notes.into_iter().flatten().filter_map(Value::as_str);
    let code = text("revenue", "code").and_then(revenue);
    coded(service("system"), service("code"), service("display"))
        .or_else(|| within(&REVENUES, code?))
        .or_else(|| private::words(text("revenue", "display")?))
        .or_else(|| notes.into_iter().find_map(private::words))
}

/// The area of a claim every counted line of which is of one, the first's; of
/// one with no such line, what its own name says.
pub fn claim(r: &Record) -> Option<&'static str> {
    let lines = r.fields.get("items").and_then(Value::as_array);
    let counted = |l: &&Value| claims::counted(l);
    let mut counted = lines.into_iter().flatten().filter(counted).map(line);
    let Some(first) = counted.next() else {
        return private::words(r.display.split(" · ").next()?);
    };
    counted.all(|a| a.is_some()).then_some(first?)
}

/// The area of a procedure.
pub fn procedure(r: &Record) -> Option<&'static str> {
    let code = r.code.as_ref();
    let system = code.and_then(|c| c.system.as_deref());
    let number = code.and_then(|c| c.code.as_deref());
    let names = [
        Some(r.display.as_str()),
        code.and_then(|c| c.display.as_deref()),
    ];
    names.into_iter().find_map(|n| coded(system, number, n))
}

/// What the rules read of a claim's lines ([`crate::given`]).
pub fn read_of(items: &Value) -> Value {
    let keep = |l: &Value| {
        let parts = ["service", "revenue", "payer_notes", "counted"];
        let had = parts
            .iter()
            .filter_map(|k| Some((k.to_string(), l.get(*k)?.clone())));
        Value::Object(had.collect())
    };
    Value::Array(items.as_array().into_iter().flatten().map(keep).collect())
}

/// The area any words or any number of a line of text says. The five digits
/// after a state's letters are a place's.
pub fn text(line: &str) -> Option<&'static str> {
    static CODES: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\b(?:[A-Z][0-9]{4}|[0-9]{4}[0-9FU])\b").unwrap());
    static PLACE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b([A-Z]{2})[ ,.]+$").unwrap());
    let placed = |at: usize| {
        let before = PLACE.captures(&line[..at]);
        before.is_some_and(|c| crate::providers::state(&c[1]).is_some())
    };
    let mut codes = CODES.find_iter(line).filter(|m| !placed(m.start()));
    private::words(line).or_else(|| codes.find_map(|m| by_number(m.as_str())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const CPT: Option<&str> = Some("http://www.ama-assn.org/go/cpt");
    const HCPCS: Option<&str> = Some("https://bluebutton.cms.gov/resources/codesystem/hcpcs");

    #[test]
    fn every_number_of_the_tables_reads() {
        let written = |table: &[(&str, &str)]| -> usize {
            let all = table.iter().flat_map(|(_, t)| t.split_whitespace());
            all.count()
        };
        assert_eq!(RANGES.len(), written(&NUMBERS));
        assert_eq!(REVENUES.len(), written(&REVENUE));
        for (a, z, _) in RANGES.iter().chain(REVENUES.iter()) {
            assert_eq!(a.0, z.0);
            assert!(a.1 <= z.1);
        }
        // None is in two areas.
        for (i, (a, z, _)) in RANGES.iter().enumerate() {
            for (b, y, _) in &RANGES[i + 1..] {
                assert!(a.0 != b.0 || z.1 < b.1 || y.1 < a.1, "{a:?} {b:?}");
            }
        }
    }

    #[test]
    fn a_service_is_told_by_its_number() {
        for (system, code, area) in [
            (CPT, "90837", Some(MENTAL_HEALTH)),
            (CPT, "96127", Some(MENTAL_HEALTH)),
            (HCPCS, "G0444", Some(MENTAL_HEALTH)),
            (CPT, "3725F", Some(MENTAL_HEALTH)),
            (CPT, "99408", Some(SUBSTANCE_USE)),
            (CPT, "1036F", Some(SUBSTANCE_USE)),
            (HCPCS, "g0442", Some(SUBSTANCE_USE)),
            (HCPCS, "H0020", Some(SUBSTANCE_USE)),
            (HCPCS, "H0031", Some(MENTAL_HEALTH)),
            (CPT, "87491", Some(SEXUAL_HEALTH)),
            (CPT, "0402U", Some(SEXUAL_HEALTH)),
            (CPT, "58970", Some(REPRODUCTIVE)),
            (CPT, "81025", Some(REPRODUCTIVE)),
            (HCPCS, "J7300", Some(REPRODUCTIVE)),
            (None, "90837", Some(MENTAL_HEALTH)),
            (CPT, "99213", None),
            (CPT, "36415", None),
            (CPT, "96110", None),
            (CPT, "0001U", None),
            (HCPCS, "J0585", None),
            (HCPCS, "G04444", None),
            (Some("http://hl7.org/fhir/sid/ndc"), "90837", None),
            (Some("http://snomed.info/sct"), "90837", None),
        ] {
            assert_eq!(coded(system, Some(code), None), area, "{code}");
        }
    }

    #[test]
    fn a_service_is_told_by_its_name() {
        for (name, area) in [
            ("Chlamydia, amplified", Some(SEXUAL_HEALTH)),
            ("Psychotherapy, 45 minutes", Some(MENTAL_HEALTH)),
            ("Depression screening", Some(MENTAL_HEALTH)),
            ("Contraceptive implant", Some(REPRODUCTIVE)),
            ("Sertraline 50 mg", Some(MENTAL_HEALTH)),
            ("Tobacco counseling", Some(SUBSTANCE_USE)),
            ("Office visit", None),
            ("Hearing test", None),
        ] {
            assert_eq!(coded(None, None, Some(name)), area, "{name}");
        }
        assert_eq!(
            line(&json!({"revenue": {"code": "0900", "display": "Treatment"}})),
            Some(MENTAL_HEALTH)
        );
        assert_eq!(
            line(&json!({"revenue": {"code": "0300", "display": "Behavioral health lab"}})),
            Some(MENTAL_HEALTH)
        );
        assert_eq!(
            line(&json!({"payer_notes": ["Mental health is paid apart"]})),
            Some(MENTAL_HEALTH)
        );
        assert_eq!(line(&json!({"revenue": {"code": "0300"}})), None);
        assert_eq!(line(&json!({})), None);
    }

    /// Five digits are a code, but for a place's after its state.
    #[test]
    fn a_line_of_text_is_told_by_its_numbers_too() {
        for (line, area) in [
            ("03/05/2024 90837 Visit 120.00", Some(MENTAL_HEALTH)),
            ("G0442 Screening", Some(SUBSTANCE_USE)),
            ("Example Street 1, Example, CA 90837-0001", None),
            ("Example, CA  90837", None),
            ("Unit AB 90837", Some(MENTAL_HEALTH)),
            ("Account 190837", None),
            ("Total 908.37", None),
            ("99213 Office visit", None),
        ] {
            assert_eq!(text(line), area, "{line}");
        }
    }
}
