//! medrec: a household's medical records in a plain Drive folder. The
//! binary's commands live in `main.rs`; the mechanisms they use live here so
//! the tests can reach them.

pub mod about;
pub mod against;
pub mod batch;
pub mod bill;
pub mod bills;
pub mod changes;
pub mod claims;
pub mod code_names;
pub mod config;
pub mod corrections;
pub mod dicom;
pub mod documents;
pub mod events;
pub mod folder;
pub mod given;
pub mod hash;
pub mod ids;
pub mod imaging;
pub mod import;
pub mod intake;
pub mod labels;
pub mod maint;
pub mod marks;
pub mod mcp;
pub mod model;
pub mod normalize;
pub mod notes;
pub mod nppes;
pub mod parts;
pub mod pdfgen;
pub mod people;
pub mod plain;
pub mod private;
pub mod proc;
pub mod prompt;
pub mod provider_labels;
pub mod providers;
pub mod questionnaires;
pub mod raw;
pub mod refresh;
pub mod reindex;
pub mod review;
pub mod roles;
pub mod services;
pub mod sources;
pub mod standing;
pub mod store;
pub mod stream;
pub mod summary;
pub mod tables;
#[doc(hidden)]
pub mod testdata;
pub mod version;
pub mod view;
pub mod viewfiles;
pub mod views;

/// Create `path` owner-only (0600) and write `bytes`; never replaces a file.
pub fn write_private(path: &std::path::Path, bytes: &[u8]) -> Result<(), pk_cli_core::CliError> {
    pk_cli_drive::private_file::write(path, bytes, pk_cli_drive::private_file::Replace::Never)
        .map_err(|e| pk_cli_core::CliError::Other(format!("writing {}: {e}", path.display())))
}

/// `s` as a terminal shows it without being steered: no control characters
/// (escape sequences, newlines), and none of the invisible format characters
/// that reorder or hide text (bidi overrides and isolates, zero-width marks,
/// line and paragraph separators, tag characters). For text a provider
/// supplies and medrec prints.
pub fn printable(s: &str) -> String {
    s.chars().filter(|&c| !hidden(c)).collect()
}

fn hidden(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{00AD}'
                | '\u{0600}'..='\u{0605}'
                | '\u{061C}'
                | '\u{06DD}'
                | '\u{070F}'
                | '\u{180E}'
                | '\u{200B}'..='\u{200F}'
                | '\u{2028}'..='\u{202E}'
                | '\u{2060}'..='\u{2064}'
                | '\u{2066}'..='\u{206F}'
                | '\u{FEFF}'
                | '\u{FFF9}'..='\u{FFFB}'
                | '\u{E0000}'..='\u{E007F}'
        )
}

#[cfg(test)]
mod tests {
    #[test]
    fn printable_drops_what_can_steer_a_terminal() {
        for (input, shown) in [
            (
                "Outside Records are not available",
                "Outside Records are not available",
            ),
            ("a\u{1b}[2Jb\u{1b}]0;title\u{7}c", "a[2Jb]0;titlec"),
            ("line\nbreak\r\ttab", "linebreaktab"),
            ("abc\u{202E}fed\u{2066}x\u{2069}", "abcfedx"),
            ("zero\u{200B}width\u{FEFF}\u{2028}", "zerowidth"),
            ("tag\u{E0041}", "tag"),
            (
                "Cl\u{00ED}nica S\u{00E3}o Paulo",
                "Cl\u{00ED}nica S\u{00E3}o Paulo",
            ),
            ("", ""),
        ] {
            assert_eq!(super::printable(input), shown, "{input:?}");
        }
    }
}

#[cfg(test)]
pub(crate) mod testutil {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A fresh directory under the system temp dir, unique per call.
    pub fn temp_dir(label: &str) -> PathBuf {
        static SEQ: AtomicU32 = AtomicU32::new(0);
        let p = std::env::temp_dir().join(format!(
            "medrec-test-{label}-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }
}

#[cfg(test)]
mod drive_tests;
