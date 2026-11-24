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

/// A request refused by rule 1.
impl Valid {
    /// When the request was made (Unix seconds).
    /// The requester's name as shown, never trusted.
    pub fn requested_by_name(&self) -> Option<String> {
        let by = self.kept.get("requested_by")?;
        by.get("name")
            .or_else(|| by.get("email"))
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    pub fn requested_at(&self) -> Option<i64> {
        self.kept
            .get("requested_at")
            .and_then(Value::as_str)
            .and_then(time::parse)
    }
}

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
    let event = match (v.get("event"), v.get("scope")) {
        (Some(_), Some(_)) => return bad("it has both a scope and an event".into()),
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
        (None, _) => None,
    };
    let scope = match v.get("scope") {
        _ if event.is_some() => event.expect("checked"),
        Some(Value::Object(s)) if s.len() == 1 && s.get("all") == Some(&Value::Bool(true)) => {
            Scope::All
        }
        Some(Value::Object(s)) if s.len() == 1 => {
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
    };
    let mut kept = Map::new();
    kept.insert("schema".into(), REQUEST_SCHEMA.into());
    kept.insert("id".into(), name_id.clone().into());
    kept.insert("requested_at".into(), requested_at.into());
    let key = if matches!(scope, Scope::Event { .. }) {
        "event"
    } else {
        "scope"
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
