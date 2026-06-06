//! `unknowns` command — report scanned files matched by no active DAT.
//!
//! Read-only on purpose. Quarantining unknowns is deliberately *not* wired in
//! here: a source with no DAT coverage (an archive kept for reference) would
//! have every file reported as unknown, so sweeping them automatically would be
//! a footgun. This surfaces the list for review instead.

use anyhow::{Context, Result};
use rusqlite::Connection;
use std::collections::BTreeMap;
use std::path::PathBuf;

use super::{get_data_dir, open_database};

/// A scanned loose file matched by no active DAT: (source root, relative path, size).
type UnknownFile = (String, String, i64);

/// Find loose files matched by no active DAT rom. Mirrors the scan/plan match
/// logic — SHA1, headerless SHA1, or CRC+size — so a file matched only on CRC
/// (CRC-only DATs) is not falsely reported as unknown.
fn find_unknown_files(conn: &Connection) -> Result<Vec<UnknownFile>> {
    let mut stmt = conn.prepare(
        "SELECT s.path AS source_root, fl.path AS rel_path, f.size
         FROM file_locations fl
         JOIN files f ON fl.sha1 = f.sha1
         JOIN sources s ON fl.source_id = s.id
         WHERE fl.archive_path IS NULL
           AND NOT EXISTS (
             SELECT 1 FROM dat_roms r
             JOIN dat_games g ON r.game_id = g.id
             JOIN dat_nodes n ON g.node_id = n.id
             JOIN collection_versions cv ON n.version_id = cv.id AND cv.is_active = 1
             WHERE (r.sha1 IS NOT NULL AND (r.sha1 = f.sha1 OR r.sha1 = f.sha1_no_header))
                OR (r.sha1 IS NULL AND r.crc32 IS NOT NULL
                    AND r.crc32 = f.crc32 AND r.size = f.size)
           )
         ORDER BY s.path, fl.path",
    )?;

    let rows = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Run the unknowns command.
pub fn run(data_dir: Option<PathBuf>) -> Result<()> {
    let db = open_database(data_dir.clone())?;
    let unknowns = find_unknown_files(db.conn())?;

    if unknowns.is_empty() {
        println!("No unknown files: every scanned loose file matches an active DAT.");
        return Ok(());
    }

    // Per-source rollup and the full path list.
    let mut by_source: BTreeMap<&str, (usize, u64)> = BTreeMap::new();
    let mut lines: Vec<String> = Vec::with_capacity(unknowns.len());
    for (root, rel, size) in &unknowns {
        let entry = by_source.entry(root.as_str()).or_default();
        entry.0 += 1;
        entry.1 += *size as u64;
        lines.push(format!("{}/{}", root.trim_end_matches('/'), rel));
    }

    println!("Unknown files (matched by no active DAT):");
    println!();
    for (source, (count, bytes)) in &by_source {
        println!(
            "  {:40}  {} files, {}",
            source,
            count,
            crate::util::format_bytes(*bytes)
        );
    }
    println!();
    println!("Total: {} files", unknowns.len());

    let out = get_data_dir(data_dir)?.join("unknown-files.txt");
    lines.push(String::new()); // trailing newline
    std::fs::write(&out, lines.join("\n")).context("Failed to write unknown-files list")?;
    println!("Full list written to: {}", out.display());
    println!();
    println!("Note: a source with no DAT coverage lists all its files here. Review");
    println!("before acting — these are reported, not touched.");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    #[test]
    fn find_unknown_files_excludes_matched_and_archive_entries() {
        let db = Database::open_in_memory().unwrap();
        let conn = db.conn();

        // Two scanned loose files: one matched by an active DAT, one not.
        conn.execute(
            "INSERT INTO sources (id, path, case_sensitive) VALUES (1, '/lib', 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO files (sha1, size, crc32) VALUES ('MATCHED', 10, 'AAAA'), ('STRAY', 20, 'BBBB')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO file_locations (sha1, source_id, path, archive_path) VALUES
                ('MATCHED', 1, 'good.rom', NULL),
                ('STRAY', 1, 'mystery.rom', NULL),
                ('STRAY', 1, 'inside.zip', 'entry.rom')",
            [],
        )
        .unwrap();

        // An active DAT covering only MATCHED.
        conn.execute(
            "INSERT INTO collections (id, name, source_type) VALUES (1, 'C', 'tosec')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO collection_versions (id, collection_id, version, dat_path, is_active)
             VALUES (1, 1, '1', '/x.dat', 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO dat_nodes (id, version_id, name, node_type, path) VALUES (1, 1, 'C', 'dat', 'C')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO dat_games (id, node_id, name) VALUES (1, 1, 'g')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO dat_roms (game_id, name, size, sha1) VALUES (1, 'r', 10, 'MATCHED')",
            [],
        )
        .unwrap();

        let unknown = find_unknown_files(conn).unwrap();
        // Only the loose, unmatched file — not the matched one, not the archive entry.
        assert_eq!(unknown.len(), 1);
        assert_eq!(unknown[0].1, "mystery.rom");
    }
}
