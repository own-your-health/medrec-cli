//! Plain names, bundled with medrec and never fetched: what a visit was from
//! its billing code, billing descriptors written in capitals put in title
//! case, and the names that belong to a records connection rather than to a
//! place a person goes.

/// A visit's plain name from its CPT evaluation-and-management code: office
/// visits, preventive checkups, newborn care and emergency room visits.
/// `None` for any other code.
pub fn visit(code: &str) -> Option<&'static str> {
    let code = code.trim();
    if code.len() != 5 {
        return None;
    }
    let n: u32 = code.parse().ok()?;
    Some(match n {
        99202..=99205 => "New-patient visit",
        99211..=99215 => "Follow-up visit",
        // Preventive medicine by age: under 1, 1 to 17, 18 and over; the
        // first of each range is a new patient, the 9939x an established one.
        99381 | 99391 => "Well-baby checkup",
        99382..=99384 | 99392..=99394 => "Well-child checkup",
        99385..=99387 | 99395..=99397 => "Preventive checkup",
        99460..=99463 => "Newborn care",
        99281..=99285 => "Emergency room visit",
        _ => return None,
    })
}

/// Whether a claim line's code bills only for giving a vaccine (CPT
/// 90460–90461, 90471–90474; HCPCS G0008–G0010), which names no service.
pub fn administration(code: &str) -> bool {
    matches!(
        code.trim(),
        "90460" | "90461" | "90471" | "90472" | "90473" | "90474" | "G0008" | "G0009" | "G0010"
    )
}

/// Whether a code's `system` is CPT, or absent (a claim line's bare code).
pub fn is_cpt(system: Option<&str>) -> bool {
    system.is_none_or(|s| s.to_ascii_lowercase().contains("cpt"))
}

/// Words that name only the kind of visit, as billing does.
const VISIT_WORDS: &[&str] = &[
    "a",
    "and",
    "appointment",
    "appt",
    "care",
    "e",
    "em",
    "enc",
    "encounter",
    "est",
    "estab",
    "established",
    "exam",
    "f",
    "follow",
    "followup",
    "for",
    "fu",
    "high",
    "level",
    "lev",
    "low",
    "lvl",
    "m",
    "mdm",
    "min",
    "mins",
    "minute",
    "minutes",
    "mod",
    "moderate",
    "new",
    "of",
    "ofc",
    "office",
    "op",
    "outpatient",
    "outpt",
    "pat",
    "patient",
    "pt",
    "return",
    "sf",
    "straightforward",
    "the",
    "to",
    "u",
    "up",
    "visit",
    "visits",
    "w",
    "with",
];

/// Whether `text` says only what kind of visit it was, in billing words
/// ("Return appointment, 20 minutes"), and so no reason a person would give.
pub fn only_visit_words(text: &str) -> bool {
    let mut words = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .peekable();
    words.peek().is_some()
        && words.all(|w| {
            w.chars().all(|c| c.is_ascii_digit())
                || VISIT_WORDS.contains(&w.to_lowercase().as_str())
        })
}

/// Words kept in capitals when a descriptor is put in title case.
const ACRONYMS: &[&str] = &[
    "BMP", "CBC", "CMP", "COVID", "CT", "ECG", "ED", "EEG", "EKG", "ENT", "ER", "GYN", "HIV",
    "HPV", "ICU", "IM", "IV", "MDM", "MMR", "MRI", "NICU", "OB", "PSA", "RSV", "TB", "TSH", "UA",
];

/// Billing abbreviations a title spells out.
const SPELLED: &[(&str, &str)] = &[
    ("INJ", "Injection"),
    ("OFC", "Office"),
    ("OUTPT", "Outpatient"),
    ("VST", "Visit"),
];

/// Words a title keeps in lower case after its first.
const SMALL: &[&str] = &[
    "a", "an", "and", "by", "for", "in", "o", "of", "on", "or", "per", "the", "to", "w", "with",
];

/// `name` in title case when it is written in capitals, its acronyms kept,
/// and at most `max` characters, cut at a word.
pub fn tidy(name: &str, max: usize) -> String {
    let name = name.trim();
    let letters = name.chars().filter(|c| c.is_alphabetic()).count();
    let shouting = letters >= 2 && !name.chars().any(char::is_lowercase);
    let s = if shouting {
        title_case(name)
    } else {
        name.to_string()
    };
    crate::summary::clip(&s, max)
}

fn title_case(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut word = String::new();
    let mut first = true;
    for c in name.chars().chain(std::iter::once(' ')) {
        if c.is_alphabetic() {
            word.push(c);
            continue;
        }
        if !word.is_empty() {
            out.push_str(&title_word(&word, first));
            first = false;
            word.clear();
        }
        out.push(c);
    }
    out.pop();
    out
}

fn title_word(word: &str, first: bool) -> String {
    let upper = word.to_uppercase();
    let lower = word.to_lowercase();
    if let Some((_, s)) = SPELLED.iter().find(|(a, _)| *a == upper) {
        return s.to_string();
    }
    if ACRONYMS.contains(&upper.as_str()) {
        return upper;
    }
    if !first && SMALL.contains(&lower.as_str()) {
        return lower;
    }
    let mut cs = lower.chars();
    cs.next()
        .map(|c| c.to_uppercase().chain(cs).collect())
        .unwrap_or_default()
}

/// Portal and records platforms: their names name the connection, never a
/// place a person goes.
const PLATFORMS: &[&str] = &[
    "allscripts",
    "athena",
    "athenahealth",
    "athenapatient",
    "cerner",
    "eclinicalworks",
    "epic",
    "followmyhealth",
    "healow",
    "healthelife",
    "icanotes",
    "modmed",
    "modernizing medicine",
    "mychart",
    "nextgen",
    "nextgen healthcare",
    "oracle health",
    "patient portal",
    "veradigm",
];

/// Whether `name` names a records connection rather than a place: a portal
/// or records platform's own name ("athenahealth"), or a name for every
/// practice a connection serves ("… (all practices)").
pub fn is_connection(name: &str) -> bool {
    let n = name.trim().to_lowercase();
    if n.ends_with("(all practices)") {
        return true;
    }
    let core = n.split('(').next().unwrap_or("").trim();
    PLATFORMS.contains(&core)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visit_codes_have_plain_names_and_other_codes_none() {
        for (code, name) in [
            ("99202", "New-patient visit"),
            ("99205", "New-patient visit"),
            ("99211", "Follow-up visit"),
            ("99215", "Follow-up visit"),
            ("99381", "Well-baby checkup"),
            ("99391", "Well-baby checkup"),
            ("99383", "Well-child checkup"),
            ("99394", "Well-child checkup"),
            ("99386", "Preventive checkup"),
            ("99397", "Preventive checkup"),
            ("99460", "Newborn care"),
            ("99463", "Newborn care"),
            ("99281", "Emergency room visit"),
            ("99285", "Emergency room visit"),
        ] {
            assert_eq!(visit(code), Some(name), "{code}");
        }
        for code in [
            "99201", "99206", "99216", "99380", "99388", "99398", "99464", "99286", "9921",
            "992130", "J1100", "",
        ] {
            assert_eq!(visit(code), None, "{code}");
        }
        assert!(is_cpt(Some("http://www.ama-assn.org/go/cpt")));
        assert!(is_cpt(None));
        assert!(!is_cpt(Some("http://snomed.info/sct")));
    }

    #[test]
    fn giving_a_vaccine_is_an_administration_code() {
        for c in ["90460", "90461", "90471", "90474", "G0008", "G0010"] {
            assert!(administration(c), "{c}");
        }
        for c in ["90462", "90470", "90475", "90686", "G0011", "99213"] {
            assert!(!administration(c), "{c}");
        }
    }

    #[test]
    fn a_reason_in_billing_words_says_only_the_kind_of_visit() {
        for t in [
            "Return appointment, 20 minutes",
            "ESTABLISHED PATIENT ENCOUNTER, LOW LEVEL",
            "Follow up",
            "E/M visit",
            "Office Visit",
        ] {
            assert!(only_visit_words(t), "{t}");
        }
        for t in [
            "Ear pain",
            "Annual physical",
            "Follow up on rash",
            "Well child 12 month",
            "",
            "  ",
        ] {
            assert!(!only_visit_words(t), "{t}");
        }
    }

    #[test]
    fn capitals_become_title_case_with_acronyms_kept_and_long_names_cut() {
        assert_eq!(
            tidy("OFC APPT FOR A NEW PATIENT", 48),
            "Office Appt for a New Patient"
        );
        assert_eq!(tidy("CT HEAD W/O CONTRAST", 48), "CT Head w/o Contrast");
        assert_eq!(tidy("MRI OF THE KNEE", 48), "MRI of the Knee");
        assert_eq!(tidy("INJ OF A JOINT", 48), "Injection of a Joint");
        // Mixed case is the writer's own, and stays.
        assert_eq!(tidy("Ear pain", 48), "Ear pain");
        assert_eq!(tidy("A1C", 48), "A1C");
        let long = tidy(
            "VITAMIN AND MINERAL CHECK WITH ADDITIONAL CALCULATIONS AND A WRITTEN REPORT",
            40,
        );
        assert!(long.chars().count() <= 40, "{long}");
        assert!(long.starts_with("Vitamin and Mineral Check"), "{long}");
    }

    #[test]
    fn a_connections_name_is_not_a_place() {
        for n in [
            "athenahealth (all practices)",
            "athenahealth",
            "MyChart",
            "Example Health (all practices)",
            " Epic ",
        ] {
            assert!(is_connection(n), "{n}");
        }
        for n in [
            "Example Pediatrics",
            "Epic Dermatology",
            "Example Health MyChart Clinic",
            "Athena Family Medicine",
        ] {
            assert!(!is_connection(n), "{n}");
        }
    }
}
