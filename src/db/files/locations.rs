use std::collections::HashSet;

use anyhow::Result;
use rusqlite::{Connection, params};

use super::FileLocation;

/// Add or update a file location.
///
/// A loose path physically holds exactly one content, so re-scanning it must
/// *replace* the recorded hash. The `ON CONFLICT(source_id, path, archive_path)`
/// upsert handles that for archive entries, but **not** for loose files:
/// SQLite's UNIQUE index treats NULL `archive_path` values as distinct, so two
/// rows with the same `(source_id, path)` and a NULL `archive_path` never
/// conflict — a re-scan of a path whose content changed would accumulate a second
/// row beside the stale one. (That gap is what let two different ROMs share one
/// loose path in the catalogue and collide on repack.) So for a loose file,
/// delete any existing row at this `(source_id, path)` first, then insert.
pub fn upsert_file_location(
    conn: &Connection,
    sha1: &str,
    source_id: i64,
    path: &str,
    archive_path: Option<&str>,
) -> Result<i64> {
    match archive_path {
        None => {
            conn.execute(
                "DELETE FROM file_locations
                  WHERE source_id = ?1 AND path = ?2 AND archive_path IS NULL",
                params![source_id, path],
            )?;
            conn.execute(
                "INSERT INTO file_locations (sha1, source_id, path, archive_path)
                 VALUES (?1, ?2, ?3, NULL)",
                params![sha1, source_id, path],
            )?;
        }
        Some(ap) => {
            conn.execute(
                "INSERT INTO file_locations (sha1, source_id, path, archive_path) VALUES (?, ?, ?, ?)
                 ON CONFLICT(source_id, path, archive_path) DO UPDATE SET
                    sha1 = excluded.sha1,
                    last_seen = datetime('now')",
                params![sha1, source_id, path, ap],
            )?;
        }
    }
    Ok(conn.last_insert_rowid())
}

/// Move every file-location row for a container — a loose file, or all entries
/// of an archive — from one (source, path) to another. Used after a move or
/// relocate so the catalogue reflects the file's new home (and a re-plan
/// converges without a re-scan). Returns the number of rows moved.
pub fn relocate_locations(
    conn: &Connection,
    old_source_id: i64,
    old_path: &str,
    new_source_id: i64,
    new_path: &str,
) -> Result<usize> {
    let n = conn.execute(
        "UPDATE file_locations SET source_id = ?1, path = ?2 WHERE source_id = ?3 AND path = ?4",
        params![new_source_id, new_path, old_source_id, old_path],
    )?;
    Ok(n)
}

/// Remove every file-location row at a (source, path) — used after the file
/// leaves the tracked sources (quarantine or delete). Returns rows removed.
pub fn remove_locations_at(conn: &Connection, source_id: i64, path: &str) -> Result<usize> {
    let n = conn.execute(
        "DELETE FROM file_locations WHERE source_id = ?1 AND path = ?2",
        params![source_id, path],
    )?;
    Ok(n)
}

/// Get all locations for a file
pub fn get_file_locations(conn: &Connection, sha1: &str) -> Result<Vec<FileLocation>> {
    let mut stmt = conn.prepare(
        "SELECT id, sha1, source_id, path, archive_path, last_seen
         FROM file_locations WHERE sha1 = ?",
    )?;

    let locations = stmt
        .query_map([sha1], |row| {
            Ok(FileLocation {
                id: row.get(0)?,
                sha1: row.get(1)?,
                source_id: row.get(2)?,
                path: row.get(3)?,
                archive_path: row.get(4)?,
                last_seen: row.get(5)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(locations)
}

/// Count files in a source
pub fn count_files_in_source(conn: &Connection, source_id: i64) -> Result<i64> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(DISTINCT sha1) FROM file_locations WHERE source_id = ?",
        [source_id],
        |row| row.get(0),
    )?;
    Ok(count)
}

/// Collect the relative paths already catalogued for a source.
///
/// Used by incremental scans to pick up files that exist on disk but were
/// never recorded — added with an older mtime, or missed when an earlier scan
/// was interrupted — which the modified-since-last-scan filter alone skips.
pub fn catalogued_paths(conn: &Connection, source_id: i64) -> Result<HashSet<String>> {
    let mut stmt = conn.prepare("SELECT DISTINCT path FROM file_locations WHERE source_id = ?")?;
    let rows = stmt.query_map([source_id], |row| row.get::<_, String>(0))?;
    let mut paths = HashSet::new();
    for path in rows {
        paths.insert(path?);
    }
    Ok(paths)
}

/// Every distinct content SHA1 catalogued at a physical path within a source.
///
/// A loose file holds one content; an archive holds one per entry. Used to check
/// that deleting a path can't destroy the only copy of any content it holds.
pub fn contents_at_location(conn: &Connection, source_id: i64, path: &str) -> Result<Vec<String>> {
    let mut stmt = conn
        .prepare("SELECT DISTINCT sha1 FROM file_locations WHERE source_id = ?1 AND path = ?2")?;
    let rows = stmt.query_map(params![source_id, path], |row| row.get::<_, String>(0))?;
    let mut out = Vec::new();
    for sha1 in rows {
        out.push(sha1?);
    }
    Ok(out)
}

/// Remove stale file locations (not seen since a given time)
pub fn remove_stale_locations(conn: &Connection, source_id: i64, before: &str) -> Result<i64> {
    let deleted = conn.execute(
        "DELETE FROM file_locations WHERE source_id = ? AND last_seen < ?",
        params![source_id, before],
    )?;
    Ok(deleted as i64)
}
