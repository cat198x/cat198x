//! Stats command - show overall statistics across all collections

use anyhow::Result;
use std::path::PathBuf;

use super::open_database;
use crate::db::dats::{MergeMode, calculate_merge_mode_stats};
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
pub fn run(grouped: bool, data_dir: Option<PathBuf>) -> Result<()> {
    let db = open_database(data_dir)?;
    let conn = db.conn();

    let stats = gather_stats(conn)?;

    print_stats(&stats, grouped);

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

    let total_files_scanned: i64 =
        conn.query_row("SELECT COUNT(*) FROM files", [], |row| row.get(0))?;

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
    // Use the same merge-mode-aware computation as `status`, so the two
    // commands can never disagree. The previous bespoke query here matched ROMs
    // by SHA1 only (`r.sha1 IN (SELECT sha1 FROM files)`), so CRC-only DATs —
    // all of FinalBurn Neo, older MAME — counted zero and read as 0% complete
    // even when fully present. calculate_merge_mode_stats matches by SHA1 or
    // CRC+size and counts unique ROMs.
    let s = calculate_merge_mode_stats(conn, version_id, MergeMode::NonMerged, true)?;

    Ok(CollectionStats {
        name: collection_name.to_string(),
        total_games: s.total_games as i64,
        total_roms: s.total_roms as i64,
        have_roms: s.have_roms as i64,
        total_size: s.total_bytes as i64,
        have_size: s.have_bytes as i64,
    })
}

/// The rollup key for a collection: the segment before the first " - ".
/// "Sinclair ZX Spectrum - Applications - [TAP]" -> "Sinclair ZX Spectrum".
/// A name with no " - " is its own group.
fn group_key(name: &str) -> &str {
    match name.split_once(" - ") {
        Some((head, _)) => head,
        None => name,
    }
}

/// Roll collections up by [`group_key`], summing their totals. Returns one
/// `CollectionStats` per group, sorted by group name.
fn group_collections(collections: &[CollectionStats]) -> Vec<CollectionStats> {
    use std::collections::BTreeMap;

    let mut groups: BTreeMap<String, CollectionStats> = BTreeMap::new();
    for c in collections {
        let key = group_key(&c.name).to_string();
        let entry = groups
            .entry(key.clone())
            .or_insert_with(|| CollectionStats {
                name: key,
                total_games: 0,
                total_roms: 0,
                have_roms: 0,
                total_size: 0,
                have_size: 0,
            });
        entry.total_games += c.total_games;
        entry.total_roms += c.total_roms;
        entry.have_roms += c.have_roms;
        entry.total_size += c.total_size;
        entry.have_size += c.have_size;
    }
    groups.into_values().collect()
}

/// Print one block of rows (per-collection or per-group) with progress bars.
fn print_rows(heading: &str, rows: &[CollectionStats]) {
    if rows.is_empty() {
        return;
    }
    println!("{}:", heading);
    println!();

    let max_name_len = rows.iter().map(|c| c.name.len()).max().unwrap_or(10);

    for row in rows {
        let pct = if row.total_roms > 0 {
            row.have_roms * 100 / row.total_roms
        } else {
            0
        };
        let bar = progress_bar(pct as usize, 20);
        println!(
            "  {:width$}  {} {:>3}%  {}/{}",
            row.name,
            bar,
            pct,
            row.have_roms,
            row.total_roms,
            width = max_name_len
        );
    }
    println!();
}

/// Print statistics to stdout
fn print_stats(stats: &OverallStats, grouped: bool) {
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

    println!(
        "Overall: {}/{} ROMs ({}%)",
        have_roms, total_roms, overall_pct
    );
    println!(
        "         {} / {} total",
        crate::util::format_bytes(have_size as u64),
        crate::util::format_bytes(total_size as u64)
    );
    println!();

    // Per-collection (or, with --group, per-group) breakdown.
    if grouped {
        let groups = group_collections(&stats.collections);
        print_rows(
            &format!("Groups ({} collections rolled up)", stats.collections.len()),
            &groups,
        );
    } else {
        print_rows("Collections", &stats.collections);
    }

    // Sources and files
    println!("Sources: {} registered", stats.total_sources);
    println!(
        "Files:   {} unique hashes indexed",
        stats.total_files_scanned
    );

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

    #[test]
    fn group_key_takes_segment_before_first_dash() {
        assert_eq!(
            group_key("Sinclair ZX Spectrum - Applications - [TAP]"),
            "Sinclair ZX Spectrum"
        );
        assert_eq!(group_key("Acorn BBC - Magazines - Laserbug"), "Acorn BBC");
    }

    #[test]
    fn group_key_without_dash_is_whole_name() {
        assert_eq!(group_key("MAME 0.261"), "MAME 0.261");
    }

    fn stat(name: &str, have: i64, total: i64) -> CollectionStats {
        CollectionStats {
            name: name.to_string(),
            total_games: total,
            total_roms: total,
            have_roms: have,
            total_size: have * 10,
            have_size: have * 10,
        }
    }

    #[test]
    fn group_collections_sums_by_key_and_sorts() {
        let rows = vec![
            stat("Sinclair ZX Spectrum - Applications - [TAP]", 3, 4),
            stat("Sinclair ZX Spectrum - Games - [TAP]", 5, 10),
            stat("Acorn BBC - Magazines - Laserbug", 2, 2),
        ];

        let grouped = group_collections(&rows);

        assert_eq!(grouped.len(), 2);
        // BTreeMap order: "Acorn BBC" before "Sinclair ZX Spectrum".
        assert_eq!(grouped[0].name, "Acorn BBC");
        assert_eq!(grouped[0].have_roms, 2);
        assert_eq!(grouped[0].total_roms, 2);
        assert_eq!(grouped[1].name, "Sinclair ZX Spectrum");
        assert_eq!(grouped[1].have_roms, 8);
        assert_eq!(grouped[1].total_roms, 14);
    }
}
