//! Catalogue-to-held-file matching queries used by the planner.

use anyhow::Result;
use rusqlite::Connection;
use std::collections::HashSet;

/// A version with fewer ROMs than this cannot plausibly reach the planner's
/// expansion cap, so the expensive join can be skipped for small collections.
const GUARD_ROM_THRESHOLD: i64 = 50_000;

/// A matched ROM ready for planning.
#[derive(Debug, Clone)]
pub struct MatchedRom {
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
    /// True for a `<disk>` (CHD): stored loose in a machine folder as
    /// `<game>/<rom_name>.chd`, never packed into an archive.
    pub is_disk: bool,
}

/// The held-content SHA1s whose content satisfies more than one distinct DAT
/// game across all active versions.
pub(crate) fn compute_shared_content(conn: &Connection) -> Result<HashSet<String>> {
    let mut stmt = conn.prepare(
        "WITH active_roms AS (
             SELECT r.sha1 AS rom_sha1, r.crc32 AS rom_crc32, r.size AS rom_size,
                    g.id AS game_id
               FROM dat_roms r
               JOIN dat_games g ON g.id = r.game_id
               JOIN dat_nodes dn ON dn.id = g.node_id
               JOIN collection_versions cv ON cv.id = dn.version_id
              WHERE cv.is_active = 1
         ),
         matched AS (
             SELECT f.sha1 AS file_sha1, ar.game_id
               FROM files f JOIN active_roms ar ON f.sha1 = ar.rom_sha1
              WHERE ar.rom_sha1 IS NOT NULL AND ar.rom_sha1 <> ''
             UNION
             SELECT f.sha1, ar.game_id
               FROM files f JOIN active_roms ar ON f.sha1_no_header = ar.rom_sha1
              WHERE ar.rom_sha1 IS NOT NULL AND ar.rom_sha1 <> ''
             UNION
             SELECT f.sha1, ar.game_id
               FROM files f JOIN active_roms ar
                    ON f.crc32 = ar.rom_crc32 AND f.size = ar.rom_size
              WHERE ar.rom_sha1 IS NULL AND ar.rom_crc32 IS NOT NULL
         )
         SELECT file_sha1
           FROM matched
          WHERE file_sha1 IN (SELECT sha1 FROM file_locations)
          GROUP BY file_sha1
         HAVING COUNT(DISTINCT game_id) > 1",
    )?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    let mut set = HashSet::new();
    for r in rows {
        set.insert(r?);
    }
    Ok(set)
}

/// The source archive files whose inner entries satisfy more than one distinct
/// DAT game.
pub(crate) fn compute_shared_containers(conn: &Connection) -> Result<HashSet<String>> {
    let mut stmt = conn.prepare(
        "WITH container_games AS (
             SELECT fl.source_id, fl.path, g.id AS game_id
               FROM file_locations fl
               JOIN files f ON f.sha1 = fl.sha1
               JOIN dat_roms r ON r.sha1 = f.sha1
               JOIN dat_games g ON g.id = r.game_id
               JOIN dat_nodes dn ON dn.id = g.node_id
               JOIN collection_versions cv ON cv.id = dn.version_id
              WHERE cv.is_active = 1 AND fl.archive_path IS NOT NULL
                AND r.sha1 IS NOT NULL AND r.sha1 <> ''
             UNION
             SELECT fl.source_id, fl.path, g.id
               FROM file_locations fl
               JOIN files f ON f.sha1 = fl.sha1
               JOIN dat_roms r ON r.sha1 = f.sha1_no_header
               JOIN dat_games g ON g.id = r.game_id
               JOIN dat_nodes dn ON dn.id = g.node_id
               JOIN collection_versions cv ON cv.id = dn.version_id
              WHERE cv.is_active = 1 AND fl.archive_path IS NOT NULL
                AND r.sha1 IS NOT NULL AND r.sha1 <> ''
             UNION
             SELECT fl.source_id, fl.path, g.id
               FROM file_locations fl
               JOIN files f ON f.sha1 = fl.sha1
               JOIN dat_roms r ON r.crc32 = f.crc32 AND r.size = f.size
               JOIN dat_games g ON g.id = r.game_id
               JOIN dat_nodes dn ON dn.id = g.node_id
               JOIN collection_versions cv ON cv.id = dn.version_id
              WHERE cv.is_active = 1 AND fl.archive_path IS NOT NULL
                AND r.sha1 IS NULL AND r.crc32 IS NOT NULL
         )
         SELECT s.path || '/' || cg.path
           FROM container_games cg
           JOIN sources s ON s.id = cg.source_id
          GROUP BY cg.source_id, cg.path
         HAVING COUNT(DISTINCT cg.game_id) > 1",
    )?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    let mut set = HashSet::new();
    for r in rows {
        set.insert(r?);
    }
    Ok(set)
}

/// Count the match-rows a version's plan would materialise, bounded to
/// `cap + 1`.
pub(crate) fn count_match_rows_capped(conn: &Connection, version_id: i64, cap: i64) -> Result<i64> {
    let rom_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM dat_roms r
         JOIN dat_games g ON r.game_id = g.id
         JOIN dat_nodes n ON g.node_id = n.id
         WHERE n.version_id = ?1 AND r.status != 'nodump'",
        [version_id],
        |row| row.get(0),
    )?;
    if rom_count < GUARD_ROM_THRESHOLD {
        return Ok(rom_count.min(cap));
    }
    count_expansion_capped(conn, version_id, cap)
}

/// The exact match expansion, counted only up to `cap + 1`.
pub(crate) fn count_expansion_capped(conn: &Connection, version_id: i64, cap: i64) -> Result<i64> {
    let count: i64 = conn.query_row(
        "WITH vroms AS (
            SELECT r.id, r.sha1, r.crc32, r.size
            FROM dat_roms r
            JOIN dat_games g ON r.game_id = g.id
            JOIN dat_nodes n ON g.node_id = n.id
            WHERE n.version_id = ?1 AND r.status != 'nodump'
         ),
         matched AS (
            SELECT vr.id AS rom_id, f.sha1 AS msha1
            FROM vroms vr JOIN files f ON f.sha1 = vr.sha1 WHERE vr.sha1 IS NOT NULL
            UNION
            SELECT vr.id, f.sha1
            FROM vroms vr JOIN files f ON f.sha1_no_header = vr.sha1 WHERE vr.sha1 IS NOT NULL
            UNION
            SELECT vr.id, f.sha1
            FROM vroms vr JOIN files f ON f.crc32 = vr.crc32 AND f.size = vr.size
            WHERE vr.sha1 IS NULL AND vr.crc32 IS NOT NULL
         )
         SELECT COUNT(*) FROM (
            SELECT 1 FROM matched m JOIN file_locations fl ON fl.sha1 = m.msha1 LIMIT ?2
         )",
        rusqlite::params![version_id, cap + 1],
        |row| row.get(0),
    )?;
    Ok(count)
}

/// Find all ROMs in one collection version that have a matching held file.
pub(crate) fn find_matched_roms(
    conn: &Connection,
    version_id: i64,
    _collection_name: &str,
    split: bool,
) -> Result<Vec<MatchedRom>> {
    let mut stmt = conn.prepare(
        "WITH vroms AS (
            SELECT r.id, r.game_id, r.name, r.sha1, r.crc32, r.size, r.is_disk
            FROM dat_roms r
            JOIN dat_games g ON r.game_id = g.id
            JOIN dat_nodes n ON g.node_id = n.id
            WHERE n.version_id = ?1 AND r.status != 'nodump'
              AND (?2 = 0 OR g.parent_name IS NULL OR r.merge_tag IS NULL)
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
         SELECT g.name, vr.name, m.sha1, m.size, fl.path, s.path, fl.archive_path, vr.is_disk
         FROM matched m
         JOIN vroms vr ON vr.id = m.rom_id
         JOIN dat_games g ON vr.game_id = g.id
         JOIN file_locations fl ON fl.sha1 = m.sha1
         JOIN sources s ON fl.source_id = s.id
         ORDER BY g.name, vr.name",
    )?;

    let matches = stmt
        .query_map(rusqlite::params![version_id, split], |row| {
            Ok(MatchedRom {
                game_name: row.get(0)?,
                rom_name: row.get(1)?,
                sha1: row.get(2)?,
                size: row.get(3)?,
                source_path: row.get(4)?,
                source_root: row.get(5)?,
                archive_path: row.get(6)?,
                is_disk: row.get(7)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(matches)
}
