//! ROM selection preferences for 1G1R filtering
//!
//! Provides configuration for selecting a single preferred ROM from
//! multiple regional variants or dump versions of the same game.

use super::ParsedGameName;

/// Default region priority order
/// Designed for English-speaking users with NTSC preference
pub const DEFAULT_REGION_PRIORITY: &[&str] = &[
    "World",
    "USA",
    "Europe",
    "Japan",
    "Australia",
    "Canada",
    "UK",
    "France",
    "Germany",
    "Spain",
    "Italy",
    "Netherlands",
    "Sweden",
    "Brazil",
    "Korea",
    "China",
    "Taiwan",
    "Hong Kong",
    "Russia",
    "Asia",
    "Scandinavia",
];

/// Preferences for ROM selection
#[derive(Debug, Clone)]
pub struct FilterPreferences {
    /// Region priority order (first = most preferred)
    pub region_priority: Vec<String>,
    /// Exclude cracks, trainers, hacks (TOSEC flags: cr, t, h, f, m, tr, o, u, v)
    pub exclude_modified: bool,
    /// Exclude bad dumps (TOSEC flag: b)
    pub exclude_bad_dumps: bool,
    /// Exclude betas, protos, demos
    pub exclude_prereleases: bool,
    /// Prefer verified dumps (GoodTools [!] flag)
    pub prefer_verified: bool,
    /// Prefer parent ROMs over clones (for MAME-style DATs)
    pub prefer_parent: bool,
}

impl Default for FilterPreferences {
    fn default() -> Self {
        Self {
            region_priority: DEFAULT_REGION_PRIORITY
                .iter()
                .map(|s| s.to_string())
                .collect(),
            exclude_modified: true,
            exclude_bad_dumps: true,
            exclude_prereleases: false, // Some users want betas
            prefer_verified: true,
            prefer_parent: true,
        }
    }
}

impl FilterPreferences {
    /// Create preferences with a custom region priority
    pub fn with_regions(regions: Vec<String>) -> Self {
        Self {
            region_priority: regions,
            ..Default::default()
        }
    }

    /// Check if a parsed ROM should be excluded based on preferences
    pub fn should_exclude(&self, parsed: &ParsedGameName) -> bool {
        if self.exclude_modified && !parsed.is_original() {
            return true;
        }
        if self.exclude_bad_dumps && parsed.bad_dump {
            return true;
        }
        if self.exclude_prereleases && parsed.is_prerelease {
            return true;
        }
        false
    }

    /// Get the region priority score for a parsed ROM (lower = better)
    /// Returns None if no matching region found
    fn region_score(&self, parsed: &ParsedGameName) -> Option<usize> {
        // Find the best (lowest) score among all regions
        parsed
            .regions
            .iter()
            .filter_map(|region| {
                self.region_priority
                    .iter()
                    .position(|r| r.eq_ignore_ascii_case(region))
            })
            .min()
    }

    /// Compare two ROMs and return which is preferred
    /// Returns Ordering::Less if `a` is preferred, Greater if `b` is preferred
    fn compare(&self, a: &ParsedGameName, b: &ParsedGameName) -> std::cmp::Ordering {
        use std::cmp::Ordering;

        // First: prefer verified dumps
        if self.prefer_verified {
            match (a.verified, b.verified) {
                (true, false) => return Ordering::Less,
                (false, true) => return Ordering::Greater,
                _ => {}
            }
        }

        // Second: prefer better region
        let a_region = self.region_score(a);
        let b_region = self.region_score(b);
        match (a_region, b_region) {
            (Some(a_score), Some(b_score)) => {
                if a_score != b_score {
                    return a_score.cmp(&b_score);
                }
            }
            (Some(_), None) => return Ordering::Less,
            (None, Some(_)) => return Ordering::Greater,
            (None, None) => {}
        }

        // Third: prefer non-revised versions (Rev A < Rev B < no revision)
        // Actually prefer base version over revisions
        match (&a.revision, &b.revision) {
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            _ => {}
        }

        // Equal preference
        Ordering::Equal
    }
}

/// A ROM candidate for selection
#[derive(Debug, Clone)]
pub struct RomCandidate<'a> {
    /// Original ROM name
    pub name: &'a str,
    /// Parsed name information
    pub parsed: ParsedGameName,
    /// Is this a parent ROM (for MAME-style DATs)
    pub is_parent: bool,
}

impl<'a> RomCandidate<'a> {
    /// Create a new candidate from a ROM name
    pub fn new(name: &'a str) -> Self {
        Self {
            name,
            parsed: super::parse_game_name(name),
            is_parent: false,
        }
    }

    /// Create a candidate with parent status
    pub fn with_parent_status(name: &'a str, is_parent: bool) -> Self {
        Self {
            name,
            parsed: super::parse_game_name(name),
            is_parent,
        }
    }
}

/// Select the preferred ROM from a group of candidates
///
/// Returns the name of the preferred ROM, or None if all candidates
/// were excluded by the preferences.
pub fn select_preferred<'a>(
    candidates: &[RomCandidate<'a>],
    prefs: &FilterPreferences,
) -> Option<&'a str> {
    // Filter out excluded ROMs
    let valid: Vec<_> = candidates
        .iter()
        .filter(|c| !prefs.should_exclude(&c.parsed))
        .collect();

    if valid.is_empty() {
        return None;
    }

    // If prefer_parent is set and we have a parent, prefer it
    if prefs.prefer_parent
        && let Some(parent) = valid.iter().find(|c| c.is_parent)
    {
        return Some(parent.name);
    }

    // Find the best candidate by comparing preferences
    let best = valid
        .iter()
        .min_by(|a, b| prefs.compare(&a.parsed, &b.parsed));

    best.map(|c| c.name)
}

/// Group ROMs by their base title for 1G1R processing
///
/// Returns a map of base title -> list of ROM names with that title
pub fn group_by_title<'a>(names: &[&'a str]) -> std::collections::HashMap<String, Vec<&'a str>> {
    use std::collections::HashMap;

    let mut groups: HashMap<String, Vec<&'a str>> = HashMap::new();

    for name in names {
        let parsed = super::parse_game_name(name);
        groups.entry(parsed.title).or_default().push(name);
    }

    groups
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_preferences() {
        let prefs = FilterPreferences::default();
        assert!(prefs.exclude_modified);
        assert!(prefs.exclude_bad_dumps);
        assert!(!prefs.exclude_prereleases);
        assert!(prefs.prefer_verified);
    }

    #[test]
    fn test_should_exclude_cracked() {
        let prefs = FilterPreferences::default();
        let parsed = super::super::parse_game_name("Game (1990)(Publisher)(US)[cr Razor]");
        assert!(prefs.should_exclude(&parsed));
    }

    #[test]
    fn test_should_exclude_bad_dump() {
        let prefs = FilterPreferences::default();
        let parsed = super::super::parse_game_name("Game (USA)[b]");
        assert!(prefs.should_exclude(&parsed));
    }

    #[test]
    fn test_should_not_exclude_clean() {
        let prefs = FilterPreferences::default();
        let parsed = super::super::parse_game_name("Game (USA)");
        assert!(!prefs.should_exclude(&parsed));
    }

    #[test]
    fn test_region_priority() {
        let prefs = FilterPreferences::default();

        let usa = super::super::parse_game_name("Game (USA)");
        let europe = super::super::parse_game_name("Game (Europe)");
        let japan = super::super::parse_game_name("Game (Japan)");

        // USA should score better than Europe
        assert!(prefs.region_score(&usa) < prefs.region_score(&europe));
        // Europe should score better than Japan
        assert!(prefs.region_score(&europe) < prefs.region_score(&japan));
    }

    #[test]
    fn test_world_preferred() {
        let prefs = FilterPreferences::default();

        let world = super::super::parse_game_name("Game (World)");
        let usa = super::super::parse_game_name("Game (USA)");

        // World is the most preferred
        assert!(prefs.region_score(&world) < prefs.region_score(&usa));
    }

    #[test]
    fn test_select_preferred_simple() {
        let prefs = FilterPreferences::default();

        let candidates = vec![
            RomCandidate::new("Game (Europe)"),
            RomCandidate::new("Game (USA)"),
            RomCandidate::new("Game (Japan)"),
        ];

        let selected = select_preferred(&candidates, &prefs);
        assert_eq!(selected, Some("Game (USA)"));
    }

    #[test]
    fn test_select_preferred_excludes_cracks() {
        let prefs = FilterPreferences::default();

        let candidates = vec![
            RomCandidate::new("Game (USA)[cr PDX]"),
            RomCandidate::new("Game (Europe)"),
        ];

        // Should pick Europe because USA version is cracked
        let selected = select_preferred(&candidates, &prefs);
        assert_eq!(selected, Some("Game (Europe)"));
    }

    #[test]
    fn test_select_preferred_verified() {
        let prefs = FilterPreferences::default();

        let candidates = vec![
            RomCandidate::new("Game (USA)"),
            RomCandidate::new("Game (USA) [!]"),
        ];

        // Should pick verified version
        let selected = select_preferred(&candidates, &prefs);
        assert_eq!(selected, Some("Game (USA) [!]"));
    }

    #[test]
    fn test_select_preferred_all_excluded() {
        let prefs = FilterPreferences::default();

        let candidates = vec![
            RomCandidate::new("Game (USA)[cr]"),
            RomCandidate::new("Game (USA)[t]"),
            RomCandidate::new("Game (USA)[b]"),
        ];

        // All are excluded
        let selected = select_preferred(&candidates, &prefs);
        assert!(selected.is_none());
    }

    #[test]
    fn test_select_preferred_parent() {
        let prefs = FilterPreferences {
            prefer_parent: true,
            ..Default::default()
        };

        let candidates = vec![
            RomCandidate::with_parent_status("Game (USA)", true),
            RomCandidate::with_parent_status("Game (World)", false),
        ];

        // Should prefer parent even though World ranks higher
        let selected = select_preferred(&candidates, &prefs);
        assert_eq!(selected, Some("Game (USA)"));
    }

    #[test]
    fn test_group_by_title() {
        let names = vec![
            "Super Mario Bros (USA)",
            "Super Mario Bros (Europe)",
            "Tetris (USA)",
            "Tetris (Japan)",
        ];

        let groups = group_by_title(&names);

        assert_eq!(groups.len(), 2);
        assert_eq!(groups.get("Super Mario Bros").unwrap().len(), 2);
        assert_eq!(groups.get("Tetris").unwrap().len(), 2);
    }

    #[test]
    fn test_custom_region_priority() {
        let prefs = FilterPreferences::with_regions(vec![
            "Japan".to_string(),
            "USA".to_string(),
            "Europe".to_string(),
        ]);

        let candidates = vec![
            RomCandidate::new("Game (USA)"),
            RomCandidate::new("Game (Japan)"),
        ];

        // Japan should be preferred with custom priority
        let selected = select_preferred(&candidates, &prefs);
        assert_eq!(selected, Some("Game (Japan)"));
    }

    #[test]
    fn test_prefer_base_over_revision() {
        let prefs = FilterPreferences::default();

        let candidates = vec![
            RomCandidate::new("Game (USA) (Rev A)"),
            RomCandidate::new("Game (USA)"),
        ];

        // Should prefer base version over revision
        let selected = select_preferred(&candidates, &prefs);
        assert_eq!(selected, Some("Game (USA)"));
    }
}
