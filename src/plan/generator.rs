//! Plan generation logic

use anyhow::{Context, Result};
use rusqlite::Connection;
use sha2::{Digest as Sha2Digest, Sha256};
use std::collections::HashMap;
use std::path::Path;

use super::{Plan, SourceRef};
use crate::db::{collections, config as db_config};
use crate::filter::{RomCandidate, parse_game_name, select_preferred};

/// A matched ROM ready for planning
#[derive(Debug, Clone)]
pub struct MatchedRom {
    /// Collection name
    pub collection: String,
    /// Game name
    pub game_name: String,
    /// ROM name (filename within game folder)
    pub rom_name: String,
    /// SHA1 hash
    pub sha1: String,
    /// File size
    pub size: i64,
    /// Source file location
    pub source_path: String,
    /// Source directory root
    pub source_root: String,
    /// Archive path (None for loose files)
    pub archive_path: Option<String>,
}

/// Generate a plan for all configured collections
///
/// If `dat_filter` is provided, only collections matching the glob pattern
/// will be included in the plan.
pub fn generate_plan(conn: &Connection) -> Result<Plan> {
    generate_plan_filtered(conn, None)
}

/// Generate a plan with optional collection name filtering
///
/// The filter supports glob patterns:
/// - `*` matches any sequence of characters
/// - `?` matches any single character
/// - Case-insensitive matching
pub fn generate_plan_filtered(conn: &Connection, dat_filter: Option<&str>) -> Result<Plan> {
    // Calculate state hash
    let state_hash = compute_state_hash(conn)?;
    let mut plan = Plan::new(state_hash);

    // Get all collections with configured dest_path
    let configs = db_config::list_all_configs(conn)?;

    if configs.is_empty() {
        println!("No collections configured with destination paths.");
        println!();
        println!("Configure a destination with:");
        println!("  cat198x config set <collection> dest_path <path>");
        return Ok(plan);
    }

    // Filter configs by pattern if provided
    let configs: Vec<_> = if let Some(pattern) = dat_filter {
        configs
            .into_iter()
            .filter(|cfg| glob_match(pattern, &cfg.path_pattern))
            .collect()
    } else {
        configs
    };

    if configs.is_empty() {
        if let Some(pattern) = dat_filter {
            println!("No collections match the filter: {}", pattern);
        }
        return Ok(plan);
    }

    // Process each configured collection
    for cfg in &configs {
        let dest_path = match &cfg.dest_path {
            Some(p) => p,
            None => continue, // Skip collections without dest_path
        };

        // Find the collection
        let collection = match collections::get_collection_by_name(conn, &cfg.path_pattern)? {
            Some(c) => c,
            None => {
                println!(
                    "Warning: Config exists for '{}' but no matching collection found",
                    cfg.path_pattern
                );
                continue;
            }
        };

        // Get active version
        let version = match collections::get_active_version(conn, collection.id)? {
            Some(v) => v,
            None => {
                println!(
                    "Warning: No active version for collection '{}'",
                    collection.name
                );
                continue;
            }
        };

        println!("Planning for: {} (v{})", collection.name, version.version);

        // Find all matched ROMs for this version
        let matches = find_matched_roms(conn, version.id, &collection.name)?;

        // Apply 1G1R filtering if enabled
        let matches = if let Some(extra) = &cfg.extra_config {
            if extra.one_g_one_r {
                let prefs = extra.to_filter_preferences();
                let filtered = apply_one_g_one_r_filter(&matches, &prefs);
                let original_count = matches.len();
                let filtered_count = filtered.len();
                if filtered_count < original_count {
                    println!(
                        "  1G1R: {} -> {} ROMs (filtered {} variants)",
                        original_count,
                        filtered_count,
                        original_count - filtered_count
                    );
                }
                filtered
            } else {
                matches
            }
        } else {
            matches
        };

        let mut already_correct = 0;
        let mut copy_count = 0;

        for m in matches {
            // Build destination path: dest_path/game_name/rom_name
            // For single-ROM games, just use: dest_path/rom_name
            let dest = build_dest_path(dest_path, &m.game_name, &m.rom_name);

            // Check if file already exists at destination with correct hash
            if is_file_correct_at_dest(&dest, &m.sha1)? {
                already_correct += 1;
                continue;
            }

            // Build full source path
            let full_source = format!("{}/{}", m.source_root, m.source_path);

            plan.add_copy(
                SourceRef {
                    path: full_source,
                    archive_path: m.archive_path,
                    sha1: m.sha1,
                },
                dest,
                m.size as u64,
            );
            copy_count += 1;
        }

        plan.summary.already_correct += already_correct;

        println!(
            "  {} already correct, {} to copy",
            already_correct, copy_count
        );
    }

    Ok(plan)
}

/// Find all ROMs that have matching files in sources
fn find_matched_roms(
    conn: &Connection,
    version_id: i64,
    collection_name: &str,
) -> Result<Vec<MatchedRom>> {
    // Match each DAT ROM to a file we hold. A DAT SHA1 may be the headered or
    // headerless form, so match either of the file's hashes; a SHA1-less DAT
    // entry matches on CRC + size. We select the *file's* sha1 and size (not
    // the DAT's), because that's the true content placed at the destination and
    // what `is_file_correct_at_dest` re-hashes to verify.
    let mut stmt = conn.prepare(
        "SELECT
            g.name as game_name,
            r.name as rom_name,
            f.sha1,
            f.size,
            fl.path as source_path,
            s.path as source_root,
            fl.archive_path
         FROM dat_roms r
         JOIN dat_games g ON r.game_id = g.id
         JOIN dat_nodes n ON g.node_id = n.id
         JOIN files f ON
                (r.sha1 IS NOT NULL AND (f.sha1 = r.sha1 OR f.sha1_no_header = r.sha1))
             OR (r.sha1 IS NULL AND r.crc32 IS NOT NULL AND f.crc32 = r.crc32 AND f.size = r.size)
         JOIN file_locations fl ON f.sha1 = fl.sha1
         JOIN sources s ON fl.source_id = s.id
         WHERE n.version_id = ?
           AND r.status != 'nodump'
         ORDER BY g.name, r.name",
    )?;

    let matches = stmt
        .query_map([version_id], |row| {
            Ok(MatchedRom {
                collection: collection_name.to_string(),
                game_name: row.get(0)?,
                rom_name: row.get(1)?,
                sha1: row.get(2)?,
                size: row.get(3)?,
                source_path: row.get(4)?,
                source_root: row.get(5)?,
                archive_path: row.get(6)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(matches)
}

/// Build destination path for a ROM
fn build_dest_path(dest_root: &str, _game_name: &str, rom_name: &str) -> String {
    // For now, use flat structure: dest_root/rom_name
    // Phase 2 will add proper game folder structure based on output_format
    format!("{}/{}", dest_root.trim_end_matches('/'), rom_name)
}

/// Check if a file at the destination already has the correct hash
fn is_file_correct_at_dest(path: &str, expected_sha1: &str) -> Result<bool> {
    use sha1::Digest as Sha1Digest;

    let path = Path::new(path);
    if !path.exists() {
        return Ok(false);
    }

    // Hash the file and compare
    let contents = std::fs::read(path).context("Failed to read destination file")?;
    let mut hasher = sha1::Sha1::new();
    Sha1Digest::update(&mut hasher, &contents);
    let hash = Sha1Digest::finalize(hasher);
    let actual_sha1 = crate::util::hex_upper(hash);

    Ok(actual_sha1.eq_ignore_ascii_case(expected_sha1))
}

/// Compute state hash for plan validation
pub fn compute_state_hash(conn: &Connection) -> Result<String> {
    let mut hasher = Sha256::new();

    // 1. Active version IDs (sorted)
    let mut active_ids: Vec<i64> = Vec::new();
    let colls = collections::list_collections(conn)?;
    for coll in &colls {
        if let Some(ver) = collections::get_active_version(conn, coll.id)? {
            active_ids.push(ver.id);
        }
    }
    active_ids.sort();
    for id in &active_ids {
        hasher.update(id.to_le_bytes());
    }

    // 2. File catalog fingerprint (row count + max last_seen)
    let (file_count, max_last_seen): (i64, Option<String>) = conn.query_row(
        "SELECT COUNT(*), MAX(last_seen) FROM file_locations",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    hasher.update(file_count.to_le_bytes());
    if let Some(ts) = max_last_seen {
        hasher.update(ts.as_bytes());
    }

    // 3. Destination config hash
    let configs = db_config::list_all_configs(conn)?;
    for cfg in &configs {
        hasher.update(cfg.path_pattern.as_bytes());
        if let Some(ref dest) = cfg.dest_path {
            hasher.update(dest.as_bytes());
        }
    }

    let result = hasher.finalize();
    Ok(crate::util::hex_lower(result))
}

/// Simple glob pattern matching (case-insensitive)
///
/// Supports:
/// - `*` matches any sequence of characters (including empty)
/// - `?` matches exactly one character
fn glob_match(pattern: &str, text: &str) -> bool {
    glob_match_impl(
        pattern.to_lowercase().as_bytes(),
        text.to_lowercase().as_bytes(),
    )
}

fn glob_match_impl(pattern: &[u8], text: &[u8]) -> bool {
    let mut p = 0;
    let mut t = 0;
    let mut star_p = None;
    let mut star_t = 0;

    while t < text.len() {
        if p < pattern.len() && (pattern[p] == b'?' || pattern[p] == text[t]) {
            // Match single character or ?
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == b'*' {
            // Match * - remember position for backtracking
            star_p = Some(p);
            star_t = t;
            p += 1;
        } else if let Some(sp) = star_p {
            // Backtrack: * matches one more character
            p = sp + 1;
            star_t += 1;
            t = star_t;
        } else {
            return false;
        }
    }

    // Check remaining pattern is all *
    while p < pattern.len() && pattern[p] == b'*' {
        p += 1;
    }

    p == pattern.len()
}

/// Count missing ROMs (ROMs in DAT but not in file catalog)
pub fn count_missing_roms(conn: &Connection, version_id: i64) -> Result<i64> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM dat_roms r
         JOIN dat_games g ON r.game_id = g.id
         JOIN dat_nodes n ON g.node_id = n.id
         WHERE n.version_id = ?
           AND r.status != 'nodump'
           AND (r.sha1 IS NOT NULL OR r.crc32 IS NOT NULL)
           AND NOT EXISTS (
               SELECT 1 FROM files f
               WHERE (r.sha1 IS NOT NULL AND (f.sha1 = r.sha1 OR f.sha1_no_header = r.sha1))
                  OR (r.sha1 IS NULL AND f.crc32 = r.crc32 AND f.size = r.size)
           )",
        [version_id],
        |row| row.get(0),
    )?;
    Ok(count)
}

/// Apply 1G1R filtering to a list of matched ROMs
///
/// Groups ROMs by their base title (extracted from game_name) and selects
/// the preferred variant based on region priority and dump quality.
fn apply_one_g_one_r_filter(
    matches: &[MatchedRom],
    prefs: &crate::filter::FilterPreferences,
) -> Vec<MatchedRom> {
    // Group matches by parsed title
    let mut groups: HashMap<String, Vec<&MatchedRom>> = HashMap::new();

    for m in matches {
        let parsed = parse_game_name(&m.game_name);
        groups.entry(parsed.title).or_default().push(m);
    }

    // Select best from each group
    let mut result = Vec::new();

    for (_title, group) in groups {
        if group.len() == 1 {
            // Only one variant, keep it (if not excluded)
            let m = group[0];
            let parsed = parse_game_name(&m.game_name);
            if !prefs.should_exclude(&parsed) {
                result.push(m.clone());
            }
        } else {
            // Multiple variants - select the preferred one
            let candidates: Vec<_> = group
                .iter()
                .map(|m| RomCandidate::new(&m.game_name))
                .collect();

            if let Some(preferred_name) = select_preferred(&candidates, prefs) {
                // Find and clone the matching ROM
                if let Some(m) = group.iter().find(|m| m.game_name == preferred_name) {
                    result.push((*m).clone());
                }
            }
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    fn setup_db() -> Database {
        Database::open_in_memory().unwrap()
    }

    #[test]
    fn test_compute_state_hash_empty() {
        let db = setup_db();
        let conn = db.conn();

        let hash = compute_state_hash(conn).unwrap();
        assert!(!hash.is_empty());
        assert_eq!(hash.len(), 64); // SHA256 hex = 64 chars
    }

    #[test]
    fn test_compute_state_hash_deterministic() {
        let db = setup_db();
        let conn = db.conn();

        let hash1 = compute_state_hash(conn).unwrap();
        let hash2 = compute_state_hash(conn).unwrap();
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_build_dest_path() {
        assert_eq!(
            build_dest_path("/roms/nes", "Super Mario Bros", "mario.nes"),
            "/roms/nes/mario.nes"
        );

        assert_eq!(
            build_dest_path("/roms/nes/", "Game", "game.rom"),
            "/roms/nes/game.rom"
        );
    }

    #[test]
    fn test_generate_plan_no_config() {
        let db = setup_db();
        let conn = db.conn();

        let plan = generate_plan(conn).unwrap();
        assert!(plan.is_empty());
    }

    #[test]
    fn test_glob_match_exact() {
        assert!(glob_match("MAME", "MAME"));
        assert!(glob_match("mame", "MAME")); // case insensitive
        assert!(!glob_match("MAME", "MAME 2020"));
    }

    #[test]
    fn test_glob_match_star() {
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
    fn test_glob_match_question() {
        // ? matches exactly one character
        assert!(glob_match("MAME 202?", "MAME 2020"));
        assert!(glob_match("MAME 202?", "MAME 2024"));
        assert!(!glob_match("MAME 202?", "MAME 20"));
        assert!(!glob_match("MAME 202?", "MAME 20245"));
    }

    #[test]
    fn test_glob_match_complex() {
        assert!(glob_match("*NES*", "Nintendo - NES"));
        assert!(glob_match("*NES*", "NES"));
        assert!(glob_match("*-*", "Nintendo - NES"));
        assert!(glob_match("Nintendo - *", "Nintendo - Game Boy"));
        assert!(glob_match("???", "NES"));
        assert!(!glob_match("???", "SNES"));
    }

    #[test]
    fn test_glob_match_empty() {
        assert!(glob_match("", ""));
        assert!(!glob_match("", "text"));
        assert!(glob_match("*", ""));
    }

    fn make_test_rom(game_name: &str) -> MatchedRom {
        MatchedRom {
            collection: "Test".to_string(),
            game_name: game_name.to_string(),
            rom_name: format!("{}.rom", game_name),
            sha1: "abc123".to_string(),
            size: 1024,
            source_path: "/source/test.rom".to_string(),
            source_root: "/source".to_string(),
            archive_path: None,
        }
    }

    #[test]
    fn test_one_g_one_r_selects_usa_over_europe() {
        use crate::filter::FilterPreferences;

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
    fn test_one_g_one_r_excludes_cracks() {
        use crate::filter::FilterPreferences;

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
    fn test_one_g_one_r_excludes_bad_dumps() {
        use crate::filter::FilterPreferences;

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
    fn test_one_g_one_r_different_games_not_merged() {
        use crate::filter::FilterPreferences;

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
    fn test_one_g_one_r_custom_region_priority() {
        use crate::filter::FilterPreferences;

        let matches = vec![make_test_rom("Game (USA)"), make_test_rom("Game (Japan)")];

        // Prefer Japan over USA
        let prefs = FilterPreferences::with_regions(vec!["Japan".to_string(), "USA".to_string()]);
        let filtered = apply_one_g_one_r_filter(&matches, &prefs);

        assert_eq!(filtered.len(), 1);
        assert!(filtered[0].game_name.contains("Japan"));
    }
}
