//! Plan generation logic

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};
use sha2::{Digest as Sha2Digest, Sha256};
use std::collections::HashMap;
use std::path::Path;

use super::{CollectionPlanStat, Plan, SourceRef};
use crate::config::OutputFormat;
use crate::db::{collections, config as db_config, dats};
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
    generate_plan_filtered(conn, None, None, OutputFormat::Loose)
}

/// Generate a plan with optional collection name filtering
///
/// The filter supports glob patterns:
/// - `*` matches any sequence of characters
/// - `?` matches any single character
/// - Case-insensitive matching
pub fn generate_plan_filtered(
    conn: &Connection,
    dat_filter: Option<&str>,
    default_dest: Option<&str>,
    default_format: OutputFormat,
) -> Result<Plan> {
    // Calculate state hash
    let state_hash = compute_state_hash(conn)?;
    let mut plan = Plan::new(state_hash);

    // Plan every collection, not only those with an explicit dest_path: a
    // library-wide `default_dest_path` should reach collections that were never
    // individually configured. Each collection's destination is resolved below.
    let all_collections = collections::list_collections(conn)?;

    let mut planned_any = false;
    let mut filter_matched_any = false;
    let mut skipped_no_dest: Vec<String> = Vec::new();

    for collection in &all_collections {
        if let Some(pattern) = dat_filter
            && !glob_match(pattern, &collection.name)
        {
            continue;
        }
        filter_matched_any = true;

        // Only collections with an active version can be planned.
        let version = match collections::get_active_version(conn, collection.id)? {
            Some(v) => v,
            None => continue,
        };

        let cfg = db_config::get_collection_config(conn, &collection.name)?;

        // The collection's library path (set by recursive `dat add`), used when
        // falling back to the library-wide default destination.
        let hierarchy =
            dats::primary_node_path(conn, version.id)?.unwrap_or_else(|| collection.name.clone());
        let explicit = cfg.as_ref().and_then(|c| c.dest_path.as_deref());

        let dest_root = match resolve_dest_root(explicit, default_dest, &hierarchy) {
            Some(root) => root,
            None => {
                // No destination resolved — recorded and reported, never silent.
                skipped_no_dest.push(collection.name.clone());
                continue;
            }
        };

        planned_any = true;
        println!("Planning for: {} (v{})", collection.name, version.version);

        // Find all matched ROMs for this version
        let matches = find_matched_roms(conn, version.id, &collection.name)?;

        // Apply 1G1R filtering if enabled for this collection.
        let matches = match cfg.as_ref().and_then(|c| c.extra_config.as_ref()) {
            Some(extra) if extra.one_g_one_r => {
                let prefs = extra.to_filter_preferences();
                let original_count = matches.len();
                let filtered = apply_one_g_one_r_filter(&matches, &prefs);
                if filtered.len() < original_count {
                    println!(
                        "  1G1R: {} -> {} ROMs (filtered {} variants)",
                        original_count,
                        filtered.len(),
                        original_count - filtered.len()
                    );
                }
                filtered
            }
            _ => matches,
        };

        // The effective output format: per-collection setting, else the
        // library-wide default. Loose copies each ROM into place; zip/torrentzip
        // packs each game into one archive.
        let format = resolve_output_format(
            cfg.as_ref().and_then(|c| c.output_format.as_deref()),
            default_format,
        );

        let mut already_correct = 0;
        let mut to_write = 0;
        let mut bytes = 0u64;

        match archive_format_tag(format) {
            None => {
                // LOOSE: one file per ROM. A single-ROM game stays flat
                // (dest/rom); a multi-ROM game gets a folder (dest/game/rom),
                // so count ROMs per game up front.
                let mut roms_per_game: HashMap<String, usize> = HashMap::new();
                for m in &matches {
                    *roms_per_game.entry(m.game_name.clone()).or_insert(0) += 1;
                }

                for m in matches {
                    let multi_rom = roms_per_game.get(&m.game_name).copied().unwrap_or(1) > 1;
                    let dest = build_dest_path(&dest_root, &m.game_name, &m.rom_name, multi_rom);

                    if is_file_correct_at_dest(conn, &dest, &m.sha1)? {
                        already_correct += 1;
                        continue;
                    }

                    let full_source = format!("{}/{}", m.source_root, m.source_path);
                    bytes += m.size as u64;
                    plan.add_copy(
                        SourceRef {
                            path: full_source,
                            archive_path: m.archive_path,
                            sha1: m.sha1,
                            entry_name: None,
                        },
                        dest,
                        m.size as u64,
                    );
                    to_write += 1;
                }
                println!(
                    "  {} already correct, {} to copy",
                    already_correct, to_write
                );
            }
            Some(tag) => {
                // ARCHIVE: one archive per game, named <dest_root>/<game>.zip,
                // with entries carrying canonical ROM names.
                let games = group_for_archive(matches);

                for game in games {
                    let dest = format!("{}/{}.zip", dest_root.trim_end_matches('/'), game.name);

                    if is_archive_correct_at_dest(&dest, &game.expected, tag)? {
                        already_correct += game.expected.len();
                        continue;
                    }

                    bytes += game.size;
                    plan.add_repack(game.sources, dest, tag.to_string(), game.size);
                    to_write += 1;
                }
                println!(
                    "  {} ROMs already archived, {} archive(s) to build",
                    already_correct, to_write
                );
            }
        }

        plan.summary.already_correct += already_correct;
        plan.per_collection.push(CollectionPlanStat {
            name: collection.name.clone(),
            node_path: hierarchy,
            to_write,
            already_correct,
            bytes,
        });
    }

    // Never skip silently: report collections left out because no destination
    // could be resolved, and how to include them. The full list rides on the
    // plan so the caller can write it out for review.
    if !skipped_no_dest.is_empty() {
        println!();
        println!(
            "{} collection(s) skipped — no destination resolved.",
            skipped_no_dest.len()
        );
        println!("  Set one per collection:  cat198x config set <collection> dest_path <path>");
        println!("  or library-wide:         cat198x config set-default dest_path <path>");
    }

    if let Some(pattern) = dat_filter
        && !filter_matched_any
    {
        println!("No collections match the filter: {}", pattern);
    } else if !planned_any && skipped_no_dest.is_empty() {
        println!("No collections with an active version to plan.");
    }

    plan.skipped_no_dest = skipped_no_dest;
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
/// The effective output format: an explicit per-collection setting wins,
/// otherwise the library-wide default. An unrecognised string falls back to the
/// default rather than failing the whole plan.
fn resolve_output_format(explicit: Option<&str>, default: OutputFormat) -> OutputFormat {
    match explicit.map(str::to_ascii_lowercase).as_deref() {
        Some("loose") => OutputFormat::Loose,
        Some("zip") => OutputFormat::Zip,
        Some("torrentzip") => OutputFormat::TorrentZip,
        _ => default,
    }
}

/// The repack format tag for an archive format, or `None` for loose (which is
/// copied, not repacked).
fn archive_format_tag(format: OutputFormat) -> Option<&'static str> {
    match format {
        OutputFormat::Loose => None,
        OutputFormat::Zip => Some("zip"),
        OutputFormat::TorrentZip => Some("torrentzip"),
    }
}

/// A game's ROMs gathered for a single archive.
struct ArchiveGame {
    name: String,
    sources: Vec<SourceRef>,
    /// (entry name, sha1) pairs, used to check an existing archive is correct.
    expected: Vec<(String, String)>,
    size: u64,
}

/// Group matched ROMs by game for archive output — one [`ArchiveGame`] per game,
/// sorted by name for stable plans. Each source carries its canonical ROM name
/// as the archive entry name.
fn group_for_archive(matches: Vec<MatchedRom>) -> Vec<ArchiveGame> {
    use std::collections::BTreeMap;

    let mut games: BTreeMap<String, ArchiveGame> = BTreeMap::new();
    for m in matches {
        let game = games
            .entry(m.game_name.clone())
            .or_insert_with(|| ArchiveGame {
                name: m.game_name.clone(),
                sources: Vec::new(),
                expected: Vec::new(),
                size: 0,
            });
        game.expected.push((m.rom_name.clone(), m.sha1.clone()));
        game.size += m.size as u64;
        game.sources.push(SourceRef {
            path: format!("{}/{}", m.source_root, m.source_path),
            archive_path: m.archive_path,
            sha1: m.sha1,
            entry_name: Some(m.rom_name),
        });
    }
    games.into_values().collect()
}

/// Whether the archive at `dest` already holds exactly the expected entries
/// (matching names and SHA1s) *and* is in the requested container format. A
/// missing, differing, or wrong-format archive returns `false`, so it is
/// (re)built; an exact match is left untouched, keeping re-runs no-ops.
///
/// For `torrentzip`, a content-correct plain ZIP is still rebuilt, because the
/// container format itself is part of "correct" (TorrentZIP determinism). For
/// `zip`, any content-correct ZIP — including a TorrentZIP — passes.
fn is_archive_correct_at_dest(
    dest: &str,
    expected: &[(String, String)],
    format: &str,
) -> Result<bool> {
    let path = Path::new(dest);
    if !path.exists() {
        return Ok(false);
    }

    let mut have: HashMap<String, String> = HashMap::new();
    for entry in crate::scanner::archive::hash_archive_entries(path)? {
        if let Some(hashes) = entry.hashes {
            have.insert(entry.name, hashes.sha1);
        }
    }

    if have.len() != expected.len() {
        return Ok(false);
    }
    let content_ok = expected
        .iter()
        .all(|(name, sha1)| have.get(name).is_some_and(|h| h.eq_ignore_ascii_case(sha1)));
    if !content_ok {
        return Ok(false);
    }

    // Container-format check: a TorrentZIP target must actually be TorrentZIP.
    if format == "torrentzip" && !crate::archive::is_torrentzip_stamped(path)? {
        return Ok(false);
    }

    Ok(true)
}

/// Resolve a collection's destination root, in order of precedence:
///   1. an explicit per-collection `dest_path`, used as-is;
///   2. otherwise the library-wide `default_dest` joined with the collection's
///      library path (`hierarchy`), so a whole set is tidied from one setting;
///   3. otherwise `None` — no destination, and the caller skips the collection.
fn resolve_dest_root(
    explicit: Option<&str>,
    default_dest: Option<&str>,
    hierarchy: &str,
) -> Option<String> {
    match explicit {
        Some(p) => Some(p.to_string()),
        None => default_dest.map(|base| format!("{}/{}", base.trim_end_matches('/'), hierarchy)),
    }
}

/// Build the on-disk destination for one ROM under its collection's root.
///
/// Loose layout: a single-ROM game is placed flat as `dest_root/rom_name` — the
/// common TOSEC case, where one "game" is one file and a wrapping folder would
/// just be noise. A multi-ROM game gets its own folder,
/// `dest_root/game_name/rom_name`, so its parts stay together and don't collide
/// with other games' files.
fn build_dest_path(dest_root: &str, game_name: &str, rom_name: &str, multi_rom: bool) -> String {
    let root = dest_root.trim_end_matches('/');
    if multi_rom {
        format!("{}/{}/{}", root, game_name, rom_name)
    } else {
        format!("{}/{}", root, rom_name)
    }
}

/// Check if a file at the destination already has the correct hash
fn is_file_correct_at_dest(conn: &Connection, path: &str, expected_sha1: &str) -> Result<bool> {
    use sha1::Digest as Sha1Digest;

    // Fast path: if the scan already indexed a loose file at this exact path
    // with the expected hash, trust the catalogue instead of re-reading it.
    // For an in-place tidy the destination *is* a scanned source file, so this
    // avoids re-hashing the whole library over the network — the scan just did.
    if catalogued_file_has_sha1(conn, path, expected_sha1)? {
        return Ok(true);
    }

    let fs_path = Path::new(path);
    if !fs_path.exists() {
        return Ok(false);
    }

    // Fall back to hashing on disk (destination not in the catalogue, or its
    // recorded hash differs — re-verify the real bytes rather than trust stale).
    let contents = std::fs::read(fs_path).context("Failed to read destination file")?;
    let mut hasher = sha1::Sha1::new();
    Sha1Digest::update(&mut hasher, &contents);
    let hash = Sha1Digest::finalize(hasher);
    let actual_sha1 = crate::util::hex_upper(hash);

    Ok(actual_sha1.eq_ignore_ascii_case(expected_sha1))
}

/// Whether the catalogue holds a loose file at the absolute path `abs_path`
/// whose recorded SHA1 matches `expected_sha1`.
fn catalogued_file_has_sha1(
    conn: &Connection,
    abs_path: &str,
    expected_sha1: &str,
) -> Result<bool> {
    let found: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM file_locations fl
             JOIN sources s ON fl.source_id = s.id
             WHERE fl.archive_path IS NULL
               AND (s.path || '/' || fl.path) = ?1
               AND fl.sha1 = ?2 COLLATE NOCASE
             LIMIT 1",
            rusqlite::params![abs_path, expected_sha1],
            |row| row.get(0),
        )
        .optional()?;
    Ok(found.is_some())
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
    fn test_build_dest_path_single_rom_is_flat() {
        // A single-ROM game is placed flat, with no redundant game folder.
        assert_eq!(
            build_dest_path("/roms/nes", "Super Mario Bros", "mario.nes", false),
            "/roms/nes/mario.nes"
        );
        // A trailing slash on the root is normalised away.
        assert_eq!(
            build_dest_path("/roms/nes/", "Game", "game.rom", false),
            "/roms/nes/game.rom"
        );
    }

    #[test]
    fn test_build_dest_path_multi_rom_gets_game_folder() {
        assert_eq!(
            build_dest_path("/roms/nes", "Multi Disk Game", "disk1.img", true),
            "/roms/nes/Multi Disk Game/disk1.img"
        );
        assert_eq!(
            build_dest_path("/roms/nes", "Multi Disk Game", "disk2.img", true),
            "/roms/nes/Multi Disk Game/disk2.img"
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
    fn plan_records_collections_skipped_for_no_destination() {
        let db = setup_db();
        let conn = db.conn();

        // A collection with an active version but no dest_path and no default.
        let cid = collections::create_collection(conn, "No Dest Coll", "tosec").unwrap();
        let vid = collections::add_version(conn, cid, "1.0", "/tmp/x.dat", true).unwrap();
        dats::create_node(conn, vid, None, "No Dest Coll", "dat", "No Dest Coll").unwrap();

        let plan = generate_plan_filtered(conn, None, None, OutputFormat::Loose).unwrap();
        assert!(plan.is_empty(), "no destination → no operations");
        assert_eq!(plan.skipped_no_dest, vec!["No Dest Coll".to_string()]);
    }

    #[test]
    fn catalogued_file_has_sha1_matches_scanned_loose_file() {
        let db = setup_db();
        let conn = db.conn();
        conn.execute(
            "INSERT INTO sources (id, path, case_sensitive) VALUES (1, '/lib/TOSEC', 0)",
            [],
        )
        .unwrap();
        conn.execute("INSERT INTO files (sha1, size) VALUES ('ABC123', 10)", [])
            .unwrap();
        conn.execute(
            "INSERT INTO file_locations (sha1, source_id, path, archive_path)
             VALUES ('ABC123', 1, 'Acorn/game.rom', NULL)",
            [],
        )
        .unwrap();

        // Exact path + hash → trusted (case-insensitive on the hash).
        assert!(catalogued_file_has_sha1(conn, "/lib/TOSEC/Acorn/game.rom", "ABC123").unwrap());
        assert!(catalogued_file_has_sha1(conn, "/lib/TOSEC/Acorn/game.rom", "abc123").unwrap());
        // Wrong hash or wrong path → not trusted (falls back to disk hashing).
        assert!(!catalogued_file_has_sha1(conn, "/lib/TOSEC/Acorn/game.rom", "DEF456").unwrap());
        assert!(!catalogued_file_has_sha1(conn, "/lib/TOSEC/Acorn/other.rom", "ABC123").unwrap());
    }

    #[test]
    fn resolve_dest_root_prefers_explicit_path() {
        // An explicit per-collection dest_path wins and is used verbatim,
        // ignoring both the default and the hierarchy.
        assert_eq!(
            resolve_dest_root(Some("/explicit/here"), Some("/lib"), "Acorn/BBC"),
            Some("/explicit/here".to_string())
        );
    }

    #[test]
    fn resolve_dest_root_falls_back_to_default_plus_hierarchy() {
        assert_eq!(
            resolve_dest_root(None, Some("/Volumes/Data"), "TOSEC-PIX/Acorn/BBC"),
            Some("/Volumes/Data/TOSEC-PIX/Acorn/BBC".to_string())
        );
        // A trailing slash on the default base is normalised away.
        assert_eq!(
            resolve_dest_root(None, Some("/Volumes/Data/"), "TOSEC/Sinclair"),
            Some("/Volumes/Data/TOSEC/Sinclair".to_string())
        );
    }

    #[test]
    fn resolve_dest_root_is_none_without_explicit_or_default() {
        // Neither an explicit path nor a default: no destination, caller skips.
        assert_eq!(resolve_dest_root(None, None, "Acorn/BBC"), None);
    }

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
    }

    #[test]
    fn group_for_archive_collects_roms_per_game_with_canonical_entry_names() {
        let matches = vec![
            MatchedRom {
                collection: "C".into(),
                game_name: "Game".into(),
                rom_name: "disk1.img".into(),
                sha1: "AAA".into(),
                size: 10,
                source_path: "src/a.img".into(),
                source_root: "/roms".into(),
                archive_path: None,
            },
            MatchedRom {
                collection: "C".into(),
                game_name: "Game".into(),
                rom_name: "disk2.img".into(),
                sha1: "BBB".into(),
                size: 20,
                source_path: "src/b.img".into(),
                source_root: "/roms".into(),
                archive_path: None,
            },
        ];

        let games = group_for_archive(matches);
        assert_eq!(games.len(), 1);
        let g = &games[0];
        assert_eq!(g.name, "Game");
        assert_eq!(g.size, 30);
        assert_eq!(g.sources.len(), 2);
        // Sources carry canonical ROM names as their archive entry names.
        assert_eq!(g.sources[0].entry_name.as_deref(), Some("disk1.img"));
        assert_eq!(g.sources[0].path, "/roms/src/a.img");
        assert_eq!(g.expected.len(), 2);
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
