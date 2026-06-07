//! Plan generation logic

use anyhow::Result;
use rusqlite::Connection;
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

/// Options controlling plan generation.
#[derive(Debug, Clone, Default)]
pub struct PlanOptions {
    /// Glob over collection names; `None` plans every collection.
    pub dat_filter: Option<String>,
    /// Library-wide destination root for collections without their own dest_path.
    pub default_dest: Option<String>,
    /// Output format for collections without their own setting.
    pub default_format: OutputFormat,
    /// Move files into place (and delete the source) instead of copying — a true
    /// in-place tidy rather than a duplicating copy. Off (copy) by default.
    pub move_files: bool,
}

/// Generate a plan for all configured collections with default options.
pub fn generate_plan(conn: &Connection) -> Result<Plan> {
    generate_plan_filtered(conn, &PlanOptions::default())
}

/// Generate a plan from the given options.
///
/// `dat_filter` supports glob patterns (`*`, `?`, case-insensitive) over
/// collection names.
pub fn generate_plan_filtered(conn: &Connection, opts: &PlanOptions) -> Result<Plan> {
    let dat_filter = opts.dat_filter.as_deref();
    let default_dest = opts.default_dest.as_deref();
    let default_format = opts.default_format;

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

        // Find all matched ROMs for this version.
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

        // Effective output format, in precedence order: an explicit
        // per-collection setting, then a per-set rule (a config row keyed on the
        // set — the top segment of the library path, e.g. "TOSEC-PIX"), then the
        // library-wide default. The per-set tier lets whole sets diverge — TOSEC
        // kept as zip, TOSEC-PIX left loose for later PDF/collateral extraction —
        // without configuring every collection. Loose copies each ROM into place;
        // zip/torrentzip packs each game into one archive.
        let explicit_format = cfg.as_ref().and_then(|c| c.output_format.clone());
        let set_format = match explicit_format {
            Some(_) => None,
            None => {
                let set = hierarchy.split('/').next().unwrap_or(hierarchy.as_str());
                // Only consult a per-set rule when there is a set prefix; a flat
                // collection name is not a set and must not match itself here.
                if set != hierarchy {
                    db_config::get_collection_config(conn, set)?.and_then(|c| c.output_format)
                } else {
                    None
                }
            }
        };
        let format = resolve_output_format(
            explicit_format.as_deref().or(set_format.as_deref()),
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

                    // Already correct when the held file is a loose file already
                    // sitting at its canonical destination. The match carries the
                    // file's current location, so this is an in-memory comparison
                    // — no per-file disk stat or catalogue scan, which is what
                    // makes planning a whole library over a network mount viable.
                    let full_source = format!("{}/{}", m.source_root, m.source_path);
                    if m.archive_path.is_none() && full_source == dest {
                        already_correct += 1;
                        continue;
                    }

                    bytes += m.size as u64;
                    let source = SourceRef {
                        path: full_source,
                        archive_path: m.archive_path,
                        sha1: m.sha1,
                        entry_name: None,
                    };
                    if opts.move_files {
                        plan.add_move(source, dest, m.size as u64);
                    } else {
                        plan.add_copy(source, dest, m.size as u64);
                    }
                    to_write += 1;
                }
                let verb = if opts.move_files { "move" } else { "copy" };
                println!(
                    "  {} already correct, {} to {}",
                    already_correct, to_write, verb
                );
            }
            Some(tag) => {
                // ARCHIVE: one archive per game, named <dest_root>/<game>.zip,
                // with entries carrying canonical ROM names.
                let games = group_for_archive(matches);

                let ext = archive_extension(tag);
                for game in games {
                    let dest = format!("{}/{}.{}", dest_root.trim_end_matches('/'), game.name, ext);

                    // The matched entries already live in the archive at `dest`
                    // when every source's container is `dest` itself — an
                    // in-memory check (the matches carry their location), so a
                    // correctly-placed archive needs no disk read.
                    let at_dest =
                        !game.sources.is_empty() && game.sources.iter().all(|s| s.path == dest);
                    if archive_at_dest_is_correct(at_dest, &dest, tag)? {
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

/// Find all ROMs in one collection version that have a matching held file.
///
/// Performance is critical here — this runs once per collection, and a full
/// library is thousands of collections. The match has three modes: a DAT SHA1
/// may be the file's headered or headerless hash, and a SHA1-less DAT entry
/// matches on CRC + size. Expressed as a single `OR` join, SQLite can't drive
/// from this version's ROMs into the file index and instead scans the whole
/// `files` table per call (~13s each on a real library). Splitting the modes
/// into a `UNION` lets each branch use an index (files PK on `sha1`,
/// `idx_files_sha1_no_header`), so the query starts from the version's ROMs and
/// runs in milliseconds. We select the *file's* sha1 and size (not the DAT's) —
/// that's the true content placed at the destination, which
/// `is_file_correct_at_dest` verifies.
fn find_matched_roms(
    conn: &Connection,
    version_id: i64,
    collection_name: &str,
) -> Result<Vec<MatchedRom>> {
    let mut stmt = conn.prepare(
        "WITH vroms AS (
            SELECT r.id, r.game_id, r.name, r.sha1, r.crc32, r.size
            FROM dat_roms r
            JOIN dat_games g ON r.game_id = g.id
            JOIN dat_nodes n ON g.node_id = n.id
            WHERE n.version_id = ?1 AND r.status != 'nodump'
         ),
         matched AS (
            SELECT vr.id AS rom_id, f.sha1, f.size
            FROM vroms vr JOIN files f ON f.sha1 = vr.sha1
            WHERE vr.sha1 IS NOT NULL
            UNION
            SELECT vr.id, f.sha1, f.size
            FROM vroms vr JOIN files f ON f.sha1_no_header = vr.sha1
            WHERE vr.sha1 IS NOT NULL
            UNION
            SELECT vr.id, f.sha1, f.size
            FROM vroms vr JOIN files f ON f.crc32 = vr.crc32 AND f.size = vr.size
            WHERE vr.sha1 IS NULL AND vr.crc32 IS NOT NULL
         )
         SELECT g.name, vr.name, m.sha1, m.size, fl.path, s.path, fl.archive_path
         FROM matched m
         JOIN vroms vr ON vr.id = m.rom_id
         JOIN dat_games g ON vr.game_id = g.id
         JOIN file_locations fl ON fl.sha1 = m.sha1
         JOIN sources s ON fl.source_id = s.id
         ORDER BY g.name, vr.name",
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
        Some("7z") => OutputFormat::SevenZip,
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
        OutputFormat::SevenZip => Some("7z"),
    }
}

/// The archive file extension for a repack format tag.
fn archive_extension(tag: &str) -> &'static str {
    if tag == "7z" { "7z" } else { "zip" }
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

/// Whether the game's archive already exists, correct, at `dest`.
///
/// Entry correctness is established in memory by the caller: `at_dest` is true
/// when every matched source already lives in the archive at `dest` (the matches
/// carry their location, so no archive is opened). For `zip` that is sufficient
/// — the right entries are present. `torrentzip` additionally requires the
/// deterministic container stamp, which only the file itself can confirm, so
/// that one case reads the archive; a content-correct plain ZIP is rebuilt to
/// earn the stamp.
fn archive_at_dest_is_correct(at_dest: bool, dest: &str, format: &str) -> Result<bool> {
    if !at_dest {
        return Ok(false);
    }
    if format == "torrentzip" {
        return crate::archive::is_torrentzip_stamped(Path::new(dest));
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

        let plan = generate_plan_filtered(conn, &PlanOptions::default()).unwrap();
        assert!(plan.is_empty(), "no destination → no operations");
        assert_eq!(plan.skipped_no_dest, vec!["No Dest Coll".to_string()]);
    }

    #[test]
    fn archive_at_dest_is_correct_trusts_in_memory_placement() {
        // zip: every matched source already in the archive at dest → correct,
        // with no disk access.
        assert!(archive_at_dest_is_correct(true, "/lib/x.zip", "zip").unwrap());
        // Not all sources at dest → must (re)build; no disk access for either
        // format when at_dest is false.
        assert!(!archive_at_dest_is_correct(false, "/lib/x.zip", "zip").unwrap());
        assert!(!archive_at_dest_is_correct(false, "/lib/x.zip", "torrentzip").unwrap());
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
