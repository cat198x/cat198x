use anyhow::Result;
use rusqlite::{Connection, params};

use super::{Disposition, Source};

/// Add a source directory
pub fn add_source(conn: &Connection, path: &str, case_sensitive: bool) -> Result<i64> {
    conn.execute(
        "INSERT INTO sources (path, case_sensitive) VALUES (?, ?)",
        params![path, case_sensitive],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Remove a source directory
pub fn remove_source(conn: &Connection, path: &str) -> Result<bool> {
    let deleted = conn.execute("DELETE FROM sources WHERE path = ?", [path])?;
    Ok(deleted > 0)
}

/// Set a source's disposition by path. Returns whether a source matched.
pub fn set_source_disposition(
    conn: &Connection,
    path: &str,
    disposition: Disposition,
) -> Result<bool> {
    let updated = conn.execute(
        "UPDATE sources SET disposition = ?1 WHERE path = ?2",
        params![disposition.as_str(), path],
    )?;
    Ok(updated > 0)
}

/// Get a source by path
pub fn get_source_by_path(conn: &Connection, path: &str) -> Result<Option<Source>> {
    let mut stmt = conn.prepare(
        "SELECT id, path, case_sensitive, added_at, last_scanned, disposition \
         FROM sources WHERE path = ?",
    )?;

    let result = stmt.query_row([path], row_to_source);

    match result {
        Ok(s) => Ok(Some(s)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// List all sources
pub fn list_sources(conn: &Connection) -> Result<Vec<Source>> {
    let mut stmt = conn.prepare(
        "SELECT id, path, case_sensitive, added_at, last_scanned, disposition \
         FROM sources ORDER BY path",
    )?;

    let sources = stmt
        .query_map([], row_to_source)?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(sources)
}

/// Build a `Source` from a row selecting
/// `id, path, case_sensitive, added_at, last_scanned, disposition`.
fn row_to_source(row: &rusqlite::Row) -> rusqlite::Result<Source> {
    Ok(Source {
        id: row.get(0)?,
        path: row.get(1)?,
        case_sensitive: row.get(2)?,
        added_at: row.get(3)?,
        last_scanned: row.get(4)?,
        disposition: Disposition::parse(&row.get::<_, String>(5)?),
    })
}

/// Update last scanned time for a source
pub fn update_source_scanned(conn: &Connection, source_id: i64) -> Result<()> {
    conn.execute(
        "UPDATE sources SET last_scanned = datetime('now') WHERE id = ?",
        [source_id],
    )?;
    Ok(())
}

/// Resolve an absolute path to the source that contains it (the longest matching
/// source prefix) and the path relative to that source. `None` when no source
/// contains the path. Pure over `sources`, so callers list sources once and
/// resolve many paths without re-querying.
pub fn resolve_in_sources(sources: &[Source], abs_path: &str) -> Option<(i64, String)> {
    sources
        .iter()
        .filter_map(|s| {
            let root = s.path.trim_end_matches('/');
            abs_path
                .strip_prefix(root)
                .and_then(|rest| rest.strip_prefix('/'))
                .map(|rel| (root.len(), s.id, rel.to_string()))
        })
        .max_by_key(|(prefix_len, _, _)| *prefix_len)
        .map(|(_, id, rel)| (id, rel))
}
