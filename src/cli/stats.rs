//! Stats command - show overall statistics across all collections

use anyhow::Result;
use std::path::PathBuf;

use super::open_database;
use crate::db::dats::{MergeMode, calculate_merge_mode_stats};
use crate::db::{collections, dats, files};

/// Dimension to roll collections up by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GroupBy {
    /// Leading name segment, e.g. "Sinclair ZX Spectrum - *" → "Sinclair ZX Spectrum".
    System,
    /// Top of the library path, e.g. "TOSEC-PIX/Acorn/..." → "TOSEC-PIX".
    Set,
}

impl GroupBy {
    fn parse(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "system" => Ok(GroupBy::System),
            "set" => Ok(GroupBy::Set),
            _ => anyhow::bail!("Unknown group-by '{}' (use 'system' or 'set')", s),
        }
    }
}

/// Collection statistics
#[derive(Debug)]
#[allow(dead_code)] // total_games reserved for future "games complete" display
struct CollectionStats {
    name: String,
    /// The collection's library path (set by recursive `dat add`), or its name.
    node_path: String,
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
pub fn run(group_by: Option<&str>, data_dir: Option<PathBuf>) -> Result<()> {
    let group_by = group_by.map(GroupBy::parse).transpose()?;

    let db = open_database(data_dir)?;
    let conn = db.conn();

    let stats = gather_stats(conn)?;

    print_stats(&stats, group_by);

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

    let node_path =
        dats::primary_node_path(conn, version_id)?.unwrap_or_else(|| collection_name.to_string());

    Ok(CollectionStats {
        name: collection_name.to_string(),
        node_path,
        total_games: s.total_games as i64,
        total_roms: s.total_roms as i64,
        have_roms: s.have_roms as i64,
        total_size: s.total_bytes as i64,
        have_size: s.have_bytes as i64,
    })
}

/// The rollup key for a collection under a grouping dimension.
/// - System: the segment before the first " - " ("Sinclair ZX Spectrum - … →
///   "Sinclair ZX Spectrum"); a name with no " - " is its own group.
/// - Set: the top segment of the library path ("TOSEC-PIX/Acorn/… → "TOSEC-PIX");
///   a flat path (no recursive add yet) is its own group.
fn group_key(c: &CollectionStats, by: GroupBy) -> &str {
    match by {
        GroupBy::System => match c.name.split_once(" - ") {
            Some((head, _)) => head,
            None => &c.name,
        },
        GroupBy::Set => c.node_path.split('/').next().unwrap_or(&c.node_path),
    }
}

/// Roll collections up by [`group_key`], summing their totals. Returns one
/// `CollectionStats` per group, sorted by group name.
fn group_collections(collections: &[CollectionStats], by: GroupBy) -> Vec<CollectionStats> {
    use std::collections::BTreeMap;

    let mut groups: BTreeMap<String, CollectionStats> = BTreeMap::new();
    for c in collections {
        let key = group_key(c, by).to_string();
        let entry = groups
            .entry(key.clone())
            .or_insert_with(|| CollectionStats {
                name: key.clone(),
                node_path: key,
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
fn print_stats(stats: &OverallStats, group_by: Option<GroupBy>) {
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

    // Per-collection (or, with --group-by, per-group) breakdown.
    if let Some(by) = group_by {
        let groups = group_collections(&stats.collections, by);
        let label = match by {
            GroupBy::System => "system",
            GroupBy::Set => "set",
        };
        print_rows(
            &format!(
                "Groups by {} ({} collections rolled up)",
                label,
                stats.collections.len()
            ),
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

    fn stat(name: &str, node_path: &str, have: i64, total: i64) -> CollectionStats {
        CollectionStats {
            name: name.to_string(),
            node_path: node_path.to_string(),
            total_games: total,
            total_roms: total,
            have_roms: have,
            total_size: have * 10,
            have_size: have * 10,
        }
    }

    #[test]
    fn group_by_parses_known_dimensions_only() {
        assert_eq!(GroupBy::parse("system").unwrap(), GroupBy::System);
        assert_eq!(GroupBy::parse("Set").unwrap(), GroupBy::Set);
        assert!(GroupBy::parse("publisher").is_err());
    }

    #[test]
    fn group_key_system_uses_segment_before_first_dash() {
        let c = stat(
            "Sinclair ZX Spectrum - Applications - [TAP]",
            "TOSEC/Sinclair/ZX Spectrum",
            1,
            1,
        );
        assert_eq!(group_key(&c, GroupBy::System), "Sinclair ZX Spectrum");
        // No " - " → its own group.
        let m = stat("MAME 0.261", "MAME 0.261", 1, 1);
        assert_eq!(group_key(&m, GroupBy::System), "MAME 0.261");
    }

    #[test]
    fn group_key_set_uses_top_of_library_path() {
        let c = stat(
            "Acorn BBC - Magazines - Laserbug",
            "TOSEC-PIX/Acorn/BBC/Magazines/Laserbug",
            1,
            1,
        );
        assert_eq!(group_key(&c, GroupBy::Set), "TOSEC-PIX");
        // Flat path (no recursive add yet) → its own group.
        let f = stat("Flat Coll", "Flat Coll", 1, 1);
        assert_eq!(group_key(&f, GroupBy::Set), "Flat Coll");
    }

    #[test]
    fn group_collections_by_system_sums_and_sorts() {
        let rows = vec![
            stat("Sinclair ZX Spectrum - Applications", "p", 3, 4),
            stat("Sinclair ZX Spectrum - Games", "p", 5, 10),
            stat("Acorn BBC - Magazines", "p", 2, 2),
        ];
        let grouped = group_collections(&rows, GroupBy::System);
        assert_eq!(grouped.len(), 2);
        assert_eq!(grouped[0].name, "Acorn BBC"); // BTreeMap order
        assert_eq!(grouped[1].name, "Sinclair ZX Spectrum");
        assert_eq!(grouped[1].have_roms, 8);
        assert_eq!(grouped[1].total_roms, 14);
    }

    #[test]
    fn group_collections_by_set_rolls_up_whole_set() {
        let rows = vec![
            stat(
                "Acorn BBC - Magazines",
                "TOSEC-PIX/Acorn/BBC/Magazines",
                1,
                2,
            ),
            stat("Sony - Books", "TOSEC-PIX/Sony/Books", 3, 3),
            stat(
                "Sinclair ZX Spectrum - Games",
                "TOSEC/Sinclair/ZX Spectrum",
                4,
                8,
            ),
        ];
        let grouped = group_collections(&rows, GroupBy::Set);
        assert_eq!(grouped.len(), 2);
        assert_eq!(grouped[0].name, "TOSEC"); // BTreeMap: TOSEC before TOSEC-PIX
        assert_eq!(grouped[0].have_roms, 4);
        assert_eq!(grouped[1].name, "TOSEC-PIX");
        assert_eq!(grouped[1].have_roms, 4);
        assert_eq!(grouped[1].total_roms, 5);
    }
}
