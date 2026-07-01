use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};

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

/// Correct a version's root DAT node name after a parser fix (`dat repair-names`).
///
/// Always updates the node `name`. The `path` is only rewritten when it still
/// mirrors the old name — i.e. it was the flat-add fallback (`node_path =
/// header.name`). A real recorded hierarchy path (e.g. `Acorn/BBC/…`) never
/// equals the bare name, so it is left untouched. SQLite evaluates every
/// assignment's right-hand side against the row's original values, so the
/// `CASE WHEN path = name` test sees the pre-update name.
pub fn rename_dat_node(conn: &Connection, version_id: i64, new_name: &str) -> Result<()> {
    conn.execute(
        "UPDATE dat_nodes
            SET path = CASE WHEN path = name THEN ?1 ELSE path END,
                name = ?1
          WHERE version_id = ?2 AND parent_id IS NULL",
        params![new_name, version_id],
    )?;
    Ok(())
}

/// The `path` of a version's DAT node — the collection's place in the library
/// tree, recorded by recursive `dat add` (e.g. `Acorn/BBC/Magazines/Laserbug`),
/// or the flat collection name otherwise. `None` if the version has no node.
pub fn primary_node_path(conn: &Connection, version_id: i64) -> Result<Option<String>> {
    let path = conn
        .query_row(
            "SELECT path FROM dat_nodes WHERE version_id = ? ORDER BY id LIMIT 1",
            [version_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    Ok(path)
}

/// Nest a version's primary node under its own name: `path` becomes `path/name`
/// (e.g. `MAME/Software List` → `MAME/Software List/32x`).
///
/// Recursive `dat add` records the *directory* a DAT was found in as the node
/// path, so sibling DATs in one directory share a path and collide on their
/// destination root. Appending the node's own name gives each a distinct
/// destination — and a distinct library-tree node. Returns the new path, or
/// `None` if the version has no node. Idempotent only by inspection: calling it
/// twice nests twice, so callers gate on a real collision.
pub fn nest_primary_node_under_name(conn: &Connection, version_id: i64) -> Result<Option<String>> {
    let node = conn
        .query_row(
            "SELECT id, path, name FROM dat_nodes WHERE version_id = ? ORDER BY id LIMIT 1",
            [version_id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?;
    let Some((id, path, name)) = node else {
        return Ok(None);
    };
    let new_path = format!("{path}/{name}");
    conn.execute(
        "UPDATE dat_nodes SET path = ?1 WHERE id = ?2",
        params![new_path, id],
    )?;
    Ok(Some(new_path))
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
    // INSERT OR IGNORE because a DAT can list the same game name twice. TOSEC's
    // ISO sets in particular contain accidental double-listings — the second
    // <game> is byte-identical to the first (same description, same ROM CRC/MD5/
    // SHA1), e.g. "CPC Games CD, The" in the IBM PC CD compilations DAT and
    // "Smickeonn - The Game" in the Dreamcast homebrew DAT. A plain INSERT trips
    // UNIQUE(node_id, name) and aborts the whole DAT import, silently dropping
    // the entire collection. Skipping the duplicate keeps completeness correct:
    // the identical ROM is already catalogued, and the caller's create_rom calls
    // (also INSERT OR IGNORE) collapse onto the existing game row.
    let inserted = conn.execute(
        "INSERT OR IGNORE INTO dat_games (node_id, name, description, parent_name, is_bios, is_device, is_mechanical)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
        params![node_id, name, description, parent_name, is_bios, is_device, is_mechanical],
    )?;
    if inserted == 0 {
        // Already present — return the existing row's id so any ROMs the
        // duplicate listing carries attach to the real game rather than a stale
        // last_insert_rowid from an unrelated prior insert.
        return Ok(conn.query_row(
            "SELECT id FROM dat_games WHERE node_id = ? AND name = ?",
            params![node_id, name],
            |row| row.get(0),
        )?);
    }
    Ok(conn.last_insert_rowid())
}

/// Create a ROM entry (a `<rom>`).
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
    insert_dat_rom(
        conn, game_id, name, size, sha1, md5, crc32, status, merge_tag, false,
    )
}

/// Create a disk entry (a `<disk>` / CHD). A disk has no size or CRC, and its
/// `sha1` is the CHD's internal logical-data hash, not the `.chd` file hash.
pub fn create_disk(
    conn: &Connection,
    game_id: i64,
    name: &str,
    sha1: Option<&str>,
    md5: Option<&str>,
    status: &str,
    merge_tag: Option<&str>,
) -> Result<i64> {
    insert_dat_rom(
        conn, game_id, name, 0, sha1, md5, None, status, merge_tag, true,
    )
}

/// Insert a `dat_roms` row for either a `<rom>` or a `<disk>` (`is_disk`).
#[allow(clippy::too_many_arguments)]
fn insert_dat_rom(
    conn: &Connection,
    game_id: i64,
    name: &str,
    size: i64,
    sha1: Option<&str>,
    md5: Option<&str>,
    crc32: Option<&str>,
    status: &str,
    merge_tag: Option<&str>,
    is_disk: bool,
) -> Result<i64> {
    // INSERT OR IGNORE because a game can legitimately list the same ROM name
    // twice — MAME/FBNeo arcade and console DATs repeat a shared BIOS/merge ROM
    // (identical name, size and CRC) across a parent and its merge entries. A
    // plain INSERT trips the UNIQUE(game_id, name) constraint and aborts the
    // whole DAT import (this silently dropped FBNeo's arcade.dat and msx.dat).
    // The duplicate is the same file, so skipping it leaves completeness correct.
    let inserted = conn.execute(
        "INSERT OR IGNORE INTO dat_roms (game_id, name, size, sha1, md5, crc32, status, merge_tag, is_disk)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![game_id, name, size, sha1, md5, crc32, status, merge_tag, is_disk],
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
