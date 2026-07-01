use anyhow::Result;
use rusqlite::{Connection, params};

use super::File;

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
