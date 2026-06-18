//! File and location CRUD operations

use anyhow::Result;
use rusqlite::{Connection, params};

/// Whether a source's content may leave it. See
/// `decisions/source-disposition.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    /// Staging: content may leave the tree and the source be freed (moved out).
    Consume,
    /// Content is never lost from the tree — reorganised within it, copied out,
    /// but never removed. The library and reference masters are `preserve`.
    Preserve,
}

impl Disposition {
    /// The canonical lowercase string stored in the database.
    pub fn as_str(self) -> &'static str {
        match self {
            Disposition::Consume => "consume",
            Disposition::Preserve => "preserve",
        }
    }

    /// Parse the stored string; unknown values fall back to the safe
    /// `Preserve` (a malformed disposition must never authorise removal).
    pub fn parse(s: &str) -> Disposition {
        match s {
            "consume" => Disposition::Consume,
            _ => Disposition::Preserve,
        }
    }
}

/// A source directory
#[derive(Debug, Clone)]
pub struct Source {
    pub id: i64,
    pub path: String,
    pub case_sensitive: bool,
    pub added_at: String,
    pub last_scanned: Option<String>,
    /// Whether this source may be consumed (emptied) or its content preserved.
    pub disposition: Disposition,
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

/// Does the inventory contain a file matching this DAT MD5?
///
/// For DAT entries that carry an MD5 but no SHA1 — notably the ZXDB-derived
/// Spectrum DAT, whose `downloads` table records only `file_md5`. MD5 is
/// collision-resistant enough to key on directly, like SHA1, so no size guard
/// is needed (unlike CRC32). Stored MD5s are uppercase, as are DAT MD5s.
pub fn has_matching_md5(conn: &Connection, md5: &str) -> Result<bool> {
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM files WHERE md5 = ?1)",
        [md5],
        |row| row.get(0),
    )?;
    Ok(exists)
}

/// Get a file by SHA1
pub fn get_file_by_sha1(conn: &Connection, sha1: &str) -> Result<Option<File>> {
    let mut stmt =
        conn.prepare("SELECT sha1, md5, crc32, size, first_seen FROM files WHERE sha1 = ?")?;

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
pub fn catalogued_paths(
    conn: &Connection,
    source_id: i64,
) -> Result<std::collections::HashSet<String>> {
    let mut stmt = conn.prepare("SELECT DISTINCT path FROM file_locations WHERE source_id = ?")?;
    let rows = stmt.query_map([source_id], |row| row.get::<_, String>(0))?;
    let mut paths = std::collections::HashSet::new();
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    fn setup_db() -> Database {
        Database::open_in_memory().unwrap()
    }

    fn src(id: i64, path: &str) -> Source {
        Source {
            id,
            path: path.to_string(),
            case_sensitive: false,
            added_at: String::new(),
            last_scanned: None,
            disposition: Disposition::Preserve,
        }
    }

    #[test]
    fn source_disposition_defaults_preserve_and_is_settable() {
        let db = setup_db();
        let conn = db.conn();
        add_source(conn, "/ToSort/X", false).unwrap();

        // A freshly added source defaults to the safe preserve.
        let s = get_source_by_path(conn, "/ToSort/X").unwrap().unwrap();
        assert_eq!(s.disposition, Disposition::Preserve);

        // Settable to consume, and read back by both accessors.
        assert!(set_source_disposition(conn, "/ToSort/X", Disposition::Consume).unwrap());
        assert_eq!(
            get_source_by_path(conn, "/ToSort/X")
                .unwrap()
                .unwrap()
                .disposition,
            Disposition::Consume
        );
        assert_eq!(
            list_sources(conn).unwrap()[0].disposition,
            Disposition::Consume
        );

        // No matching source → no update.
        assert!(!set_source_disposition(conn, "/nope", Disposition::Consume).unwrap());
    }

    #[test]
    fn resolve_in_sources_picks_longest_matching_prefix() {
        let sources = vec![
            src(1, "/lib/ROMs"),
            src(2, "/lib/ROMs/TOSEC"), // more specific — should win for paths under it
            src(3, "/lib/ToSort"),
        ];

        // Longest-prefix wins: a path under both /lib/ROMs and /lib/ROMs/TOSEC
        // resolves to the more specific source, with the right relative path.
        assert_eq!(
            resolve_in_sources(&sources, "/lib/ROMs/TOSEC/Acorn/game.zip"),
            Some((2, "Acorn/game.zip".to_string()))
        );
        assert_eq!(
            resolve_in_sources(&sources, "/lib/ROMs/Other/game.rom"),
            Some((1, "Other/game.rom".to_string()))
        );
        assert_eq!(
            resolve_in_sources(&sources, "/lib/ToSort/x.zip"),
            Some((3, "x.zip".to_string()))
        );
        // Outside every source → None (the file leaves the catalogue's view).
        assert_eq!(resolve_in_sources(&sources, "/elsewhere/x.rom"), None);
        // A path equal to a source root (no file under it) → None.
        assert_eq!(resolve_in_sources(&sources, "/lib/ROMs"), None);
    }

    #[test]
    fn test_catalogued_paths_returns_distinct_paths_per_source() {
        let db = setup_db();
        let conn = db.conn();
        let s1 = add_source(conn, "/lib/a", false).unwrap();
        let s2 = add_source(conn, "/lib/b", false).unwrap();

        upsert_file(conn, "AAAA", None, None, None, 1).unwrap();
        upsert_file(conn, "BBBB", None, None, None, 1).unwrap();
        upsert_file(conn, "CCCC", None, None, None, 1).unwrap();

        // Two loose files and a two-entry archive under source 1.
        upsert_file_location(conn, "AAAA", s1, "loose1.rom", None).unwrap();
        upsert_file_location(conn, "BBBB", s1, "pack.zip", Some("inner1.rom")).unwrap();
        upsert_file_location(conn, "CCCC", s1, "pack.zip", Some("inner2.rom")).unwrap();
        // A file under source 2 must not leak into source 1's set.
        upsert_file_location(conn, "AAAA", s2, "other.rom", None).unwrap();

        let paths = catalogued_paths(conn, s1).unwrap();
        // The archive's two entries collapse to one distinct container path.
        assert_eq!(paths.len(), 2);
        assert!(paths.contains("loose1.rom"));
        assert!(paths.contains("pack.zip"));
        assert!(!paths.contains("other.rom"));

        assert!(catalogued_paths(conn, 999).unwrap().is_empty());
    }

    #[test]
    fn test_has_matching_file_by_either_hash() {
        let db = setup_db();
        let conn = db.conn();
        // A headered file: its full-file SHA1 differs from its headerless SHA1.
        upsert_file(conn, "FULLHASH", Some("HEADERLESSHASH"), None, None, 1024).unwrap();

        // A DAT records either the headered or the headerless hash; both forms
        // must find the file, and an unrelated hash must not.
        assert!(
            has_matching_file(conn, "FULLHASH").unwrap(),
            "headered DAT hash"
        );
        assert!(
            has_matching_file(conn, "HEADERLESSHASH").unwrap(),
            "headerless DAT hash"
        );
        assert!(!has_matching_file(conn, "NOPE").unwrap(), "unknown hash");
    }

    #[test]
    fn test_has_matching_md5() {
        let db = setup_db();
        let conn = db.conn();
        // A file the scanner recorded with an MD5 (e.g. from a WoS/ZXDB-sourced
        // scan whose only shared hash with the DAT is md5).
        upsert_file(
            conn,
            "SOMESHA1",
            None,
            Some("85A60F488607FFB0DBAC35ECE7F3E79C"),
            None,
            14245,
        )
        .unwrap();

        // A ZXDB-derived DAT carries only md5; it must find the file by md5
        // alone, and an unrelated md5 must not.
        assert!(
            has_matching_md5(conn, "85A60F488607FFB0DBAC35ECE7F3E79C").unwrap(),
            "md5-only DAT entry matches by md5"
        );
        assert!(
            !has_matching_md5(conn, "00000000000000000000000000000000").unwrap(),
            "unknown md5"
        );
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
        let source = get_source_by_path(conn, "/home/user/roms")
            .unwrap()
            .unwrap();
        assert!(source.last_scanned.is_none());

        // Update scanned time
        update_source_scanned(conn, id).unwrap();

        let source = get_source_by_path(conn, "/home/user/roms")
            .unwrap()
            .unwrap();
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

        upsert_file(
            conn,
            sha1,
            None,
            Some("811B027EAF99C2DEF7B933C5208636DE"),
            Some("3337EC46"),
            40976,
        )
        .unwrap();

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
    fn rescanning_a_loose_path_replaces_the_hash_not_accumulates() {
        let db = setup_db();
        let conn = db.conn();
        let source_id = add_source(conn, "/roms", true).unwrap();
        let a = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
        let b = "BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB";
        upsert_file(conn, a, None, None, None, 100).unwrap();
        upsert_file(conn, b, None, None, None, 200).unwrap();

        // First scan sees content A at the loose path; a later scan sees the path
        // now holding content B (e.g. a flat layout overwrote it).
        upsert_file_location(conn, a, source_id, "FBN/zoop (usa).bin", None).unwrap();
        upsert_file_location(conn, b, source_id, "FBN/zoop (usa).bin", None).unwrap();

        // Exactly one row remains at that loose path, carrying the latest content —
        // not two rows that would let the planner source a stale hash.
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM file_locations
                  WHERE source_id = ?1 AND path = ?2 AND archive_path IS NULL",
                params![source_id, "FBN/zoop (usa).bin"],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            count, 1,
            "a loose path holds one content; the row is replaced"
        );
        assert!(get_file_locations(conn, a).unwrap().is_empty());
        assert_eq!(get_file_locations(conn, b).unwrap().len(), 1);
    }

    #[test]
    fn distinct_archive_entries_at_one_path_coexist() {
        let db = setup_db();
        let conn = db.conn();
        let source_id = add_source(conn, "/roms", true).unwrap();
        let a = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
        let b = "BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB";
        upsert_file(conn, a, None, None, None, 100).unwrap();
        upsert_file(conn, b, None, None, None, 200).unwrap();

        // Two different entries inside one archive are distinct rows — the loose
        // replacement must not touch the archive-entry path.
        upsert_file_location(conn, a, source_id, "games.zip", Some("a.rom")).unwrap();
        upsert_file_location(conn, b, source_id, "games.zip", Some("b.rom")).unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM file_locations WHERE source_id = ?1 AND path = ?2",
                params![source_id, "games.zip"],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 2, "distinct archive entries coexist");
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
