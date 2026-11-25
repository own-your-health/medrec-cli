//! The household's word on provider labels (`marks.json` `providers`): what
//! a printed name is called, that two printed names are one provider or are
//! not, and that a label is wrong. It wins over every match, edits no
//! record and can be undone. `crate::labels` applies it; `medrec labels` and
//! provider requests both write it here.

use std::collections::{BTreeMap, BTreeSet};

use pk_cli_core::CliError;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::folder::Folder;
use crate::marks::{READ, RECORDS, WRITE};
use crate::model::{Household, Person};
use crate::private;
use crate::refresh::time;

/// Printed names one correction may give.
const MAX_PRINTED: usize = 10;
const MAX_PRINTED_CHARS: usize = 200;
const MAX_TEXT: usize = 120;
const MAX_PHONE: usize = 40;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Correction {
    pub id: String,
    /// The names as the person's items print them, exactly.
    pub printed: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub set: Option<Set>,
    /// That the names are one provider, or (`false`) are not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub same: Option<bool>,
    /// That whatever label the names would have is wrong.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub wrong: bool,
    /// The one record, document or event it is about; every item of the
    /// person's that prints the name without it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<String>,
    /// An event's own members when it was said, as a private mark's.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub members: Vec<String>,
    /// When it was said: for a request, [`Request::at`]. The list is in this
    /// order.
    pub at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<String>,
    /// What a later medrec writes here, kept as it is.
    #[serde(flatten)]
    pub other: Map<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Set {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub practice: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phone: Option<String>,
    /// What a later medrec writes here, kept as it is.
    #[serde(flatten)]
    pub other: Map<String, Value>,
}

impl Set {
    /// Whether it gives a phone and nothing that says who.
    pub fn phone_only(&self) -> bool {
        self.phone.is_some()
            && self.name.is_none()
            && self.practice.is_none()
            && self.role.is_none()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Set,
    Same,
    Different,
    Wrong,
}

impl Kind {
    fn word(self) -> &'static str {
        match self {
            Kind::Set => "set",
            Kind::Same => "same",
            Kind::Different => "different",
            Kind::Wrong => "wrong",
        }
    }

    /// Whether it says what one label is: the latest such for a name and an
    /// item is the one that counts.
    fn says_what(self) -> bool {
        matches!(self, Kind::Set | Kind::Wrong)
    }
}

impl Correction {
    /// None for a kind only a later medrec knows.
    pub fn kind(&self) -> Option<Kind> {
        match (&self.set, self.same, self.wrong) {
            (Some(_), None, false) => Some(Kind::Set),
            (None, Some(true), false) => Some(Kind::Same),
            (None, Some(false), false) => Some(Kind::Different),
            (None, None, true) => Some(Kind::Wrong),
            _ => None,
        }
    }

    fn says_what(&self) -> bool {
        self.kind().is_some_and(Kind::says_what)
    }

    fn names(&self) -> BTreeSet<&str> {
        self.printed.iter().map(String::as_str).collect()
    }

    /// Whether it is of the same names and item, and of the same sort: the
    /// later of two replaces the earlier whole.
    fn of_the_same(&self, other: &Correction) -> bool {
        self.kind().is_some()
            && self.says_what() == other.says_what()
            && self.names() == other.names()
            && self.item == other.item
    }

    fn says_the_same(&self, other: &Correction) -> bool {
        self.of_the_same(other) && self.kind() == other.kind() && self.set == other.set
    }

    fn said_at(&self) -> i64 {
        secs(&self.at)
    }
}

/// What the household took back (`marks.json` `providers_undone`): a request
/// asked before then and served after does not say it again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Undone {
    pub printed: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<String>,
    /// A [`Kind`]'s word, or one only a later medrec knows.
    pub kind: String,
    pub at: String,
    /// What a later medrec writes here, kept as it is.
    #[serde(flatten)]
    pub other: Map<String, Value>,
}

impl Undone {
    fn kind(&self) -> Option<Kind> {
        [Kind::Set, Kind::Same, Kind::Different, Kind::Wrong]
            .into_iter()
            .find(|k| k.word() == self.kind)
    }

    /// Whether what was taken back is of the same names and item as `c`,
    /// and of the same sort.
    fn was_of_the_same(&self, c: &Correction) -> bool {
        self.kind().map(Kind::says_what) == c.kind().map(Kind::says_what)
            && c.kind().is_some()
            && self.item == c.item
            && self
                .printed
                .iter()
                .map(String::as_str)
                .collect::<BTreeSet<_>>()
                == c.names()
    }
}

fn secs(at: &str) -> i64 {
    time::parse(at).unwrap_or(0)
}

/// The request a correction came from. No part of it is trusted.
#[derive(Debug, Clone, Copy)]
pub struct Request<'a> {
    pub id: &'a str,
    pub by: Option<&'a str>,
    /// When it was asked, the same each time it is served.
    pub at: i64,
}

/// What became of what the household said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Done {
    Recorded(Box<Correction>),
    /// The last word on it already.
    Already(Box<Correction>),
    Undone,
    /// Of a request to undo what is not there: it is undone.
    NothingToUndo,
    /// Of a request: something said of the same after it was asked stands.
    SaidLater,
}

/// Why it is refused. Nothing it says repeats what was asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    NotPrinted,
    NotItsItem,
    NotTheirs,
}

impl Refusal {
    pub fn words(self) -> &'static str {
        match self {
            Refusal::NotPrinted => "a name it gives is not one the person's items print",
            Refusal::NotItsItem => {
                "the item it names is not one of the person's that prints the name"
            }
            Refusal::NotTheirs => "the id to undo is not one of the person's corrections",
        }
    }
}

#[derive(Debug)]
pub enum Failed {
    Refused(Refusal),
    /// The folder did not give or take a file. The error's message can quote
    /// the file.
    Folder {
        doing: &'static str,
        error: CliError,
    },
}

impl From<Refusal> for Failed {
    fn from(r: Refusal) -> Failed {
        Failed::Refused(r)
    }
}

impl From<Failed> for CliError {
    fn from(f: Failed) -> CliError {
        match f {
            Failed::Refused(r) => CliError::NotFound(r.words().into()),
            Failed::Folder { error, .. } => error,
        }
    }
}

/// What the household says of a provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum What {
    Set(Set),
    Same(bool),
    Wrong,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ask {
    Say {
        printed: Vec<String>,
        what: What,
        item: Option<String>,
    },
    Undo(String),
}

impl Ask {
    /// `set`, `same`, `different`, `wrong` or `undo`.
    pub fn word(&self) -> &'static str {
        match self {
            Ask::Say { what, .. } => match what {
                What::Set(_) => "set",
                What::Same(true) => "same",
                What::Same(false) => "different",
                What::Wrong => "wrong",
            },
            Ask::Undo(_) => "undo",
        }
    }

    /// From a request's `provider`. What is wrong with it never repeats what
    /// it holds.
    pub fn parse(v: &Map<String, Value>) -> Result<Ask, &'static str> {
        const KEYS: &[&str] = &["person", "printed", "set", "same", "wrong", "item", "undo"];
        if v.keys().any(|k| !KEYS.contains(&k.as_str())) {
            return Err("it has a key medrec does not know");
        }
        let given = ["set", "same", "wrong", "undo"]
            .into_iter()
            .filter(|k| v.contains_key(*k))
            .count();
        if given != 1 {
            return Err("it is not one of set, same, wrong or undo");
        }
        if let Some(u) = v.get("undo") {
            let id = u.as_str().unwrap_or("");
            if v.contains_key("printed") || v.contains_key("item") {
                return Err("an undo takes only the id to undo");
            }
            return Ask::Undo(id.to_string()).checked();
        }
        let texts = |x: &Value| -> Option<Vec<String>> {
            x.as_array()?
                .iter()
                .map(|s| s.as_str().map(str::to_string))
                .collect()
        };
        let printed = v
            .get("printed")
            .and_then(texts)
            .ok_or("its printed is not a list of names")?;
        let item = match v.get("item") {
            None => None,
            Some(Value::String(s)) => Some(s.clone()),
            Some(_) => return Err("its item is not an id"),
        };
        let what = if let Some(s) = v.get("set") {
            let s = s.as_object().ok_or("its set is not an object")?;
            let mut set = Set::default();
            for (k, x) in s {
                let x = x.as_str().ok_or("its set has a part that is not text")?;
                let part = match k.as_str() {
                    "name" => &mut set.name,
                    "practice" => &mut set.practice,
                    "role" => &mut set.role,
                    "phone" => &mut set.phone,
                    _ => {
                        return Err("its set has a part that is not name, practice, role or phone")
                    }
                };
                *part = Some(x.to_string());
            }
            What::Set(set)
        } else if let Some(s) = v.get("same") {
            What::Same(s.as_bool().ok_or("its same is not true or false")?)
        } else if v.get("wrong") == Some(&Value::Bool(true)) {
            What::Wrong
        } else {
            return Err("its wrong is not true");
        };
        Ask::Say {
            printed,
            what,
            item,
        }
        .checked()
    }

    /// Itself, with its texts trimmed, when nothing is wrong with it.
    pub fn checked(self) -> Result<Ask, &'static str> {
        let (printed, what, item) = match self {
            Ask::Undo(id) if crate::ids::is_valid("o", &id) => return Ok(Ask::Undo(id)),
            Ask::Undo(_) => return Err("its undo is not an id (o_…)"),
            Ask::Say {
                printed,
                what,
                item,
            } => (printed, what, item),
        };
        let names: BTreeSet<&String> = printed.iter().collect();
        if names.len() != printed.len() {
            return Err("it gives a printed name twice");
        }
        if printed.len() > MAX_PRINTED {
            return Err("it gives more than 10 printed names");
        }
        let least = if matches!(what, What::Same(_)) { 2 } else { 1 };
        if printed.len() < least {
            return Err(match least {
                2 => "it gives fewer than two printed names",
                _ => "it gives no printed name",
            });
        }
        if printed
            .iter()
            .any(|p| p.is_empty() || p.chars().count() > MAX_PRINTED_CHARS)
        {
            return Err("a printed name is empty or over 200 characters");
        }
        if let Some(i) = &item {
            if !["r", "d", "e"].iter().any(|k| crate::ids::is_valid(k, i)) {
                return Err("its item is not a record, document or event id");
            }
            if matches!(what, What::Same(_)) || printed.len() != 1 {
                return Err("an item goes with one printed name, set or wrong");
            }
        }
        let what = match what {
            What::Set(s) => What::Set(Set {
                name: text(s.name)?,
                practice: text(s.practice)?,
                role: text(s.role)?,
                phone: phone(s.phone)?,
                other: Map::new(),
            }),
            w => w,
        };
        if what == What::Set(Set::default()) {
            return Err("its set gives no name, practice, role or phone");
        }
        Ok(Ask::Say {
            printed,
            what,
            item,
        })
    }
}

fn text(s: Option<String>) -> Result<Option<String>, &'static str> {
    let Some(s) = s else { return Ok(None) };
    let t = s.trim();
    if t.is_empty() {
        return Err("a part of its set is empty");
    }
    if t.chars().count() > MAX_TEXT || t.chars().any(char::is_control) {
        return Err("a part of its set is over 120 characters or not plain text");
    }
    Ok(Some(t.to_string()))
}

fn phone(s: Option<String>) -> Result<Option<String>, &'static str> {
    let Some(t) = text(s)? else { return Ok(None) };
    let digits = t.chars().filter(char::is_ascii_digit).count();
    let plain = t
        .to_ascii_lowercase()
        .replace("ext", "x")
        .chars()
        .all(|c| c.is_ascii_digit() || " ()+-.x".contains(c));
    if !plain || !(7..=20).contains(&digits) || t.len() > MAX_PHONE {
        return Err("its phone is not a phone number");
    }
    Ok(Some(t))
}

/// Write what the household says of a provider to the person's marks, or
/// undo it. From a terminal (no `request`) it is the last word. A request is
/// its word as of when it was asked: it can be served late, and again, and
/// then changes nothing that was said or taken back after it.
pub fn decide(
    folder: &Folder,
    h: &Household,
    p: &Person,
    ask: &Ask,
    request: Option<&Request>,
) -> Result<Done, Failed> {
    let failed = |doing: &'static str| move |error: CliError| Failed::Folder { doing, error };

    let mut m = folder.marks(p).map_err(failed(READ))?;
    let now = time::now();
    let last = m.providers.iter().map(|c| c.said_at());
    let last = last.chain(m.providers_undone.iter().map(|u| secs(&u.at)));
    let said = match request {
        Some(r) => r.at.max(0),
        None => last.max().unwrap_or(now).max(now),
    };
    m.providers_undone
        .retain(|u| u.kind().is_none() || now - secs(&u.at) < crate::marks::UNDONE_KEEP_SECS);
    let (printed, what, item) = match ask {
        Ask::Undo(id) => {
            let Some(i) = m.providers.iter().position(|c| c.id == *id) else {
                return match request {
                    Some(_) => Ok(Done::NothingToUndo),
                    None => Err(Refusal::NotTheirs.into()),
                };
            };
            let c = m.providers.remove(i);
            m.providers_undone.retain(|u| !u.was_of_the_same(&c));
            if let Some(kind) = c.kind() {
                m.providers_undone.push(Undone {
                    at: time::fmt(said.max(c.said_at())),
                    printed: c.printed,
                    item: c.item,
                    kind: kind.word().into(),
                    other: Map::new(),
                });
            }
            folder.save_marks(p, &m).map_err(failed(WRITE))?;
            return Ok(Done::Undone);
        }
        Ask::Say {
            printed,
            what,
            item,
        } => (printed, what, item),
    };
    let idx = folder.index(p).map_err(failed(RECORDS))?;
    let events = crate::events::of_person(folder, h, p).map_err(failed(RECORDS))?;
    let prints: BTreeMap<String, BTreeSet<String>> = crate::labels::printed_names(&idx, &events);
    let known = printed
        .iter()
        .all(|n| prints.values().any(|names| names.contains(n)));
    if !known {
        return Err(Refusal::NotPrinted.into());
    }
    let mut members = Vec::new();
    if let Some(i) = item {
        if !prints
            .get(i)
            .is_some_and(|names| names.contains(&printed[0]))
        {
            return Err(Refusal::NotItsItem.into());
        }
        members = events["events"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|e| e["id"] == **i)
            .map(private::own_members)
            .unwrap_or_default();
    }
    let new = Correction {
        id: crate::ids::new("o"),
        printed: printed.clone(),
        set: match what {
            What::Set(s) => Some(s.clone()),
            _ => None,
        },
        same: match what {
            What::Same(b) => Some(*b),
            _ => None,
        },
        wrong: *what == What::Wrong,
        item: item.clone(),
        members,
        at: time::fmt(said),
        by: request.and_then(|r| r.by).map(str::to_string),
        request: request.map(|r| r.id.to_string()),
        other: Map::new(),
    };
    let later = |at: i64| request.is_some() && at > said;
    // In the order they were said: what was said later of some of the names
    // still is.
    let place = |all: &[Correction]| {
        let last = all.iter().rposition(|c| !later(c.said_at()));
        last.map_or(0, |i| i + 1)
    };
    if let Some(i) = m.providers.iter().rposition(|c| c.of_the_same(&new)) {
        let held = &m.providers[i];
        // Said since of some of the names: it is said again over that.
        let since = m.providers[i + 1..].iter().any(|c| {
            c.says_what() == new.says_what()
                && c.item == new.item
                && !c.names().is_disjoint(&new.names())
                && !later(c.said_at())
        });
        if held.says_the_same(&new) && !since {
            if said <= held.said_at() {
                return Ok(Done::Already(Box::new(held.clone())));
            }
            // Said again: what was asked between the two is older than it.
            let mut c = m.providers.remove(i);
            c.at = new.at;
            m.providers.insert(place(&m.providers), c.clone());
            folder.save_marks(p, &m).map_err(failed(WRITE))?;
            return Ok(Done::Already(Box::new(c)));
        }
        if later(held.said_at()) {
            return Ok(Done::SaidLater);
        }
    }
    let undone = |u: &Undone| u.was_of_the_same(&new) && secs(&u.at) >= said;
    if request.is_some() && m.providers_undone.iter().any(undone) {
        return Ok(Done::SaidLater);
    }
    m.providers_undone.retain(|u| !u.was_of_the_same(&new));
    m.providers.retain(|c| !c.of_the_same(&new));
    m.providers.insert(place(&m.providers), new.clone());
    folder.save_marks(p, &m).map_err(failed(WRITE))?;
    Ok(Done::Recorded(Box::new(new)))
}
