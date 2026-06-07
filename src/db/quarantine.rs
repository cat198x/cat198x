//! Quarantine database operations
//!
//! The quarantine holds files that were removed from their destinations
//! but shouldn't be immediately deleted. Files can be restored to sources
//! or permanently deleted (pruned).

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};

/// Reason why a file was quarantined
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuarantineReason {
    /// File exists in OLD DAT but not NEW DAT
    SetRemoved,
    /// File would be overwritten with different content
    ContentChanged,
    /// File is no longer needed at its current location
    PathChanged,
    /// Content is already held at its canonical destination by another copy
    Duplicate,
}

impl QuarantineReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            QuarantineReason::SetRemoved => "set_removed",
            QuarantineReason::ContentChanged => "content_changed",
            QuarantineReason::PathChanged => "path_changed",
            QuarantineReason::Duplicate => "duplicate",
        }
    }

    /// Parse a quarantine reason from its string representation
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "set_removed" => Some(QuarantineReason::SetRemoved),
            "content_changed" => Some(QuarantineReason::ContentChanged),
            "path_changed" => Some(QuarantineReason::PathChanged),
            "duplicate" => Some(QuarantineReason::Duplicate),
            _ => None,
        }
    }

    pub fn description(&self) -> &'static str {
        match self {
            QuarantineReason::SetRemoved => "Set no longer in active DAT",
            QuarantineReason::ContentChanged => "Would be overwritten with different content",
            QuarantineReason::PathChanged => "No longer needed at original location",
            QuarantineReason::Duplicate => "Duplicate of a copy already in the library",
        }
    }
}

/// A quarantined file entry
#[derive(Debug, Clone)]
pub struct QuarantineEntry {
    pub id: i64,
    pub sha1: String,
    pub original_path: String,
    pub quarantine_path: String,
    pub size: i64,
    pub reason: QuarantineReason,
    pub collection_name: Option<String>,
    pub quarantined_at: String,
}

/// Add a file to the quarantine registry
pub fn add_entry(
    conn: &Connection,
    sha1: &str,
    original_path: &str,
    quarantine_path: &str,
    size: i64,
    reason: QuarantineReason,
    collection_name: Option<&str>,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO quarantine (sha1, original_path, quarantine_path, size, reason, collection_name)
         VALUES (?, ?, ?, ?, ?, ?)",
        rusqlite::params![
            sha1,
            original_path,
            quarantine_path,
            size,
            reason.as_str(),
            collection_name
        ],
    )
    .context("Failed to add quarantine entry")?;

    Ok(conn.last_insert_rowid())
}

/// Remove a quarantine entry by ID
pub fn remove_entry(conn: &Connection, id: i64) -> Result<bool> {
    let rows = conn.execute("DELETE FROM quarantine WHERE id = ?", [id])?;
    Ok(rows > 0)
}

/// Remove a quarantine entry by quarantine path
pub fn remove_by_path(conn: &Connection, quarantine_path: &str) -> Result<bool> {
    let rows = conn.execute(
        "DELETE FROM quarantine WHERE quarantine_path = ?",
        [quarantine_path],
    )?;
    Ok(rows > 0)
}

/// Get a quarantine entry by ID
pub fn get_entry(conn: &Connection, id: i64) -> Result<Option<QuarantineEntry>> {
    let mut stmt = conn.prepare(
        "SELECT id, sha1, original_path, quarantine_path, size, reason, collection_name, quarantined_at
         FROM quarantine WHERE id = ?",
    )?;

    let entry = stmt
        .query_row([id], |row| {
            let reason_str: String = row.get(5)?;
            Ok(QuarantineEntry {
                id: row.get(0)?,
                sha1: row.get(1)?,
                original_path: row.get(2)?,
                quarantine_path: row.get(3)?,
                size: row.get(4)?,
                reason: QuarantineReason::parse(&reason_str)
                    .unwrap_or(QuarantineReason::PathChanged),
                collection_name: row.get(6)?,
                quarantined_at: row.get(7)?,
            })
        })
        .optional()?;

    Ok(entry)
}

/// List all quarantine entries
pub fn list_entries(conn: &Connection) -> Result<Vec<QuarantineEntry>> {
    let mut stmt = conn.prepare(
        "SELECT id, sha1, original_path, quarantine_path, size, reason, collection_name, quarantined_at
         FROM quarantine ORDER BY quarantined_at DESC",
    )?;

    let entries = stmt
        .query_map([], |row| {
            let reason_str: String = row.get(5)?;
            Ok(QuarantineEntry {
                id: row.get(0)?,
                sha1: row.get(1)?,
                original_path: row.get(2)?,
                quarantine_path: row.get(3)?,
                size: row.get(4)?,
                reason: QuarantineReason::parse(&reason_str)
                    .unwrap_or(QuarantineReason::PathChanged),
                collection_name: row.get(6)?,
                quarantined_at: row.get(7)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(entries)
}

/// List quarantine entries filtered by collection name pattern
pub fn list_entries_by_collection(
    conn: &Connection,
    collection_pattern: &str,
) -> Result<Vec<QuarantineEntry>> {
    let mut stmt = conn.prepare(
        "SELECT id, sha1, original_path, quarantine_path, size, reason, collection_name, quarantined_at
         FROM quarantine
         WHERE collection_name LIKE ?
         ORDER BY quarantined_at DESC",
    )?;

    // Convert glob to SQL LIKE pattern
    let like_pattern = collection_pattern.replace('*', "%").replace('?', "_");

    let entries = stmt
        .query_map([like_pattern], |row| {
            let reason_str: String = row.get(5)?;
            Ok(QuarantineEntry {
                id: row.get(0)?,
                sha1: row.get(1)?,
                original_path: row.get(2)?,
                quarantine_path: row.get(3)?,
                size: row.get(4)?,
                reason: QuarantineReason::parse(&reason_str)
                    .unwrap_or(QuarantineReason::PathChanged),
                collection_name: row.get(6)?,
                quarantined_at: row.get(7)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(entries)
}

/// Count quarantine entries
pub fn count_entries(conn: &Connection) -> Result<i64> {
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM quarantine", [], |row| row.get(0))?;
    Ok(count)
}

/// Get total size of quarantined files
pub fn total_size(conn: &Connection) -> Result<i64> {
    let size: i64 = conn.query_row("SELECT COALESCE(SUM(size), 0) FROM quarantine", [], |row| {
        row.get(0)
    })?;
    Ok(size)
}

/// Get quarantine summary grouped by reason
pub fn summary_by_reason(conn: &Connection) -> Result<Vec<(QuarantineReason, i64, i64)>> {
    let mut stmt = conn.prepare(
        "SELECT reason, COUNT(*), COALESCE(SUM(size), 0)
         FROM quarantine
         GROUP BY reason
         ORDER BY reason",
    )?;

    let results = stmt
        .query_map([], |row| {
            let reason_str: String = row.get(0)?;
            let count: i64 = row.get(1)?;
            let size: i64 = row.get(2)?;
            let reason =
                QuarantineReason::parse(&reason_str).unwrap_or(QuarantineReason::PathChanged);
            Ok((reason, count, size))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(results)
}

/// Get quarantine summary grouped by collection
pub fn summary_by_collection(conn: &Connection) -> Result<Vec<(Option<String>, i64, i64)>> {
    let mut stmt = conn.prepare(
        "SELECT collection_name, COUNT(*), COALESCE(SUM(size), 0)
         FROM quarantine
         GROUP BY collection_name
         ORDER BY collection_name",
    )?;

    let results = stmt
        .query_map([], |row| {
            let collection: Option<String> = row.get(0)?;
            let count: i64 = row.get(1)?;
            let size: i64 = row.get(2)?;
            Ok((collection, count, size))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    fn setup_db() -> Database {
        Database::open_in_memory().unwrap()
    }

    #[test]
    fn test_add_and_get_entry() {
        let db = setup_db();
        let conn = db.conn();

        let id = add_entry(
            conn,
            "abc123sha1",
            "/original/path/game.rom",
            "abc123sha1_game.rom",
            1024,
            QuarantineReason::SetRemoved,
            Some("MAME"),
        )
        .unwrap();

        let entry = get_entry(conn, id).unwrap().unwrap();
        assert_eq!(entry.sha1, "abc123sha1");
        assert_eq!(entry.original_path, "/original/path/game.rom");
        assert_eq!(entry.quarantine_path, "abc123sha1_game.rom");
        assert_eq!(entry.size, 1024);
        assert_eq!(entry.reason, QuarantineReason::SetRemoved);
        assert_eq!(entry.collection_name, Some("MAME".to_string()));
    }

    #[test]
    fn test_list_entries() {
        let db = setup_db();
        let conn = db.conn();

        add_entry(
            conn,
            "sha1_1",
            "/path/a.rom",
            "sha1_1_a.rom",
            100,
            QuarantineReason::SetRemoved,
            Some("MAME"),
        )
        .unwrap();

        add_entry(
            conn,
            "sha1_2",
            "/path/b.rom",
            "sha1_2_b.rom",
            200,
            QuarantineReason::ContentChanged,
            Some("TOSEC"),
        )
        .unwrap();

        let entries = list_entries(conn).unwrap();
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn test_remove_entry() {
        let db = setup_db();
        let conn = db.conn();

        let id = add_entry(
            conn,
            "sha1",
            "/path/file.rom",
            "sha1_file.rom",
            100,
            QuarantineReason::PathChanged,
            None,
        )
        .unwrap();

        assert!(remove_entry(conn, id).unwrap());
        assert!(get_entry(conn, id).unwrap().is_none());
    }

    #[test]
    fn test_count_and_total_size() {
        let db = setup_db();
        let conn = db.conn();

        add_entry(
            conn,
            "sha1",
            "/a.rom",
            "a.rom",
            100,
            QuarantineReason::SetRemoved,
            None,
        )
        .unwrap();
        add_entry(
            conn,
            "sha2",
            "/b.rom",
            "b.rom",
            200,
            QuarantineReason::SetRemoved,
            None,
        )
        .unwrap();

        assert_eq!(count_entries(conn).unwrap(), 2);
        assert_eq!(total_size(conn).unwrap(), 300);
    }

    #[test]
    fn test_summary_by_reason() {
        let db = setup_db();
        let conn = db.conn();

        add_entry(
            conn,
            "sha1",
            "/a.rom",
            "a.rom",
            100,
            QuarantineReason::SetRemoved,
            None,
        )
        .unwrap();
        add_entry(
            conn,
            "sha2",
            "/b.rom",
            "b.rom",
            200,
            QuarantineReason::SetRemoved,
            None,
        )
        .unwrap();
        add_entry(
            conn,
            "sha3",
            "/c.rom",
            "c.rom",
            50,
            QuarantineReason::ContentChanged,
            None,
        )
        .unwrap();

        let summary = summary_by_reason(conn).unwrap();
        assert_eq!(summary.len(), 2);
    }

    #[test]
    fn test_quarantine_reason_roundtrip() {
        assert_eq!(
            QuarantineReason::parse(QuarantineReason::SetRemoved.as_str()),
            Some(QuarantineReason::SetRemoved)
        );
        assert_eq!(
            QuarantineReason::parse(QuarantineReason::ContentChanged.as_str()),
            Some(QuarantineReason::ContentChanged)
        );
        assert_eq!(
            QuarantineReason::parse(QuarantineReason::PathChanged.as_str()),
            Some(QuarantineReason::PathChanged)
        );
    }
}
