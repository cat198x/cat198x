//! ClrMamePro DAT format parser
//!
//! Parses the text-based ClrMamePro format used by many DAT distributions.
//! Format example:
//! ```text
//! clrmamepro (
//!     name "Example DAT"
//!     description "Example"
//!     version "1.0"
//! )
//!
//! game (
//!     name "pacman"
//!     description "Pac-Man"
//!     rom ( name "pacman.bin" size 4096 crc 12345678 )
//! )
//! ```

use anyhow::{Context, Result};
use std::path::Path;

use super::types::*;

/// Parse a ClrMamePro format DAT file
pub fn parse_clrmamepro_file(path: &Path) -> Result<(DatHeader, Vec<DatGameEntry>)> {
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read DAT file: {:?}", path))?;

    parse_clrmamepro(&contents)
}

/// Parse ClrMamePro format DAT content from a string
pub fn parse_clrmamepro(content: &str) -> Result<(DatHeader, Vec<DatGameEntry>)> {
    let tokens = tokenize(content);
    let mut pos = 0;

    let mut header = DatHeader::default();
    let mut games = Vec::new();

    while pos < tokens.len() {
        match tokens[pos].as_str() {
            "clrmamepro" | "header" => {
                pos += 1;
                if pos < tokens.len() && tokens[pos] == "(" {
                    let (block, new_pos) = parse_block(&tokens, pos)?;
                    pos = new_pos;
                    header = parse_header_block(&block);
                }
            }
            "game" | "machine" | "resource" => {
                pos += 1;
                if pos < tokens.len() && tokens[pos] == "(" {
                    let (block, new_pos) = parse_block(&tokens, pos)?;
                    pos = new_pos;
                    if let Some(game) = parse_game_block(&block) {
                        games.push(game);
                    }
                }
            }
            _ => pos += 1,
        }
    }

    Ok((header, games))
}

/// Tokenize ClrMamePro format into words, quoted strings, and parentheses
fn tokenize(content: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut chars = content.chars().peekable();

    while let Some(&c) = chars.peek() {
        match c {
            // Whitespace - skip
            ' ' | '\t' | '\n' | '\r' => {
                chars.next();
            }
            // Comment - skip to end of line
            ';' | '#' => {
                while let Some(&ch) = chars.peek() {
                    chars.next();
                    if ch == '\n' {
                        break;
                    }
                }
            }
            // Parentheses - single token
            '(' | ')' => {
                tokens.push(c.to_string());
                chars.next();
            }
            // Quoted string
            '"' => {
                chars.next(); // consume opening quote
                let mut s = String::new();
                while let Some(&ch) = chars.peek() {
                    chars.next();
                    if ch == '"' {
                        break;
                    }
                    // Handle escaped quotes
                    if ch == '\\'
                        && let Some(&next) = chars.peek()
                        && next == '"'
                    {
                        s.push('"');
                        chars.next();
                        continue;
                    }
                    s.push(ch);
                }
                tokens.push(s);
            }
            // Unquoted word
            _ => {
                let mut word = String::new();
                while let Some(&ch) = chars.peek() {
                    if ch.is_whitespace() || ch == '(' || ch == ')' || ch == '"' {
                        break;
                    }
                    word.push(ch);
                    chars.next();
                }
                if !word.is_empty() {
                    tokens.push(word);
                }
            }
        }
    }

    tokens
}

/// Parse a parenthesised block, returning content and new position
fn parse_block(tokens: &[String], start: usize) -> Result<(Vec<String>, usize)> {
    if start >= tokens.len() || tokens[start] != "(" {
        anyhow::bail!("Expected '(' at position {}", start);
    }

    let mut pos = start + 1;
    let mut depth = 1;
    let mut content = Vec::new();

    while pos < tokens.len() && depth > 0 {
        match tokens[pos].as_str() {
            "(" => {
                depth += 1;
                content.push(tokens[pos].clone());
            }
            ")" => {
                depth -= 1;
                if depth > 0 {
                    content.push(tokens[pos].clone());
                }
            }
            _ => {
                content.push(tokens[pos].clone());
            }
        }
        pos += 1;
    }

    Ok((content, pos))
}

/// Parse header block into DatHeader
fn parse_header_block(tokens: &[String]) -> DatHeader {
    let mut header = DatHeader::default();
    let mut i = 0;

    while i < tokens.len() {
        let key = tokens[i].to_lowercase();
        i += 1;

        if i < tokens.len() && tokens[i] != "(" {
            let value = &tokens[i];
            match key.as_str() {
                "name" => header.name = value.clone(),
                "description" => header.description = Some(value.clone()),
                "version" => header.version = Some(value.clone()),
                "author" => header.author = Some(value.clone()),
                "homepage" | "url" => header.homepage = Some(value.clone()),
                "category" => header.category = Some(value.clone()),
                _ => {}
            }
            i += 1;
        }
    }

    header
}

/// Parse game block into DatGameEntry
fn parse_game_block(tokens: &[String]) -> Option<DatGameEntry> {
    let mut game = DatGameEntry {
        name: String::new(),
        description: None,
        clone_of: None,
        rom_of: None,
        is_bios: false,
        is_device: false,
        is_mechanical: false,
        roms: Vec::new(),
        devices: Vec::new(),
    };

    let mut i = 0;

    while i < tokens.len() {
        let key = tokens[i].to_lowercase();
        i += 1;

        if i >= tokens.len() {
            break;
        }

        // Check for nested block (rom, disk, etc.)
        if tokens[i] == "("
            && let Ok((block, new_i)) = parse_block(tokens, i)
        {
            if key == "rom"
                && let Some(rom) = parse_rom_block(&block)
            {
                game.roms.push(rom);
            }
            i = new_i;
            continue;
        }

        // Simple key-value
        let value = &tokens[i];
        match key.as_str() {
            "name" => game.name = value.clone(),
            "description" => game.description = Some(value.clone()),
            "cloneof" => game.clone_of = Some(value.clone()),
            "romof" => game.rom_of = Some(value.clone()),
            "isbios" => game.is_bios = value == "yes",
            "isdevice" => game.is_device = value == "yes",
            "ismechanical" => game.is_mechanical = value == "yes",
            _ => {}
        }
        i += 1;
    }

    if game.name.is_empty() {
        return None;
    }

    Some(game)
}

/// Parse rom block into DatRomEntry
fn parse_rom_block(tokens: &[String]) -> Option<DatRomEntry> {
    let mut rom = DatRomEntry {
        name: String::new(),
        size: 0,
        crc32: None,
        md5: None,
        sha1: None,
        status: RomStatus::Good,
        merge: None,
        is_disk: false,
    };

    let mut i = 0;

    while i < tokens.len() {
        let key = tokens[i].to_lowercase();
        i += 1;

        if i >= tokens.len() {
            break;
        }

        let value = &tokens[i];
        match key.as_str() {
            "name" => rom.name = value.clone(),
            "size" => rom.size = value.parse().unwrap_or(0),
            "crc" | "crc32" => rom.crc32 = Some(value.to_uppercase()),
            "md5" => rom.md5 = Some(value.to_uppercase()),
            "sha1" => rom.sha1 = Some(value.to_uppercase()),
            "status" | "flags" => rom.status = RomStatus::parse(value),
            "merge" => rom.merge = Some(value.clone()),
            _ => {}
        }
        i += 1;
    }

    if rom.name.is_empty() {
        return None;
    }

    Some(rom)
}

/// Check if content appears to be ClrMamePro format (vs XML)
pub fn is_clrmamepro_format(content: &str) -> bool {
    let trimmed = content.trim_start();

    // XML starts with <? or <! or <datafile or <mame
    if trimmed.starts_with("<?")
        || trimmed.starts_with("<!")
        || trimmed.starts_with("<datafile")
        || trimmed.starts_with("<mame")
        || trimmed.starts_with("<softwarelist")
    {
        return false;
    }

    // ClrMamePro format typically starts with clrmamepro, header, or game
    let lower = trimmed.to_lowercase();
    lower.starts_with("clrmamepro")
        || lower.starts_with("header")
        || lower.starts_with("game")
        || lower.starts_with("machine")
        || lower.starts_with("resource")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tokenize_simple() {
        let tokens = tokenize("name \"test\"");
        assert_eq!(tokens, vec!["name", "test"]);
    }

    #[test]
    fn test_tokenize_with_parens() {
        let tokens = tokenize("game ( name \"pacman\" )");
        assert_eq!(tokens, vec!["game", "(", "name", "pacman", ")"]);
    }

    #[test]
    fn test_tokenize_unquoted_values() {
        let tokens = tokenize("size 4096 crc ABCD1234");
        assert_eq!(tokens, vec!["size", "4096", "crc", "ABCD1234"]);
    }

    #[test]
    fn test_tokenize_comments() {
        let tokens = tokenize("name \"test\" ; this is a comment\nsize 100");
        assert_eq!(tokens, vec!["name", "test", "size", "100"]);
    }

    #[test]
    fn test_parse_simple_dat() {
        let content = r#"
clrmamepro (
    name "Test DAT"
    description "A test DAT file"
    version "1.0"
    author "Test Author"
)

game (
    name "pacman"
    description "Pac-Man"
    rom ( name "pacman.bin" size 4096 crc ABCD1234 )
)
"#;

        let (header, games) = parse_clrmamepro(content).unwrap();

        assert_eq!(header.name, "Test DAT");
        assert_eq!(header.description, Some("A test DAT file".to_string()));
        assert_eq!(header.version, Some("1.0".to_string()));
        assert_eq!(header.author, Some("Test Author".to_string()));

        assert_eq!(games.len(), 1);
        assert_eq!(games[0].name, "pacman");
        assert_eq!(games[0].description, Some("Pac-Man".to_string()));
        assert_eq!(games[0].roms.len(), 1);
        assert_eq!(games[0].roms[0].name, "pacman.bin");
        assert_eq!(games[0].roms[0].size, 4096);
        assert_eq!(games[0].roms[0].crc32, Some("ABCD1234".to_string()));
    }

    #[test]
    fn test_parse_multiple_roms() {
        let content = r#"
game (
    name "multi"
    rom ( name "prg.bin" size 1000 sha1 ABC123 )
    rom ( name "chr.bin" size 2000 sha1 DEF456 )
)
"#;

        let (_, games) = parse_clrmamepro(content).unwrap();

        assert_eq!(games[0].roms.len(), 2);
        assert_eq!(games[0].roms[0].name, "prg.bin");
        assert_eq!(games[0].roms[1].name, "chr.bin");
    }

    #[test]
    fn test_parse_clone_relationship() {
        let content = r#"
game (
    name "pacman"
    description "Pac-Man"
)

game (
    name "puckman"
    cloneof "pacman"
    romof "pacman"
    description "Puck Man"
)
"#;

        let (_, games) = parse_clrmamepro(content).unwrap();

        assert_eq!(games.len(), 2);
        assert!(games[0].clone_of.is_none());
        assert_eq!(games[1].clone_of, Some("pacman".to_string()));
        assert_eq!(games[1].rom_of, Some("pacman".to_string()));
    }

    #[test]
    fn test_parse_rom_all_hashes() {
        let content = r#"
game (
    name "test"
    rom ( name "test.bin" size 100 crc AAAA md5 BBBBBBBB sha1 CCCCCCCC )
)
"#;

        let (_, games) = parse_clrmamepro(content).unwrap();

        assert_eq!(games[0].roms[0].crc32, Some("AAAA".to_string()));
        assert_eq!(games[0].roms[0].md5, Some("BBBBBBBB".to_string()));
        assert_eq!(games[0].roms[0].sha1, Some("CCCCCCCC".to_string()));
    }

    #[test]
    fn test_parse_rom_status() {
        let content = r#"
game (
    name "test"
    rom ( name "good.bin" size 100 )
    rom ( name "bad.bin" size 100 status baddump )
    rom ( name "no.bin" size 100 status nodump )
)
"#;

        let (_, games) = parse_clrmamepro(content).unwrap();

        assert_eq!(games[0].roms[0].status, RomStatus::Good);
        assert_eq!(games[0].roms[1].status, RomStatus::BadDump);
        assert_eq!(games[0].roms[2].status, RomStatus::NoDump);
    }

    #[test]
    fn test_parse_bios_device_mechanical() {
        let content = r#"
game (
    name "neogeo"
    isbios "yes"
)

game (
    name "device"
    isdevice "yes"
)

game (
    name "pinball"
    ismechanical "yes"
)
"#;

        let (_, games) = parse_clrmamepro(content).unwrap();

        assert!(games[0].is_bios);
        assert!(!games[0].is_device);

        assert!(games[1].is_device);
        assert!(!games[1].is_bios);

        assert!(games[2].is_mechanical);
    }

    #[test]
    fn test_is_clrmamepro_format() {
        // ClrMamePro format
        assert!(is_clrmamepro_format("clrmamepro ( name \"test\" )"));
        assert!(is_clrmamepro_format("game ( name \"test\" )"));
        assert!(is_clrmamepro_format("  \n  clrmamepro ("));

        // XML format
        assert!(!is_clrmamepro_format("<?xml version=\"1.0\"?>"));
        assert!(!is_clrmamepro_format("<!DOCTYPE datafile>"));
        assert!(!is_clrmamepro_format("<datafile>"));
        assert!(!is_clrmamepro_format("  <?xml"));
    }

    #[test]
    fn test_parse_merge_attribute() {
        let content = r#"
game (
    name "clone"
    cloneof "parent"
    rom ( name "shared.bin" merge "shared.bin" size 100 crc AAAA )
)
"#;

        let (_, games) = parse_clrmamepro(content).unwrap();

        assert_eq!(games[0].roms[0].merge, Some("shared.bin".to_string()));
    }

    #[test]
    fn test_parse_header_keyword() {
        // Some DATs use "header" instead of "clrmamepro"
        let content = r#"
header (
    name "Test"
    version "1.0"
)

game (
    name "test"
)
"#;

        let (header, games) = parse_clrmamepro(content).unwrap();

        assert_eq!(header.name, "Test");
        assert_eq!(games.len(), 1);
    }

    #[test]
    fn test_hash_case_normalisation() {
        let content = r#"
game (
    name "test"
    rom ( name "test.bin" size 100 crc abcd1234 md5 deadbeef sha1 cafebabe )
)
"#;

        let (_, games) = parse_clrmamepro(content).unwrap();

        assert_eq!(games[0].roms[0].crc32, Some("ABCD1234".to_string()));
        assert_eq!(games[0].roms[0].md5, Some("DEADBEEF".to_string()));
        assert_eq!(games[0].roms[0].sha1, Some("CAFEBABE".to_string()));
    }

    #[test]
    fn test_parse_machine_keyword() {
        // MAME-style DATs sometimes use "machine" instead of "game"
        let content = r#"
clrmamepro ( name "MAME" )
machine (
    name "pacman"
    description "Pac-Man"
)
"#;

        let (_, games) = parse_clrmamepro(content).unwrap();

        assert_eq!(games.len(), 1);
        assert_eq!(games[0].name, "pacman");
    }

    #[test]
    fn test_parse_resource_keyword() {
        // Some DATs use "resource" for BIOS sets
        let content = r#"
resource (
    name "neogeo"
    description "Neo-Geo BIOS"
)
"#;

        let (_, games) = parse_clrmamepro(content).unwrap();

        assert_eq!(games.len(), 1);
        assert_eq!(games[0].name, "neogeo");
    }
}
