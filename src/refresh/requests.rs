//! `_requests/`: what a reader asks `medrec refresh` to import. A request
//! is untrusted input (data-contract.md, `_requests/`, rule 1).

use serde_json::{json, Map, Value};

use super::contract::{Handled, REQUEST_MAX_BYTES, REQUEST_SCHEMA, SHOWN_MAX};
use super::time;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    All,
    Person(String),
    Source(String),
    /// Not an import: the household's decision on an event, "Not the same
    /// visit" (`apart`) or "Attach to this visit" (`together`).
    Event {
        person: String,
        kind: String,
        members: [String; 2],
    },
    /// Not an import: the household marking a bill paid.
    Paid {
        person: String,
        document: String,
        /// `YYYY-MM-DD`.
        on: String,
        /// A decimal with two places.
        amount: String,
        currency: String,
    },
    /// Not an import: the household taking back a payment it marked.
    Unpaid {
        person: String,
        payment: String,
    },
    /// Not an import: the household marking an item private, not private,
    /// or (`None`) back to the rules.
    Private {
        person: String,
        item: String,
        private: Option<bool>,
    },
    /// Not an import: what the household says of a provider its items print.
    Provider {
        person: String,
        ask: Box<crate::corrections::Ask>,
    },
}

impl Scope {
    /// Whether it is the household's decision, written to a person's
    /// `overrides.json` or `marks.json`, rather than an import.
    pub fn is_decision(&self) -> bool {
        matches!(
            self,
            Scope::Event { .. }
                | Scope::Paid { .. }
                | Scope::Unpaid { .. }
                | Scope::Private { .. }
                | Scope::Provider { .. }
        )
    }
}

/// A request medrec will serve.
#[derive(Debug, Clone)]
pub struct Valid {
    pub name: String,
    pub id: String,
    pub scope: Scope,
    /// The fields `done/` keeps, re-serialised.
    kept: Map<String, Value>,
}

impl Valid {
    /// The requester's name as shown, never trusted.
    pub fn requested_by_name(&self) -> Option<String> {
        let by = self.kept.get("requested_by")?;
        by.get("name")
            .or_else(|| by.get("email"))
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    /// When the request was made (Unix seconds).
    pub fn requested_at(&self) -> Option<i64> {
        self.kept
            .get("requested_at")
            .and_then(Value::as_str)
            .and_then(time::parse)
    }
}

/// A request refused by rule 1.
#[derive(Debug, Clone)]
pub struct Invalid {
    pub name: String,
    pub id: Option<String>,
    pub done_name: String,
    pub bytes: u64,
    /// `None` for a file over the size limit, which is not read.
    pub sha256: Option<String>,
    pub why: String,
}

#[derive(Debug, Clone)]
pub enum Parsed {
    Valid(Valid),
    Invalid(Invalid),
}

impl Parsed {
    pub fn name(&self) -> &str {
        match self {
            Parsed::Valid(v) => &v.name,
            Parsed::Invalid(i) => &i.name,
        }
    }
}

/// What a request may name, from household.json and the refresh settings.
pub trait Known {
    fn person(&self, id: &str) -> bool;
    fn source(&self, id: &str) -> bool;
    fn set_up(&self, source: &str) -> bool;
}

/// The id in a pending file's name, `<YYYYMMDDTHHMMSSZ>_<x id>.json`.
pub fn id_of_name(name: &str) -> Option<String> {
    let stem = name.strip_suffix(".json")?;
    let (at, id) = stem.split_once('_')?;
    let ab = at.as_bytes();
    let at_ok = ab.len() == 16
        && ab[8] == b'T'
        && ab[15] == b'Z'
        && ab[..8].iter().all(u8::is_ascii_digit)
        && ab[9..15].iter().all(u8::is_ascii_digit);
    (at_ok && crate::ids::is_valid("x", id)).then(|| id.to_string())
}

/// The `done/` name for a file whose name is not a request's.
pub fn invalid_name(name: &str) -> String {
    format!(
        "invalid_{}.json",
        &crate::hash::sha256_hex(name.as_bytes())[..16]
    )
}

/// A file too large to read.
pub fn too_large(name: &str, bytes: u64) -> Parsed {
    invalid(
        name,
        None,
        bytes,
        format!("it is {bytes} bytes; a request is at most {REQUEST_MAX_BYTES}"),
    )
}

/// The name of `done/`'s answer to the request file `name`.
pub fn done_name_of(name: &str) -> String {
    if id_of_name(name).is_some() {
        name.to_string()
    } else {
        invalid_name(name)
    }
}

fn invalid(name: &str, body: Option<&[u8]>, bytes: u64, why: String) -> Parsed {
    let id = id_of_name(name);
    Parsed::Invalid(Invalid {
        name: name.to_string(),
        done_name: done_name_of(name),
        id,
        bytes,
        sha256: body.map(crate::hash::sha256_hex),
        why,
    })
}

pub fn parse(name: &str, body: &[u8], known: &dyn Known) -> Parsed {
    let bytes = body.len() as u64;
    let bad = |why: String| invalid(name, Some(body), bytes, why);
    if bytes > REQUEST_MAX_BYTES {
        return bad(format!(
            "it is {bytes} bytes; a request is at most {REQUEST_MAX_BYTES}"
        ));
    }
    let Some(name_id) = id_of_name(name) else {
        return bad("its name is not <YYYYMMDDTHHMMSSZ>_<id>.json".into());
    };
    let Ok(Value::Object(v)) = serde_json::from_slice::<Value>(body) else {
        return bad("it is not a JSON object".into());
    };
    if v.get("schema").and_then(Value::as_str) != Some(REQUEST_SCHEMA) {
        return bad(format!("its schema is not {REQUEST_SCHEMA}"));
    }
    if v.get("id").and_then(Value::as_str) != Some(name_id.as_str()) {
        return bad("its id does not match its file name".into());
    }
    let requested_at = v.get("requested_at").and_then(Value::as_str);
    let Some(requested_at) = requested_at.filter(|t| time::parse(t).is_some()) else {
        return bad("its requested_at is not an RFC 3339 time".into());
    };
    let is_member = |s: &str| crate::ids::is_valid("r", s) || crate::ids::is_valid("d", s);
    let given = ["scope", "event", "paid", "unpaid", "private", "provider"]
        .into_iter()
        .filter(|k| v.get(*k).is_some())
        .count();
    if given > 1 {
        return bad(
            "it has more than one of a scope, an event, a paid, an unpaid, a private and a provider"
                .into(),
        );
    }
    let provider = match v.get("provider") {
        Some(Value::Object(e)) => {
            let person = e.get("person").and_then(Value::as_str).unwrap_or("");
            if !(crate::ids::is_valid("p", person) && known.person(person)) {
                return bad("its provider names no person in household.json".into());
            }
            match crate::corrections::Ask::parse(e) {
                Ok(ask) => Some(Scope::Provider {
                    person: person.into(),
                    ask: Box::new(ask),
                }),
                Err(why) => return bad(format!("its provider is refused: {why}")),
            }
        }
        Some(_) => return bad("its provider is not an object".into()),
        None => None,
    };
    let private = match v.get("private") {
        Some(Value::Object(e)) => {
            let person = e.get("person").and_then(Value::as_str).unwrap_or("");
            if !(crate::ids::is_valid("p", person) && known.person(person)) {
                return bad("its private names no person in household.json".into());
            }
            let item = e.get("item").and_then(Value::as_str).unwrap_or("");
            let private = match e.get("private") {
                Some(Value::Bool(b)) => Some(*b),
                Some(Value::Null) => None,
                _ => return bad("its private's private is not true, false or null".into()),
            };
            if !["r", "d", "e"]
                .iter()
                .any(|k| crate::ids::is_valid(k, item))
            {
                return bad("its private's item is not a record, document or event id".into());
            }
            Some(Scope::Private {
                person: person.into(),
                item: item.into(),
                private,
            })
        }
        Some(_) => return bad("its private is not an object".into()),
        None => None,
    };
    let paid = match v.get("paid") {
        Some(Value::Object(e)) => {
            let person = e.get("person").and_then(Value::as_str).unwrap_or("");
            if !(crate::ids::is_valid("p", person) && known.person(person)) {
                return bad("its paid names no person in household.json".into());
            }
            let document = e.get("document").and_then(Value::as_str).unwrap_or("");
            let on = e.get("on").and_then(Value::as_str).unwrap_or("");
            let (amount, currency) = match e.get("amount") {
                Some(Value::String(a)) => (a.as_str(), "USD"),
                Some(Value::Object(a)) => (
                    a.get("amount").and_then(Value::as_str).unwrap_or(""),
                    a.get("currency").and_then(Value::as_str).unwrap_or(""),
                ),
                _ => ("", ""),
            };
            let two_places = amount.split_once('.').is_none_or(|(_, c)| c.len() <= 2);
            let units = crate::claims::units(amount).filter(|u| *u > 0 && two_places);
            let currency_ok =
                currency.len() == 3 && currency.bytes().all(|b| b.is_ascii_uppercase());
            match units {
                Some(u)
                    if crate::ids::is_valid("d", document)
                        && crate::events::day(on).is_some()
                        && on.len() == 10
                        && currency_ok =>
                {
                    Some(Scope::Paid {
                        person: person.into(),
                        document: document.into(),
                        on: on.into(),
                        amount: crate::claims::decimal(u),
                        currency: currency.into(),
                    })
                }
                _ => return bad(
                    "its paid is not a document id, a day (YYYY-MM-DD) and an amount above zero"
                        .into(),
                ),
            }
        }
        Some(_) => return bad("its paid is not an object".into()),
        None => None,
    };
    let unpaid = match v.get("unpaid") {
        Some(Value::Object(e)) => {
            let person = e.get("person").and_then(Value::as_str).unwrap_or("");
            if !(crate::ids::is_valid("p", person) && known.person(person)) {
                return bad("its unpaid names no person in household.json".into());
            }
            let payment = e.get("payment").and_then(Value::as_str).unwrap_or("");
            if !crate::ids::is_valid("o", payment) {
                return bad("its unpaid's payment is not a payment id (o_…)".into());
            }
            Some(Scope::Unpaid {
                person: person.into(),
                payment: payment.into(),
            })
        }
        Some(_) => return bad("its unpaid is not an object".into()),
        None => None,
    };
    let decision = paid.or(unpaid).or(private).or(provider);
    let event = match (v.get("event"), decision) {
        (_, Some(decision)) => Some(decision),
        (Some(Value::Object(e)), None) => {
            let person = e.get("person").and_then(Value::as_str).unwrap_or("");
            if !(crate::ids::is_valid("p", person) && known.person(person)) {
                return bad("its event names no person in household.json".into());
            }
            let kinds: Vec<&str> = ["apart", "together"]
                .into_iter()
                .filter(|k| e.contains_key(*k))
                .collect();
            let [kind] = kinds.as_slice() else {
                return bad("its event is not one of apart or together".into());
            };
            let members: Vec<&str> = e
                .get(*kind)
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            match members.as_slice() {
                [a, b] if a != b && is_member(a) && is_member(b) => Some(Scope::Event {
                    person: person.into(),
                    kind: (*kind).into(),
                    members: [(*a).into(), (*b).into()],
                }),
                _ => return bad(format!("its {kind} is not two record or document ids")),
            }
        }
        (Some(_), None) => return bad("its event is not an object".into()),
        (None, None) => None,
    };
    let scope = match (event, v.get("scope")) {
        (Some(event), _) => event,
        (None, Some(Value::Object(s)))
            if s.len() == 1 && s.get("all") == Some(&Value::Bool(true)) =>
        {
            Scope::All
        }
        (None, Some(Value::Object(s))) if s.len() == 1 => {
            match (
                s.get("person").and_then(Value::as_str),
                s.get("source").and_then(Value::as_str),
            ) {
                (Some(p), None) if crate::ids::is_valid("p", p) && known.person(p) => {
                    Scope::Person(p.into())
                }
                (Some(_), None) => return bad("its scope names no person in household.json".into()),
                (None, Some(src)) if crate::ids::is_valid("s", src) && known.source(src) => {
                    if !known.set_up(src) {
                        return bad("its source is not set up for refresh".into());
                    }
                    Scope::Source(src.into())
                }
                (None, Some(_)) => return bad("its scope names no source in household.json".into()),
                _ => return bad("its scope is not all, a person or a source".into()),
            }
        }
        _ => return bad("its scope is not all, a person or a source".into()),
    };
    let scope_json = match &scope {
        Scope::All => json!({"all": true}),
        Scope::Person(p) => json!({ "person": p }),
        Scope::Source(s) => json!({ "source": s }),
        Scope::Event {
            person,
            kind,
            members,
        } => json!({ "person": person, kind.as_str(): members }),
        Scope::Paid {
            person,
            document,
            on,
            amount,
            currency,
        } => {
            let mut p = json!({"person": person, "document": document, "on": on, "amount": amount});
            if currency != "USD" {
                p["amount"] = json!({"amount": amount, "currency": currency});
            }
            p
        }
        Scope::Unpaid { person, payment } => json!({"person": person, "payment": payment}),
        Scope::Private {
            person,
            item,
            private,
        } => json!({"person": person, "item": item, "private": private}),
        // Everyone's outcomes show together: never the names or the text.
        Scope::Provider { person, ask } => json!({"person": person, "kind": ask.word()}),
    };
    let mut kept = Map::new();
    kept.insert("schema".into(), REQUEST_SCHEMA.into());
    kept.insert("id".into(), name_id.clone().into());
    kept.insert("requested_at".into(), requested_at.into());
    let key = match scope {
        Scope::Event { .. } => "event",
        Scope::Paid { .. } => "paid",
        Scope::Unpaid { .. } => "unpaid",
        Scope::Private { .. } => "private",
        Scope::Provider { .. } => "provider",
        _ => "scope",
    };
    kept.insert(key.into(), scope_json);
    kept.insert(
        "requested_by".into(),
        shown(v.get("requested_by"), &["email", "name"]),
    );
    kept.insert("app".into(), shown(v.get("app"), &["name", "version"]));
    Parsed::Valid(Valid {
        name: name.to_string(),
        id: name_id,
        scope,
        kept,
    })
}

/// `keys` of an object of strings, each cut to [`SHOWN_MAX`]; `null`
/// otherwise.
fn shown(v: Option<&Value>, keys: &[&str]) -> Value {
    let Some(Value::Object(o)) = v else {
        return Value::Null;
    };
    let mut out = Map::new();
    for k in keys {
        if let Some(s) = o.get(*k).and_then(Value::as_str) {
            out.insert((*k).to_string(), super::contract::clip(s, SHOWN_MAX).into());
        }
    }
    Value::Object(out)
}

/// The `done/` file for a request: written by medrec, not copied.
pub fn done(p: &Parsed, handled: &Handled) -> Value {
    let handled = serde_json::to_value(handled).expect("handled serializes");
    match p {
        Parsed::Valid(v) => {
            let mut o = v.kept.clone();
            o.insert("handled".into(), handled);
            Value::Object(o)
        }
        Parsed::Invalid(i) => json!({
            "schema": REQUEST_SCHEMA,
            "id": i.id,
            "received": {"bytes": i.bytes, "sha256": i.sha256},
            "handled": handled,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct K;
    impl Known for K {
        fn person(&self, id: &str) -> bool {
            id == "p_aaaaaaaaaaaaaaaaaaaa"
        }
        fn source(&self, id: &str) -> bool {
            id == "s_aaaaaaaaaaaaaaaaaaaa" || id == "s_bbbbbbbbbbbbbbbbbbbb"
        }
        fn set_up(&self, id: &str) -> bool {
            id == "s_aaaaaaaaaaaaaaaaaaaa"
        }
    }

    const NAME: &str = "20261122T200312Z_x_aaaaaaaaaaaaaaaaaaaa.json";

    fn body(scope: Value) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "schema": "medrec-request/v1",
            "id": "x_aaaaaaaaaaaaaaaaaaaa",
            "requested_at": "2026-11-22T20:03:12Z",
            "requested_by": {"email": "alex@example.com", "name": "Alex Testperson", "extra": "dropped"},
            "scope": scope,
            "app": {"name": "own-your-health-app", "version": "0.1.0"},
            "command": "rm -rf /",
        }))
        .unwrap()
    }

    fn why(p: Parsed) -> String {
        match p {
            Parsed::Invalid(i) => i.why,
            Parsed::Valid(v) => panic!("valid: {:?}", v.scope),
        }
    }

    #[test]
    fn a_request_names_all_a_person_or_a_set_up_source() {
        for (scope, want) in [
            (json!({"all": true}), Scope::All),
            (
                json!({"person": "p_aaaaaaaaaaaaaaaaaaaa"}),
                Scope::Person("p_aaaaaaaaaaaaaaaaaaaa".into()),
            ),
            (
                json!({"source": "s_aaaaaaaaaaaaaaaaaaaa"}),
                Scope::Source("s_aaaaaaaaaaaaaaaaaaaa".into()),
            ),
        ] {
            match parse(NAME, &body(scope), &K) {
                Parsed::Valid(v) => assert_eq!(v.scope, want),
                Parsed::Invalid(i) => panic!("{}", i.why),
            }
        }
        assert!(why(parse(
            NAME,
            &body(json!({"source": "s_bbbbbbbbbbbbbbbbbbbb"})),
            &K
        ))
        .contains("not set up"));
        assert!(why(parse(
            NAME,
            &body(json!({"source": "s_cccccccccccccccccccc"})),
            &K
        ))
        .contains("no source"));
        assert!(why(parse(
            NAME,
            &body(json!({"person": "p_cccccccccccccccccccc"})),
            &K
        ))
        .contains("no person"));
        assert!(why(parse(
            NAME,
            &body(json!({"all": true, "person": "p_aaaaaaaaaaaaaaaaaaaa"})),
            &K
        ))
        .contains("scope"));
        assert!(why(parse(NAME, &body(json!("all")), &K)).contains("scope"));
    }

    #[test]
    fn a_paid_request_names_a_person_a_bill_a_day_and_an_amount() {
        let body = |extra: Value| {
            let mut v = json!({
                "schema": REQUEST_SCHEMA,
                "id": "x_aaaaaaaaaaaaaaaaaaaa",
                "requested_at": "2026-11-22T20:03:12Z",
            });
            for (k, x) in extra.as_object().unwrap() {
                v[k] = x.clone();
            }
            serde_json::to_vec(&v).unwrap()
        };
        let pid = "p_aaaaaaaaaaaaaaaaaaaa";
        let d = "d_bbbbbbbbbbbbbbbbbbbb";
        for amount in [
            json!("120.5"),
            json!({"amount": "120.50", "currency": "USD"}),
        ] {
            let paid = json!({"paid": {"person": pid, "document": d, "on": "2026-11-20", "amount": amount}});
            match parse(NAME, &body(paid), &K) {
                Parsed::Valid(v) => {
                    assert_eq!(
                        v.scope,
                        Scope::Paid {
                            person: pid.into(),
                            document: d.into(),
                            on: "2026-11-20".into(),
                            amount: "120.50".into(),
                            currency: "USD".into(),
                        }
                    );
                    assert!(v.scope.is_decision());
                }
                Parsed::Invalid(i) => panic!("{}", i.why),
            }
        }
        for (paid, want) in [
            (
                json!({"paid": {"person": pid, "document": d, "on": "2026-11-20", "amount": "0.00"}}),
                "above zero",
            ),
            (
                json!({"paid": {"person": pid, "document": d, "on": "2026-11-20", "amount": "-5.00"}}),
                "above zero",
            ),
            (
                json!({"paid": {"person": pid, "document": d, "on": "2026-11-20", "amount": "5.001"}}),
                "above zero",
            ),
            (
                json!({"paid": {"person": pid, "document": d, "on": "20 Nov", "amount": "5.00"}}),
                "a day",
            ),
            (
                json!({"paid": {"person": pid, "document": "r_bbbbbbbbbbbbbbbbbbbb", "on": "2026-11-20", "amount": "5.00"}}),
                "a document id",
            ),
            (
                json!({"paid": {"person": pid, "document": d, "on": "2026-11-20", "amount": {"amount": "5.00", "currency": "usd"}}}),
                "above zero",
            ),
            (
                json!({"paid": {"person": "p_nosuchpersonaaaaaaaa", "document": d, "on": "2026-11-20", "amount": "5.00"}}),
                "no person",
            ),
            (json!({"paid": "yes"}), "not an object"),
            (
                json!({"paid": {"person": pid, "document": d, "on": "2026-11-20", "amount": "5.00"}, "event": {"person": pid, "apart": [d, d]}}),
                "more than one",
            ),
        ] {
            let why = why(parse(NAME, &body(paid.clone()), &K));
            assert!(why.contains(want), "{paid}: {why}");
        }
    }

    #[test]
    fn an_unpaid_request_names_a_person_and_a_payment() {
        let body = |extra: Value| {
            let mut v = json!({
                "schema": REQUEST_SCHEMA,
                "id": "x_aaaaaaaaaaaaaaaaaaaa",
                "requested_at": "2026-11-22T20:03:12Z",
            });
            for (k, x) in extra.as_object().unwrap() {
                v[k] = x.clone();
            }
            serde_json::to_vec(&v).unwrap()
        };
        let (pid, payment) = ("p_aaaaaaaaaaaaaaaaaaaa", "o_bbbbbbbbbbbbbbbbbbbb");
        let unpaid = json!({"person": pid, "payment": payment});
        match parse(NAME, &body(json!({"unpaid": unpaid})), &K) {
            Parsed::Valid(v) => {
                assert_eq!(
                    v.scope,
                    Scope::Unpaid {
                        person: pid.into(),
                        payment: payment.into(),
                    }
                );
                assert!(v.scope.is_decision());
                assert_eq!(v.kept["unpaid"], unpaid);
            }
            Parsed::Invalid(i) => panic!("{}", i.why),
        }
        for (req, want) in [
            (
                json!({"unpaid": {"person": pid, "payment": "d_bbbbbbbbbbbbbbbbbbbb"}}),
                "not a payment id",
            ),
            (json!({"unpaid": {"person": pid}}), "not a payment id"),
            (
                json!({"unpaid": {"person": "p_nosuchpersonaaaaaaaa", "payment": payment}}),
                "no person",
            ),
            (json!({"unpaid": payment}), "not an object"),
            (
                json!({"unpaid": unpaid, "private": {"person": pid, "item": "r_aaaaaaaaaaaaaaaaaaaa", "private": true}}),
                "more than one",
            ),
        ] {
            let why = why(parse(NAME, &body(req.clone()), &K));
            assert!(why.contains(want), "{req}: {why}");
        }
    }

    #[test]
    fn a_private_request_names_a_person_an_item_and_true_false_or_null() {
        let body = |extra: Value| {
            let mut v = json!({
                "schema": REQUEST_SCHEMA,
                "id": "x_aaaaaaaaaaaaaaaaaaaa",
                "requested_at": "2026-11-22T20:03:12Z",
            });
            for (k, x) in extra.as_object().unwrap() {
                v[k] = x.clone();
            }
            serde_json::to_vec(&v).unwrap()
        };
        let pid = "p_aaaaaaaaaaaaaaaaaaaa";
        for (item, private, want) in [
            ("r_aaaaaaaaaaaaaaaaaaaa", json!(true), Some(true)),
            ("d_bbbbbbbbbbbbbbbbbbbb", json!(false), Some(false)),
            ("e_cccccccccccccccccccc", json!(null), None),
        ] {
            let req = json!({"private": {"person": pid, "item": item, "private": private}});
            match parse(NAME, &body(req), &K) {
                Parsed::Valid(v) => {
                    assert_eq!(
                        v.scope,
                        Scope::Private {
                            person: pid.into(),
                            item: item.into(),
                            private: want,
                        }
                    );
                    assert!(v.scope.is_decision());
                    assert_eq!(v.kept["private"]["private"], private);
                }
                Parsed::Invalid(i) => panic!("{}", i.why),
            }
        }
        for (req, want) in [
            (
                json!({"private": {"person": pid, "item": "o_aaaaaaaaaaaaaaaaaaaa", "private": true}}),
                "not a record, document or event id",
            ),
            (
                json!({"private": {"person": pid, "item": "r_aaaaaaaaaaaaaaaaaaaa", "private": "yes"}}),
                "true, false or null",
            ),
            (
                json!({"private": {"person": pid, "item": "r_aaaaaaaaaaaaaaaaaaaa"}}),
                "true, false or null",
            ),
            (
                json!({"private": {"person": "p_nosuchpersonaaaaaaaa", "item": "r_aaaaaaaaaaaaaaaaaaaa", "private": true}}),
                "no person",
            ),
            (json!({"private": true}), "not an object"),
            (
                json!({"private": {"person": pid, "item": "r_aaaaaaaaaaaaaaaaaaaa", "private": true}, "scope": {"all": true}}),
                "more than one",
            ),
        ] {
            let why = why(parse(NAME, &body(req.clone()), &K));
            assert!(why.contains(want), "{req}: {why}");
        }
    }

    #[test]
    fn a_provider_request_says_one_thing_and_its_done_file_repeats_none_of_it() {
        let body = |provider: &Value| {
            serde_json::to_vec(&json!({
                "schema": REQUEST_SCHEMA,
                "id": "x_aaaaaaaaaaaaaaaaaaaa",
                "requested_at": "2026-11-22T20:03:12Z",
                "provider": provider,
            }))
            .unwrap()
        };
        let pid = "p_aaaaaaaaaaaaaaaaaaaa";
        let o = "o_aaaaaaaaaaaaaaaaaaaa";
        let r = "r_aaaaaaaaaaaaaaaaaaaa";
        for (mut req, word) in [
            (
                json!({"printed": ["J SAMPLE"], "set": {"role": " Radiologist "}}),
                "set",
            ),
            (
                json!({"printed": ["J SAMPLE"], "set": {"phone": "555-010-0123 x4"}, "item": r}),
                "set",
            ),
            (
                json!({"printed": ["J SAMPLE", "Jordan Sample"], "same": true}),
                "same",
            ),
            (
                json!({"printed": ["J SAMPLE", "A SAMPLE"], "same": false}),
                "different",
            ),
            (
                json!({"printed": ["J SAMPLE"], "wrong": true, "item": r}),
                "wrong",
            ),
            (json!({"undo": o}), "undo"),
        ] {
            req["person"] = pid.into();
            match parse(NAME, &body(&req), &K) {
                Parsed::Valid(v) => {
                    assert!(v.scope.is_decision(), "{req}");
                    assert_eq!(v.kept["provider"], json!({"person": pid, "kind": word}));
                    let kept = Value::Object(v.kept).to_string();
                    assert!(
                        !kept.contains("SAMPLE") && !kept.contains("adiologist"),
                        "{kept}"
                    );
                }
                Parsed::Invalid(i) => panic!("{req}: {}", i.why),
            }
        }
        let long = "x".repeat(201);
        for (mut req, want) in [
            (json!({"printed": ["J SAMPLE"]}), "not one of"),
            (
                json!({"printed": ["J SAMPLE"], "wrong": true, "same": false}),
                "not one of",
            ),
            (
                json!({"printed": ["J SAMPLE"], "wrong": false}),
                "wrong is not true",
            ),
            (
                json!({"printed": ["J SAMPLE"], "same": true}),
                "fewer than two",
            ),
            (
                json!({"printed": ["J SAMPLE", "J SAMPLE"], "same": true}),
                "twice",
            ),
            (json!({"printed": [], "wrong": true}), "no printed name"),
            (json!({"printed": [""], "wrong": true}), "empty or over"),
            (json!({"printed": [long], "wrong": true}), "empty or over"),
            (json!({"printed": "J SAMPLE", "wrong": true}), "not a list"),
            (json!({"printed": ["J SAMPLE"], "set": {}}), "gives no name"),
            (
                json!({"printed": ["J SAMPLE"], "set": {"role": "  "}}),
                "empty",
            ),
            (
                json!({"printed": ["J SAMPLE"], "set": {"role": 4}}),
                "not text",
            ),
            (
                json!({"printed": ["J SAMPLE"], "set": {"npi": "0"}}),
                "not name, practice",
            ),
            (
                json!({"printed": ["J SAMPLE"], "set": {"phone": "ask at the desk"}}),
                "not a phone",
            ),
            (
                json!({"printed": ["J SAMPLE"], "set": {"role": "a\nb"}}),
                "not plain",
            ),
            (
                json!({"printed": ["J SAMPLE"], "wrong": true, "item": o}),
                "not a record",
            ),
            (
                json!({"printed": ["J SAMPLE", "A"], "same": true, "item": r}),
                "goes with one printed name",
            ),
            (json!({"undo": "r_aaaaaaaaaaaaaaaaaaaa"}), "not an id"),
            (json!({"undo": o, "printed": ["J SAMPLE"]}), "only the id"),
            (json!({"undo": o, "why": "x"}), "does not know"),
        ] {
            req["person"] = pid.into();
            let why = why(parse(NAME, &body(&req), &K));
            assert!(why.contains(want), "{req}: {why}");
            assert!(!why.contains("SAMPLE"), "{why}");
        }
        assert!(why(parse(NAME, &body(&json!(true)), &K)).contains("not an object"));
        let nobody = json!({"person": "p_nosuchpersonaaaaaaaa", "undo": o});
        assert!(why(parse(NAME, &body(&nobody), &K)).contains("no person"));
    }

    #[test]
    fn an_event_request_names_a_person_and_two_of_their_records_or_documents() {
        let body = |extra: Value| {
            let mut v = json!({
                "schema": REQUEST_SCHEMA,
                "id": "x_aaaaaaaaaaaaaaaaaaaa",
                "requested_at": "2026-11-22T20:03:12Z",
            });
            for (k, x) in extra.as_object().unwrap() {
                v[k] = x.clone();
            }
            serde_json::to_vec(&v).unwrap()
        };
        let pid = "p_aaaaaaaaaaaaaaaaaaaa";
        let (a, b) = ("r_aaaaaaaaaaaaaaaaaaaa", "d_bbbbbbbbbbbbbbbbbbbb");
        match parse(
            NAME,
            &body(json!({"event": {"person": pid, "apart": [a, b]}})),
            &K,
        ) {
            Parsed::Valid(v) => assert_eq!(
                v.scope,
                Scope::Event {
                    person: pid.into(),
                    kind: "apart".into(),
                    members: [a.into(), b.into()],
                }
            ),
            Parsed::Invalid(i) => panic!("{}", i.why),
        }
        for (event, want) in [
            (
                json!({"event": {"person": pid, "merge": [a, b]}}),
                "apart or together",
            ),
            (
                json!({"event": {"person": pid, "apart": [a, b], "together": [a, b]}}),
                "apart or together",
            ),
            (
                json!({"event": {"person": pid, "apart": [a, a]}}),
                "two record or document ids",
            ),
            (
                json!({"event": {"person": pid, "apart": [a]}}),
                "two record or document ids",
            ),
            (
                json!({"event": {"person": pid, "apart": [a, b, a]}}),
                "two record or document ids",
            ),
            (
                json!({"event": {"person": pid, "apart": [a, "e_cccccccccccccccccccc"]}}),
                "two record or document ids",
            ),
            (
                json!({"event": {"person": "p_nosuchpersonaaaaaaaa", "apart": [a, b]}}),
                "no person",
            ),
            (json!({"event": [a, b]}), "not an object"),
            (
                json!({"event": {"person": pid, "apart": [a, b]}, "scope": {"all": true}}),
                "more than one",
            ),
        ] {
            let why = why(parse(NAME, &body(event.clone()), &K));
            assert!(why.contains(want), "{event}: {why}");
        }
    }

    #[test]
    fn a_request_is_refused_on_its_size_schema_id_or_name() {
        let big = vec![b' '; 5000];
        assert!(why(parse(NAME, &big, &K)).contains("at most 4096"));
        assert!(why(parse(NAME, b"[1]", &K)).contains("JSON object"));
        let mut v: Value = serde_json::from_slice(&body(json!({"all": true}))).unwrap();
        v["schema"] = "other/v1".into();
        assert!(why(parse(NAME, &serde_json::to_vec(&v).unwrap(), &K)).contains("schema"));
        let other = "20261122T200312Z_x_bbbbbbbbbbbbbbbbbbbb.json";
        assert!(why(parse(other, &body(json!({"all": true})), &K)).contains("does not match"));
        match parse("notes.json", b"{}", &K) {
            Parsed::Invalid(i) => {
                assert_eq!(i.id, None);
                assert!(
                    i.done_name.starts_with("invalid_")
                        && i.done_name.len() == "invalid_.json".len() + 16
                );
            }
            Parsed::Valid(_) => panic!("valid"),
        }
        assert_eq!(id_of_name(NAME).as_deref(), Some("x_aaaaaaaaaaaaaaaaaaaa"));
        for bad in [
            "2026112T200312Z_x_aaaaaaaaaaaaaaaaaaaa.json",
            "20261122T200312Z_x_aaaa.json",
            "20261122T200312Z_p_aaaaaaaaaaaaaaaaaaaa.json",
        ] {
            assert_eq!(id_of_name(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_done_file_keeps_only_the_known_fields_cut_to_size() {
        let mut v: Value = serde_json::from_slice(&body(json!({"all": true}))).unwrap();
        v["requested_by"]["name"] = "n".repeat(500).into();
        let p = parse(NAME, &serde_json::to_vec(&v).unwrap(), &K);
        let h = Handled {
            picked_up_at: "2026-11-22T20:04:01Z".into(),
            finished_at: "2026-11-22T20:04:31Z".into(),
            outcome: "ok".into(),
            sources: Vec::new(),
            message: "0 sources imported".into(),
            by: "medrec 0.3.0".into(),
        };
        let d = done(&p, &h);
        let keys: Vec<&str> = d.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            [
                "schema",
                "id",
                "requested_at",
                "scope",
                "requested_by",
                "app",
                "handled"
            ]
        );
        assert_eq!(d["requested_by"]["name"].as_str().unwrap().len(), 200);
        assert!(d["requested_by"].get("extra").is_none());
        let bad = done(&parse(NAME, b"[]", &K), &h);
        assert_eq!(bad["id"], "x_aaaaaaaaaaaaaaaaaaaa");
        assert_eq!(bad["received"]["bytes"], 2);
        assert_eq!(bad["received"]["sha256"].as_str().unwrap().len(), 64);
        assert!(bad.get("scope").is_none());
        let large = done(&too_large(NAME, 10_000_000), &h);
        assert_eq!(large["received"]["sha256"], Value::Null);
    }
}
