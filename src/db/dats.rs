//! DAT node, game, and ROM CRUD operations

use anyhow::Result;
use rusqlite::{Connection, params};
use std::collections::{HashMap, HashSet};

/// Merge mode for MAME-style ROM sets
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MergeMode {
    /// Every game contains all its ROMs (no inheritance)
    #[default]
    NonMerged,
    /// Clones only have unique ROMs; inherited ROMs come from parent
    Split,
    /// Parent contains all ROMs including clones (no separate clone archives)
    Merged,
}

/// A node in the DAT hierarchy
#[derive(Debug, Clone)]
pub struct DatNode {
    pub id: i64,
    pub version_id: i64,
    pub parent_id: Option<i64>,
    pub name: String,
    pub node_type: String,
    pub path: String,
}

/// A game/set from a DAT
#[derive(Debug, Clone)]
pub struct DatGame {
    pub id: i64,
    pub node_id: i64,
    pub name: String,
    pub description: Option<String>,
    pub parent_name: Option<String>,
    pub is_bios: bool,
    pub is_device: bool,
    pub is_mechanical: bool,
}

/// A ROM within a game
#[derive(Debug, Clone)]
pub struct DatRom {
    pub id: i64,
    pub game_id: i64,
    pub name: String,
    pub size: i64,
    pub sha1: Option<String>,
    pub md5: Option<String>,
    pub crc32: Option<String>,
    pub status: String,
    pub merge_tag: Option<String>,
}

/// Create a DAT node
pub fn create_node(
    conn: &Connection,
    version_id: i64,
    parent_id: Option<i64>,
    name: &str,
    node_type: &str,
    path: &str,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO dat_nodes (version_id, parent_id, name, node_type, path) VALUES (?, ?, ?, ?, ?)",
        params![version_id, parent_id, name, node_type, path],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Create a game entry
#[allow(clippy::too_many_arguments)]
pub fn create_game(
    conn: &Connection,
    node_id: i64,
    name: &str,
    description: Option<&str>,
    parent_name: Option<&str>,
    is_bios: bool,
    is_device: bool,
    is_mechanical: bool,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO dat_games (node_id, name, description, parent_name, is_bios, is_device, is_mechanical)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
        params![node_id, name, description, parent_name, is_bios, is_device, is_mechanical],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Create a ROM entry
#[allow(clippy::too_many_arguments)]
pub fn create_rom(
    conn: &Connection,
    game_id: i64,
    name: &str,
    size: i64,
    sha1: Option<&str>,
    md5: Option<&str>,
    crc32: Option<&str>,
    status: &str,
    merge_tag: Option<&str>,
) -> Result<i64> {
    // INSERT OR IGNORE because a game can legitimately list the same ROM name
    // twice — MAME/FBNeo arcade and console DATs repeat a shared BIOS/merge ROM
    // (identical name, size and CRC) across a parent and its merge entries. A
    // plain INSERT trips the UNIQUE(game_id, name) constraint and aborts the
    // whole DAT import (this silently dropped FBNeo's arcade.dat and msx.dat).
    // The duplicate is the same file, so skipping it leaves completeness correct.
    let inserted = conn.execute(
        "INSERT OR IGNORE INTO dat_roms (game_id, name, size, sha1, md5, crc32, status, merge_tag)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        params![game_id, name, size, sha1, md5, crc32, status, merge_tag],
    )?;
    if inserted == 0 {
        // Already present — return the existing row's id, not a stale
        // last_insert_rowid from an unrelated prior insert.
        return Ok(conn.query_row(
            "SELECT id FROM dat_roms WHERE game_id = ? AND name = ?",
            params![game_id, name],
            |row| row.get(0),
        )?);
    }
    Ok(conn.last_insert_rowid())
}

/// Get games for a node
pub fn get_games_for_node(conn: &Connection, node_id: i64) -> Result<Vec<DatGame>> {
    let mut stmt = conn.prepare(
        "SELECT id, node_id, name, description, parent_name, is_bios, is_device, is_mechanical
         FROM dat_games WHERE node_id = ? ORDER BY name",
    )?;

    let games = stmt
        .query_map([node_id], |row| {
            Ok(DatGame {
                id: row.get(0)?,
                node_id: row.get(1)?,
                name: row.get(2)?,
                description: row.get(3)?,
                parent_name: row.get(4)?,
                is_bios: row.get(5)?,
                is_device: row.get(6)?,
                is_mechanical: row.get(7)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(games)
}

/// Get ROMs for a game
pub fn get_roms_for_game(conn: &Connection, game_id: i64) -> Result<Vec<DatRom>> {
    let mut stmt = conn.prepare(
        "SELECT id, game_id, name, size, sha1, md5, crc32, status, merge_tag
         FROM dat_roms WHERE game_id = ? ORDER BY name",
    )?;

    let roms = stmt
        .query_map([game_id], |row| {
            Ok(DatRom {
                id: row.get(0)?,
                game_id: row.get(1)?,
                name: row.get(2)?,
                size: row.get(3)?,
                sha1: row.get(4)?,
                md5: row.get(5)?,
                crc32: row.get(6)?,
                status: row.get(7)?,
                merge_tag: row.get(8)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(roms)
}

/// Count total games and ROMs for a version
pub fn count_games_and_roms(conn: &Connection, version_id: i64) -> Result<(i64, i64)> {
    let game_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM dat_games g
         JOIN dat_nodes n ON g.node_id = n.id
         WHERE n.version_id = ?",
        [version_id],
        |row| row.get(0),
    )?;

    let rom_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM dat_roms r
         JOIN dat_games g ON r.game_id = g.id
         JOIN dat_nodes n ON g.node_id = n.id
         WHERE n.version_id = ?",
        [version_id],
        |row| row.get(0),
    )?;

    Ok((game_count, rom_count))
}

/// Get a game by name within a version
pub fn get_game_by_name(conn: &Connection, version_id: i64, name: &str) -> Result<Option<DatGame>> {
    let mut stmt = conn.prepare(
        "SELECT g.id, g.node_id, g.name, g.description, g.parent_name, g.is_bios, g.is_device, g.is_mechanical
         FROM dat_games g
         JOIN dat_nodes n ON g.node_id = n.id
         WHERE n.version_id = ? AND g.name = ?",
    )?;

    let result = stmt.query_row(params![version_id, name], |row| {
        Ok(DatGame {
            id: row.get(0)?,
            node_id: row.get(1)?,
            name: row.get(2)?,
            description: row.get(3)?,
            parent_name: row.get(4)?,
            is_bios: row.get(5)?,
            is_device: row.get(6)?,
            is_mechanical: row.get(7)?,
        })
    });

    match result {
        Ok(g) => Ok(Some(g)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Get all games for a version
pub fn get_games_for_version(conn: &Connection, version_id: i64) -> Result<Vec<DatGame>> {
    let mut stmt = conn.prepare(
        "SELECT g.id, g.node_id, g.name, g.description, g.parent_name, g.is_bios, g.is_device, g.is_mechanical
         FROM dat_games g
         JOIN dat_nodes n ON g.node_id = n.id
         WHERE n.version_id = ?
         ORDER BY g.name",
    )?;

    let games = stmt
        .query_map([version_id], |row| {
            Ok(DatGame {
                id: row.get(0)?,
                node_id: row.get(1)?,
                name: row.get(2)?,
                description: row.get(3)?,
                parent_name: row.get(4)?,
                is_bios: row.get(5)?,
                is_device: row.get(6)?,
                is_mechanical: row.get(7)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(games)
}

/// Find a ROM by SHA1 hash
pub fn find_rom_by_sha1(conn: &Connection, version_id: i64, sha1: &str) -> Result<Option<DatRom>> {
    let mut stmt = conn.prepare(
        "SELECT r.id, r.game_id, r.name, r.size, r.sha1, r.md5, r.crc32, r.status, r.merge_tag
         FROM dat_roms r
         JOIN dat_games g ON r.game_id = g.id
         JOIN dat_nodes n ON g.node_id = n.id
         WHERE n.version_id = ? AND r.sha1 = ?",
    )?;

    let result = stmt.query_row(params![version_id, sha1], |row| {
        Ok(DatRom {
            id: row.get(0)?,
            game_id: row.get(1)?,
            name: row.get(2)?,
            size: row.get(3)?,
            sha1: row.get(4)?,
            md5: row.get(5)?,
            crc32: row.get(6)?,
            status: row.get(7)?,
            merge_tag: row.get(8)?,
        })
    });

    match result {
        Ok(r) => Ok(Some(r)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Get all ROMs for a version (game_name included for convenience)
pub fn get_roms_for_version(conn: &Connection, version_id: i64) -> Result<Vec<(String, DatRom)>> {
    let mut stmt = conn.prepare(
        "SELECT g.name, r.id, r.game_id, r.name, r.size, r.sha1, r.md5, r.crc32, r.status, r.merge_tag
         FROM dat_roms r
         JOIN dat_games g ON r.game_id = g.id
         JOIN dat_nodes n ON g.node_id = n.id
         WHERE n.version_id = ?
         ORDER BY g.name, r.name",
    )?;

    let roms = stmt
        .query_map([version_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                DatRom {
                    id: row.get(1)?,
                    game_id: row.get(2)?,
                    name: row.get(3)?,
                    size: row.get(4)?,
                    sha1: row.get(5)?,
                    md5: row.get(6)?,
                    crc32: row.get(7)?,
                    status: row.get(8)?,
                    merge_tag: row.get(9)?,
                },
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(roms)
}

/// How a required ROM is identified for "do we have it?" matching.
///
/// SHA1 is preferred — it's collision-proof and matches either the headered or
/// the headerless form of a file. When a DAT entry carries no SHA1 we fall back
/// to MD5 (also collision-proof — the ZXDB-derived Spectrum DAT records only
/// `file_md5`), then to CRC32 + size (size guards CRC's higher collision rate).
/// Entries with none of these are unverifiable and are dropped from requirements.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RomKey {
    Sha1(String),
    Md5(String),
    CrcSize(String, i64),
}

/// Build the match key for a DAT ROM, or `None` if it carries no usable hash.
fn rom_key(rom: &DatRom) -> Option<RomKey> {
    if let Some(sha1) = &rom.sha1 {
        Some(RomKey::Sha1(sha1.clone()))
    } else if let Some(md5) = &rom.md5 {
        Some(RomKey::Md5(md5.clone()))
    } else {
        rom.crc32
            .as_ref()
            .map(|crc| RomKey::CrcSize(crc.clone(), rom.size))
    }
}

/// Is a required ROM present in the file inventory?
pub fn rom_present(conn: &Connection, key: &RomKey) -> Result<bool> {
    match key {
        RomKey::Sha1(sha1) => crate::db::files::has_matching_file(conn, sha1),
        RomKey::Md5(md5) => crate::db::files::has_matching_md5(conn, md5),
        RomKey::CrcSize(crc, size) => crate::db::files::has_matching_crc_size(conn, crc, *size),
    }
}

/// Map each ROM match key in a version to its size, so byte totals can be
/// summed over the same unique keys used for completeness counting.
fn rom_sizes_by_key(conn: &Connection, version_id: i64) -> Result<HashMap<RomKey, i64>> {
    let mut stmt = conn.prepare(
        "SELECT r.sha1, r.md5, r.crc32, r.size
         FROM dat_roms r
         JOIN dat_games g ON r.game_id = g.id
         JOIN dat_nodes n ON g.node_id = n.id
         WHERE n.version_id = ?",
    )?;
    let rows = stmt.query_map([version_id], |row| {
        Ok((
            row.get::<_, Option<String>>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, i64>(3)?,
        ))
    })?;
    let mut map = HashMap::new();
    for row in rows {
        let (sha1, md5, crc32, size) = row?;
        let key = if let Some(s) = sha1 {
            RomKey::Sha1(s)
        } else if let Some(m) = md5 {
            RomKey::Md5(m)
        } else if let Some(c) = crc32 {
            RomKey::CrcSize(c, size)
        } else {
            continue;
        };
        map.entry(key).or_insert(size);
    }
    Ok(map)
}

/// ROM requirement for a game, accounting for merge mode
#[derive(Debug, Clone)]
pub struct GameRomRequirements {
    /// Game name
    pub game_name: String,
    /// Is this game a clone (has a parent)
    pub is_clone: bool,
    /// Is this game a BIOS set
    pub is_bios: bool,
    /// Is this game a device set
    pub is_device: bool,
    /// Match keys of the ROMs required for this game to be complete.
    /// In split mode, this excludes ROMs that should come from the parent.
    pub required_roms: Vec<RomKey>,
    /// Number of ROMs with nodump status (excluded from completeness)
    pub nodump_count: usize,
}

/// Options for filtering games in ROM requirement calculations
#[derive(Debug, Clone, Default)]
pub struct RequirementOptions {
    /// Exclude mechanical sets (slot machines, etc.)
    pub exclude_mechanical: bool,
    /// Exclude BIOS sets from main count (still tracked separately)
    pub exclude_bios: bool,
    /// Exclude device sets from main count (still tracked separately)
    pub exclude_devices: bool,
}

/// Calculate ROM requirements for all games in a version, accounting for merge mode
///
/// - NonMerged: Every game needs all its ROMs locally
/// - Split: Clones inherit ROMs with merge_tag from their parent
/// - Merged: Only parents exist; clones don't have separate archives
pub fn calculate_rom_requirements(
    conn: &Connection,
    version_id: i64,
    merge_mode: MergeMode,
    exclude_mechanical: bool,
) -> Result<Vec<GameRomRequirements>> {
    calculate_rom_requirements_with_options(
        conn,
        version_id,
        merge_mode,
        &RequirementOptions {
            exclude_mechanical,
            exclude_bios: false,
            exclude_devices: false,
        },
    )
}

/// Calculate ROM requirements with full filtering options
pub fn calculate_rom_requirements_with_options(
    conn: &Connection,
    version_id: i64,
    merge_mode: MergeMode,
    options: &RequirementOptions,
) -> Result<Vec<GameRomRequirements>> {
    // Get all games for this version
    let games = get_games_for_version(conn, version_id)?;

    // Build a map of game_id -> ROMs
    let mut game_roms: HashMap<i64, Vec<DatRom>> = HashMap::new();
    for game in &games {
        let roms = get_roms_for_game(conn, game.id)?;
        game_roms.insert(game.id, roms);
    }

    let mut requirements = Vec::new();

    for game in &games {
        // Skip mechanical sets if configured
        if options.exclude_mechanical && game.is_mechanical {
            continue;
        }

        // Skip BIOS sets if configured
        if options.exclude_bios && game.is_bios {
            continue;
        }

        // Skip device sets if configured
        if options.exclude_devices && game.is_device {
            continue;
        }

        // In merged mode, clones don't have separate archives
        if merge_mode == MergeMode::Merged && game.parent_name.is_some() {
            continue;
        }

        let roms = game_roms.get(&game.id).cloned().unwrap_or_default();
        let is_clone = game.parent_name.is_some();

        let mut required_roms = Vec::new();
        let mut nodump_count = 0;

        for rom in &roms {
            // Skip nodump ROMs
            if rom.status == "nodump" {
                nodump_count += 1;
                continue;
            }

            // In split mode, ROMs with merge_tag come from the parent
            if merge_mode == MergeMode::Split && is_clone && rom.merge_tag.is_some() {
                // This ROM should be in the parent, not here
                continue;
            }

            // SHA1, or CRC32 + size for SHA1-less DAT entries; ROMs with no
            // usable hash are unverifiable and dropped.
            if let Some(key) = rom_key(rom) {
                required_roms.push(key);
            }
        }

        // In merged mode, parents also need all clone ROMs
        if merge_mode == MergeMode::Merged {
            // Find all clones of this parent
            for other_game in &games {
                if other_game.parent_name.as_ref() == Some(&game.name) {
                    let clone_roms = game_roms.get(&other_game.id).cloned().unwrap_or_default();
                    for rom in &clone_roms {
                        if rom.status == "nodump" {
                            nodump_count += 1;
                            continue;
                        }
                        if let Some(key) = rom_key(rom) {
                            // Avoid duplicates (merged ROMs)
                            if !required_roms.contains(&key) {
                                required_roms.push(key);
                            }
                        }
                    }
                }
            }
        }

        requirements.push(GameRomRequirements {
            game_name: game.name.clone(),
            is_clone,
            is_bios: game.is_bios,
            is_device: game.is_device,
            required_roms,
            nodump_count,
        });
    }

    Ok(requirements)
}

/// Statistics for merge-mode aware completeness
#[derive(Debug, Clone, Default)]
pub struct MergeModeStats {
    /// Total games (accounting for merge mode - clones excluded in merged mode)
    pub total_games: usize,
    /// Games that are complete (have all required ROMs)
    pub complete_games: usize,
    /// Games that are partially complete
    pub partial_games: usize,
    /// Games with no ROMs at all
    pub missing_games: usize,
    /// Total unique ROMs required (accounting for merge mode)
    pub total_roms: usize,
    /// ROMs we have
    pub have_roms: usize,
    /// Nodump ROMs excluded from calculations
    pub nodump_roms: usize,
    /// Number of BIOS sets included in counts
    pub bios_sets: usize,
    /// Number of device sets included in counts
    pub device_sets: usize,
    /// Total size in bytes of the unique required ROMs
    pub total_bytes: u64,
    /// Total size in bytes of the required ROMs we have
    pub have_bytes: u64,
}

/// Calculate merge-mode aware completeness statistics
pub fn calculate_merge_mode_stats(
    conn: &Connection,
    version_id: i64,
    merge_mode: MergeMode,
    exclude_mechanical: bool,
) -> Result<MergeModeStats> {
    let requirements =
        calculate_rom_requirements(conn, version_id, merge_mode, exclude_mechanical)?;

    // Collect all unique required ROMs and count BIOS/device sets
    let mut all_required: HashSet<RomKey> = HashSet::new();
    let mut total_nodump = 0;
    let mut bios_count = 0;
    let mut device_count = 0;

    for req in &requirements {
        for key in &req.required_roms {
            all_required.insert(key.clone());
        }
        total_nodump += req.nodump_count;

        if req.is_bios {
            bios_count += 1;
        }
        if req.is_device {
            device_count += 1;
        }
    }

    // Count how many we have
    let mut have: HashSet<RomKey> = HashSet::new();
    for key in &all_required {
        if rom_present(conn, key)? {
            have.insert(key.clone());
        }
    }

    // Byte totals over the same unique ROM keys, so size and count stay
    // consistent and `stats` can report GB without a second matching path.
    let size_by_key = rom_sizes_by_key(conn, version_id)?;
    let sum_bytes = |keys: &HashSet<RomKey>| -> u64 {
        keys.iter()
            .filter_map(|k| size_by_key.get(k))
            .map(|&s| s.max(0) as u64)
            .sum()
    };
    let total_bytes = sum_bytes(&all_required);
    let have_bytes = sum_bytes(&have);

    // Calculate per-game stats
    let mut complete = 0;
    let mut partial = 0;
    let mut missing = 0;

    for req in &requirements {
        if req.required_roms.is_empty() {
            // Game has no ROMs (or all nodump) - consider it complete
            complete += 1;
            continue;
        }

        let have_count = req
            .required_roms
            .iter()
            .filter(|key| have.contains(*key))
            .count();

        if have_count == req.required_roms.len() {
            complete += 1;
        } else if have_count > 0 {
            partial += 1;
        } else {
            missing += 1;
        }
    }

    Ok(MergeModeStats {
        total_games: requirements.len(),
        complete_games: complete,
        partial_games: partial,
        missing_games: missing,
        total_roms: all_required.len(),
        have_roms: have.len(),
        nodump_roms: total_nodump,
        bios_sets: bios_count,
        device_sets: device_count,
        total_bytes,
        have_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Database, collections};

    fn setup_db() -> Database {
        Database::open_in_memory().unwrap()
    }

    /// Helper to create a collection and version for tests
    fn create_test_collection_version(conn: &Connection) -> (i64, i64) {
        let coll_id = collections::create_collection(conn, "Nintendo - NES", "nointro").unwrap();
        let version_id =
            collections::add_version(conn, coll_id, "20231215", "/path/to.dat", true).unwrap();
        (coll_id, version_id)
    }

    #[test]
    fn test_crc_only_dat_entry_is_required_and_matched_by_crc_size() {
        let db = setup_db();
        let conn = db.conn();
        let (_, version_id) = create_test_collection_version(conn);
        let node_id = create_node(conn, version_id, None, "root", "dat", "root").unwrap();
        let game_id =
            create_game(conn, node_id, "crcgame", None, None, false, false, false).unwrap();
        // A DAT entry with only a CRC32 and size — no SHA1. Previously dropped
        // from requirements entirely, which let a game falsely read "complete".
        create_rom(
            conn,
            game_id,
            "a.rom",
            1024,
            None,
            None,
            Some("DEADBEEF"),
            "good",
            None,
        )
        .unwrap();

        // It is now a requirement, keyed on CRC + size.
        let reqs =
            calculate_rom_requirements(conn, version_id, MergeMode::NonMerged, false).unwrap();
        let game = reqs.iter().find(|r| r.game_name == "crcgame").unwrap();
        assert_eq!(
            game.required_roms,
            vec![RomKey::CrcSize("DEADBEEF".to_string(), 1024)]
        );

        // Not owned yet: counted as required, not as have.
        let stats =
            calculate_merge_mode_stats(conn, version_id, MergeMode::NonMerged, false).unwrap();
        assert_eq!(stats.total_roms, 1);
        assert_eq!(stats.have_roms, 0);
        assert_eq!(stats.complete_games, 0);
        // Byte totals follow the same CRC-only key: required but not yet had.
        // (This is what `stats` reports as GB; the SHA1-only path showed zero.)
        assert_eq!(stats.total_bytes, 1024);
        assert_eq!(stats.have_bytes, 0);

        // A file with a matching CRC + size makes it present (no SHA1 needed).
        crate::db::files::upsert_file(conn, "SHA1_OF_FILE", None, None, Some("DEADBEEF"), 1024)
            .unwrap();
        let stats =
            calculate_merge_mode_stats(conn, version_id, MergeMode::NonMerged, false).unwrap();
        assert_eq!(stats.have_roms, 1);
        assert_eq!(stats.complete_games, 1);
        assert_eq!(stats.have_bytes, 1024);

        // Right CRC but wrong size must NOT match — size guards CRC collisions.
        assert!(!crate::db::files::has_matching_crc_size(conn, "DEADBEEF", 2048).unwrap());
    }

    #[test]
    fn test_create_node() {
        let db = setup_db();
        let conn = db.conn();
        let (_, version_id) = create_test_collection_version(conn);

        let node_id = create_node(
            conn,
            version_id,
            None,
            "Nintendo - NES",
            "root",
            "Nintendo - NES",
        )
        .unwrap();
        assert!(node_id > 0);
    }

    #[test]
    fn test_create_nested_nodes() {
        let db = setup_db();
        let conn = db.conn();
        let (_, version_id) = create_test_collection_version(conn);

        // Create hierarchy: TOSEC > Commodore > Amiga
        let root = create_node(conn, version_id, None, "TOSEC", "root", "TOSEC").unwrap();
        let manufacturer = create_node(
            conn,
            version_id,
            Some(root),
            "Commodore",
            "manufacturer",
            "TOSEC/Commodore",
        )
        .unwrap();
        let system = create_node(
            conn,
            version_id,
            Some(manufacturer),
            "Amiga",
            "system",
            "TOSEC/Commodore/Amiga",
        )
        .unwrap();

        assert!(root > 0);
        assert!(manufacturer > 0);
        assert!(system > 0);
    }

    #[test]
    fn test_create_game() {
        let db = setup_db();
        let conn = db.conn();
        let (_, version_id) = create_test_collection_version(conn);

        let node_id = create_node(conn, version_id, None, "NES", "root", "NES").unwrap();

        let game_id = create_game(
            conn,
            node_id,
            "Super Mario Bros. (World)",
            Some("Super Mario Bros. (World)"),
            None,
            false,
            false,
            false,
        )
        .unwrap();

        assert!(game_id > 0);
    }

    #[test]
    fn test_create_clone_game() {
        let db = setup_db();
        let conn = db.conn();
        let (_, version_id) = create_test_collection_version(conn);

        let node_id = create_node(conn, version_id, None, "NES", "root", "NES").unwrap();

        // Parent game
        create_game(
            conn,
            node_id,
            "Super Mario Bros. (World)",
            None,
            None,
            false,
            false,
            false,
        )
        .unwrap();

        // Clone
        let clone_id = create_game(
            conn,
            node_id,
            "Super Mario Bros. (USA)",
            None,
            Some("Super Mario Bros. (World)"),
            false,
            false,
            false,
        )
        .unwrap();

        assert!(clone_id > 0);
    }

    #[test]
    fn test_create_bios_game() {
        let db = setup_db();
        let conn = db.conn();
        let (_, version_id) = create_test_collection_version(conn);

        let node_id = create_node(conn, version_id, None, "MAME", "root", "MAME").unwrap();

        let bios_id = create_game(
            conn,
            node_id,
            "neogeo",
            Some("Neo-Geo BIOS"),
            None,
            true, // is_bios
            false,
            false,
        )
        .unwrap();

        assert!(bios_id > 0);
    }

    #[test]
    fn test_create_rom() {
        let db = setup_db();
        let conn = db.conn();
        let (_, version_id) = create_test_collection_version(conn);

        let node_id = create_node(conn, version_id, None, "NES", "root", "NES").unwrap();
        let game_id = create_game(
            conn,
            node_id,
            "Super Mario Bros.",
            None,
            None,
            false,
            false,
            false,
        )
        .unwrap();

        let rom_id = create_rom(
            conn,
            game_id,
            "Super Mario Bros. (World).nes",
            40976,
            Some("FACEE9C577A5262DBE33AC4930BB0B58C8C037F7"),
            Some("811B027EAF99C2DEF7B933C5208636DE"),
            Some("3337EC46"),
            "good",
            None,
        )
        .unwrap();

        assert!(rom_id > 0);
    }

    #[test]
    fn test_create_rom_duplicate_name_is_deduped_not_an_error() {
        // MAME/FBNeo DATs list a shared BIOS/merge ROM twice within a game
        // (same name, size, CRC). Importing it must not abort on the
        // UNIQUE(game_id, name) constraint — the duplicate is skipped and the
        // existing row id returned, so the whole DAT still imports.
        let db = setup_db();
        let conn = db.conn();
        let (_, version_id) = create_test_collection_version(conn);
        let node_id = create_node(conn, version_id, None, "MSX", "root", "MSX").unwrap();
        let game_id =
            create_game(conn, node_id, "zoom909k", None, None, false, false, false).unwrap();

        let first = create_rom(
            conn,
            game_id,
            "msx.rom",
            32768,
            None,
            None,
            Some("a317e6b4"),
            "good",
            Some("msx.rom"),
        )
        .unwrap();
        // Same name again (the merge duplicate) — must not error.
        let second = create_rom(
            conn,
            game_id,
            "msx.rom",
            32768,
            None,
            None,
            Some("a317e6b4"),
            "good",
            None,
        )
        .unwrap();

        assert_eq!(first, second, "duplicate should return the existing row id");
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM dat_roms WHERE game_id = ? AND name = ?",
                params![game_id, "msx.rom"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "duplicate ROM name should be stored once");
    }

    #[test]
    fn test_create_rom_with_merge_tag() {
        let db = setup_db();
        let conn = db.conn();
        let (_, version_id) = create_test_collection_version(conn);

        let node_id = create_node(conn, version_id, None, "MAME", "root", "MAME").unwrap();
        let game_id =
            create_game(conn, node_id, "pacman", None, None, false, false, false).unwrap();

        let rom_id = create_rom(
            conn,
            game_id,
            "pacman.6e",
            4096,
            Some("ABC123"),
            None,
            Some("12345678"),
            "good",
            Some("puckman"), // merge tag for merged sets
        )
        .unwrap();

        assert!(rom_id > 0);
    }

    #[test]
    fn test_get_games_for_node() {
        let db = setup_db();
        let conn = db.conn();
        let (_, version_id) = create_test_collection_version(conn);

        let node_id = create_node(conn, version_id, None, "NES", "root", "NES").unwrap();

        create_game(conn, node_id, "Zelda", None, None, false, false, false).unwrap();
        create_game(conn, node_id, "Mario", None, None, false, false, false).unwrap();
        create_game(conn, node_id, "Metroid", None, None, false, false, false).unwrap();

        let games = get_games_for_node(conn, node_id).unwrap();
        assert_eq!(games.len(), 3);

        // Should be sorted by name
        assert_eq!(games[0].name, "Mario");
        assert_eq!(games[1].name, "Metroid");
        assert_eq!(games[2].name, "Zelda");
    }

    #[test]
    fn test_get_roms_for_game() {
        let db = setup_db();
        let conn = db.conn();
        let (_, version_id) = create_test_collection_version(conn);

        let node_id = create_node(conn, version_id, None, "MAME", "root", "MAME").unwrap();
        let game_id =
            create_game(conn, node_id, "pacman", None, None, false, false, false).unwrap();

        create_rom(
            conn,
            game_id,
            "pacman.6e",
            4096,
            Some("SHA1_A"),
            None,
            None,
            "good",
            None,
        )
        .unwrap();
        create_rom(
            conn,
            game_id,
            "pacman.6f",
            4096,
            Some("SHA1_B"),
            None,
            None,
            "good",
            None,
        )
        .unwrap();
        create_rom(
            conn,
            game_id,
            "pacman.6h",
            4096,
            Some("SHA1_C"),
            None,
            None,
            "good",
            None,
        )
        .unwrap();

        let roms = get_roms_for_game(conn, game_id).unwrap();
        assert_eq!(roms.len(), 3);

        // Should be sorted by name
        assert_eq!(roms[0].name, "pacman.6e");
        assert_eq!(roms[1].name, "pacman.6f");
        assert_eq!(roms[2].name, "pacman.6h");
    }

    #[test]
    fn test_count_games_and_roms() {
        let db = setup_db();
        let conn = db.conn();
        let (_, version_id) = create_test_collection_version(conn);

        let node_id = create_node(conn, version_id, None, "NES", "root", "NES").unwrap();

        // 2 games, 3 ROMs total
        let game1 = create_game(conn, node_id, "Game1", None, None, false, false, false).unwrap();
        let game2 = create_game(conn, node_id, "Game2", None, None, false, false, false).unwrap();

        create_rom(
            conn,
            game1,
            "game1.nes",
            1000,
            Some("SHA1"),
            None,
            None,
            "good",
            None,
        )
        .unwrap();
        create_rom(
            conn,
            game2,
            "game2a.nes",
            2000,
            Some("SHA2"),
            None,
            None,
            "good",
            None,
        )
        .unwrap();
        create_rom(
            conn,
            game2,
            "game2b.nes",
            3000,
            Some("SHA3"),
            None,
            None,
            "good",
            None,
        )
        .unwrap();

        let (games, roms) = count_games_and_roms(conn, version_id).unwrap();
        assert_eq!(games, 2);
        assert_eq!(roms, 3);
    }

    #[test]
    fn test_find_rom_by_sha1() {
        let db = setup_db();
        let conn = db.conn();
        let (_, version_id) = create_test_collection_version(conn);

        let node_id = create_node(conn, version_id, None, "NES", "root", "NES").unwrap();
        let game_id = create_game(conn, node_id, "Mario", None, None, false, false, false).unwrap();

        let target_sha1 = "FACEE9C577A5262DBE33AC4930BB0B58C8C037F7";
        create_rom(
            conn,
            game_id,
            "mario.nes",
            40976,
            Some(target_sha1),
            None,
            None,
            "good",
            None,
        )
        .unwrap();

        let found = find_rom_by_sha1(conn, version_id, target_sha1).unwrap();
        assert!(found.is_some());

        let rom = found.unwrap();
        assert_eq!(rom.name, "mario.nes");
        assert_eq!(rom.size, 40976);
    }

    #[test]
    fn test_find_rom_by_sha1_not_found() {
        let db = setup_db();
        let conn = db.conn();
        let (_, version_id) = create_test_collection_version(conn);

        let found = find_rom_by_sha1(conn, version_id, "NONEXISTENT").unwrap();
        assert!(found.is_none());
    }

    #[test]
    fn test_find_rom_wrong_version() {
        let db = setup_db();
        let conn = db.conn();

        // Create two versions
        let coll_id = collections::create_collection(conn, "NES", "nointro").unwrap();
        let version1 = collections::add_version(conn, coll_id, "v1", "/v1.dat", false).unwrap();
        let version2 = collections::add_version(conn, coll_id, "v2", "/v2.dat", true).unwrap();

        // Add ROM only to version1
        let node1 = create_node(conn, version1, None, "NES", "root", "NES").unwrap();
        let game1 = create_game(conn, node1, "Mario", None, None, false, false, false).unwrap();
        let sha1 = "ABC123";
        create_rom(
            conn,
            game1,
            "mario.nes",
            1000,
            Some(sha1),
            None,
            None,
            "good",
            None,
        )
        .unwrap();

        // Should find in version1
        assert!(find_rom_by_sha1(conn, version1, sha1).unwrap().is_some());

        // Should NOT find in version2
        assert!(find_rom_by_sha1(conn, version2, sha1).unwrap().is_none());
    }

    /// Helper to create a MAME-like parent/clone structure for merge mode tests
    fn create_mame_structure(conn: &Connection, version_id: i64) -> (i64, i64) {
        let node_id = create_node(conn, version_id, None, "MAME", "root", "MAME").unwrap();

        // Parent game: pacman
        let parent_id =
            create_game(conn, node_id, "pacman", None, None, false, false, false).unwrap();
        // Parent ROMs
        create_rom(
            conn,
            parent_id,
            "pacman.5e",
            4096,
            Some("SHA1_PACMAN_5E"),
            None,
            None,
            "good",
            None,
        )
        .unwrap();
        create_rom(
            conn,
            parent_id,
            "pacman.5f",
            4096,
            Some("SHA1_PACMAN_5F"),
            None,
            None,
            "good",
            None,
        )
        .unwrap();
        create_rom(
            conn,
            parent_id,
            "prom.7f",
            256,
            Some("SHA1_PROM"),
            None,
            None,
            "good",
            None,
        )
        .unwrap();

        // Clone game: mspacman (clones from pacman)
        let clone_id = create_game(
            conn,
            node_id,
            "mspacman",
            None,
            Some("pacman"),
            false,
            false,
            false,
        )
        .unwrap();
        // Clone's unique ROMs
        create_rom(
            conn,
            clone_id,
            "mspacman.5e",
            4096,
            Some("SHA1_MSPACMAN_5E"),
            None,
            None,
            "good",
            None,
        )
        .unwrap();
        create_rom(
            conn,
            clone_id,
            "mspacman.5f",
            4096,
            Some("SHA1_MSPACMAN_5F"),
            None,
            None,
            "good",
            None,
        )
        .unwrap();
        // Clone's inherited ROM (has merge tag pointing to parent)
        create_rom(
            conn,
            clone_id,
            "prom.7f",
            256,
            Some("SHA1_PROM"),
            None,
            None,
            "good",
            Some("prom.7f"),
        )
        .unwrap();

        (parent_id, clone_id)
    }

    #[test]
    fn test_merge_mode_non_merged_requires_all_roms() {
        let db = setup_db();
        let conn = db.conn();
        let (_, version_id) = create_test_collection_version(conn);
        create_mame_structure(conn, version_id);

        let requirements =
            calculate_rom_requirements(conn, version_id, MergeMode::NonMerged, false).unwrap();

        // Should have 2 games
        assert_eq!(requirements.len(), 2);

        // Find parent and clone
        let parent = requirements
            .iter()
            .find(|r| r.game_name == "pacman")
            .unwrap();
        let clone = requirements
            .iter()
            .find(|r| r.game_name == "mspacman")
            .unwrap();

        // Parent needs 3 ROMs
        assert_eq!(parent.required_roms.len(), 3);
        assert!(!parent.is_clone);

        // Clone ALSO needs 3 ROMs (including the shared prom.7f)
        assert_eq!(clone.required_roms.len(), 3);
        assert!(clone.is_clone);
        assert!(
            clone
                .required_roms
                .contains(&RomKey::Sha1("SHA1_PROM".to_string()))
        );
    }

    #[test]
    fn test_merge_mode_split_excludes_inherited_roms() {
        let db = setup_db();
        let conn = db.conn();
        let (_, version_id) = create_test_collection_version(conn);
        create_mame_structure(conn, version_id);

        let requirements =
            calculate_rom_requirements(conn, version_id, MergeMode::Split, false).unwrap();

        // Should have 2 games
        assert_eq!(requirements.len(), 2);

        let parent = requirements
            .iter()
            .find(|r| r.game_name == "pacman")
            .unwrap();
        let clone = requirements
            .iter()
            .find(|r| r.game_name == "mspacman")
            .unwrap();

        // Parent still needs all 3 ROMs
        assert_eq!(parent.required_roms.len(), 3);

        // Clone only needs 2 ROMs (excluding inherited prom.7f with merge_tag)
        assert_eq!(clone.required_roms.len(), 2);
        assert!(
            clone
                .required_roms
                .contains(&RomKey::Sha1("SHA1_MSPACMAN_5E".to_string()))
        );
        assert!(
            clone
                .required_roms
                .contains(&RomKey::Sha1("SHA1_MSPACMAN_5F".to_string()))
        );
        assert!(
            !clone
                .required_roms
                .contains(&RomKey::Sha1("SHA1_PROM".to_string()))
        );
    }

    #[test]
    fn test_merge_mode_merged_only_parents() {
        let db = setup_db();
        let conn = db.conn();
        let (_, version_id) = create_test_collection_version(conn);
        create_mame_structure(conn, version_id);

        let requirements =
            calculate_rom_requirements(conn, version_id, MergeMode::Merged, false).unwrap();

        // Should only have 1 game (parent only, clone doesn't exist as separate archive)
        assert_eq!(requirements.len(), 1);

        let parent = &requirements[0];
        assert_eq!(parent.game_name, "pacman");

        // Parent needs all ROMs including clone's unique ROMs
        // 3 parent ROMs + 2 unique clone ROMs = 5 (but SHA1_PROM is shared, so still 5)
        assert_eq!(parent.required_roms.len(), 5);
        assert!(
            parent
                .required_roms
                .contains(&RomKey::Sha1("SHA1_PACMAN_5E".to_string()))
        );
        assert!(
            parent
                .required_roms
                .contains(&RomKey::Sha1("SHA1_PACMAN_5F".to_string()))
        );
        assert!(
            parent
                .required_roms
                .contains(&RomKey::Sha1("SHA1_PROM".to_string()))
        );
        assert!(
            parent
                .required_roms
                .contains(&RomKey::Sha1("SHA1_MSPACMAN_5E".to_string()))
        );
        assert!(
            parent
                .required_roms
                .contains(&RomKey::Sha1("SHA1_MSPACMAN_5F".to_string()))
        );
    }

    #[test]
    fn test_merge_mode_excludes_nodump() {
        let db = setup_db();
        let conn = db.conn();
        let (_, version_id) = create_test_collection_version(conn);

        let node_id = create_node(conn, version_id, None, "MAME", "root", "MAME").unwrap();
        let game_id =
            create_game(conn, node_id, "testgame", None, None, false, false, false).unwrap();

        // 2 good ROMs, 1 nodump
        create_rom(
            conn,
            game_id,
            "rom1.bin",
            1000,
            Some("SHA1_ROM1"),
            None,
            None,
            "good",
            None,
        )
        .unwrap();
        create_rom(
            conn,
            game_id,
            "rom2.bin",
            1000,
            Some("SHA1_ROM2"),
            None,
            None,
            "good",
            None,
        )
        .unwrap();
        create_rom(
            conn, game_id, "pal.bin", 256, None, None, None, "nodump", None,
        )
        .unwrap();

        let requirements =
            calculate_rom_requirements(conn, version_id, MergeMode::NonMerged, false).unwrap();

        assert_eq!(requirements.len(), 1);
        let req = &requirements[0];

        // Only 2 required ROMs (nodump excluded)
        assert_eq!(req.required_roms.len(), 2);
        assert_eq!(req.nodump_count, 1);
    }

    #[test]
    fn test_merge_mode_excludes_mechanical() {
        let db = setup_db();
        let conn = db.conn();
        let (_, version_id) = create_test_collection_version(conn);

        let node_id = create_node(conn, version_id, None, "MAME", "root", "MAME").unwrap();

        // Regular game
        let game1 = create_game(conn, node_id, "pacman", None, None, false, false, false).unwrap();
        create_rom(
            conn,
            game1,
            "pacman.bin",
            1000,
            Some("SHA1_PACMAN"),
            None,
            None,
            "good",
            None,
        )
        .unwrap();

        // Mechanical game (slot machine)
        let game2 =
            create_game(conn, node_id, "slotmachine", None, None, false, false, true).unwrap();
        create_rom(
            conn,
            game2,
            "slot.bin",
            1000,
            Some("SHA1_SLOT"),
            None,
            None,
            "good",
            None,
        )
        .unwrap();

        // With exclude_mechanical = true
        let requirements =
            calculate_rom_requirements(conn, version_id, MergeMode::NonMerged, true).unwrap();
        assert_eq!(requirements.len(), 1);
        assert_eq!(requirements[0].game_name, "pacman");

        // With exclude_mechanical = false
        let requirements =
            calculate_rom_requirements(conn, version_id, MergeMode::NonMerged, false).unwrap();
        assert_eq!(requirements.len(), 2);
    }

    #[test]
    fn test_merge_mode_stats_calculation() {
        use crate::db::files;

        let db = setup_db();
        let conn = db.conn();
        let (_, version_id) = create_test_collection_version(conn);
        create_mame_structure(conn, version_id);

        // Add a source and some files to our inventory
        let source_id = files::add_source(conn, "/roms", false).unwrap();

        // Only add the parent's ROMs to inventory (not the clone's unique ROMs)
        files::upsert_file(conn, "SHA1_PACMAN_5E", None, None, None, 4096).unwrap();
        files::upsert_file(conn, "SHA1_PACMAN_5F", None, None, None, 4096).unwrap();
        files::upsert_file(conn, "SHA1_PROM", None, None, None, 256).unwrap();

        let _ = source_id; // unused, just need files in db

        // Non-merged: need all 6 unique ROMs (3 parent + 3 clone, but SHA1_PROM shared = 5)
        let stats =
            calculate_merge_mode_stats(conn, version_id, MergeMode::NonMerged, false).unwrap();
        assert_eq!(stats.total_games, 2);
        assert_eq!(stats.total_roms, 5); // 5 unique SHA1s
        assert_eq!(stats.have_roms, 3); // We have 3 ROMs
        assert_eq!(stats.complete_games, 1); // Parent is complete
        assert_eq!(stats.partial_games, 1); // Clone has prom but missing unique ROMs

        // Split mode: clone only needs unique ROMs (2)
        let stats = calculate_merge_mode_stats(conn, version_id, MergeMode::Split, false).unwrap();
        assert_eq!(stats.total_games, 2);
        // Total unique required: parent 3 + clone 2 = 5
        assert_eq!(stats.total_roms, 5);
        assert_eq!(stats.have_roms, 3);
        assert_eq!(stats.complete_games, 1); // Parent is complete
        assert_eq!(stats.missing_games, 1); // Clone is missing (0 of its 2 unique)

        // Merged mode: only parent, needs all ROMs
        let stats = calculate_merge_mode_stats(conn, version_id, MergeMode::Merged, false).unwrap();
        assert_eq!(stats.total_games, 1);
        assert_eq!(stats.total_roms, 5); // Parent needs all 5 unique
        assert_eq!(stats.have_roms, 3);
        assert_eq!(stats.partial_games, 1); // Parent is partial (missing clone ROMs)
    }

    #[test]
    fn test_bios_device_tracking_in_stats() {
        let db = setup_db();
        let conn = db.conn();
        let (_, version_id) = create_test_collection_version(conn);

        let node_id = create_node(conn, version_id, None, "MAME", "root", "MAME").unwrap();

        // Regular game
        let game1 = create_game(conn, node_id, "mslug", None, None, false, false, false).unwrap();
        create_rom(
            conn,
            game1,
            "mslug.bin",
            1000,
            Some("SHA1_MSLUG"),
            None,
            None,
            "good",
            None,
        )
        .unwrap();

        // BIOS set
        let bios = create_game(conn, node_id, "neogeo", None, None, true, false, false).unwrap();
        create_rom(
            conn,
            bios,
            "neogeo.bin",
            2000,
            Some("SHA1_NEOGEO"),
            None,
            None,
            "good",
            None,
        )
        .unwrap();

        // Device set
        let device = create_game(conn, node_id, "ymz280b", None, None, false, true, false).unwrap();
        create_rom(
            conn,
            device,
            "ymz.bin",
            500,
            Some("SHA1_YMZ"),
            None,
            None,
            "good",
            None,
        )
        .unwrap();

        let stats =
            calculate_merge_mode_stats(conn, version_id, MergeMode::NonMerged, false).unwrap();

        assert_eq!(stats.total_games, 3);
        assert_eq!(stats.bios_sets, 1);
        assert_eq!(stats.device_sets, 1);
        // 3 - 1 BIOS - 1 device = 1 regular game
        assert_eq!(stats.total_games - stats.bios_sets - stats.device_sets, 1);
    }

    #[test]
    fn test_exclude_bios_and_devices() {
        let db = setup_db();
        let conn = db.conn();
        let (_, version_id) = create_test_collection_version(conn);

        let node_id = create_node(conn, version_id, None, "MAME", "root", "MAME").unwrap();

        // Regular game
        let game1 = create_game(conn, node_id, "mslug", None, None, false, false, false).unwrap();
        create_rom(
            conn,
            game1,
            "mslug.bin",
            1000,
            Some("SHA1_MSLUG"),
            None,
            None,
            "good",
            None,
        )
        .unwrap();

        // BIOS set
        let bios = create_game(conn, node_id, "neogeo", None, None, true, false, false).unwrap();
        create_rom(
            conn,
            bios,
            "neogeo.bin",
            2000,
            Some("SHA1_NEOGEO"),
            None,
            None,
            "good",
            None,
        )
        .unwrap();

        // Device set
        let device = create_game(conn, node_id, "ymz280b", None, None, false, true, false).unwrap();
        create_rom(
            conn,
            device,
            "ymz.bin",
            500,
            Some("SHA1_YMZ"),
            None,
            None,
            "good",
            None,
        )
        .unwrap();

        // Without exclusions: 3 games
        let requirements = calculate_rom_requirements_with_options(
            conn,
            version_id,
            MergeMode::NonMerged,
            &RequirementOptions::default(),
        )
        .unwrap();
        assert_eq!(requirements.len(), 3);

        // With BIOS exclusion: 2 games
        let requirements = calculate_rom_requirements_with_options(
            conn,
            version_id,
            MergeMode::NonMerged,
            &RequirementOptions {
                exclude_bios: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(requirements.len(), 2);
        assert!(!requirements.iter().any(|r| r.game_name == "neogeo"));

        // With device exclusion: 2 games
        let requirements = calculate_rom_requirements_with_options(
            conn,
            version_id,
            MergeMode::NonMerged,
            &RequirementOptions {
                exclude_devices: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(requirements.len(), 2);
        assert!(!requirements.iter().any(|r| r.game_name == "ymz280b"));

        // With both exclusions: 1 game (only regular games)
        let requirements = calculate_rom_requirements_with_options(
            conn,
            version_id,
            MergeMode::NonMerged,
            &RequirementOptions {
                exclude_bios: true,
                exclude_devices: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(requirements.len(), 1);
        assert_eq!(requirements[0].game_name, "mslug");
    }
}
