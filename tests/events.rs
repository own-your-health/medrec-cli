//! The events view over an invented household of two (tests/fixtures/events,
//! written by its make.py): one event per real-world visit, stay or fill.

use medrec::events::{self, Decision, Input, Linked, Overrides};
use medrec::model::{Household, Person, PersonIndex, PersonText};
use serde_json::{json, Value};

/// US Eastern in November: the zone the fixtures' days are read in.
fn eastern(_: i64) -> Option<i64> {
    Some(-5 * 3600)
}

fn load<T: serde::de::DeserializeOwned>(name: &str) -> T {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/events")
        .join(name);
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

struct World {
    h: Household,
    parent: PersonIndex,
    parent_text: PersonText,
    child: PersonIndex,
    child_text: PersonText,
}

fn world() -> World {
    World {
        h: load("household.json"),
        parent: load("parent.index.json"),
        parent_text: load("parent.text.json"),
        child: load("child.index.json"),
        child_text: load("child.text.json"),
    }
}

fn events_of(w: &World, who: usize, overrides: Option<&Overrides>) -> Value {
    let (idx, text, oidx, otext) = if who == 0 {
        (&w.parent, &w.parent_text, &w.child, &w.child_text)
    } else {
        (&w.child, &w.child_text, &w.parent, &w.parent_text)
    };
    let person = &w.h.people[who];
    let other = &w.h.people[1 - who];
    assert_eq!(events::linked_people(&w.h, person).len(), 1);
    events::build(&Input {
        person,
        index: idx,
        texts: text,
        sources: &w.h.sources,
        overrides,
        linked: vec![Linked {
            person: other,
            index: oidx,
            texts: Some(otext),
        }],
        utc_offset: eastern,
    })
}

#[test]
fn dump() {
    let Ok(dir) = std::env::var("EVENTS_DUMP") else {
        return;
    };
    let w = world();
    for who in 0..2 {
        let v = events_of(&w, who, None);
        std::fs::write(
            format!("{dir}/{who}.json"),
            serde_json::to_string_pretty(&v).unwrap(),
        )
        .unwrap();
    }
}

/// The event holding a member, by its record or document id.
fn holding<'a>(v: &'a Value, id: &str) -> &'a Value {
    v["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| {
            e["members"]
                .as_array()
                .unwrap()
                .iter()
                .any(|m| m["record"] == id || m["document"] == id)
        })
        .unwrap_or_else(|| panic!("no event holds {id}"))
}

fn member<'a>(e: &'a Value, id: &str) -> &'a Value {
    e["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["record"] == id || m["document"] == id)
        .unwrap()
}

fn ids(e: &Value) -> Vec<&str> {
    e["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["record"].as_str().or(m["document"].as_str()).unwrap())
        .collect()
}

const ER: &str = "r_burnroomaaaaaaaaaaaa";

#[test]
fn every_claim_is_in_one_event_and_each_shape_is_one_event() {
    let w = world();
    let v = events_of(&w, 0, None);
    assert_eq!(v["schema"], "medrec-events/v1");
    let evs = v["events"].as_array().unwrap();

    // Every claim is in exactly one event.
    for r in w.parent.records.iter().filter(|r| r.kind == "claim") {
        let n = evs
            .iter()
            .filter(|e| ids(e).contains(&r.id.as_str()))
            .count();
        assert_eq!(n, 1, "{} is in {n} events", r.id);
    }
    // Newest first, by start, then end, then id.
    let keys: Vec<(String, String, String)> = evs
        .iter()
        .map(|e| {
            (
                e["start"].as_str().unwrap().into(),
                e["end"].as_str().unwrap().into(),
                e["id"].as_str().unwrap().into(),
            )
        })
        .collect();
    let mut sorted = keys.clone();
    sorted.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)).then(a.2.cmp(&b.2)));
    assert_eq!(keys, sorted);
    // The same folder makes the same ids.
    assert_eq!(v, events_of(&w, 0, None));

    // Two claims of one facility on one day are one visit, with its doctor's;
    // a statement joins by the amount of the claim it bills.
    let er = holding(&v, ER);
    assert_eq!(
        (er["kind"].as_str(), er["confidence"].as_str()),
        (Some("er"), Some("sure"))
    );
    assert_eq!(
        ids(er),
        [
            "r_burncareaaaaaaaaaaaa",
            ER,
            "r_burndoctoraaaaaaaaaa",
            "d_burnbillaaaaaaaaaaaa"
        ]
    );
    assert_eq!(
        er["money"],
        json!({"currency": "USD", "billed": "2630.00", "allowed": "1005.00", "discount": "1625.00",
            "plan_paid": "844.00", "you_owe": "161.00", "share": {"copay": "100.00", "coinsurance": "61.00"}})
    );
    assert_eq!(member(er, "r_burndoctoraaaaaaaaaa")["role"], "hospital");
    let bill = member(er, "d_burnbillaaaaaaaaaaaa");
    assert_eq!(
        (bill["rule"].as_str(), bill["matches"].as_str()),
        (Some("D3s"), Some(ER))
    );
    assert_eq!(
        (er["place"].as_str(), er["place_from"].as_str()),
        (Some("Sample Valley Hospital"), Some("claims"))
    );
    assert_eq!(er["status"], "complete");

    // An outpatient claim takes the surgeon's that day (likely), and a line its
    // totals leave out counts nowhere.
    let knee = holding(&v, "r_kneeroomaaaaaaaaaaaa");
    assert_eq!(knee["kind"], "outpatient");
    let surgeon = member(knee, "r_kneesurgeonaaaaaaaaa");
    assert_eq!(
        (&surgeon["rule"], &surgeon["confidence"]),
        (&json!("C3"), &json!("likely"))
    );
    assert_eq!(
        (&knee["money"]["billed"], &knee["money"]["you_owe"]),
        (&json!("8300.00"), &json!("390.00"))
    );
    assert_eq!(knee["expected"][0]["what"], "bill");

    // A stay takes the doctors' and laboratory's claims inside it, and links
    // to the child's care in it.
    let stay = holding(&v, "r_restroomaaaaaaaaaaaa");
    assert_eq!(stay["kind"], "inpatient");
    assert_eq!(member(stay, "r_restdoctoraaaaaaaaaa")["role"], "hospital");
    assert_eq!(member(stay, "r_restlabaaaaaaaaaaaaa")["role"], "ancillary");
    let c = events_of(&w, 1, None);
    let newborn = holding(&c, "r_intakeoneaaaaaaaaaaa");
    assert_eq!(
        stay["links"],
        json!([{"event": newborn["id"], "person": "p_childaaaaaaaaaaaaaaa", "relation": "newborn"}]),
        "and to none after it"
    );

    // One provider's hospital days with no facility claim are one stay.
    let watch = holding(&v, "r_watchoneaaaaaaaaaaaa");
    assert_eq!(
        (&watch["kind"], &watch["expected"][0]["what"]),
        (&json!("stay_no_facility"), &json!("facility_claim"))
    );
    assert!(ids(watch).contains(&"r_watchtwoaaaaaaaaaaaa"));

    // A portal visit: its encounter by the clinician's name (sure), another
    // provider's test (likely), its result priced by that claim's line, the
    // message after it and the portal's note.
    let visit = holding(&v, "r_hipcheckaaaaaaaaaaaa");
    assert_eq!(visit["kind"], "office");
    assert_eq!(
        (visit["what"].as_str(), visit["what_from"].as_str()),
        (Some("Hip pain"), Some("portal"))
    );
    assert_eq!(
        (visit["place"].as_str(), visit["place_from"].as_str()),
        (Some("Sample Joint Clinic"), Some("portal"))
    );
    assert_eq!(visit["times"][0], "2023-04-18T09:15:00-04:00");
    assert_eq!(
        member(visit, "r_hipcheckaaaaaaaaaaaa")["confidence"],
        "sure"
    );
    assert_eq!(
        member(visit, "r_hipimageaaaaaaaaaaaa")["confidence"],
        "likely"
    );
    assert_eq!(visit["confidence"], "likely");
    assert_eq!(member(visit, "r_hipmessageaaaaaaaaaa")["rule"], "E1");
    assert_eq!(
        member(visit, "r_hipreportaaaaaaaaaaa")["priced_by"],
        json!({"record": "r_hipimageaaaaaaaaaaaa", "sequence": 1, "allowed": "31.00"})
    );
    assert_eq!(member(visit, "d_hipnoteaaaaaaaaaaaaa")["rule"], "D4");
    assert_eq!(visit["clinicians"], json!(["Quinn Placeholder, DO"]));

    // A receipt joins its visit, and its second scan is a copy of it.
    let dental = holding(&v, "r_cleaningaaaaaaaaaaaa");
    assert_eq!(dental["kind"], "dental");
    assert_eq!(
        member(dental, "d_dentreceiptaaaaaaaaa")["copies"],
        json!(["d_dentrescanaaaaaaaaaa"])
    );

    // A claim over several weeks takes no other claims.
    let series = holding(&v, "r_woundaaaaaaaaaaaaaaa");
    assert_eq!(
        (series["kind"].as_str(), ids(series).len()),
        (Some("series"), 1)
    );
    let inside = holding(&v, "r_coughaaaaaaaaaaaaaaa");
    assert_eq!(inside["kind"], "office");
    assert!(
        inside.get("place").is_none(),
        "a clinician's name is not a place"
    );

    // A pharmacy's fills of one day are one event, with the prescription; a
    // vaccine at another is another; neither waits on a bill.
    let fill = holding(&v, "r_filloneaaaaaaaaaaaaa");
    assert_eq!(fill["kind"], "fill");
    assert!(
        ids(fill).contains(&"r_filltwoaaaaaaaaaaaaa")
            && ids(fill).contains(&"r_fillthreeaaaaaaaaaaa")
    );
    assert_eq!(member(fill, "d_fillpaperaaaaaaaaaaa")["rule"], "D5");
    assert_eq!(fill["what"], "Amlodipine 5 mg, Loratadine 10 mg and 1 more");
    assert_eq!(fill["money"]["partial"], json!(["allowed"]));
    assert!(fill["money"].get("allowed").is_none());
    assert!(fill.get("expected").is_none());
    let shot = holding(&v, "r_shotaaaaaaaaaaaaaaaa");
    assert_eq!(shot["kind"], "vaccine");
    assert!(shot.get("expected").is_none());

    // Two office visits on one day: the day's tests are not assigned.
    let tests = holding(&v, "r_daytestaaaaaaaaaaaaa");
    assert_eq!(
        (tests["kind"].as_str(), tests["status"].as_str()),
        (Some("tests"), Some("unsure"))
    );
    assert!(ids(tests).contains(&"r_dayimageaaaaaaaaaaaa"));
    let mut could: Vec<&str> = tests["could_join"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap())
        .collect();
    let mut offices = vec![
        holding(&v, "r_earvisitaaaaaaaaaaaa")["id"]
            .as_str()
            .unwrap(),
        holding(&v, "r_footvisitaaaaaaaaaaa")["id"]
            .as_str()
            .unwrap(),
    ];
    could.sort();
    offices.sort();
    assert_eq!(could, offices);

    // A bill from a practice no claim names, for the only visit that day
    // and its share: sure, and the clinician is learned to belong to it.
    let hearing = holding(&v, "r_hearingaaaaaaaaaaaaa");
    let bill = member(hearing, "d_hearingbillaaaaaaaaa");
    assert_eq!(
        (bill["rule"].as_str(), bill["confidence"].as_str()),
        (Some("D3"), Some("sure"))
    );
    assert_eq!(
        (hearing["place"].as_str(), hearing["place_from"].as_str()),
        (Some("Sample Hearing Center"), Some("alias"))
    );
    assert_eq!(v["aliases"][0]["clinician"], "N TESTPERSON");

    // A paper filed under the child, dated before their birth, is in the
    // parent's only visit that day, and in none of the child's.
    let shoulder = holding(&v, "r_shoulderaaaaaaaaaaaa");
    let paper = member(shoulder, "d_misfiledaaaaaaaaaaaa");
    assert_eq!(
        (paper["rule"].as_str(), paper["person"].as_str()),
        (Some("X2"), Some("p_childaaaaaaaaaaaaaaa"))
    );
    assert!(c["events"]
        .as_array()
        .unwrap()
        .iter()
        .all(|e| !ids(e).contains(&"d_misfiledaaaaaaaaaaaa")));

    // The child's care during the stay is a stay of its own with no facility
    // claim yet, linked to the parent's and named for its hospital.
    assert_eq!(newborn["kind"], "stay_no_facility");
    assert_eq!(
        newborn["links"],
        json!([{"event": stay["id"], "person": "p_parentaaaaaaaaaaaaaa", "relation": "born_during"}])
    );
    assert_eq!(
        (newborn["place"].as_str(), newborn["place_from"].as_str()),
        (Some("Example Harbor Hospital"), Some("linked"))
    );
    assert_eq!(newborn["expected"][0]["what"], "facility_claim");
    assert!(holding(&c, "r_laterchildaaaaaaaaaa").get("links").is_none());
}

#[test]
fn the_households_decisions_outrank_the_rules() {
    let w = world();
    let decide = |kind: &str, a: &str, b: &str, n: u8| Decision {
        id: format!("o_{}", "a".repeat(19))
            + &n.to_string()
                .replace('1', "b")
                .replace('2', "c")
                .replace('3', "d")
                .replace('4', "e"),
        kind: kind.into(),
        members: [a.into(), b.into()],
        at: "2026-11-24T12:00:00Z".into(),
        by: Some("Alex Example".into()),
        request: None,
    };
    let mut o = Overrides::new("p_parentaaaaaaaaaaaaaa");
    o.events = vec![
        // The other provider's test was not that visit's.
        decide(
            "apart",
            "r_hipdoctoraaaaaaaaaaa",
            "r_hipimageaaaaaaaaaaaa",
            1,
        ),
        // The day's tests were one each office visit's.
        decide(
            "together",
            "r_earvisitaaaaaaaaaaaa",
            "r_daytestaaaaaaaaaaaaa",
            2,
        ),
        decide(
            "together",
            "r_footvisitaaaaaaaaaaa",
            "r_dayimageaaaaaaaaaaaa",
            4,
        ),
        // The receipt was for something else.
        decide(
            "apart",
            "r_cleaningaaaaaaaaaaaa",
            "d_dentreceiptaaaaaaaaa",
            3,
        ),
    ];
    let before = events_of(&w, 0, None);
    let v = events_of(&w, 0, Some(&o));
    assert_eq!(v["overrides"], 4);

    let visit = holding(&v, "r_hipcheckaaaaaaaaaaaa");
    assert!(!ids(visit).contains(&"r_hipimageaaaaaaaaaaaa"));
    assert_eq!(visit["confidence"], "sure", "what made it likely has left");
    assert_eq!(
        visit["id"],
        holding(&before, "r_hipcheckaaaaaaaaaaaa")["id"],
        "the anchor stays, so the id does"
    );
    let lab = holding(&v, "r_hipimageaaaaaaaaaaaa");
    assert_eq!(
        (
            lab["kind"].as_str(),
            member(lab, "r_hipimageaaaaaaaaaaaa")["rule"].as_str()
        ),
        (Some("tests"), Some("household"))
    );

    let first = holding(&v, "r_earvisitaaaaaaaaaaaa");
    assert_eq!(member(first, "r_daytestaaaaaaaaaaaaa")["rule"], "household");
    assert!(v["events"]
        .as_array()
        .unwrap()
        .iter()
        .all(|e| e["status"] != "unsure"));

    let dental = holding(&v, "r_cleaningaaaaaaaaaaaa");
    assert_eq!(ids(dental), ["r_cleaningaaaaaaaaaaaa"]);
    assert!(v["events"]
        .as_array()
        .unwrap()
        .iter()
        .all(|e| !ids(e).contains(&"d_dentreceiptaaaaaaaaa")));
}

#[test]
fn a_portal_stay_spans_its_days_and_a_days_doses_and_fills_say_what_they_were() {
    let idx: PersonIndex = load("portal.index.json");
    let person: Person = serde_json::from_value(json!({
        "id": idx.person, "name": "Jamie Example", "folder": "Jamie Example",
        "created_at": "2024-11-20T00:00:00Z"}))
    .unwrap();
    let texts = PersonText::new(&person.id);
    let build = |utc_offset: fn(i64) -> Option<i64>| {
        events::build(&Input {
            person: &person,
            index: &idx,
            texts: &texts,
            sources: &[],
            overrides: None,
            linked: Vec::new(),
            utc_offset,
        })
    };
    let v = build(eastern);

    // A stay seen only in the portal, from 02:45 on its first day (07:45
    // UTC), is a stay through its last, and a result inside it joins it.
    let stay = holding(&v, "r_lungstayaaaaaaaaaaaa");
    assert_eq!(
        (&stay["kind"], &stay["title"], &stay["start"], &stay["end"]),
        (
            &json!("inpatient"),
            &json!("Hospital stay"),
            &json!("2024-11-05"),
            &json!("2024-11-08")
        ),
        "{stay}"
    );
    assert_eq!(
        (
            &member(stay, "r_oxygenaaaaaaaaaaaaaa")["role"],
            &member(stay, "r_oxygenaaaaaaaaaaaaaa")["rule"]
        ),
        (&json!("satellite"), &json!("E4"))
    );

    // In a zone where 07:45 UTC is the evening before, it starts then.
    fn hawaii(_: i64) -> Option<i64> {
        Some(-10 * 3600)
    }
    let far = build(hawaii);
    assert_eq!(
        holding(&far, "r_lungstayaaaaaaaaaaaa")["start"],
        "2024-11-04"
    );
    // An offset outside the T2 range keeps the first of its days.
    fn none(_: i64) -> Option<i64> {
        None
    }
    assert_eq!(
        holding(&build(none), "r_lungstayaaaaaaaaaaaa")["start"],
        "2024-11-04"
    );

    // One practice's doses on a day are one visit; another's are not.
    let shots = holding(&v, "r_choleraaaaaaaaaaaaaa");
    assert_eq!(
        ids(shots),
        [
            "r_choleraaaaaaaaaaaaaa",
            "r_meningitisaaaaaaaaaa",
            "r_polioaaaaaaaaaaaaaaa"
        ]
    );
    assert_eq!(
        (&shots["kind"], &shots["what"], &shots["what_from"]),
        (
            &json!("vaccine"),
            &json!("Cholera, Meningitis and 1 more"),
            &json!("portal")
        )
    );
    let flu = holding(&v, "r_fluaaaaaaaaaaaaaaaaa");
    assert_eq!(ids(flu), ["r_fluaaaaaaaaaaaaaaaaa"]);
    assert_eq!(flu["what"], "Flu");

    // A fill with no claim says which drug it was.
    for (id, what) in [
        ("r_inhaleraaaaaaaaaaaaa", "Albuterol inhaler"),
        ("r_thinneraaaaaaaaaaaaa", "Warfarin 2 mg tablet"),
    ] {
        let fill = holding(&v, id);
        assert_eq!(
            (&fill["kind"], &fill["what"], &fill["what_from"]),
            (&json!("fill"), &json!(what), &json!("portal")),
            "{fill}"
        );
    }
}

#[test]
fn visits_get_plain_names_and_a_connections_name_is_never_a_place() {
    let idx: PersonIndex = load("athena.index.json");
    let person: Person = serde_json::from_value(json!({
        "id": idx.person, "name": "Robin Example", "folder": "Robin Example",
        "created_at": "2026-11-10T00:00:00Z"}))
    .unwrap();
    let texts = PersonText::new(&person.id);
    let source = |name: &str| -> medrec::model::Source {
        serde_json::from_value(json!({
            "id": "s_athenaexampleaaaaaaa", "kind": "fhir", "person": idx.person,
            "cli": "athena", "organization": "900001", "organization_name": name,
            "fhir_base": "https://athena.example.test/fhir/r4/"}))
        .unwrap()
    };
    let build = |sources: &[medrec::model::Source]| {
        events::build(&Input {
            person: &person,
            index: &idx,
            texts: &texts,
            sources,
            overrides: None,
            linked: Vec::new(),
            utc_offset: eastern,
        })
    };
    let named = [source("Sample Ear Clinic")];
    let v = build(&named);
    let what = |v: &Value, id: &str| {
        let e = holding(v, id);
        (e["what"].clone(), e["what_from"].clone())
    };
    let place = |v: &Value, id: &str| {
        let e = holding(v, id);
        (e["place"].clone(), e["place_from"].clone())
    };

    // A reason in billing words gives way to the visit code's plain name.
    assert_eq!(
        what(&v, "r_backvisitaaaaaaaaaaa"),
        (json!("Follow-up visit"), json!("portal"))
    );
    assert_eq!(
        what(&v, "r_schoolcheckaaaaaaaaa"),
        (json!("Well-child checkup"), json!("portal"))
    );
    // A reason a person gives stays.
    assert_eq!(
        what(&v, "r_earvisitaaaaaaaaaaaa"),
        (json!("Ear pain"), json!("portal"))
    );
    // A code outside the table: its descriptor, never in capitals.
    assert_eq!(
        what(&v, "r_secondopinionaaaaaaa"),
        (json!("Consult Visit with Hearing Review"), json!("portal"))
    );

    // The encounter's location, not the connection's name it gives as its
    // service provider.
    assert_eq!(
        place(&v, "r_earvisitaaaaaaaaaaaa"),
        (json!("Sample Ear Clinic North"), json!("portal"))
    );
    // An encounter naming no place takes its source's practice.
    assert_eq!(
        place(&v, "r_backvisitaaaaaaaaaaa"),
        (json!("Sample Ear Clinic"), json!("portal"))
    );
    // A visit seen only in claims takes its line's visit code's name; a
    // reason from the portal still comes first.
    let w = world();
    let parent = events_of(&w, 0, None);
    assert_eq!(
        what(&parent, "r_coughaaaaaaaaaaaaaaa"),
        (json!("Follow-up visit"), json!("claims"))
    );
    assert_eq!(
        what(&parent, "r_hearingaaaaaaaaaaaaa"),
        (json!("New-patient visit"), json!("claims"))
    );
    assert_eq!(
        what(&parent, "r_hipdoctoraaaaaaaaaaa"),
        (json!("Hip pain"), json!("portal"))
    );

    // A doctor's claim for a vaccine and giving it is a vaccine, and the
    // giving names nothing.
    let measles = holding(&v, "r_measlesclaimaaaaaaaa");
    assert_eq!(
        (&measles["kind"], &measles["title"], &measles["what"]),
        (
            &json!("vaccine"),
            &json!("Vaccine"),
            &json!("Measles vaccine")
        ),
        "{measles}"
    );

    // A source named for every practice it serves names none of them.
    let wide = [source("athenahealth (all practices)")];
    let v = build(&wide);
    assert!(
        holding(&v, "r_backvisitaaaaaaaaaaa").get("place").is_none(),
        "{v}"
    );
    assert_eq!(
        place(&v, "r_earvisitaaaaaaaaaaaa").0,
        json!("Sample Ear Clinic North")
    );
}

/// A record whose only source says it is entered in error.
fn record_in_error(idx: &mut PersonIndex, id: &str) {
    let r = idx.records.iter_mut().find(|r| r.id == id).unwrap();
    r.status = Some("entered-in-error".into());
}

/// A paper whose only source says so.
fn paper_in_error(idx: &mut PersonIndex, id: &str) {
    let d = idx.documents.iter_mut().find(|d| d.id == id).unwrap();
    let raw = format!("Records/example-one/Binary/{id}.json");
    d.provenance.raw_path = Some(raw.clone());
    d.words = vec![
        serde_json::from_value(json!({"raw_path": raw, "status": "entered-in-error"})).unwrap(),
    ];
}

fn has(v: &Value, id: &str) -> bool {
    let evs = v["events"].as_array().unwrap();
    evs.iter().any(|e| ids(e).contains(&id))
}

#[test]
fn what_is_entered_in_error_is_in_no_event_and_comes_back() {
    let before = events_of(&world(), 0, None);
    let was = holding(&before, ER);
    assert!(before.get("in_error").is_none() && before.get("events_in_error").is_none());
    let mut w = world();
    record_in_error(&mut w.parent, "r_burndoctoraaaaaaaaaa");
    paper_in_error(&mut w.parent, "d_burnbillaaaaaaaaaaaa");
    let v = events_of(&w, 0, None);
    let now = holding(&v, ER);
    assert_eq!(now["id"], was["id"]);
    assert_eq!(ids(now), ["r_burncareaaaaaaaaaaaa", ER]);
    // 161.00 less the doctor's 36.00.
    assert_eq!(was["money"]["you_owe"], "161.00");
    assert_eq!(now["money"]["you_owe"], "125.00");
    assert_eq!(now["clinicians"].as_array(), None);
    assert_eq!(
        now["in_error"],
        json!(["d_burnbillaaaaaaaaaaaa", "r_burndoctoraaaaaaaaaa"])
    );
    for id in ["d_burnbillaaaaaaaaaaaa", "r_burndoctoraaaaaaaaaa"] {
        assert_eq!(v["in_error"][id], json!({"event": was["id"]}), "{id}");
    }
    assert!(v.get("events_in_error").is_none());
    // No other event changes.
    let rest = |v: &Value| -> Vec<Value> {
        let evs = v["events"].as_array().unwrap().iter();
        evs.filter(|e| e["id"] != was["id"]).cloned().collect()
    };
    assert_eq!(rest(&v), rest(&before));
    assert_eq!(events_of(&world(), 0, None), before);
}

#[test]
fn an_event_of_nothing_else_is_none_and_can_be_asked_for() {
    let before = events_of(&world(), 0, None);
    let (cough, hearing) = ("r_coughaaaaaaaaaaaaaaa", "r_hearingaaaaaaaaaaaaa");
    let mut w = world();
    record_in_error(&mut w.parent, cough);
    record_in_error(&mut w.parent, hearing);
    paper_in_error(&mut w.parent, "d_hearingbillaaaaaaaaa");
    let v = events_of(&w, 0, None);
    for id in [cough, hearing, "d_hearingbillaaaaaaaaa"] {
        assert!(!has(&v, id), "{id}");
        assert_eq!(v["in_error"][id]["event"], holding(&before, id)["id"]);
    }
    let n = |v: &Value, k: &str| v[k].as_array().map_or(0, Vec::len);
    assert_eq!(n(&v, "events"), n(&before, "events") - 2);
    // As they would be, newest first.
    let lost = v["events_in_error"].as_array().unwrap();
    let of: Vec<(&Value, &Value)> = lost.iter().map(|e| (&e["id"], &e["in_error"])).collect();
    assert_eq!(
        of,
        [
            (
                &holding(&before, hearing)["id"],
                &json!(["d_hearingbillaaaaaaaaa", hearing])
            ),
            (&holding(&before, cough)["id"], &json!([cough])),
        ]
    );
    assert_eq!(lost[1]["members"], holding(&before, cough)["members"]);
    // The paper that told the clinician's practice tells nothing.
    assert_eq!(before["aliases"].as_array().unwrap().len(), 1);
    assert_eq!(v["aliases"], json!([]));
}

/// What the household decided of it is kept and not applied, and applies
/// again when it comes back.
#[test]
fn a_decision_on_what_is_entered_in_error_is_kept() {
    let (ear, foot) = ("r_earvisitaaaaaaaaaaaa", "r_footvisitaaaaaaaaaaa");
    let decision = Decision {
        id: "o_aaaaaaaaaaaaaaaaaaaa".into(),
        kind: "together".into(),
        members: [ear.into(), foot.into()],
        at: "2026-05-04T12:00:00Z".into(),
        by: None,
        request: None,
    };
    let mut o = Overrides::new("p_test");
    o.events.push(decision.clone());
    let together = events_of(&world(), 0, Some(&o));
    assert!(ids(holding(&together, ear)).contains(&foot));
    assert_eq!(together["overrides"], 1);
    let mut w = world();
    record_in_error(&mut w.parent, foot);
    let v = events_of(&w, 0, Some(&o));
    assert_eq!(v["overrides"], 0);
    assert!(!has(&v, foot));
    let e = holding(&v, ear);
    assert_eq!(e["in_error"], json!([foot]));
    assert_eq!(e["not_applied"], json!([decision]));
    assert_eq!(
        v["in_error"],
        json!({foot: {"event": e["id"], "not_applied": [decision]}})
    );
    // It puts nothing back, whichever of the two is in error.
    let mut w = world();
    record_in_error(&mut w.parent, ear);
    let v = events_of(&w, 0, Some(&o));
    assert_eq!(v["overrides"], 0);
    assert!(has(&v, foot) && !has(&v, ear));
    assert_eq!(v["in_error"][ear]["not_applied"], json!([decision]));
    assert_eq!(events_of(&world(), 0, Some(&o)), together);
}

/// One its sources differ on is in its event once, with the amounts of a
/// source that states it, and says that they differ.
#[test]
fn an_item_its_sources_differ_on_stays_and_says_so() {
    let before = events_of(&world(), 0, None);
    let mut w = world();
    let doctor = "r_burndoctoraaaaaaaaaa";
    let r = w
        .parent
        .records
        .iter_mut()
        .find(|r| r.id == doctor)
        .unwrap();
    let mut other = r.provenance[0].clone();
    r.provenance[0].raw_path = Some("Records/example-one/ExplanationOfBenefit/1.json".into());
    other.raw_path = Some("Records/example-two/ExplanationOfBenefit/1.json".into());
    r.provenance.push(other);
    r.words = serde_json::from_value(json!([
        {"raw_path": "Records/example-one/ExplanationOfBenefit/1.json", "status": "active"},
        {"raw_path": "Records/example-two/ExplanationOfBenefit/1.json", "status": "entered-in-error"},
    ]))
    .unwrap();
    let mut v = events_of(&w, 0, None);
    assert_eq!(member(holding(&v, ER), doctor)["differs"], true);
    assert!(v.get("in_error").is_none());
    let evs = v["events"].as_array_mut().unwrap();
    let members = evs
        .iter_mut()
        .flat_map(|e| e["members"].as_array_mut().unwrap());
    let mut marked = 0;
    for m in members {
        marked += m.as_object_mut().unwrap().remove("differs").is_some() as usize;
    }
    assert_eq!(marked, 1);
    assert_eq!(v, before);
}

/// Another person's paper (X2) is named in its own person's file alone.
#[test]
fn a_linked_persons_paper_in_error_is_in_no_event() {
    let paper = "d_misfiledaaaaaaaaaaaa";
    let before = events_of(&world(), 0, None);
    let e = holding(&before, paper);
    assert!(member(e, paper)["person"].is_string());
    let mut w = world();
    paper_in_error(&mut w.child, paper);
    let v = events_of(&w, 0, None);
    assert!(!has(&v, paper));
    assert!(v.get("in_error").is_none());
    assert!(has(&v, "r_shoulderaaaaaaaaaaaa"));
    let own = events_of(&w, 1, None);
    assert!(own["in_error"].get(paper).is_none() || own["in_error"][paper]["event"].is_string());
}
