//! DAT file parsing
//!
//! Supports two DAT formats:
//! - **Logiqx XML**: The standard XML format used by No-Intro, Redump, TOSEC, and MAME
//! - **ClrMamePro**: Text-based format used by many legacy DAT distributions
//!
//! Format is auto-detected based on file content.

pub mod clrmamepro;
pub mod parser;
pub mod types;
pub mod zxdb;

pub use clrmamepro::*;
pub use parser::*;
pub use types::*;

use anyhow::{Context, Result};
use std::path::Path;

/// Parse a DAT file, auto-detecting format (Logiqx XML or ClrMamePro)
pub fn parse_dat_file_auto(path: &Path) -> Result<(DatHeader, Vec<DatGameEntry>)> {
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read DAT file: {:?}", path))?;

    parse_dat_auto(&contents)
}

/// Parse DAT content from a string, auto-detecting format
pub fn parse_dat_auto(content: &str) -> Result<(DatHeader, Vec<DatGameEntry>)> {
    if is_clrmamepro_format(content) {
        parse_clrmamepro(content)
    } else {
        parse_dat(content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_auto_detect_xml() {
        let xml = r#"<?xml version="1.0"?>
<datafile>
  <header><name>XML DAT</name></header>
  <game name="test"><rom name="test.rom" size="100" sha1="ABC"/></game>
</datafile>"#;

        let (header, games) = parse_dat_auto(xml).unwrap();
        assert_eq!(header.name, "XML DAT");
        assert_eq!(games.len(), 1);
    }

    #[test]
    fn test_auto_detect_clrmamepro() {
        let cmp = r#"
clrmamepro ( name "CMP DAT" version "1.0" )
game ( name "test" rom ( name "test.rom" size 100 sha1 ABC ) )
"#;

        let (header, games) = parse_dat_auto(cmp).unwrap();
        assert_eq!(header.name, "CMP DAT");
        assert_eq!(games.len(), 1);
    }
}
