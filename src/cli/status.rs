//! Collection status command

use anyhow::Result;
use std::path::PathBuf;

use crate::db::{collections, dats};
use crate::db::dats::MergeMode;

use super::open_database;

/// Run the status command
pub fn run(
    collection: Option<String>,
    detailed: bool,
    merge_mode: Option<String>,
    data_dir: Option<PathBuf>,
) -> Result<()> {
    let db = open_database(data_dir)?;
    let conn = db.conn();

    // Parse merge mode (default to non-merged for simple collections)
    let mode = parse_merge_mode(merge_mode.as_deref())?;

    let colls = collections::list_collections(conn)?;

    if colls.is_empty() {
        println!("No collections imported yet.");
        println!();
        println!("Import a DAT file with:");
        println!("  romshelf dat add <path>");
        return Ok(());
    }

    // Filter to specific collection if requested
    let colls_to_show: Vec<_> = if let Some(ref name) = collection {
        colls.into_iter().filter(|c| c.name == *name).collect()
    } else {
        colls
    };

    if colls_to_show.is_empty() {
        if let Some(name) = collection {
            println!("Collection not found: {}", name);
            println!();
            println!("Available collections:");
            for c in collections::list_collections(conn)? {
                println!("  {}", c.name);
            }
        }
        return Ok(());
    }

    println!("Collection Status:");
    println!();

    for coll in &colls_to_show {
        // Get active version
        let version = match collections::get_active_version(conn, coll.id)? {
            Some(v) => v,
            None => {
                println!("{}  [no active version]", coll.name);
                continue;
            }
        };

        // Use merge-mode aware stats calculation
        let stats = dats::calculate_merge_mode_stats(
            conn,
            version.id,
            mode,
            true, // exclude_mechanical by default
        )?;

        // Calculate completion percentage
        let completion = if stats.total_roms > 0 {
            (stats.have_roms as f64 / stats.total_roms as f64) * 100.0
        } else {
            0.0
        };

        let missing = stats.total_roms - stats.have_roms;

        // Display summary with merge mode indicator
        let mode_str = match mode {
            MergeMode::NonMerged => "",
            MergeMode::Split => " (split)",
            MergeMode::Merged => " (merged)",
        };
        println!(
            "{}  v{}  [{:.1}% complete]{}",
            coll.name, version.version, completion, mode_str
        );
        println!(
            "  {} games, {} ROMs required",
            stats.total_games, stats.total_roms
        );
        println!(
            "  {} have, {} missing",
            stats.have_roms, missing
        );

        // Show additional info for MAME-style collections
        let mut extras = Vec::new();
        if stats.nodump_roms > 0 {
            extras.push(format!("{} nodump", stats.nodump_roms));
        }
        if stats.bios_sets > 0 {
            extras.push(format!("{} BIOS", stats.bios_sets));
        }
        if stats.device_sets > 0 {
            extras.push(format!("{} device", stats.device_sets));
        }
        if !extras.is_empty() {
            println!("  ({})", extras.join(", "));
        }

        if detailed {
            println!();
            show_detailed_status(conn, version.id, mode)?;
        }

        println!();
    }

    Ok(())
}

/// Parse merge mode from string
fn parse_merge_mode(mode: Option<&str>) -> Result<MergeMode> {
    match mode {
        None | Some("non-merged") | Some("nonmerged") => Ok(MergeMode::NonMerged),
        Some("split") => Ok(MergeMode::Split),
        Some("merged") => Ok(MergeMode::Merged),
        Some(other) => anyhow::bail!(
            "Unknown merge mode: '{}'. Use 'non-merged', 'split', or 'merged'",
            other
        ),
    }
}

/// Show detailed per-game status with merge-mode awareness
fn show_detailed_status(conn: &rusqlite::Connection, version_id: i64, mode: MergeMode) -> Result<()> {
    // Get game requirements accounting for merge mode
    let requirements = dats::calculate_rom_requirements(
        conn,
        version_id,
        mode,
        true, // exclude_mechanical
    )?;

    let mut complete_count = 0;
    let mut incomplete_count = 0;
    let mut missing_count = 0;

    println!("  Games:");
    for req in &requirements {
        // Count how many required ROMs we have
        let have_count: usize = req.required_roms.iter()
            .filter(|key| crate::db::dats::rom_present(conn, key).unwrap_or(false))
            .count();

        let total = req.required_roms.len();

        let status = if total == 0 || have_count == total {
            complete_count += 1;
            "✓"
        } else if have_count > 0 {
            incomplete_count += 1;
            "~"
        } else {
            missing_count += 1;
            "✗"
        };

        // Build marker string for special game types
        let mut markers = Vec::new();
        if req.is_clone {
            markers.push("clone");
        }
        if req.is_bios {
            markers.push("BIOS");
        }
        if req.is_device {
            markers.push("device");
        }
        let marker_str = if markers.is_empty() {
            String::new()
        } else {
            format!(" [{}]", markers.join(", "))
        };

        println!(
            "    {} {}{} ({}/{})",
            status, req.game_name, marker_str, have_count, total
        );
    }

    println!();
    println!(
        "  Summary: {} complete, {} partial, {} missing",
        complete_count, incomplete_count, missing_count
    );

    Ok(())
}
