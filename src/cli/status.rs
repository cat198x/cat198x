//! Collection status command

use anyhow::Result;
use std::path::PathBuf;

use crate::db::dats::MergeMode;
use crate::db::{collections, dats};

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

    // The data comes from the shared operation surface; this command only
    // formats it. Any other adapter (MCP, UI) gets the same numbers.
    let statuses = crate::ops::collection_status(conn, collection.as_deref(), mode)?;

    if statuses.is_empty() {
        if let Some(name) = collection {
            println!("Collection not found: {}", name);
            let all = collections::list_collections(conn)?;
            if !all.is_empty() {
                println!();
                println!("Available collections:");
                for c in all {
                    println!("  {}", c.name);
                }
            }
        } else {
            println!("No collections imported yet.");
            println!();
            println!("Import a DAT file with:");
            println!("  cat198x dat add <path>");
        }
        return Ok(());
    }

    let mode_str = match mode {
        MergeMode::NonMerged => "",
        MergeMode::Split => " (split)",
        MergeMode::Merged => " (merged)",
    };

    println!("Collection Status:");
    println!();

    for s in &statuses {
        let Some(version) = &s.version else {
            println!("{}  [no active version]", s.name);
            continue;
        };

        println!(
            "{}  v{}  [{:.1}% complete]{}",
            s.name, version, s.completion_pct, mode_str
        );
        println!("  {} games, {} ROMs required", s.total_games, s.total_roms);
        println!("  {} have, {} missing", s.have_roms, s.missing_roms);

        // Show additional info for MAME-style collections
        let mut extras = Vec::new();
        if s.nodump_roms > 0 {
            extras.push(format!("{} nodump", s.nodump_roms));
        }
        if s.bios_sets > 0 {
            extras.push(format!("{} BIOS", s.bios_sets));
        }
        if s.device_sets > 0 {
            extras.push(format!("{} device", s.device_sets));
        }
        if !extras.is_empty() {
            println!("  ({})", extras.join(", "));
        }

        if detailed {
            // The detailed per-game view needs the active version id; re-resolve
            // it from the name (cheap) rather than thread it through the ops type.
            if let Some(coll) = collections::get_collection_by_name(conn, &s.name)?
                && let Some(v) = collections::get_active_version(conn, coll.id)?
            {
                println!();
                show_detailed_status(conn, v.id, mode)?;
            }
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
fn show_detailed_status(
    conn: &rusqlite::Connection,
    version_id: i64,
    mode: MergeMode,
) -> Result<()> {
    // Get game requirements accounting for merge mode
    let requirements = dats::calculate_rom_requirements(
        conn, version_id, mode, true, // exclude_mechanical
    )?;

    let mut complete_count = 0;
    let mut incomplete_count = 0;
    let mut missing_count = 0;

    println!("  Games:");
    for req in &requirements {
        // Count how many required ROMs we have
        let have_count: usize = req
            .required_roms
            .iter()
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
