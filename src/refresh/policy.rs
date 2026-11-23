//! How often each provider may be imported.
//!
//! No provider CLI documents a records-export limit yet, so medrec keeps
//! floors of its own, by how the CLI reaches its provider (the README's
//! provider table): a member site, which a person also signs in to, at most
//! daily; a developer app or portal key at most every 6 hours. A CLI medrec
//! does not know is taken as a member site.

pub const DAY: u64 = 86400;
const DEVELOPER_APP_FLOOR: u64 = 6 * 3600;

/// How a CLI reaches its provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    DeveloperApp,
    PortalKey,
    MemberSite,
}

pub fn reach(cli: &str) -> Reach {
    match cli {
        "mychart" | "athena" | "kp" | "modmed" => Reach::DeveloperApp,
        "icanotes" => Reach::PortalKey,
        _ => Reach::MemberSite,
    }
}

/// How often a person's request (`_requests/`, or `medrec refresh` in a
/// terminal) may import a source: once per this many seconds, whatever its
/// schedule's floor. A person asking is a person at the provider, as when
/// they sign in themselves.
pub const REQUEST_GUARD: u64 = 15 * 60;

/// Whether a CLI's sign-in renews itself unattended: `false` for member
/// sites whose cookie session ends within hours (measured), `true` for the
/// API sources, which hold a refresh token or a portal key; `None` when not
/// known. A provider whose `auth status` gives `renews` overrides it.
pub fn renews(cli: &str) -> Option<bool> {
    match cli {
        "uhc" | "fertemr" | "caremark" | "oneblood" | "flywire" | "peryourhealth" => Some(false),
        "mychart" | "athena" | "kp" | "modmed" | "icanotes" => Some(true),
        _ => None,
    }
}

/// About how long a fresh sign-in lasts, where measured: uhc's survived 40
/// minutes idle and ended at about 45. A provider whose `auth status`
/// gives `lasts_secs` overrides it.
pub fn lasts_secs(cli: &str) -> Option<u64> {
    match cli {
        "uhc" => Some(2400),
        _ => None,
    }
}

/// The shortest time between two imports of a source from `cli`.
pub fn min_interval(cli: &str) -> u64 {
    match reach(cli) {
        Reach::DeveloperApp | Reach::PortalKey => DEVELOPER_APP_FLOOR,
        Reach::MemberSite => DAY,
    }
}

/// How often a source is imported on its schedule: its own setting, never
/// below the floor; daily by default.
pub fn every(cli: &str, setting: Option<u64>) -> u64 {
    setting.unwrap_or(DAY).max(min_interval(cli))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_sessions_are_marked_as_not_renewing() {
        for cli in [
            "uhc",
            "fertemr",
            "caremark",
            "oneblood",
            "flywire",
            "peryourhealth",
        ] {
            assert_eq!(renews(cli), Some(false), "{cli}");
        }
        for cli in ["mychart", "athena", "kp", "modmed", "icanotes"] {
            assert_eq!(renews(cli), Some(true), "{cli}");
        }
        assert_eq!(renews("labcorp"), None);
        assert_eq!(lasts_secs("uhc"), Some(2400));
        assert_eq!(lasts_secs("mychart"), None);
    }

    #[test]
    fn member_sites_are_imported_at_most_daily() {
        for cli in [
            "uhc",
            "caremark",
            "labcorp",
            "oneblood",
            "fertemr",
            "salve",
            "unknowncli",
        ] {
            assert_eq!(min_interval(cli), DAY, "{cli}");
            assert_eq!(every(cli, Some(3600)), DAY, "{cli}");
        }
        for cli in ["mychart", "athena", "kp", "modmed", "icanotes"] {
            assert_eq!(min_interval(cli), 6 * 3600, "{cli}");
            assert_eq!(every(cli, None), DAY);
            assert_eq!(every(cli, Some(3600)), 6 * 3600);
            assert_eq!(every(cli, Some(2 * DAY)), 2 * DAY);
        }
    }
}
