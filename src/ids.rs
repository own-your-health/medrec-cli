//! Opaque random identifiers for what the index refers to: people,
//! documents, records, notes, review items, imports, studies. An id names
//! nothing about the person or the record, so it is safe in a log line or
//! a command line.

use rand::RngCore;

/// RFC 4648 base32, lowercase, no padding: unambiguous in a file name on
/// every filesystem and case-insensitive ones included.
const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";

/// `<prefix>_<20 base32 chars>`: 100 random bits from the OS generator.
pub fn new(prefix: &str) -> String {
    let mut bytes = [0u8; 13];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    format!("{prefix}_{}", &base32(&bytes)[..20])
}

/// `<prefix>_<20 base32 chars>` from a digest: an id that names the same
/// thing every time it is made from the same input.
pub fn from_digest(prefix: &str, digest: &[u8]) -> String {
    format!("{prefix}_{}", &base32(&digest[..13])[..20])
}

fn base32(bytes: &[u8]) -> String {
    let mut out = String::new();
    let mut buffer: u32 = 0;
    let mut bits = 0;
    for &b in bytes {
        buffer = (buffer << 8) | u32::from(b);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((buffer >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(ALPHABET[((buffer << (5 - bits)) & 31) as usize] as char);
    }
    out
}

/// Whether `s` is an id this module could have produced for `prefix`. Ids
/// become path components, so anything read back from a file is checked
/// before it is joined onto a path.
pub fn is_valid(prefix: &str, s: &str) -> bool {
    let Some(rest) = s.strip_prefix(prefix).and_then(|r| r.strip_prefix('_')) else {
        return false;
    };
    rest.len() == 20 && rest.bytes().all(|c| ALPHABET.contains(&c))
}

/// Whether `s` is an id this module could have produced, of anything.
pub fn is_id(s: &str) -> bool {
    s.split_once('_')
        .is_some_and(|(prefix, _)| prefix.len() == 1 && is_valid(prefix, s))
        && s.starts_with(|c: char| c.is_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_prefixed_random_and_valid() {
        let a = new("p");
        let b = new("p");
        assert_ne!(a, b);
        assert!(a.starts_with("p_"));
        assert_eq!(a.len(), 22);
        assert!(is_valid("p", &a));
        assert!(!is_valid("o", &a));
        assert!(is_id(&a) && is_id(&from_digest("e", &[7; 32])));
        let text = [
            "r_aaaaaaaaaaaaaaaaaaaa b",
            "a_aaaaaaaaaaaaaaaaaaa",
            "R_aaaaaaaaaaaaaaaaaaaa",
            "1_aaaaaaaaaaaaaaaaaaaa",
            "pp_aaaaaaaaaaaaaaaaaaaa",
            "_aaaaaaaaaaaaaaaaaaaa",
            "",
        ];
        assert!(text.iter().all(|t| !is_id(t)));
    }

    #[test]
    fn path_like_or_malformed_ids_are_refused() {
        for bad in [
            "p_",
            "p_../../etc/passwd00",
            "p_AAAAAAAAAAAAAAAAAAAA",
            "p_aaaaaaaaaaaaaaaaaaaa/",
            "pp_aaaaaaaaaaaaaaaaaaaa",
            "p_aaaaaaaaaaaaaaaaaaa1",
        ] {
            assert!(!is_valid("p", bad), "{bad}");
        }
    }

    #[test]
    fn base32_matches_rfc4648_vectors() {
        assert_eq!(base32(b"f"), "my");
        assert_eq!(base32(b"fo"), "mzxq");
        assert_eq!(base32(b"foobar"), "mzxw6ytboi");
    }
}
