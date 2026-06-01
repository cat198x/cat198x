//! Stats command - show overall statistics across all collections

use anyhow::Result;
use std::path::PathBuf;

use super::open_database;
use crate::db::{collections, files};

/// Collection statistics
#[derive(Debug)]
#[allow(dead_code)] // total_games reserved for future "games complete" display
struct CollectionStats {
    name: String,
    total_games: i64,
    total_roms: i64,
    have_roms: i64,
    total_size: i64,
    have_size: i64,
}

/// Overall statistics
#[derive(Debug)]
struct OverallStats {
    collections: Vec<CollectionStats>,
    total_files_scanned: i64,
    total_sources: i64,
    quarantine_count: i64,
    quarantine_size: i64,
}

/// Run the stats command
pub fn run(data_dir: Option<PathBuf>) -> Result<()> {
    let db = open_database(data_dir)?;
    let conn = db.conn();

    let stats = gather_stats(conn)?;

    print_stats(&stats);

    Ok(())
}

/// Gather all statistics from the database
fn gather_stats(conn: &rusqlite::Connection) -> Result<OverallStats> {
    let mut collection_stats = Vec::new();

    // Get all active collection versions
    let collections = collections::list_collections(conn)?;

    for coll in collections {
        if let Some(version) = collections::get_active_version(conn, coll.id)? {
            let stats = get_collection_stats(conn, &coll.name, version.id)?;
            collection_stats.push(stats);
        }
    }

    // Get source and file counts
    let sources = files::list_sources(conn)?;
    let total_sources = sources.len() as i64;

    let total_files_scanned: i64 = conn.query_row(
        "SELECT COUNT(*) FROM files",
        [],
        |row| row.get(0),
    )?;

    // Get quarantine stats
    let (quarantine_count, quarantine_size): (i64, i64) = conn.query_row(
        "SELECT COUNT(*), COALESCE(SUM(size), 0) FROM quarantine",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;

    Ok(OverallStats {
        collections: collection_stats,
        total_files_scanned,
        total_sources,
        quarantine_count,
        quarantine_size,
    })
}

/// Get statistics for a single collection
fn get_collection_stats(
    conn: &rusqlite::Connection,
    collection_name: &str,
    version_id: i64,
) -> Result<CollectionStats> {
    // Count total games (excluding BIOS and devices)
    let total_games: i64 = conn.query_row(
        "SELECT COUNT(DISTINCT g.id)
         FROM dat_games g
         JOIN dat_nodes n ON g.node_id = n.id
         WHERE n.version_id = ?
         AND g.is_bios = 0
         AND g.is_device = 0",
        [version_id],
        |row| row.get(0),
    )?;

    // Count total ROMs and their size (excluding nodump)
    let (total_roms, total_size): (i64, i64) = conn.query_row(
        "SELECT COUNT(*), COALESCE(SUM(r.size), 0)
         FROM dat_roms r
         JOIN dat_games g ON r.game_id = g.id
         JOIN dat_nodes n ON g.node_id = n.id
         WHERE n.version_id = ?
         AND r.status != 'nodump'
         AND g.is_bios = 0
         AND g.is_device = 0",
        [version_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;

    // Count ROMs we have (matched by SHA1)
    let (have_roms, have_size): (i64, i64) = conn.query_row(
        "SELECT COUNT(*), COALESCE(SUM(r.size), 0)
         FROM dat_roms r
         JOIN dat_games g ON r.game_id = g.id
         JOIN dat_nodes n ON g.node_id = n.id
         WHERE n.version_id = ?
         AND r.status != 'nodump'
         AND g.is_bios = 0
         AND g.is_device = 0
         AND r.sha1 IN (SELECT sha1 FROM files)",
        [version_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;

    Ok(CollectionStats {
        name: collection_name.to_string(),
        total_games,
        total_roms,
        have_roms,
        total_size,
        have_size,
    })
}

/// Print statistics to stdout
fn print_stats(stats: &OverallStats) {
    println!("Cat198x Statistics");
    println!("===================");
    println!();

    // Overall totals
    let total_roms: i64 = stats.collections.iter().map(|c| c.total_roms).sum();
    let have_roms: i64 = stats.collections.iter().map(|c| c.have_roms).sum();
    let total_size: i64 = stats.collections.iter().map(|c| c.total_size).sum();
    let have_size: i64 = stats.collections.iter().map(|c| c.have_size).sum();
    let overall_pct = if total_roms > 0 {
        have_roms * 100 / total_roms
    } else {
        0
    };

    println!("Overall: {}/{} ROMs ({}%)", have_roms, total_roms, overall_pct);
    println!("         {} / {} total",
        crate::util::format_bytes(have_size as u64),
        crate::util::format_bytes(total_size as u64));
    println!();

    // Per-collection breakdown
    if !stats.collections.is_empty() {
        println!("Collections:");
        println!();

        // Find longest collection name for alignment
        let max_name_len = stats.collections.iter()
            .map(|c| c.name.len())
            .max()
            .unwrap_or(10);

        for coll in &stats.collections {
            let pct = if coll.total_roms > 0 {
                coll.have_roms * 100 / coll.total_roms
            } else {
                0
            };

            let bar = progress_bar(pct as usize, 20);

            println!(
                "  {:width$}  {} {:>3}%  {}/{}",
                coll.name,
                bar,
                pct,
                coll.have_roms,
                coll.total_roms,
                width = max_name_len
            );
        }
        println!();
    }

    // Sources and files
    println!("Sources: {} registered", stats.total_sources);
    println!("Files:   {} unique hashes indexed", stats.total_files_scanned);

    // Quarantine
    if stats.quarantine_count > 0 {
        println!();
        println!(
            "Quarantine: {} files ({})",
            stats.quarantine_count,
            crate::util::format_bytes(stats.quarantine_size as u64)
        );
    }
}

/// Generate a simple ASCII progress bar
fn progress_bar(percentage: usize, width: usize) -> String {
    let filled = (percentage * width) / 100;
    let empty = width - filled;

    format!("[{}{}]", "█".repeat(filled), "░".repeat(empty))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_progress_bar_empty() {
        assert_eq!(progress_bar(0, 10), "[░░░░░░░░░░]");
    }

    #[test]
    fn test_progress_bar_half() {
        assert_eq!(progress_bar(50, 10), "[█████░░░░░]");
    }

    #[test]
    fn test_progress_bar_full() {
        assert_eq!(progress_bar(100, 10), "[██████████]");
    }

    #[test]
    fn test_progress_bar_quarter() {
        assert_eq!(progress_bar(25, 20), "[█████░░░░░░░░░░░░░░░]");
    }
}
