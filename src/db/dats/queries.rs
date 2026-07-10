use anyhow::Result;
use rusqlite::{Connection, params};
use std::collections::HashMap;

use super::{DatGame, DatRom};

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
        "SELECT id, game_id, name, size, sha1, md5, crc32, status, merge_tag, is_disk
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
                is_disk: row.get(9)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(roms)
}

/// Every ROM for a version in one query, grouped by game id.
///
/// Replaces a per-game `get_roms_for_game` loop when the caller needs all games'
/// ROMs (completeness calculation does) — that loop issued one query per game,
/// tens of thousands for a full MAME set. Ordering within a game matches
/// `get_roms_for_game` (by name), so requirement lists are byte-for-byte the
/// same; a game with no ROMs is simply absent from the map.
pub fn get_roms_for_version_grouped(
    conn: &Connection,
    version_id: i64,
) -> Result<HashMap<i64, Vec<DatRom>>> {
    let mut stmt = conn.prepare(
        "SELECT r.id, r.game_id, r.name, r.size, r.sha1, r.md5, r.crc32, r.status, r.merge_tag, r.is_disk
           FROM dat_roms r
           JOIN dat_games g ON g.id = r.game_id
           JOIN dat_nodes n ON n.id = g.node_id
          WHERE n.version_id = ?1
          ORDER BY r.game_id, r.name",
    )?;
    let rows = stmt.query_map([version_id], |row| {
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
            is_disk: row.get(9)?,
        })
    })?;
    let mut map: HashMap<i64, Vec<DatRom>> = HashMap::new();
    for r in rows {
        let rom = r?;
        map.entry(rom.game_id).or_default().push(rom);
    }
    Ok(map)
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
        "SELECT r.id, r.game_id, r.name, r.size, r.sha1, r.md5, r.crc32, r.status, r.merge_tag, r.is_disk
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
            is_disk: row.get(9)?,
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
        "SELECT g.name, r.id, r.game_id, r.name, r.size, r.sha1, r.md5, r.crc32, r.status, r.merge_tag, r.is_disk
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
                    is_disk: row.get(10)?,
                },
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(roms)
}
