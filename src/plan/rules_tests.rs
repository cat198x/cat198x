use super::*;
use crate::filter::FilterPreferences;

#[test]
fn resolve_output_format_prefers_explicit() {
    assert_eq!(
        resolve_output_format(Some("zip"), OutputFormat::Loose),
        OutputFormat::Zip
    );
    assert_eq!(
        resolve_output_format(Some("TorrentZip"), OutputFormat::Loose),
        OutputFormat::TorrentZip
    );
    assert_eq!(
        resolve_output_format(Some("loose"), OutputFormat::Zip),
        OutputFormat::Loose
    );
}

#[test]
fn resolve_output_format_falls_back_to_default() {
    assert_eq!(
        resolve_output_format(None, OutputFormat::TorrentZip),
        OutputFormat::TorrentZip
    );
    // Unrecognised value falls back rather than failing the plan.
    assert_eq!(
        resolve_output_format(Some("rar"), OutputFormat::Zip),
        OutputFormat::Zip
    );
}

#[test]
fn archive_format_tag_maps_formats() {
    assert_eq!(archive_format_tag(OutputFormat::Loose), None);
    assert_eq!(archive_format_tag(OutputFormat::Zip), Some("zip"));
    assert_eq!(
        archive_format_tag(OutputFormat::TorrentZip),
        Some("torrentzip")
    );
    assert_eq!(archive_format_tag(OutputFormat::SevenZip), Some("7z"));
}

#[test]
fn resolve_output_format_and_extension_handle_7z() {
    assert_eq!(
        resolve_output_format(Some("7z"), OutputFormat::Loose),
        OutputFormat::SevenZip
    );
    assert_eq!(archive_extension("7z"), "7z");
    assert_eq!(archive_extension("zip"), "zip");
    assert_eq!(archive_extension("torrentzip"), "zip");
}

#[test]
fn resolve_merge_mode_prefers_explicit_then_default() {
    // The kebab-case strings match the MergeMode serde representation.
    assert_eq!(
        resolve_merge_mode(Some("split"), MergeMode::NonMerged),
        MergeMode::Split
    );
    assert_eq!(
        resolve_merge_mode(Some("merged"), MergeMode::NonMerged),
        MergeMode::Merged
    );
    assert_eq!(
        resolve_merge_mode(Some("non-merged"), MergeMode::Split),
        MergeMode::NonMerged
    );
    // Case-insensitive.
    assert_eq!(
        resolve_merge_mode(Some("Split"), MergeMode::NonMerged),
        MergeMode::Split
    );
    // Absent or unrecognised falls back to the default rather than failing.
    assert_eq!(resolve_merge_mode(None, MergeMode::Split), MergeMode::Split);
    assert_eq!(
        resolve_merge_mode(Some("clone"), MergeMode::NonMerged),
        MergeMode::NonMerged
    );
}

#[test]
fn glob_match_exact() {
    assert!(glob_match("MAME", "MAME"));
    assert!(glob_match("mame", "MAME")); // case insensitive
    assert!(!glob_match("MAME", "MAME 2020"));
}

#[test]
fn glob_match_star() {
    // * matches any sequence
    assert!(glob_match("MAME*", "MAME"));
    assert!(glob_match("MAME*", "MAME 2020"));
    assert!(glob_match("*MAME*", "FBNeo MAME 2020"));
    assert!(glob_match("*", "anything"));
    assert!(glob_match("Nintendo*", "Nintendo - NES"));
    assert!(glob_match("Nintendo*", "Nintendo - SNES"));
    assert!(!glob_match("Nintendo*", "Sega - Genesis"));
}

#[test]
fn glob_match_question() {
    // ? matches exactly one character
    assert!(glob_match("MAME 202?", "MAME 2020"));
    assert!(glob_match("MAME 202?", "MAME 2024"));
    assert!(!glob_match("MAME 202?", "MAME 20"));
    assert!(!glob_match("MAME 202?", "MAME 20245"));
}

#[test]
fn glob_match_complex() {
    assert!(glob_match("*NES*", "Nintendo - NES"));
    assert!(glob_match("*NES*", "NES"));
    assert!(glob_match("*-*", "Nintendo - NES"));
    assert!(glob_match("Nintendo - *", "Nintendo - Game Boy"));
    assert!(glob_match("???", "NES"));
    assert!(!glob_match("???", "SNES"));
}

#[test]
fn glob_match_empty() {
    assert!(glob_match("", ""));
    assert!(!glob_match("", "text"));
    assert!(glob_match("*", ""));
}

fn make_test_rom(game_name: &str) -> MatchedRom {
    MatchedRom {
        game_name: game_name.to_string(),
        rom_name: format!("{game_name}.rom"),
        sha1: "abc123".to_string(),
        size: 1024,
        source_path: "/source/test.rom".to_string(),
        source_root: "/source".to_string(),
        archive_path: None,
        is_disk: false,
    }
}

#[test]
fn one_g_one_r_selects_usa_over_europe() {
    let matches = vec![
        make_test_rom("Super Mario Bros (Europe)"),
        make_test_rom("Super Mario Bros (USA)"),
        make_test_rom("Super Mario Bros (Japan)"),
    ];

    let prefs = FilterPreferences::default();
    let filtered = apply_one_g_one_r_filter(&matches, &prefs);

    assert_eq!(filtered.len(), 1);
    assert!(filtered[0].game_name.contains("USA"));
}

#[test]
fn one_g_one_r_excludes_cracks() {
    let matches = vec![
        make_test_rom("Game (USA)[cr PDX]"),
        make_test_rom("Game (Europe)"),
    ];

    let prefs = FilterPreferences::default();
    let filtered = apply_one_g_one_r_filter(&matches, &prefs);

    assert_eq!(filtered.len(), 1);
    assert!(filtered[0].game_name.contains("Europe"));
}

#[test]
fn one_g_one_r_excludes_bad_dumps() {
    let matches = vec![
        make_test_rom("Game (USA)[b]"),
        make_test_rom("Game (Japan)"),
    ];

    let prefs = FilterPreferences::default();
    let filtered = apply_one_g_one_r_filter(&matches, &prefs);

    assert_eq!(filtered.len(), 1);
    assert!(filtered[0].game_name.contains("Japan"));
}

#[test]
fn one_g_one_r_different_games_not_merged() {
    let matches = vec![
        make_test_rom("Super Mario Bros (USA)"),
        make_test_rom("Tetris (USA)"),
    ];

    let prefs = FilterPreferences::default();
    let filtered = apply_one_g_one_r_filter(&matches, &prefs);

    // Both games should remain (different titles)
    assert_eq!(filtered.len(), 2);
}

#[test]
fn one_g_one_r_custom_region_priority() {
    let matches = vec![make_test_rom("Game (USA)"), make_test_rom("Game (Japan)")];

    // Prefer Japan over USA
    let prefs = FilterPreferences::with_regions(vec!["Japan".to_string(), "USA".to_string()]);
    let filtered = apply_one_g_one_r_filter(&matches, &prefs);

    assert_eq!(filtered.len(), 1);
    assert!(filtered[0].game_name.contains("Japan"));
}
