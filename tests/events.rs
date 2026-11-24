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

const ER: &str = "r_erfacaaaaaaaaaaaaaaa";

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

    // An ER visit of four billers is one event; the line the facility's
    // totals leave out counts nowhere, and its statement joins by amount.
    let er = holding(&v, ER);
    assert_eq!(
        (er["kind"].as_str(), er["confidence"].as_str()),
        (Some("er"), Some("sure"))
    );
    assert_eq!(ids(er).iter().filter(|i| i.starts_with("r_")).count(), 4);
    assert_eq!(
        er["money"],
        serde_json::json!({"currency": "USD", "billed": "10400.00", "allowed": "1445.00", "discount": "8955.00",
            "plan_paid": "1185.00", "you_owe": "260.00", "share": {"copay": "200.00", "coinsurance": "60.00"}})
    );
    assert_eq!(member(er, "r_erdocaaaaaaaaaaaaaaa")["role"], "hospital");
    assert_eq!(member(er, "r_erradaaaaaaaaaaaaaaa")["role"], "ancillary");
    let bill = member(er, "d_erbillaaaaaaaaaaaaaa");
    assert_eq!(
        (bill["rule"].as_str(), bill["matches"].as_str()),
        (Some("D3s"), Some(ER))
    );
    assert_eq!(
        (er["place"].as_str(), er["place_from"].as_str()),
        (Some("Example General Hospital"), Some("claims"))
    );
    assert_eq!(er["status"], "complete");

    // A birth stay takes the hospitalists' and the anesthesiologists'
    // claims, and links to the newborn's care.
    let stay = holding(&v, "r_birthfacaaaaaaaaaaaa");
    assert_eq!(stay["kind"], "inpatient");
    assert!(
        ids(stay).contains(&"r_birthanesaaaaaaaaaaa")
            && ids(stay).contains(&"r_birthobaaaaaaaaaaaaa")
    );
    let c = events_of(&w, 1, None);
    let newborn = holding(&c, "r_nboneaaaaaaaaaaaaaaa");
    assert_eq!(stay["links"][0]["event"], newborn["id"]);
    assert_eq!(stay["links"][0]["relation"], "newborn");

    // A portal visit: its encounter by the clinician's name (sure), the lab's
    // claim (likely: another provider), its results priced by the lab's
    // lines, the call after it and its summary.
    let visit = holding(&v, "r_checkupaaaaaaaaaaaaa");
    assert_eq!(visit["kind"], "office");
    assert_eq!(
        (visit["what"].as_str(), visit["what_from"].as_str()),
        (Some("Annual checkup"), Some("portal"))
    );
    assert_eq!(
        (visit["place"].as_str(), visit["place_from"].as_str()),
        (Some("Example Family Practice"), Some("portal"))
    );
    assert_eq!(visit["times"][0], "2026-06-10T14:30:00-04:00");
    assert_eq!(
        member(visit, "r_checkupaaaaaaaaaaaaa")["confidence"],
        "sure"
    );
    assert_eq!(
        member(visit, "r_checkuplabaaaaaaaaaa")["confidence"],
        "likely"
    );
    assert_eq!(visit["confidence"], "likely");
    assert_eq!(member(visit, "r_callbackaaaaaaaaaaaa")["rule"], "E1");
    assert_eq!(
        member(visit, "r_lipidaaaaaaaaaaaaaaa")["priced_by"],
        serde_json::json!({"record": "r_checkuplabaaaaaaaaaa", "sequence": 1, "allowed": "12.00"})
    );
    assert_eq!(member(visit, "d_checkupavsaaaaaaaaaa")["rule"], "D4");
    assert_eq!(visit["clinicians"], serde_json::json!(["Casey Sample, MD"]));

    // A desk receipt joins its visit, and its second scan is a copy of it.
    let ob = holding(&v, "r_obvisitaaaaaaaaaaaaa");
    assert_eq!(
        member(ob, "d_obreceiptaaaaaaaaaaa")["copies"],
        serde_json::json!(["d_obreceiptcopyaaaaaaa"])
    );

    // A course of therapy takes no other claims.
    let series = holding(&v, "r_therapyaaaaaaaaaaaaa");
    assert_eq!(
        (series["kind"].as_str(), ids(series).len()),
        (Some("series"), 1)
    );
    let inside = holding(&v, "r_therapyevalaaaaaaaaa");
    assert_eq!(inside["kind"], "office");
    assert!(
        inside.get("place").is_none(),
        "a clinician's name is not a place"
    );

    // Two fills of one pharmacy and day are one event, with the
    // prescription; a vaccine there is another; neither waits on a bill.
    let fill = holding(&v, "r_filloneaaaaaaaaaaaaa");
    assert_eq!(fill["kind"], "fill");
    assert!(ids(fill).contains(&"r_filltwoaaaaaaaaaaaaa"));
    assert_eq!(member(fill, "d_rxpaperaaaaaaaaaaaaa")["rule"], "D5");
    assert_eq!(fill["what"], "Amoxicillin 500 mg and Ibuprofen 600 mg");
    assert_eq!(fill["money"]["partial"], serde_json::json!(["allowed"]));
    assert!(fill["money"].get("allowed").is_none());
    assert!(fill.get("expected").is_none());
    assert_eq!(holding(&v, "r_flushotaaaaaaaaaaaaa")["kind"], "vaccine");

    // Two office visits on one day: the day's tests are not assigned.
    let tests = holding(&v, "r_twolabaaaaaaaaaaaaaa");
    assert_eq!(
        (tests["kind"].as_str(), tests["status"].as_str()),
        (Some("tests"), Some("unsure"))
    );
    let mut could: Vec<&str> = tests["could_join"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap())
        .collect();
    let mut offices = vec![
        holding(&v, "r_twoaaaaaaaaaaaaaaaaa")["id"]
            .as_str()
            .unwrap(),
        holding(&v, "r_twobaaaaaaaaaaaaaaaa")["id"]
            .as_str()
            .unwrap(),
    ];
    could.sort();
    offices.sort();
    assert_eq!(could, offices);

    // A bill from a practice no claim names, for the only visit that day
    // and its share: sure, and the clinician is learned to belong to it.
    let uro = holding(&v, "r_urologyaaaaaaaaaaaaa");
    assert_eq!(
        (
            member(uro, "d_urologybillaaaaaaaaa")["rule"].as_str(),
            member(uro, "d_urologybillaaaaaaaaa")["confidence"].as_str()
        ),
        (Some("D3"), Some("sure"))
    );
    assert_eq!(
        (uro["place"].as_str(), uro["place_from"].as_str()),
        (Some("Example Urology Associates"), Some("alias"))
    );
    assert_eq!(v["aliases"][0]["clinician"], "K EXAMPLE");
    // A paper filed under the child before the birth is this visit's.
    let rx = member(uro, "d_urologyrxaaaaaaaaaaa");
    assert_eq!(
        (rx["rule"].as_str(), rx["person"].as_str()),
        (Some("X2"), Some("p_childaaaaaaaaaaaaaaa"))
    );

    // The newborn's care during the stay is a stay of its own with no
    // facility claim yet, linked to the parent's and named for its hospital.
    assert_eq!(newborn["kind"], "stay_no_facility");
    assert_eq!(
        newborn["links"][0],
        serde_json::json!({"event": stay["id"], "person": "p_parentaaaaaaaaaaaaaa", "relation": "born_during"})
    );
    assert_eq!(
        (newborn["place"].as_str(), newborn["place_from"].as_str()),
        (Some("Example General Hospital"), Some("linked"))
    );
    assert_eq!(newborn["expected"][0]["what"], "facility_claim");
    assert!(c["events"]
        .as_array()
        .unwrap()
        .iter()
        .all(|e| !ids(e).contains(&"d_urologyrxaaaaaaaaaaa")));
}

#[test]
fn the_households_decisions_outrank_the_rules() {
    let w = world();
    let decide = |kind: &str, a: &str, b: &str, n: u8| Decision {
        id: format!("o_{}", "a".repeat(19))
            + &n.to_string()
                .replace('1', "b")
                .replace('2', "c")
                .replace('3', "d"),
        kind: kind.into(),
        members: [a.into(), b.into()],
        at: "2026-11-24T12:00:00Z".into(),
        by: Some("Alex Example".into()),
        request: None,
    };
    let mut o = Overrides::new("p_parentaaaaaaaaaaaaaa");
    o.events = vec![
        // The lab's claim was not that visit's.
        decide(
            "apart",
            "r_checkupdocaaaaaaaaaa",
            "r_checkuplabaaaaaaaaaa",
            1,
        ),
        // The day's tests were the first office visit's.
        decide(
            "together",
            "r_twoaaaaaaaaaaaaaaaaa",
            "r_twolabaaaaaaaaaaaaaa",
            2,
        ),
        // The receipt was for something else.
        decide(
            "apart",
            "r_obvisitaaaaaaaaaaaaa",
            "d_obreceiptaaaaaaaaaaa",
            3,
        ),
    ];
    let before = events_of(&w, 0, None);
    let v = events_of(&w, 0, Some(&o));
    assert_eq!(v["overrides"], 3);

    let visit = holding(&v, "r_checkupaaaaaaaaaaaaa");
    assert!(!ids(visit).contains(&"r_checkuplabaaaaaaaaaa"));
    assert_eq!(visit["confidence"], "sure", "what made it likely has left");
    assert_eq!(
        visit["id"],
        holding(&before, "r_checkupaaaaaaaaaaaaa")["id"],
        "the anchor stays, so the id does"
    );
    let lab = holding(&v, "r_checkuplabaaaaaaaaaa");
    assert_eq!(
        (
            lab["kind"].as_str(),
            member(lab, "r_checkuplabaaaaaaaaaa")["rule"].as_str()
        ),
        (Some("tests"), Some("household"))
    );

    let first = holding(&v, "r_twoaaaaaaaaaaaaaaaaa");
    assert_eq!(member(first, "r_twolabaaaaaaaaaaaaaa")["rule"], "household");
    assert!(v["events"]
        .as_array()
        .unwrap()
        .iter()
        .all(|e| e["status"] != "unsure"));

    let ob = holding(&v, "r_obvisitaaaaaaaaaaaaa");
    assert_eq!(ids(ob), ["r_obvisitaaaaaaaaaaaaa"]);
    assert!(v["events"]
        .as_array()
        .unwrap()
        .iter()
        .all(|e| !ids(e).contains(&"d_obreceiptaaaaaaaaaaa")));
}

#[test]
fn a_portal_stay_spans_its_days_and_a_days_doses_and_fills_say_what_they_were() {
    let idx: PersonIndex = load("portal.index.json");
    let person: Person = serde_json::from_value(json!({
        "id": idx.person, "name": "Jamie Example", "folder": "Jamie Example",
        "created_at": "2026-11-10T00:00:00Z"}))
    .unwrap();
    let texts = PersonText::new(&person.id);
    let build = |utc_offset: fn(i64) -> Option<i64>| {
        events::build(&Input {
            person: &person,
            index: &idx,
            texts: &texts,
            overrides: None,
            linked: Vec::new(),
            utc_offset,
        })
    };
    let v = build(eastern);

    // A stay seen only in the portal, from 01:10 on its first day (06:10
    // UTC), is a stay through its last, and a result inside it joins it.
    let stay = holding(&v, "r_birthstayaaaaaaaaaaa");
    assert_eq!(
        (&stay["kind"], &stay["title"], &stay["start"], &stay["end"]),
        (
            &json!("inpatient"),
            &json!("Hospital stay"),
            &json!("2026-11-08"),
            &json!("2026-11-10")
        ),
        "{stay}"
    );
    assert_eq!(
        (
            &member(stay, "r_bilirubinaaaaaaaaaaa")["role"],
            &member(stay, "r_bilirubinaaaaaaaaaaa")["rule"]
        ),
        (&json!("satellite"), &json!("E4"))
    );

    // In a zone where 06:10 UTC is the evening before, it starts then.
    fn hawaii(_: i64) -> Option<i64> {
        Some(-10 * 3600)
    }
    let far = build(hawaii);
    assert_eq!(
        holding(&far, "r_birthstayaaaaaaaaaaa")["start"],
        "2026-11-07"
    );
    // An offset outside the T2 range keeps the first of its days.
    fn none(_: i64) -> Option<i64> {
        None
    }
    assert_eq!(
        holding(&build(none), "r_birthstayaaaaaaaaaaa")["start"],
        "2026-11-07"
    );

    // One practice's doses on a day are one visit; another's are not.
    let shots = holding(&v, "r_dtapaaaaaaaaaaaaaaaa");
    assert_eq!(
        ids(shots),
        [
            "r_dtapaaaaaaaaaaaaaaaa",
            "r_hibaaaaaaaaaaaaaaaaa",
            "r_polioaaaaaaaaaaaaaaa",
            "r_pcvaaaaaaaaaaaaaaaaa"
        ]
    );
    assert_eq!(
        (&shots["kind"], &shots["what"], &shots["what_from"]),
        (
            &json!("vaccine"),
            &json!("DTaP, Hib and 2 more"),
            &json!("portal")
        )
    );
    let flu = holding(&v, "r_flushotaaaaaaaaaaaaa");
    assert_eq!(ids(flu), ["r_flushotaaaaaaaaaaaaa"]);
    assert_eq!(flu["what"], "Influenza");

    // A fill with no claim says which drug it was.
    for (id, what) in [
        (
            "r_amoxfillaaaaaaaaaaaa",
            "Amoxicillin 250 mg/5 mL oral suspension",
        ),
        (
            "r_ibufillaaaaaaaaaaaaa",
            "Ibuprofen 100 mg/5 mL oral suspension",
        ),
    ] {
        let fill = holding(&v, id);
        assert_eq!(
            (&fill["kind"], &fill["what"], &fill["what_from"]),
            (&json!("fill"), &json!(what), &json!("portal")),
            "{fill}"
        );
    }
}
