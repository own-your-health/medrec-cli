//! Profile names stay in local config. What medrec writes into the records
//! folder carries none, even where a provider's own message quotes one (its
//! "sign in again with `<cli> auth login --profile <NAME>`").

use super::contract::ErrorInfo;

/// What stands in for a provider profile's name.
pub const PROVIDER_PROFILE: &str = "<provider profile>";
/// What stands in for a medrec profile's name.
pub const PROFILE: &str = "<profile>";

/// The names to keep out of the folder, each with what replaces it.
#[derive(Debug, Default)]
pub struct Names(Vec<(String, &'static str)>);

impl Names {
    pub fn new<'a>(
        provider: impl IntoIterator<Item = &'a str>,
        medrec: impl IntoIterator<Item = &'a str>,
    ) -> Self {
        let mut v: Vec<(String, &'static str)> = Vec::new();
        let tagged = provider
            .into_iter()
            .map(|n| (n, PROVIDER_PROFILE))
            .chain(medrec.into_iter().map(|n| (n, PROFILE)));
        for (n, with) in tagged {
            if !n.is_empty() && !v.iter().any(|(x, _)| x == n) {
                v.push((n.to_string(), with));
            }
        }
        // Longest first, so `adult1-nyu` goes whole, not as `adult1` and a
        // suffix.
        v.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(&b.0)));
        Names(v)
    }

    /// `s` with every name that stands alone (not inside a longer word)
    /// replaced, in one pass, so a replacement is never read again.
    pub fn scrub(&self, s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut i = 0;
        'at: while i < s.len() {
            if !s[..i].chars().next_back().is_some_and(name_char) {
                for (n, with) in &self.0 {
                    if s[i..].starts_with(n.as_str())
                        && !s[i + n.len()..].chars().next().is_some_and(name_char)
                    {
                        out.push_str(with);
                        i += n.len();
                        continue 'at;
                    }
                }
            }
            let c = s[i..].chars().next().expect("i is on a char boundary");
            out.push(c);
            i += c.len_utf8();
        }
        out
    }

    pub fn error(&self, e: &mut ErrorInfo) {
        e.message = self.scrub(&e.message);
        if let Some(r) = &mut e.remedy {
            *r = self.scrub(r);
        }
    }

    /// One line of an `attempts/` file, its error scrubbed. A line that
    /// names none, or does not read, stays as it is.
    pub fn line(&self, l: &str) -> String {
        if self.scrub(l) == l {
            return l.to_string();
        }
        let Ok(mut v) = serde_json::from_str::<serde_json::Value>(l) else {
            return l.to_string();
        };
        for key in ["message", "remedy"] {
            if let Some(serde_json::Value::String(s)) = v.pointer_mut(&format!("/error/{key}")) {
                *s = self.scrub(s);
            }
        }
        serde_json::to_string(&v).unwrap_or_else(|_| l.to_string())
    }
}

/// A character a profile name may hold: a name is lowercase letters,
/// digits, `-` and `_`.
fn name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_standing_alone_is_replaced_and_one_inside_a_word_is_not() {
        let n = Names::new(["adult1", "adult1-ccf", "profile"], ["home"]);
        assert_eq!(
            n.scrub("sign in again: `uhc auth login --browser --profile adult1 --overwrite`"),
            "sign in again: `uhc auth login --browser --profile <provider profile> --overwrite`"
        );
        assert_eq!(
            n.scrub("profile adult1-ccf expired; adult1."),
            "<provider profile> <provider profile> expired; <provider profile>."
        );
        assert_eq!(
            n.scrub("adult12 xadult1 adult1_b"),
            "adult12 xadult1 adult1_b"
        );
        assert_eq!(
            n.scrub("`medrec --profile home`"),
            "`medrec --profile <profile>`"
        );
        assert_eq!(n.scrub("§ café adult1 ü"), "§ café <provider profile> ü");
    }

    #[test]
    fn an_attempt_line_is_rewritten_only_where_its_error_names_one() {
        let n = Names::new(["adult1"], []);
        let clean = r#"{"attempt":"t_a","outcome":"ok","error":null}"#;
        assert_eq!(n.line(clean), clean);
        let named = r#"{"attempt":"t_b","error":{"code":"auth","message":"profile adult1 expired","remedy":"`x auth login --profile adult1`"}}"#;
        let v: serde_json::Value = serde_json::from_str(&n.line(named)).unwrap();
        assert_eq!(v["error"]["message"], "profile <provider profile> expired");
        assert_eq!(
            v["error"]["remedy"],
            "`x auth login --profile <provider profile>`"
        );
        assert_eq!(v["attempt"], "t_b");
    }
}
