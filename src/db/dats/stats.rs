use anyhow::Result;
use rusqlite::Connection;
use std::collections::{HashMap, HashSet};

use super::{DatRom, MergeMode, get_games_for_version, get_roms_for_version_grouped};

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

/// `?,?,…` — `n` bound-parameter placeholders for an `IN (…)` clause.
fn placeholders(n: usize) -> String {
    let mut s = String::with_capacity(n * 2);
    for i in 0..n {
        if i > 0 {
            s.push(',');
        }
        s.push('?');
    }
    s
}

/// Which of `keys` the inventory holds — the bulk form of [`rom_present`].
///
/// A per-key `rom_present` loop issues one query per ROM (hundreds of thousands
/// for a full MAME/aggregate set), which dominates completeness calculation.
/// This batches the lookups into `IN (…)` queries while applying the *same*
/// matching rules: a SHA1 key matches `files.sha1` or `files.sha1_no_header`; an
/// MD5 key matches `files.md5`; a CRC+size key matches `files.crc32` at the same
/// size.
pub fn present_keys(conn: &Connection, keys: &HashSet<RomKey>) -> Result<HashSet<RomKey>> {
    // Stay well under SQLite's bound-variable cap. The SHA1 query binds each
    // value twice (sha1 OR sha1_no_header), so its batch is the smallest.
    const BATCH: usize = 400;
    let mut present: HashSet<RomKey> = HashSet::new();

    let mut sha1s: Vec<&str> = Vec::new();
    let mut md5s: Vec<&str> = Vec::new();
    let mut crc_size: Vec<(&str, i64)> = Vec::new();
    for k in keys {
        match k {
            RomKey::Sha1(s) => sha1s.push(s),
            RomKey::Md5(m) => md5s.push(m),
            RomKey::CrcSize(c, sz) => crc_size.push((c, *sz)),
        }
    }

    // SHA1: present if held as either the headered or the headerless hash. Two
    // single-column `IN` lookups joined by UNION, NOT `sha1 IN (…) OR
    // sha1_no_header IN (…)` — an OR across two columns uses neither index and
    // full-scans the (huge) files table per batch, the very cost this function
    // exists to avoid. Each arm here uses its index (the `sha1` primary key and
    // `idx_files_sha1_no_header`).
    for chunk in sha1s.chunks(BATCH) {
        let ph = placeholders(chunk.len());
        let mut stmt = conn.prepare(&format!(
            "SELECT sha1 FROM files WHERE sha1 IN ({ph}) \
             UNION SELECT sha1_no_header FROM files WHERE sha1_no_header IN ({ph})"
        ))?;
        let found: HashSet<String> = stmt
            .query_map(
                rusqlite::params_from_iter(chunk.iter().chain(chunk.iter())),
                |row| row.get::<_, String>(0),
            )?
            .collect::<Result<_, _>>()?;
        for &s in chunk {
            if found.contains(s) {
                present.insert(RomKey::Sha1(s.to_string()));
            }
        }
    }

    // MD5.
    for chunk in md5s.chunks(BATCH) {
        let ph = placeholders(chunk.len());
        let mut stmt = conn.prepare(&format!("SELECT md5 FROM files WHERE md5 IN ({ph})"))?;
        let found: HashSet<String> = stmt
            .query_map(rusqlite::params_from_iter(chunk.iter()), |row| {
                row.get::<_, String>(0)
            })?
            .collect::<Result<_, _>>()?;
        for &m in chunk {
            if found.contains(m) {
                present.insert(RomKey::Md5(m.to_string()));
            }
        }
    }

    // CRC32 + size: match on CRC in the batch, confirm the size in memory.
    for chunk in crc_size.chunks(BATCH) {
        let ph = placeholders(chunk.len());
        let mut stmt = conn.prepare(&format!(
            "SELECT crc32, size FROM files WHERE crc32 IN ({ph})"
        ))?;
        let mut found: HashSet<(String, i64)> = HashSet::new();
        let rows = stmt.query_map(
            rusqlite::params_from_iter(chunk.iter().map(|(c, _)| *c)),
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
        )?;
        for r in rows {
            found.insert(r?);
        }
        for &(c, sz) in chunk {
            if found.contains(&(c.to_string(), sz)) {
                present.insert(RomKey::CrcSize(c.to_string(), sz));
            }
        }
    }

    Ok(present)
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

    // All ROMs for the version in one query, grouped by game — not a query per
    // game (tens of thousands for a full MAME set).
    let game_roms = get_roms_for_version_grouped(conn, version_id)?;

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

    // Count how many we have — one batched lookup, not a query per ROM.
    let have = present_keys(conn, &all_required)?;

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
