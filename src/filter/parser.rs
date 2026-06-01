//! Game name parsing for region and flag extraction
//!
//! Parses game names from No-Intro and TOSEC formats to extract:
//! - Regions/countries
//! - Dump flags (cracked, trained, hacked, etc.)
//! - Languages
//! - Version info

use std::collections::HashSet;

/// Parsed information from a game name
#[derive(Debug, Clone, Default)]
pub struct ParsedGameName {
    /// Base title without flags
    pub title: String,
    /// Regions/countries found (e.g., "USA", "Europe", "JP")
    pub regions: Vec<String>,
    /// Languages found (e.g., "En", "Fr", "De")
    pub languages: Vec<String>,
    /// Dump flags found (e.g., "cr", "t", "h", "a", "b")
    pub flags: HashSet<String>,
    /// Is this a verified/good dump?
    pub verified: bool,
    /// Is this a bad dump?
    pub bad_dump: bool,
    /// Revision/version string if found
    pub revision: Option<String>,
    /// Is this a beta/proto/demo?
    pub is_prerelease: bool,
}

impl ParsedGameName {
    /// Check if this is an "original" release (no cracks, trainers, hacks)
    pub fn is_original(&self) -> bool {
        !self.flags.iter().any(|f| {
            matches!(
                f.as_str(),
                "cr" | "t" | "h" | "f" | "p" | "m" | "tr" | "o" | "u" | "v"
            )
        })
    }

    /// Check if this is a "clean" dump (original + not bad)
    pub fn is_clean(&self) -> bool {
        self.is_original() && !self.bad_dump
    }

    /// Get primary region (first one found)
    pub fn primary_region(&self) -> Option<&str> {
        self.regions.first().map(|s| s.as_str())
    }
}

/// Parse a game name to extract regions, flags, and other metadata
pub fn parse_game_name(name: &str) -> ParsedGameName {
    let mut result = ParsedGameName::default();

    // Extract the base title (everything before first parenthesis or bracket)
    let title_end = name
        .find('(')
        .or_else(|| name.find('['))
        .unwrap_or(name.len());
    result.title = name[..title_end].trim().to_string();

    // Parse parentheses content (regions, languages, publishers, years)
    for content in extract_paren_contents(name) {
        parse_paren_content(&content, &mut result);
    }

    // Parse bracket content (dump flags)
    for content in extract_bracket_contents(name) {
        parse_bracket_content(&content, &mut result);
    }

    result
}

/// Extract all contents within parentheses
fn extract_paren_contents(name: &str) -> Vec<String> {
    let mut results = Vec::new();
    let mut depth = 0;
    let mut start = 0;

    for (i, c) in name.char_indices() {
        match c {
            '(' => {
                if depth == 0 {
                    start = i + 1;
                }
                depth += 1;
            }
            ')' => {
                depth -= 1;
                if depth == 0 {
                    results.push(name[start..i].to_string());
                }
            }
            _ => {}
        }
    }

    results
}

/// Extract all contents within square brackets
fn extract_bracket_contents(name: &str) -> Vec<String> {
    let mut results = Vec::new();
    let mut depth = 0;
    let mut start = 0;

    for (i, c) in name.char_indices() {
        match c {
            '[' => {
                if depth == 0 {
                    start = i + 1;
                }
                depth += 1;
            }
            ']' => {
                depth -= 1;
                if depth == 0 {
                    results.push(name[start..i].to_string());
                }
            }
            _ => {}
        }
    }

    results
}

/// Parse content from parentheses
fn parse_paren_content(content: &str, result: &mut ParsedGameName) {
    let content_lower = content.to_lowercase();

    // Check for No-Intro style regions
    for region in NO_INTRO_REGIONS {
        if content.eq_ignore_ascii_case(region) {
            result.regions.push(region.to_string());
            return;
        }
    }

    // Check for multi-region (e.g., "USA, Europe")
    if content.contains(',') {
        for part in content.split(',') {
            let part = part.trim();
            for region in NO_INTRO_REGIONS {
                if part.eq_ignore_ascii_case(region) {
                    result.regions.push(region.to_string());
                }
            }
        }
        if !result.regions.is_empty() {
            return;
        }
    }

    // Check for TOSEC-style ISO country codes
    if content.len() == 2 && content.chars().all(|c| c.is_ascii_uppercase())
        && let Some(region) = iso_to_region(content) {
            result.regions.push(region.to_string());
            return;
        }

    // Check for multi-country TOSEC (e.g., "US-EU")
    if content.contains('-') && content.len() <= 8 {
        for part in content.split('-') {
            if part.len() == 2 && part.chars().all(|c| c.is_ascii_uppercase())
                && let Some(region) = iso_to_region(part) {
                    result.regions.push(region.to_string());
                }
        }
        if !result.regions.is_empty() {
            return;
        }
    }

    // Check for languages
    for lang in LANGUAGES {
        if content.eq_ignore_ascii_case(lang) {
            result.languages.push(lang.to_string());
            return;
        }
    }

    // Check for revision/version
    if content_lower.starts_with("rev ")
        || content_lower.starts_with("v")
        || content_lower.starts_with("version")
    {
        result.revision = Some(content.to_string());
        return;
    }

    // Check for beta/proto/demo
    if matches!(
        content_lower.as_str(),
        "beta" | "proto" | "prototype" | "demo" | "sample" | "preview"
    ) {
        result.is_prerelease = true;
    }
}

/// Parse content from square brackets (dump flags)
fn parse_bracket_content(content: &str, result: &mut ParsedGameName) {
    let content_lower = content.to_lowercase();

    // Check for verified dump marker
    if content == "!" {
        result.verified = true;
        return;
    }

    // Check for bad dump
    if content_lower == "b" || content_lower.starts_with("b ") {
        result.bad_dump = true;
        result.flags.insert("b".to_string());
        return;
    }

    // Extract flag prefix (e.g., "cr" from "cr PDX")
    let flag = if let Some(space_pos) = content.find(' ') {
        &content[..space_pos]
    } else {
        content
    };

    let flag_lower = flag.to_lowercase();

    // Known TOSEC flags
    if matches!(
        flag_lower.as_str(),
        "cr" | "f" | "h" | "m" | "p" | "t" | "tr" | "o" | "u" | "v" | "a"
    ) {
        result.flags.insert(flag_lower);
    }
}

/// Convert ISO 3166-1 alpha-2 code to region name
fn iso_to_region(code: &str) -> Option<&'static str> {
    match code.to_uppercase().as_str() {
        "US" => Some("USA"),
        "JP" => Some("Japan"),
        "EU" => Some("Europe"),
        "DE" => Some("Germany"),
        "FR" => Some("France"),
        "GB" | "UK" => Some("UK"),
        "ES" => Some("Spain"),
        "IT" => Some("Italy"),
        "BR" => Some("Brazil"),
        "AU" => Some("Australia"),
        "CA" => Some("Canada"),
        "CN" => Some("China"),
        "KR" => Some("Korea"),
        "NL" => Some("Netherlands"),
        "SE" => Some("Sweden"),
        "RU" => Some("Russia"),
        "PL" => Some("Poland"),
        "PT" => Some("Portugal"),
        "TW" => Some("Taiwan"),
        "HK" => Some("Hong Kong"),
        _ => None,
    }
}

/// No-Intro region names
const NO_INTRO_REGIONS: &[&str] = &[
    "World",
    "USA",
    "Europe",
    "Japan",
    "Australia",
    "Brazil",
    "Canada",
    "China",
    "France",
    "Germany",
    "Hong Kong",
    "Italy",
    "Korea",
    "Netherlands",
    "Russia",
    "Spain",
    "Sweden",
    "Taiwan",
    "UK",
    "Asia",
    "Scandinavia",
];

/// Common language codes
const LANGUAGES: &[&str] = &[
    "En", "Fr", "De", "Es", "It", "Ja", "Pt", "Zh", "Ko", "Nl", "Sv", "Ru", "Pl",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_nointro_usa() {
        let parsed = parse_game_name("Super Mario Bros (USA)");
        assert_eq!(parsed.title, "Super Mario Bros");
        assert_eq!(parsed.regions, vec!["USA"]);
        assert!(parsed.is_clean());
    }

    #[test]
    fn test_parse_nointro_multi_region() {
        let parsed = parse_game_name("Tetris (USA, Europe)");
        assert_eq!(parsed.title, "Tetris");
        assert_eq!(parsed.regions, vec!["USA", "Europe"]);
    }

    #[test]
    fn test_parse_nointro_world() {
        let parsed = parse_game_name("Pokemon Red (World)");
        assert_eq!(parsed.regions, vec!["World"]);
    }

    #[test]
    fn test_parse_nointro_with_revision() {
        let parsed = parse_game_name("Legend of Zelda, The (USA) (Rev A)");
        assert_eq!(parsed.title, "Legend of Zelda, The");
        assert_eq!(parsed.regions, vec!["USA"]);
        assert_eq!(parsed.revision, Some("Rev A".to_string()));
    }

    #[test]
    fn test_parse_tosec_basic() {
        let parsed = parse_game_name("Game Name (1990)(Publisher)(US)");
        assert_eq!(parsed.title, "Game Name");
        assert_eq!(parsed.regions, vec!["USA"]);
    }

    #[test]
    fn test_parse_tosec_cracked() {
        let parsed = parse_game_name("Game Name (1990)(Publisher)(US)[cr Razor]");
        assert_eq!(parsed.regions, vec!["USA"]);
        assert!(parsed.flags.contains("cr"));
        assert!(!parsed.is_original());
        assert!(!parsed.is_clean());
    }

    #[test]
    fn test_parse_tosec_trainer() {
        let parsed = parse_game_name("Game Name (1990)(Publisher)(US)[t +3]");
        assert!(parsed.flags.contains("t"));
        assert!(!parsed.is_original());
    }

    #[test]
    fn test_parse_tosec_hacked() {
        let parsed = parse_game_name("Game Name (1990)(Publisher)(US)[h Intro]");
        assert!(parsed.flags.contains("h"));
        assert!(!parsed.is_original());
    }

    #[test]
    fn test_parse_tosec_bad_dump() {
        let parsed = parse_game_name("Game Name (1990)(Publisher)(US)[b]");
        assert!(parsed.bad_dump);
        assert!(!parsed.is_clean());
    }

    #[test]
    fn test_parse_tosec_alternate() {
        let parsed = parse_game_name("Game Name (1990)(Publisher)(US)[a]");
        assert!(parsed.flags.contains("a"));
        // Alternate is still "original" (not a hack/crack), just a different dump
        assert!(parsed.is_original());
    }

    #[test]
    fn test_parse_verified_dump() {
        let parsed = parse_game_name("Game Name (USA) [!]");
        assert!(parsed.verified);
    }

    #[test]
    fn test_parse_beta() {
        let parsed = parse_game_name("Game Name (USA) (Beta)");
        assert!(parsed.is_prerelease);
    }

    #[test]
    fn test_parse_multi_country_tosec() {
        let parsed = parse_game_name("Game Name (1990)(Publisher)(US-EU)");
        assert_eq!(parsed.regions, vec!["USA", "Europe"]);
    }

    #[test]
    fn test_is_clean() {
        // Clean: original + not bad
        let clean = parse_game_name("Game (USA)");
        assert!(clean.is_clean());

        // Not clean: cracked
        let cracked = parse_game_name("Game (USA)[cr]");
        assert!(!cracked.is_clean());

        // Not clean: bad dump
        let bad = parse_game_name("Game (USA)[b]");
        assert!(!bad.is_clean());

        // Not clean: trained
        let trained = parse_game_name("Game (USA)[t]");
        assert!(!trained.is_clean());
    }
}
