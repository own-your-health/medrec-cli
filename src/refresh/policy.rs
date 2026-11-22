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
