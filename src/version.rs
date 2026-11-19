//! Which version of a resource a store keeps: medical-common's rule
//! (`pk_med_records::version`, DESIGN §1.4). A version's content is the
//! resource without `meta.lastUpdated`, so a re-read that changes only the
//! stamp is the same version. Between versions whose content differs,
//! `Provenance::supersedes` decides.

use serde_json::Value;

pub use pk_med_records::version::{content, decide, Filing};

use crate::stream::RawResource;

/// SHA-256 of a resource's [`content`], as hex: the same across re-reads
/// that only stamp it again.
pub fn content_digest(resource: &RawResource) -> String {
    crate::hash::sha256_hex(content(resource).as_bytes())
}

/// [`content_digest`] of a parsed resource.
pub fn value_digest(resource: &Value) -> String {
    match RawResource::from_value(resource) {
        Ok(r) => content_digest(&r),
        Err(_) => crate::hash::sha256_hex(resource.to_string().as_bytes()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_digest_leaves_out_the_stamp() {
        let stamped = |s: &str| {
            json!({"resourceType": "CareTeam",
                "meta": {"lastUpdated": s, "source": "x"}, "status": "active"})
        };
        let a = value_digest(&stamped("2026-03-01T00:00:00Z"));
        assert_eq!(a, value_digest(&stamped("2026-03-09T00:00:00Z")));
        let other = json!({"resourceType": "CareTeam",
            "meta": {"lastUpdated": "2026-03-01T00:00:00Z", "source": "x"}, "status": "inactive"});
        assert_ne!(a, value_digest(&other));
        let bare =
            json!({"resourceType": "CareTeam", "meta": {"lastUpdated": "x"}, "status": "active"});
        assert_eq!(
            value_digest(&bare),
            value_digest(&json!({"resourceType": "CareTeam", "status": "active"}))
        );
    }
}
