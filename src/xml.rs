//! A small reader of XML elements, for files a publisher gives as elements
//! and text alone (CDC's CDSi supporting data). Attributes are passed over,
//! and a declaration, a comment or a processing instruction is skipped. A
//! file that is not well formed reads as none.

/// One element: its name, its text with the entities read and the ends
/// trimmed, and the elements in it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Element {
    pub name: String,
    pub text: String,
    pub children: Vec<Element>,
}

/// How deep elements may nest.
const DEPTH: usize = 64;

impl Element {
    /// The first element in it named `name`.
    pub fn child(&self, name: &str) -> Option<&Element> {
        self.children.iter().find(|c| c.name == name)
    }

    /// Every element in it named `name`.
    pub fn all<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Element> + 'a {
        self.children.iter().filter(move |c| c.name == name)
    }

    /// The text of the element at `path` (names joined by `/`), when it has
    /// any.
    pub fn text_at(&self, path: &str) -> Option<&str> {
        let mut at = self;
        for name in path.split('/') {
            at = at.child(name)?;
        }
        Some(at.text.as_str()).filter(|t| !t.is_empty())
    }
}

/// The root element of `file`, when it is well formed.
pub fn read(file: &str) -> Option<Element> {
    let mut r = Reader { s: file, at: 0 };
    r.misc()?;
    let root = r.element(0)?;
    r.misc()?;
    (r.at == r.s.len()).then_some(root)
}

struct Reader<'a> {
    s: &'a str,
    at: usize,
}

impl<'a> Reader<'a> {
    fn rest(&self) -> &'a str {
        &self.s[self.at..]
    }

    /// Past what may stand outside the root, or between elements: white
    /// space, declarations, comments and processing instructions.
    fn misc(&mut self) -> Option<()> {
        loop {
            let trimmed = self.rest().trim_start();
            self.at = self.s.len() - trimmed.len();
            if trimmed.starts_with("<?") {
                self.past("?>")?;
            } else if trimmed.starts_with("<!--") {
                self.past("-->")?;
            } else if trimmed.starts_with("<!") {
                self.past(">")?;
            } else {
                return Some(());
            }
        }
    }

    fn past(&mut self, end: &str) -> Option<()> {
        let i = self.rest().find(end)?;
        self.at += i + end.len();
        Some(())
    }

    fn element(&mut self, depth: usize) -> Option<Element> {
        if depth > DEPTH || !self.rest().starts_with('<') {
            return None;
        }
        let close = self.rest().find('>')?;
        let tag = &self.rest()[1..close];
        self.at += close + 1;
        let empty = tag.ends_with('/');
        let tag = tag.trim_end_matches('/');
        let name: String = tag.split(|c: char| c.is_whitespace()).next()?.into();
        let ok = |c: char| c.is_alphanumeric() || "_-.:".contains(c);
        if name.is_empty() || !name.chars().all(ok) {
            return None;
        }
        let mut e = Element {
            name,
            ..Element::default()
        };
        if empty {
            return Some(e);
        }
        let mut text = String::new();
        loop {
            let rest = self.rest();
            let lt = rest.find('<')?;
            text.push_str(&unescape(&rest[..lt])?);
            self.at += lt;
            let rest = self.rest();
            if let Some(end) = rest.strip_prefix("</") {
                let gt = end.find('>')?;
                if end[..gt].trim_end() != e.name {
                    return None;
                }
                self.at += 2 + gt + 1;
                e.text = text.trim().to_string();
                return Some(e);
            } else if rest.starts_with("<!--") {
                self.past("-->")?;
            } else if let Some(cdata) = rest.strip_prefix("<![CDATA[") {
                let end = cdata.find("]]>")?;
                text.push_str(&cdata[..end]);
                self.at += 9 + end + 3;
            } else if rest.starts_with("<?") {
                self.past("?>")?;
            } else {
                let child = self.element(depth + 1)?;
                e.children.push(child);
            }
        }
    }
}

/// Text with its entities read: the five XML names and numbers.
fn unescape(s: &str) -> Option<String> {
    if !s.contains('&') {
        return Some(s.to_string());
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let end = after.find(';')?;
        let name = &after[..end];
        let c = match name {
            "amp" => '&',
            "lt" => '<',
            "gt" => '>',
            "quot" => '"',
            "apos" => '\'',
            _ => {
                let n = match name.strip_prefix("#x").or_else(|| name.strip_prefix("#X")) {
                    Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                    None => name.strip_prefix('#')?.parse().ok()?,
                };
                char::from_u32(n)?
            }
        };
        out.push(c);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elements_text_and_entities_are_read() {
        let file = "<?xml version=\"1.0\"?>\r\n<!-- a note -->\r\n<root>\r\n\
            <dose><number>Dose 1</number><age>12 months - 4 days</age></dose>\r\n\
            <dose kind=\"x\"><number>Dose 2</number><age/><note>A &amp; B &#x2265; 2 &lt;3</note></dose>\r\n\
            </root>\r\n";
        let root = read(file).unwrap();
        assert_eq!(root.name, "root");
        let doses: Vec<_> = root.all("dose").collect();
        assert_eq!(doses.len(), 2);
        assert_eq!(doses[0].text_at("age"), Some("12 months - 4 days"));
        assert_eq!(doses[1].text_at("age"), None);
        assert_eq!(doses[1].child("age").map(|a| a.text.as_str()), Some(""));
        assert_eq!(doses[1].text_at("note"), Some("A & B \u{2265} 2 <3"));
        assert_eq!(root.text_at("dose/number"), Some("Dose 1"));
    }

    #[test]
    fn a_file_that_is_not_well_formed_is_none() {
        for file in [
            "<a><b></a></b>",
            "<a>",
            "<a></a><b></b>",
            "<a>&nope;</a>",
            "<a>x &amp</a>",
            "text",
            "",
            "<a></a> trailing",
        ] {
            assert_eq!(read(file), None, "{file}");
        }
        let deep = format!("{}{}", "<a>".repeat(DEPTH + 2), "</a>".repeat(DEPTH + 2));
        assert_eq!(read(&deep), None);
    }
}
