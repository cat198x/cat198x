//! File and location CRUD operations

use anyhow::Result;
use rusqlite::{params, Connection};

/// A source directory
#[derive(Debug, Clone)]
pub struct Source {
    pub id: i64,
    pub path: String,
    pub case_sensitive: bool,
    pub added_at: String,
    pub last_scanned: Option<String>,
}

/// A content-addressed file
#[derive(Debug, Clone)]
pub struct File {
    pub sha1: String,
    pub md5: Option<String>,
    pub crc32: Option<String>,
    pub size: i64,
    pub first_seen: String,
}

/// A physical location where a file exists
#[derive(Debug, Clone)]
pub struct FileLocation {
    pub id: i64,
    pub sha1: String,
    pub source_id: i64,
    pub path: String,
    pub archive_path: Option<String>,
    pub last_seen: String,
}

// === Source operations ===

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

/// Get a source by path
pub fn get_source_by_path(conn: &Connection, path: &str) -> Result<Option<Source>> {
    let mut stmt = conn.prepare(
        "SELECT id, path, case_sensitive, added_at, last_scanned FROM sources WHERE path = ?",
    )?;

    let result = stmt.query_row([path], |row| {
        Ok(Source {
            id: row.get(0)?,
            path: row.get(1)?,
            case_sensitive: row.get(2)?,
            added_at: row.get(3)?,
            last_scanned: row.get(4)?,
        })
    });

    match result {
        Ok(s) => Ok(Some(s)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// List all sources
pub fn list_sources(conn: &Connection) -> Result<Vec<Source>> {
    let mut stmt = conn.prepare(
        "SELECT id, path, case_sensitive, added_at, last_scanned FROM sources ORDER BY path",
    )?;

    let sources = stmt
        .query_map([], |row| {
            Ok(Source {
                id: row.get(0)?,
                path: row.get(1)?,
                case_sensitive: row.get(2)?,
                added_at: row.get(3)?,
                last_scanned: row.get(4)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(sources)
}

/// Update last scanned time for a source
pub fn update_source_scanned(conn: &Connection, source_id: i64) -> Result<()> {
    conn.execute(
        "UPDATE sources SET last_scanned = datetime('now') WHERE id = ?",
        [source_id],
    )?;
    Ok(())
}

// === File operations ===

/// Insert or update a file record
pub fn upsert_file(
    conn: &Connection,
    sha1: &str,
    sha1_no_header: Option<&str>,
    md5: Option<&str>,
    crc32: Option<&str>,
    size: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO files (sha1, sha1_no_header, md5, crc32, size) VALUES (?, ?, ?, ?, ?)
         ON CONFLICT(sha1) DO UPDATE SET
             sha1_no_header = COALESCE(excluded.sha1_no_header, files.sha1_no_header),
             md5 = COALESCE(excluded.md5, files.md5),
             crc32 = COALESCE(excluded.crc32, files.crc32)",
        params![sha1, sha1_no_header, md5, crc32, size],
    )?;
    Ok(())
}

/// Does the inventory contain a file matching this DAT SHA1?
///
/// A DAT records either the headered or the headerless hash, so this matches
/// against both `sha1` (the full-file hash) and `sha1_no_header`. This is the
/// single source of truth for "do we have this ROM?" — used by `status` and by
/// the merge-mode completeness calculation, so the predicate can't drift
/// between them.
pub fn has_matching_file(conn: &Connection, dat_sha1: &str) -> Result<bool> {
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM files WHERE sha1 = ?1 OR sha1_no_header = ?1)",
        [dat_sha1],
        |row| row.get(0),
    )?;
    Ok(exists)
}

/// Does the inventory contain a file matching this CRC32 + size?
///
/// For DAT entries that carry only a CRC (no SHA1). Size is required alongside
/// the CRC because CRC32 collides far more readily than SHA1.
pub fn has_matching_crc_size(conn: &Connection, crc32: &str, size: i64) -> Result<bool> {
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM files WHERE crc32 = ?1 AND size = ?2)",
        params![crc32, size],
        |row| row.get(0),
    )?;
    Ok(exists)
}

/// Get a file by SHA1
pub fn get_file_by_sha1(conn: &Connection, sha1: &str) -> Result<Option<File>> {
    let mut stmt = conn.prepare(
        "SELECT sha1, md5, crc32, size, first_seen FROM files WHERE sha1 = ?",
    )?;

    let result = stmt.query_row([sha1], |row| {
        Ok(File {
            sha1: row.get(0)?,
            md5: row.get(1)?,
            crc32: row.get(2)?,
            size: row.get(3)?,
            first_seen: row.get(4)?,
        })
    });

    match result {
        Ok(f) => Ok(Some(f)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

// === File location operations ===

/// Add or update a file location
pub fn upsert_file_location(
    conn: &Connection,
    sha1: &str,
    source_id: i64,
    path: &str,
    archive_path: Option<&str>,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO file_locations (sha1, source_id, path, archive_path) VALUES (?, ?, ?, ?)
         ON CONFLICT(source_id, path, archive_path) DO UPDATE SET
            sha1 = excluded.sha1,
            last_seen = datetime('now')",
        params![sha1, source_id, path, archive_path],
    )?;
    Ok(conn.last_insert_rowid())
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

/// Remove stale file locations (not seen since a given time)
pub fn remove_stale_locations(conn: &Connection, source_id: i64, before: &str) -> Result<i64> {
    let deleted = conn.execute(
        "DELETE FROM file_locations WHERE source_id = ? AND last_seen < ?",
        params![source_id, before],
    )?;
    Ok(deleted as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    fn setup_db() -> Database {
        Database::open_in_memory().unwrap()
    }

    #[test]
    fn test_has_matching_file_by_either_hash() {
        let db = setup_db();
        let conn = db.conn();
        // A headered file: its full-file SHA1 differs from its headerless SHA1.
        upsert_file(conn, "FULLHASH", Some("HEADERLESSHASH"), None, None, 1024).unwrap();

        // A DAT records either the headered or the headerless hash; both forms
        // must find the file, and an unrelated hash must not.
        assert!(has_matching_file(conn, "FULLHASH").unwrap(), "headered DAT hash");
        assert!(has_matching_file(conn, "HEADERLESSHASH").unwrap(), "headerless DAT hash");
        assert!(!has_matching_file(conn, "NOPE").unwrap(), "unknown hash");
    }

    // === Source tests ===

    #[test]
    fn test_add_and_get_source() {
        let db = setup_db();
        let conn = db.conn();

        let id = add_source(conn, "/home/user/roms", true).unwrap();
        assert!(id > 0);

        let source = get_source_by_path(conn, "/home/user/roms").unwrap();
        assert!(source.is_some());

        let s = source.unwrap();
        assert_eq!(s.id, id);
        assert_eq!(s.path, "/home/user/roms");
        assert!(s.case_sensitive);
        assert!(s.last_scanned.is_none());
    }

    #[test]
    fn test_get_nonexistent_source() {
        let db = setup_db();
        let conn = db.conn();

        let source = get_source_by_path(conn, "/does/not/exist").unwrap();
        assert!(source.is_none());
    }

    #[test]
    fn test_list_sources() {
        let db = setup_db();
        let conn = db.conn();

        add_source(conn, "/path/z", false).unwrap();
        add_source(conn, "/path/a", true).unwrap();
        add_source(conn, "/path/m", true).unwrap();

        let sources = list_sources(conn).unwrap();
        assert_eq!(sources.len(), 3);

        // Should be sorted by path
        assert_eq!(sources[0].path, "/path/a");
        assert_eq!(sources[1].path, "/path/m");
        assert_eq!(sources[2].path, "/path/z");
    }

    #[test]
    fn test_remove_source() {
        let db = setup_db();
        let conn = db.conn();

        add_source(conn, "/home/user/roms", true).unwrap();

        let removed = remove_source(conn, "/home/user/roms").unwrap();
        assert!(removed);

        let source = get_source_by_path(conn, "/home/user/roms").unwrap();
        assert!(source.is_none());

        // Removing non-existent should return false
        let removed = remove_source(conn, "/home/user/roms").unwrap();
        assert!(!removed);
    }

    #[test]
    fn test_update_source_scanned() {
        let db = setup_db();
        let conn = db.conn();

        let id = add_source(conn, "/home/user/roms", true).unwrap();

        // Initially no last_scanned
        let source = get_source_by_path(conn, "/home/user/roms").unwrap().unwrap();
        assert!(source.last_scanned.is_none());

        // Update scanned time
        update_source_scanned(conn, id).unwrap();

        let source = get_source_by_path(conn, "/home/user/roms").unwrap().unwrap();
        assert!(source.last_scanned.is_some());
    }

    #[test]
    fn test_duplicate_source_path_fails() {
        let db = setup_db();
        let conn = db.conn();

        add_source(conn, "/home/user/roms", true).unwrap();
        let result = add_source(conn, "/home/user/roms", false);

        assert!(result.is_err());
    }

    // === File tests ===

    #[test]
    fn test_upsert_and_get_file() {
        let db = setup_db();
        let conn = db.conn();

        let sha1 = "FACEE9C577A5262DBE33AC4930BB0B58C8C037F7";

        upsert_file(conn, sha1, None, Some("811B027EAF99C2DEF7B933C5208636DE"), Some("3337EC46"), 40976).unwrap();

        let file = get_file_by_sha1(conn, sha1).unwrap();
        assert!(file.is_some());

        let f = file.unwrap();
        assert_eq!(f.sha1, sha1);
        assert_eq!(f.md5, Some("811B027EAF99C2DEF7B933C5208636DE".to_string()));
        assert_eq!(f.crc32, Some("3337EC46".to_string()));
        assert_eq!(f.size, 40976);
    }

    #[test]
    fn test_upsert_file_updates_hashes() {
        let db = setup_db();
        let conn = db.conn();

        let sha1 = "ABCD1234567890ABCDEF1234567890ABCDEF1234";

        // Insert with only SHA1
        upsert_file(conn, sha1, None, None, None, 1000).unwrap();

        let file = get_file_by_sha1(conn, sha1).unwrap().unwrap();
        assert!(file.md5.is_none());
        assert!(file.crc32.is_none());

        // Update with MD5 and CRC32
        upsert_file(conn, sha1, None, Some("DEADBEEF"), Some("12345678"), 1000).unwrap();

        let file = get_file_by_sha1(conn, sha1).unwrap().unwrap();
        assert_eq!(file.md5, Some("DEADBEEF".to_string()));
        assert_eq!(file.crc32, Some("12345678".to_string()));
    }

    #[test]
    fn test_get_nonexistent_file() {
        let db = setup_db();
        let conn = db.conn();

        let file = get_file_by_sha1(conn, "DOESNOTEXIST").unwrap();
        assert!(file.is_none());
    }

    // === File location tests ===

    #[test]
    fn test_upsert_and_get_file_location() {
        let db = setup_db();
        let conn = db.conn();

        let sha1 = "FACEE9C577A5262DBE33AC4930BB0B58C8C037F7";
        let source_id = add_source(conn, "/roms", true).unwrap();
        upsert_file(conn, sha1, None, None, None, 40976).unwrap();

        // Add loose file location
        upsert_file_location(conn, sha1, source_id, "nes/mario.nes", None).unwrap();

        let locations = get_file_locations(conn, sha1).unwrap();
        assert_eq!(locations.len(), 1);
        assert_eq!(locations[0].path, "nes/mario.nes");
        assert!(locations[0].archive_path.is_none());
    }

    #[test]
    fn test_file_location_in_archive() {
        let db = setup_db();
        let conn = db.conn();

        let sha1 = "FACEE9C577A5262DBE33AC4930BB0B58C8C037F7";
        let source_id = add_source(conn, "/roms", true).unwrap();
        upsert_file(conn, sha1, None, None, None, 40976).unwrap();

        // Add file inside archive
        upsert_file_location(conn, sha1, source_id, "games.zip", Some("mario.nes")).unwrap();

        let locations = get_file_locations(conn, sha1).unwrap();
        assert_eq!(locations.len(), 1);
        assert_eq!(locations[0].path, "games.zip");
        assert_eq!(locations[0].archive_path, Some("mario.nes".to_string()));
    }

    #[test]
    fn test_multiple_locations_for_same_file() {
        let db = setup_db();
        let conn = db.conn();

        let sha1 = "FACEE9C577A5262DBE33AC4930BB0B58C8C037F7";
        let source1 = add_source(conn, "/roms1", true).unwrap();
        let source2 = add_source(conn, "/roms2", true).unwrap();
        upsert_file(conn, sha1, None, None, None, 40976).unwrap();

        upsert_file_location(conn, sha1, source1, "mario.nes", None).unwrap();
        upsert_file_location(conn, sha1, source2, "backup/mario.nes", None).unwrap();
        upsert_file_location(conn, sha1, source1, "games.zip", Some("mario.nes")).unwrap();

        let locations = get_file_locations(conn, sha1).unwrap();
        assert_eq!(locations.len(), 3);
    }

    #[test]
    fn test_count_files_in_source() {
        let db = setup_db();
        let conn = db.conn();

        let source_id = add_source(conn, "/roms", true).unwrap();

        // Add 3 unique files
        upsert_file(conn, "SHA1_FILE1", None, None, None, 100).unwrap();
        upsert_file(conn, "SHA1_FILE2", None, None, None, 200).unwrap();
        upsert_file(conn, "SHA1_FILE3", None, None, None, 300).unwrap();

        upsert_file_location(conn, "SHA1_FILE1", source_id, "file1.rom", None).unwrap();
        upsert_file_location(conn, "SHA1_FILE2", source_id, "file2.rom", None).unwrap();
        upsert_file_location(conn, "SHA1_FILE3", source_id, "file3.rom", None).unwrap();
        // Same file in different location (shouldn't increase count)
        upsert_file_location(conn, "SHA1_FILE1", source_id, "backup/file1.rom", None).unwrap();

        let count = count_files_in_source(conn, source_id).unwrap();
        assert_eq!(count, 3); // 3 unique SHA1s, not 4 locations
    }

    #[test]
    fn test_remove_stale_locations() {
        let db = setup_db();
        let conn = db.conn();

        let source_id = add_source(conn, "/roms", true).unwrap();
        upsert_file(conn, "SHA1_FILE1", None, None, None, 100).unwrap();

        upsert_file_location(conn, "SHA1_FILE1", source_id, "file1.rom", None).unwrap();

        // Remove locations older than a future date (should remove all)
        let deleted = remove_stale_locations(conn, source_id, "2099-01-01 00:00:00").unwrap();
        assert_eq!(deleted, 1);

        let locations = get_file_locations(conn, "SHA1_FILE1").unwrap();
        assert_eq!(locations.len(), 0);
    }
}
