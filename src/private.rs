//! Private items (`views/<pid>/private.json`, `medrec-private/v1`): a
//! person's records, documents and events about mental health,
//! reproductive and fertility care, sexual health or substance use, and
//! the medicines tied to them. They come from codes (ICD-10), the names of
//! the places and clinicians, the portal they came from, the medicines and
//! tests themselves, and the household's own marks in `marks.json`.
//!
//! It is discretion, not access control: a reader keeps such an item off
//! shared screens and household-wide answers, and shows it on the person's
//! own pages. Anyone who can read the folder can read every item.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use pk_cli_core::CliError;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::model::{Document, Household, Person, PersonIndex, Record};

pub const SCHEMA: &str = "medrec-private/v1";

pub const MENTAL_HEALTH: &str = "mental_health";
pub const REPRODUCTIVE: &str = "reproductive";
pub const SEXUAL_HEALTH: &str = "sexual_health";
pub const SUBSTANCE_USE: &str = "substance_use";
/// An item the household marked private that no rule does.
pub const HOUSEHOLD: &str = "household";

/// What the household marked on one item, over the rules.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mark {
    pub id: String,
    /// A record (`r_…`), document (`d_…`) or event (`e_…`) of the person's.
    pub item: String,
    pub private: bool,
    pub at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<String>,
    /// An event's own members when it was marked: when the event's id
    /// changes (its anchor did), the mark follows them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub members: Vec<String>,
}

/// An event's own members, by record or document id: those without
/// `person` (another person's paper).
fn own_members(e: &Value) -> Vec<String> {
    e["members"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|m| m.get("person").is_none())
        .filter_map(|m| m["record"].as_str().or(m["document"].as_str()))
        .map(str::to_string)
        .collect()
}

/// Whether a mark on an event applies to the event `eid` with these own
/// members: it names it, or it names an event no longer there and shares
/// a member with it.
fn marks_event(m: &Mark, eid: &str, members: &[String], events: &BTreeSet<&str>) -> bool {
    m.item == eid
        || (m.item.starts_with("e_")
            && !events.contains(m.item.as_str())
            && m.members.iter().any(|x| members.contains(x)))
}

/// Why an item is private: its area, and what said so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Why {
    area: &'static str,
    from: &'static str,
}

impl Why {
    fn json(self) -> Value {
        json!({"area": self.area, "from": self.from})
    }
}

// ---------------------------------------------------------------- the rules

/// The area of an ICD-10 code.
fn icd10_area(code: &str) -> Option<&'static str> {
    let c: String = code
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect::<String>()
        .to_ascii_uppercase();
    let n: u32 = c.get(1..3)?.parse().ok()?;
    let sub = c.get(3..4);
    match c.as_bytes()[0] {
        b'F' if (10..=19).contains(&n) => Some(SUBSTANCE_USE),
        b'F' => Some(MENTAL_HEALTH),
        b'A' if (50..=64).contains(&n) => Some(SEXUAL_HEALTH),
        b'B' if n == 20 => Some(SEXUAL_HEALTH),
        b'O' if n <= 8 => Some(REPRODUCTIVE),
        b'N' if n == 46 || (96..=98).contains(&n) => Some(REPRODUCTIVE),
        b'Z' => match (n, sub) {
            (21, _) => Some(SEXUAL_HEALTH),
            (11, Some("3" | "4")) | (20, Some("2" | "6")) | (72, Some("5")) => Some(SEXUAL_HEALTH),
            (30..=32, _) | (33, Some("2")) => Some(REPRODUCTIVE),
            (71, Some("4" | "5")) => Some(SUBSTANCE_USE),
            _ => None,
        },
        _ => None,
    }
}

fn is_icd10(system: Option<&str>) -> bool {
    system.is_some_and(|s| {
        let s = s.to_ascii_lowercase();
        s.contains("icd-10") || s.contains("icd10")
    })
}

/// The words of a place's or clinician's name, credentials included, that
/// say what they treat. Substance use first: a name can say both.
static PLACES: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    [
        (
            r"addiction|substance (?:use|abuse)|detox\w*|chemical dependency|opioid treatment|methadone clinic|recovery center|lcadc|ladc|cadc|casac",
            SUBSTANCE_USE,
        ),
        (
            r"psychiatr\w*|psycholog\w*|behaviou?ral health|mental health|counsel(?:l)?ing|psychotherap\w*|pmhnp|psyd|lcsw|licsw|lmhc|lpcc?|lmft",
            MENTAL_HEALTH,
        ),
        (
            r"fertility|reproductive|ivf|family planning|andrology",
            REPRODUCTIVE,
        ),
        (r"sexual health|std clinic|sti clinic|hiv", SEXUAL_HEALTH),
    ]
    .into_iter()
    .map(|(words, area)| (Regex::new(&format!(r"(?i)\b(?:{words})\b")).unwrap(), area))
    .collect()
});

/// Medicines used for little else, by ingredient.
static MEDICINES: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    [
        (
            "buprenorphine|naltrexone|methadone|disulfiram|acamprosate|varenicline|nicotine",
            SUBSTANCE_USE,
        ),
        (
            "sertraline|fluoxetine|paroxetine|citalopram|escitalopram|fluvoxamine|venlafaxine|desvenlafaxine|duloxetine|bupropion|mirtazapine|trazodone|vortioxetine|vilazodone|nortriptyline|lithium|quetiapine|olanzapine|risperidone|aripiprazole|ziprasidone|lurasidone|clozapine|haloperidol|paliperidone|cariprazine|brexpiprazole|buspirone|alprazolam|lorazepam|methylphenidate|amphetamine|dextroamphetamine|lisdexamfetamine|atomoxetine|viloxazine|esketamine",
            MENTAL_HEALTH,
        ),
        (
            "clomiphene|follitropin|menotropins|urofollitropin|ganirelix|cetrorelix|choriogonadotropin|chorionic gonadotropin|mifepristone|ulipristal|levonorgestrel|etonogestrel|norgestimate|drospirenone|desogestrel|norethindrone",
            REPRODUCTIVE,
        ),
        (
            "emtricitabine|bictegravir|dolutegravir|cabotegravir|rilpivirine|darunavir|efavirenz|raltegravir|lenacapavir|sildenafil|tadalafil|vardenafil|avanafil",
            SEXUAL_HEALTH,
        ),
    ]
    .into_iter()
    .map(|(words, area)| (Regex::new(&format!(r"(?i)\b(?:{words})")).unwrap(), area))
    .collect()
});

/// Tests that are about the areas themselves.
static TESTS: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    [
        (
            r"chlamydia|gonorrh\w*|trichomonas|syphilis|treponema|rpr|hiv|hsv-?2|herpes simplex (?:virus )?(?:type )?2",
            SEXUAL_HEALTH,
        ),
        (
            r"pregnancy test|hcg|anti-m[uü]llerian|amh|semen|sperm",
            REPRODUCTIVE,
        ),
        (
            r"drug screen|drugs of abuse|urine drug|toxicology screen|cannabinoids?|opiates?|fentanyl|ethyl glucuronide",
            SUBSTANCE_USE,
        ),
    ]
    .into_iter()
    .map(|(words, area)| (Regex::new(&format!(r"(?i)\b(?:{words})\b")).unwrap(), area))
    .collect()
});

fn first_area(list: &[(Regex, &'static str)], text: &str) -> Option<&'static str> {
    list.iter()
        .find(|(re, _)| re.is_match(text))
        .map(|(_, a)| *a)
}

/// The portals whose records are all of one area: a behavioral health
/// record system, a fertility clinic's.
fn source_area(r: &Record) -> Option<&'static str> {
    r.provenance
        .iter()
        .find_map(|p| match p.source.cli.as_deref() {
            Some("icanotes") => Some(MENTAL_HEALTH),
            Some("fertemr") => Some(REPRODUCTIVE),
            _ => None,
        })
}

fn field<'a>(r: &'a Record, k: &str) -> Option<&'a str> {
    r.fields.get(k).and_then(Value::as_str)
}

/// A claim's principal diagnosis: the lowest sequence, else the first.
fn principal_diagnosis(r: &Record) -> Option<(Option<&str>, &str)> {
    let list = r.fields.get("diagnoses")?.as_array()?;
    let d = list.iter().min_by_key(|d| {
        d.get("sequence")
            .and_then(Value::as_u64)
            .unwrap_or(u64::MAX)
    })?;
    let code = d.get("code")?;
    Some((
        code.get("system").and_then(Value::as_str),
        code.get("code").and_then(Value::as_str)?,
    ))
}

/// Why a record is private by the rules, if it is.
fn record_why(r: &Record) -> Option<Why> {
    if let Some(area) = source_area(r) {
        return Some(Why {
            area,
            from: "source",
        });
    }
    let by_names = |keys: &[&str]| {
        keys.iter()
            .filter_map(|k| field(r, k))
            .find_map(|n| first_area(&PLACES, n))
            .map(|area| Why {
                area,
                from: "provider",
            })
    };
    match r.kind.as_str() {
        "condition" => r
            .code
            .as_ref()
            .filter(|c| is_icd10(c.system.as_deref()))
            .and_then(|c| c.code.as_deref())
            .and_then(icd10_area)
            .map(|area| Why { area, from: "code" }),
        "claim" => principal_diagnosis(r)
            .filter(|(system, _)| is_icd10(*system))
            .and_then(|(_, code)| icd10_area(code))
            .map(|area| Why { area, from: "code" })
            .or_else(|| by_names(&["provider", "facility"])),
        "encounter" => by_names(&["service_provider", "location", "practitioner"]),
        "medication" => {
            let named = std::iter::once(r.display.as_str())
                .chain(r.code.as_ref().and_then(|c| c.display.as_deref()));
            named
                .filter_map(|n| first_area(&MEDICINES, n))
                .next()
                .map(|area| Why {
                    area,
                    from: "medicine",
                })
                .or_else(|| by_names(&["prescriber"]))
        }
        "result" | "report" => first_area(&TESTS, &r.display)
            .map(|area| Why { area, from: "test" })
            .or_else(|| by_names(&["performer"])),
        _ => None,
    }
}

/// Why a document is private by the rules, if it is: its organization.
fn document_why(d: &Document) -> Option<Why> {
    d.organization
        .as_deref()
        .and_then(|o| first_area(&PLACES, o))
        .map(|area| Why {
            area,
            from: "provider",
        })
}

// ---------------------------------------------------------------- the view

/// The person's `private.json` as `medrec views write` writes it: from
/// their index, their `events.json` and their marks.
pub fn build(p: &Person, idx: &PersonIndex, events: &Value, marks: &[Mark]) -> Value {
    let mut items: BTreeMap<String, Why> = BTreeMap::new();
    for r in &idx.records {
        if let Some(w) = record_why(r) {
            items.insert(r.id.clone(), w);
        }
    }
    for d in &idx.documents {
        if let Some(w) = document_why(d) {
            items.insert(d.id.clone(), w);
        }
    }

    // The household's marks, records and documents first: an event follows
    // what its members are.
    let mark_of: BTreeMap<&str, bool> =
        marks.iter().map(|m| (m.item.as_str(), m.private)).collect();
    let household = Why {
        area: HOUSEHOLD,
        from: "household",
    };
    let apply = |items: &mut BTreeMap<String, Why>, id: &str, mark: Option<bool>| match mark {
        Some(true) => {
            let area = items.get(id).map_or(HOUSEHOLD, |w| w.area);
            items.insert(id.to_string(), Why { area, ..household });
        }
        Some(false) => {
            items.remove(id);
        }
        None => {}
    };
    let event_ids: BTreeSet<&str> = events["events"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|e| e["id"].as_str())
        .collect();
    // A mark naming an item the person no longer has is skipped.
    let held: BTreeSet<&str> = idx
        .records
        .iter()
        .map(|r| r.id.as_str())
        .chain(idx.documents.iter().map(|d| d.id.as_str()))
        .collect();
    for (id, mark) in mark_of.iter().filter(|(id, _)| held.contains(**id)) {
        apply(&mut items, id, Some(*mark));
    }

    // An event is private when a claim or portal visit in it is, or every
    // record and paper in it is; then so is all of it.
    let mut by_event: Vec<(String, Vec<String>)> = Vec::new();
    for e in events["events"].as_array().into_iter().flatten() {
        let Some(eid) = e["id"].as_str() else {
            continue;
        };
        let ids = own_members(e);
        let lead = e["members"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|m| m.get("person").is_none())
            .filter(|m| m["kind"] == "claim" || m["role"] == "portal")
            .filter_map(|m| m["record"].as_str().or(m["document"].as_str()))
            .find_map(|id| items.get(id).copied());
        let all = (!ids.is_empty() && ids.iter().all(|id| items.contains_key(id)))
            .then(|| items[&ids[0]]);
        if let Some(w) = lead.or(all) {
            items.insert(
                eid.to_string(),
                Why {
                    area: w.area,
                    from: "members",
                },
            );
        }
        // The last mark that applies: its own, else one that followed its
        // members here.
        let mark = marks
            .iter()
            .rev()
            .find(|m| m.item == eid)
            .or_else(|| {
                marks
                    .iter()
                    .rev()
                    .find(|m| marks_event(m, eid, &ids, &event_ids))
            })
            .map(|m| m.private);
        apply(&mut items, eid, mark);
        by_event.push((eid.to_string(), ids));
    }
    for (eid, ids) in &by_event {
        let Some(w) = items.get(eid).copied() else {
            continue;
        };
        for id in ids {
            if !items.contains_key(id) && mark_of.get(id.as_str()) != Some(&false) {
                items.insert(
                    id.clone(),
                    Why {
                        area: w.area,
                        from: "event",
                    },
                );
            }
        }
    }

    let mut out = json!({"schema": SCHEMA, "person": p.id,
        "records": {}, "documents": {}, "events": {}});
    for (id, w) in items {
        let kind = match id.get(..2) {
            Some("r_") => "records",
            Some("d_") => "documents",
            Some("e_") => "events",
            _ => continue,
        };
        out[kind][id] = w.json();
    }
    out
}

/// The person's `private.json` as `medrec views write` would write it now.
pub fn of_person(
    folder: &crate::folder::Folder,
    h: &Household,
    p: &Person,
) -> Result<Value, CliError> {
    let idx = folder.index(p)?;
    Ok(with_events(folder, h, p, &idx)?.0)
}

/// The person's `private.json` now, and the `events.json` it was made from.
pub fn with_events(
    folder: &crate::folder::Folder,
    h: &Household,
    p: &Person,
    idx: &PersonIndex,
) -> Result<(Value, Value), CliError> {
    let events = crate::events::of_person(folder, h, p)?;
    let marks = folder.marks_if_readable(p)?;
    let mut v = build(
        p,
        idx,
        &events,
        marks
            .as_ref()
            .map(|m| m.private.as_slice())
            .unwrap_or_default(),
    );
    if marks.is_none() {
        v["marks_unreadable"] = Value::Bool(true);
    }
    Ok((v, events))
}

/// A person's private items by id, with their area.
#[derive(Debug, Clone, Default)]
pub struct Private {
    pub items: BTreeMap<String, String>,
}

impl Private {
    /// From a `private.json`.
    pub fn of(v: &Value) -> Private {
        let mut items = BTreeMap::new();
        for kind in ["records", "documents", "events"] {
            for (id, w) in v[kind].as_object().into_iter().flatten() {
                if let Some(a) = w["area"].as_str() {
                    items.insert(id.clone(), a.to_string());
                }
            }
        }
        Private { items }
    }

    /// The area of a private item.
    pub fn area(&self, id: &str) -> Option<&str> {
        self.items.get(id).map(String::as_str)
    }
}

/// The people holding an item: a record, document or event id.
pub fn holder<'a>(
    folder: &crate::folder::Folder,
    h: &'a Household,
    item: &str,
) -> Result<Option<&'a Person>, CliError> {
    for p in &h.people {
        if item.starts_with("e_") {
            let events = crate::events::of_person(folder, h, p)?;
            if events["events"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|e| e["id"] == item)
            {
                return Ok(Some(p));
            }
        } else {
            let idx = folder.index(p)?;
            if idx.records.iter().any(|r| r.id == item)
                || idx.documents.iter().any(|d| d.id == item)
            {
                return Ok(Some(p));
            }
        }
    }
    Ok(None)
}

/// Mark an item of the person's private (`Some(true)`), not private
/// (`Some(false)`), or back to the rules (`None`). The mark, and whether
/// anything changed.
pub fn mark(
    folder: &crate::folder::Folder,
    h: &Household,
    p: &Person,
    item: &str,
    private: Option<bool>,
    by: Option<&str>,
    request: Option<&str>,
) -> Result<(Option<Mark>, bool), CliError> {
    if !["r", "d", "e"]
        .iter()
        .any(|k| crate::ids::is_valid(k, item))
    {
        return Err(CliError::Usage(format!(
            "{} is not a record, document or event id",
            crate::printable(item)
        )));
    }
    // An event's own members, and the person's events now.
    let (known, members, events) = if item.starts_with("e_") {
        let v = crate::events::of_person(folder, h, p)?;
        let list = v["events"].as_array().cloned().unwrap_or_default();
        let members = list.iter().find(|e| e["id"] == item).map(own_members);
        let ids: BTreeSet<String> = list
            .iter()
            .filter_map(|e| e["id"].as_str().map(str::to_string))
            .collect();
        (members.is_some(), members.unwrap_or_default(), ids)
    } else {
        let idx = folder.index(p)?;
        let known =
            idx.records.iter().any(|r| r.id == item) || idx.documents.iter().any(|d| d.id == item);
        (known, Vec::new(), BTreeSet::new())
    };
    if !known {
        return Err(CliError::NotFound(format!(
            "{} is not one of {}'s records, documents or events",
            crate::printable(item),
            crate::printable(&p.name)
        )));
    }
    let mut m = folder.marks(p)?;
    // The marks that apply to the item: its own, and for an event those
    // that followed its members from an id it no longer has.
    let events: BTreeSet<&str> = events.iter().map(String::as_str).collect();
    let applies = |x: &Mark| {
        if item.starts_with("e_") {
            marks_event(x, item, &members, &events)
        } else {
            x.item == item
        }
    };
    let found: Vec<usize> = (0..m.private.len())
        .filter(|i| applies(&m.private[*i]))
        .collect();
    match (private, found.as_slice()) {
        (None, []) => return Ok((None, false)),
        (Some(b), [i]) if m.private[*i].item == item && m.private[*i].private == b => {
            return Ok((Some(m.private[*i].clone()), false))
        }
        _ => {}
    }
    let changed = !found.is_empty();
    m.private = std::mem::take(&mut m.private)
        .into_iter()
        .enumerate()
        .filter(|(i, _)| !found.contains(i))
        .map(|(_, x)| x)
        .collect();
    let Some(private) = private else {
        folder.save_marks(p, &m)?;
        return Ok((None, changed));
    };
    let new = Mark {
        id: crate::ids::new("o"),
        item: item.to_string(),
        private,
        at: crate::refresh::time::fmt(crate::refresh::time::now()),
        by: by.map(str::to_string),
        request: request.map(str::to_string),
        members,
    };
    m.private.push(new.clone());
    folder.save_marks(p, &m)?;
    Ok((Some(new), true))
}

/// Words that name no item on their own: forms, units, kinds of record and
/// visit, and the words between them.
const GENERIC: &[&str] = &[
    "tablet",
    "tablets",
    "capsule",
    "capsules",
    "oral",
    "solution",
    "suspension",
    "injection",
    "cream",
    "patch",
    "extended",
    "release",
    "delayed",
    "chewable",
    "hcl",
    "mg",
    "mcg",
    "ml",
    "unit",
    "units",
    "dose",
    "visit",
    "office",
    "encounter",
    "appointment",
    "telehealth",
    "video",
    "follow",
    "hospital",
    "clinic",
    "test",
    "tests",
    "result",
    "results",
    "lab",
    "labs",
    "panel",
    "screen",
    "screening",
    "report",
    "note",
    "notes",
    "letter",
    "bill",
    "statement",
    "summary",
    "record",
    "records",
    "document",
    "total",
    "serum",
    "blood",
    "urine",
    "level",
    "disorder",
    "unspecified",
    "other",
    "episode",
    "single",
    "recurrent",
    "moderate",
    "mild",
    "severe",
    "major",
    "minor",
    "history",
    "chronic",
    "acute",
    "with",
    "without",
    "type",
    "the",
    "and",
    "for",
    "from",
];

/// Whether a question names an item: every word of it starts a word of the
/// item's title, and one of those title words is its own (not a form, unit
/// or kind of record), so "tablets" names no medicine and "sertraline" does.
pub fn names(title: &str, query: &str) -> bool {
    let words: BTreeSet<String> = crate::intake::classify::fold(query)
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect();
    let title = crate::intake::classify::fold(title);
    let in_title: BTreeSet<&str> = title
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    let starts = |w: &str| -> Vec<&str> {
        in_title
            .iter()
            .copied()
            .filter(|t| t.starts_with(w))
            .collect()
    };
    !words.is_empty()
        && words.iter().all(|w| !starts(w).is_empty())
        && words.iter().any(|w| {
            w.chars().count() >= 3
                && starts(w)
                    .iter()
                    .any(|t| !GENERIC.contains(t) && t.chars().any(char::is_alphabetic))
        })
}

/// A map of ids to areas, as `json!` writes it, for a test.
#[cfg(test)]
fn areas(v: &Value, kind: &str) -> serde_json::Map<String, Value> {
    v[kind]
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, w)| (k.clone(), w["area"].clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icd10_codes_fall_in_their_areas() {
        for (code, area) in [
            ("F41.1", Some(MENTAL_HEALTH)),
            ("F32A", Some(MENTAL_HEALTH)),
            ("F10.20", Some(SUBSTANCE_USE)),
            ("F17.210", Some(SUBSTANCE_USE)),
            ("Z31.41", Some(REPRODUCTIVE)),
            ("Z32.00", Some(REPRODUCTIVE)),
            ("Z33.2", Some(REPRODUCTIVE)),
            ("Z33.1", None),
            ("O03.9", Some(REPRODUCTIVE)),
            ("O80", None),
            ("N97.9", Some(REPRODUCTIVE)),
            ("A56.01", Some(SEXUAL_HEALTH)),
            ("B20", Some(SEXUAL_HEALTH)),
            ("Z11.3", Some(SEXUAL_HEALTH)),
            ("Z11.59", None),
            ("Z71.41", Some(SUBSTANCE_USE)),
            ("Z71.3", None),
            ("I10", None),
            ("", None),
            ("F", None),
        ] {
            assert_eq!(icd10_area(code), area, "{code}");
        }
    }

    #[test]
    fn names_medicines_and_tests_say_their_area() {
        assert_eq!(
            first_area(&PLACES, "Example Behavioral Health"),
            Some(MENTAL_HEALTH)
        );
        assert_eq!(
            first_area(&PLACES, "Kim Example, LCSW"),
            Some(MENTAL_HEALTH)
        );
        assert_eq!(
            first_area(&PLACES, "Example Addiction and Behavioral Health"),
            Some(SUBSTANCE_USE)
        );
        assert_eq!(
            first_area(&PLACES, "Example Fertility Center"),
            Some(REPRODUCTIVE)
        );
        assert_eq!(first_area(&PLACES, "Example Physical Therapy"), None);
        assert_eq!(first_area(&PLACES, "Example Hospital"), None);
        assert_eq!(
            first_area(&MEDICINES, "Sertraline 50 mg tablet"),
            Some(MENTAL_HEALTH)
        );
        assert_eq!(
            first_area(&MEDICINES, "Vitamin D3 2,000 unit capsule"),
            None
        );
        assert_eq!(
            first_area(&TESTS, "Chlamydia trachomatis DNA"),
            Some(SEXUAL_HEALTH)
        );
        assert_eq!(first_area(&TESTS, "Hemoglobin A1c"), None);
        assert!(names("Sertraline 50 mg tablet", "sertraline"));
        assert!(names("Sertraline 50 mg tablet", "sert 50"));
        assert!(!names("Sertraline 50 mg tablet", "medicines"));
        assert!(!names("Sertraline 50 mg tablet", ""));
        assert!(!names("Sertraline 50 mg tablet", "tablet"));
        assert!(!names("Sertraline 50 mg tablet", "50 mg"));
        assert!(!names("Major depressive disorder", "major disorder"));
        assert!(names("Major depressive disorder", "depressive"));
        assert!(names("Example Behavioral Health", "behavioral"));
    }

    fn person() -> Person {
        serde_json::from_value(
            json!({"id": "p_testaaaaaaaaaaaaaaaa", "name": "Sam Example",
            "folder": "Sam Example", "created_at": "2026-01-01T00:00:00Z"}),
        )
        .unwrap()
    }

    fn record(id: &str, kind: &str, display: &str, fields: Value) -> Value {
        json!({"id": id, "kind": kind, "key": id, "date": "2026-10-28", "display": display,
            "fields": fields, "provenance": [{"source": {"kind": "fhir", "cli": "mychart"},
            "method": "fhir-api", "filed_at": "2026-11-20T10:00:00Z", "filed_by": "medrec"}]})
    }

    #[test]
    fn an_event_follows_its_visit_and_the_household_has_the_last_word() {
        let idx: PersonIndex = serde_json::from_value(json!({
            "schema": "medrec-index/v1", "person": "p_testaaaaaaaaaaaaaaaa", "rev": 1,
            "documents": [], "studies": [], "imports": [],
            "records": [
                record("r_therapyclaimaaaaaaaa", "claim", "Office visit", json!({
                    "provider": "Kim Example",
                    "diagnoses": [{"sequence": 2, "code": {"system": crate::normalize::ICD10, "code": "I10"}},
                                  {"sequence": 1, "code": {"system": crate::normalize::ICD10, "code": "F41.1"}}]})),
                record("r_therapyvisitaaaaaaaa", "encounter", "Office Visit", json!({"class": "AMB"})),
                record("r_sertralineaaaaaaaaaa", "medication", "Sertraline 50 mg tablet", json!({"source": "dispense"})),
                record("r_vitaminaaaaaaaaaaaaa", "medication", "Vitamin D3 2,000 unit capsule", json!({"source": "dispense"})),
                record("r_checkupclaimaaaaaaaa", "claim", "Office visit", json!({
                    "provider": "Example Family Practice",
                    "diagnoses": [{"sequence": 1, "code": {"system": crate::normalize::ICD10, "code": "Z00.00"}},
                                  {"sequence": 2, "code": {"system": crate::normalize::ICD10, "code": "Z11.3"}}]})),
                record("r_chlamydiaaaaaaaaaaaa", "result", "Chlamydia trachomatis DNA", json!({})),
            ]
        }))
        .unwrap();
        let events = json!({"events": [
            {"id": "e_therapyaaaaaaaaaaaaa", "members": [
                {"record": "r_therapyclaimaaaaaaaa", "kind": "claim", "role": "office"},
                {"record": "r_therapyvisitaaaaaaaa", "kind": "encounter", "role": "portal"}]},
            {"id": "e_checkupaaaaaaaaaaaaa", "members": [
                {"record": "r_checkupclaimaaaaaaaa", "kind": "claim", "role": "office"},
                {"record": "r_chlamydiaaaaaaaaaaaa", "kind": "result", "role": "satellite"}]},
            {"id": "e_fillaaaaaaaaaaaaaaaa", "members": [
                {"record": "r_sertralineaaaaaaaaaa", "kind": "medication", "role": "fill"}]},
            {"id": "e_vitaminfillaaaaaaaaa", "members": [
                {"record": "r_vitaminaaaaaaaaaaaaa", "kind": "medication", "role": "fill"}]}
        ]});
        let v = build(&person(), &idx, &events, &[]);
        assert_eq!(v["schema"], SCHEMA);
        // The therapy claim by its principal diagnosis, and its visit with
        // it; the sertraline fill by its medicine; the checkup's screening
        // result but not the checkup, whose principal diagnosis is routine.
        assert_eq!(
            areas(&v, "records"),
            json!({
                "r_chlamydiaaaaaaaaaaaa": SEXUAL_HEALTH,
                "r_sertralineaaaaaaaaaa": MENTAL_HEALTH,
                "r_therapyclaimaaaaaaaa": MENTAL_HEALTH,
                "r_therapyvisitaaaaaaaa": MENTAL_HEALTH,
            })
            .as_object()
            .unwrap()
            .clone()
        );
        assert_eq!(v["records"]["r_therapyclaimaaaaaaaa"]["from"], "code");
        assert_eq!(v["records"]["r_therapyvisitaaaaaaaa"]["from"], "event");
        assert_eq!(
            areas(&v, "events"),
            json!({"e_fillaaaaaaaaaaaaaaaa": MENTAL_HEALTH, "e_therapyaaaaaaaaaaaaa": MENTAL_HEALTH})
                .as_object()
                .unwrap()
                .clone()
        );

        // The household marks the vitamin private and the therapy visit's
        // event not: the event and the visit leave, the claim stays private
        // by its own code.
        let mark = |item: &str, private: bool| Mark {
            id: "o_markaaaaaaaaaaaaaaaa".into(),
            item: item.into(),
            private,
            at: "2026-11-24T12:00:00Z".into(),
            by: None,
            request: None,
            members: Vec::new(),
        };
        let v = build(
            &person(),
            &idx,
            &events,
            &[
                mark("r_vitaminaaaaaaaaaaaaa", true),
                mark("e_therapyaaaaaaaaaaaaa", false),
                // Items the person no longer has.
                mark("r_goneaaaaaaaaaaaaaaaa", true),
                mark("e_goneaaaaaaaaaaaaaaaa", true),
            ],
        );
        assert!(v["records"].get("r_goneaaaaaaaaaaaaaaaa").is_none(), "{v}");
        assert!(v["events"].get("e_goneaaaaaaaaaaaaaaaa").is_none(), "{v}");

        assert_eq!(
            v["records"]["r_vitaminaaaaaaaaaaaaa"],
            json!({"area": HOUSEHOLD, "from": "household"})
        );
        assert_eq!(
            v["events"]["e_vitaminfillaaaaaaaaa"],
            json!({"area": HOUSEHOLD, "from": "members"})
        );
        assert!(v["events"].get("e_therapyaaaaaaaaaaaaa").is_none(), "{v}");
        assert!(v["records"].get("r_therapyvisitaaaaaaaa").is_none(), "{v}");
        assert_eq!(v["records"]["r_therapyclaimaaaaaaaa"]["from"], "code");

        // A mark on an event whose id changed (its anchor did) follows its
        // members; one whose members are all gone is skipped.
        let followed = Mark {
            members: vec!["r_vitaminaaaaaaaaaaaaa".into()],
            ..mark("e_oldvitaminfillaaaaa", false)
        };
        let w = build(
            &person(),
            &idx,
            &events,
            &[mark("r_vitaminaaaaaaaaaaaaa", true), followed],
        );
        assert!(w["events"].get("e_vitaminfillaaaaaaaaa").is_none(), "{w}");
        assert!(w["events"].get("e_oldvitaminfillaaaaa").is_none(), "{w}");
        // Its own mark wins over one that followed.
        let own = mark("e_vitaminfillaaaaaaaaa", true);
        let followed = Mark {
            members: vec!["r_vitaminaaaaaaaaaaaaa".into()],
            ..mark("e_oldvitaminfillaaaaa", false)
        };
        let w = build(&person(), &idx, &events, &[own, followed]);
        assert_eq!(
            w["events"]["e_vitaminfillaaaaaaaaa"],
            json!({"area": HOUSEHOLD, "from": "household"})
        );
    }
}
