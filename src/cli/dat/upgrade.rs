use anyhow::{Context, Result};
use std::path::PathBuf;

use crate::cli::open_database;
use crate::dat::parse_dat_file_auto;
use crate::db::{collections, dats};

use super::{chrono_lite_version, insert_dat_entry};

/// Upgrade a collection: add new DAT version and deactivate the old one
pub(super) fn upgrade_dat(
    path: &PathBuf,
    collection_name: Option<&str>,
    data_dir: Option<PathBuf>,
) -> Result<()> {
    let abs_path =
        std::fs::canonicalize(path).with_context(|| format!("Cannot resolve path: {:?}", path))?;

    if !abs_path.is_file() {
        anyhow::bail!("Path is not a file: {}", abs_path.display());
    }

    println!("Parsing DAT file: {}", abs_path.display());

    // Parse the DAT file
    let (header, games) = parse_dat_file_auto(&abs_path)?;

    // Determine collection name
    let coll_name = collection_name
        .map(|s| s.to_string())
        .unwrap_or_else(|| header.name.clone());

    let db = open_database(data_dir)?;
    let conn = db.conn();

    // Check if collection exists
    let collection = collections::get_collection_by_name(conn, &coll_name)?.ok_or_else(|| {
        anyhow::anyhow!(
            "Collection '{}' not found.\n\n\
                 Use 'cat198x dat add' to create a new collection,\n\
                 or 'cat198x dat upgrade --collection <name>' to specify an existing collection.",
            coll_name
        )
    })?;

    // Get current active version info
    let old_version = collections::get_active_version(conn, collection.id)?;
    let old_version_str = old_version
        .as_ref()
        .map(|v| v.version.clone())
        .unwrap_or_else(|| "(none)".to_string());

    // Determine new version string
    let new_version = header.version.clone().unwrap_or_else(chrono_lite_version);

    // Check if this version already exists
    if let Some(existing) = collections::get_version_by_name(conn, collection.id, &new_version)? {
        if existing.is_active {
            anyhow::bail!(
                "Version '{}' is already the active version for '{}'",
                new_version,
                coll_name
            );
        } else {
            anyhow::bail!(
                "Version '{}' already exists for '{}'. Use 'cat198x dat activate' to switch to it.",
                new_version,
                coll_name
            );
        }
    }

    println!("  Name: {}", header.name);
    if let Some(ref ver) = header.version {
        println!("  Version: {}", ver);
    }
    println!("  Games: {}", games.len());
    println!(
        "  ROMs: {}",
        games.iter().map(|g| g.roms.len()).sum::<usize>()
    );
    println!();
    println!(
        "Upgrading '{}': {} → {}",
        coll_name, old_version_str, new_version
    );

    // Wrap the version add + node + games/ROMs in one transaction so a failed
    // upgrade rolls back instead of half-replacing the active version, and the
    // per-row inserts commit once rather than once each.
    let tx = conn.unchecked_transaction()?;

    // Add the new version (this automatically activates it and deactivates the old one)
    let path_str = abs_path.to_string_lossy();
    let version_id = collections::add_version(conn, collection.id, &new_version, &path_str, true)?;

    // Create root DAT node
    let node_id = dats::create_node(conn, version_id, None, &header.name, "dat", &header.name)?;

    // Import games and ROMs
    let mut game_count = 0;
    let mut rom_count = 0;

    for game in &games {
        let game_id = dats::create_game(
            conn,
            node_id,
            &game.name,
            game.description.as_deref(),
            game.clone_of.as_deref(),
            game.is_bios,
            game.is_device,
            game.is_mechanical,
        )?;
        game_count += 1;

        for rom in &game.roms {
            insert_dat_entry(conn, game_id, rom)?;
            rom_count += 1;
        }
    }

    tx.commit()?;

    println!();
    println!("Imported {} games with {} ROMs", game_count, rom_count);
    println!("Version '{}' is now active", new_version);

    // Show what changed if there was an old version
    if old_version.is_some() {
        println!();
        println!(
            "Previous version '{}' has been deactivated but not removed.",
            old_version_str
        );
        println!("Use 'cat198x dat diff {}' to see what changed.", coll_name);
    }

    Ok(())
}
