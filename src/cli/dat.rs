//! DAT file management commands

use anyhow::{Context, Result};
use std::path::PathBuf;

use crate::dat::{parse_dat_file_auto, DatSourceType};
use crate::db::{collections, dats};
use crate::DatCommands;

use super::{fetch, open_database};

/// Run a DAT subcommand
pub fn run(cmd: DatCommands, data_dir: Option<PathBuf>) -> Result<()> {
    match cmd {
        DatCommands::Add { path, collection } => add_dat(&path, collection.as_deref(), data_dir),
        DatCommands::Remove { target, all_versions } => remove_dat(&target, all_versions, data_dir),
        DatCommands::List { all } => list_dats(all, data_dir),
        DatCommands::Activate { collection, version } => {
            activate_version(&collection, &version, data_dir)
        }
        DatCommands::Diff { collection, from, to } => {
            diff_versions(&collection, from.as_deref(), to.as_deref(), data_dir)
        }
        DatCommands::Versions { collection } => list_versions(&collection, data_dir),
        DatCommands::Fetch { source, url, output, list } => {
            fetch::run(source.as_deref(), url.as_deref(), output, list, data_dir)
        }
        DatCommands::Upgrade { path, collection } => {
            upgrade_dat(&path, collection.as_deref(), data_dir)
        }
    }
}

fn add_dat(path: &PathBuf, collection_name: Option<&str>, data_dir: Option<PathBuf>) -> Result<()> {
    let abs_path =
        std::fs::canonicalize(path).with_context(|| format!("Cannot resolve path: {:?}", path))?;

    if !abs_path.is_file() {
        anyhow::bail!("Path is not a file: {}", abs_path.display());
    }

    println!("Parsing DAT file: {}", abs_path.display());

    // Parse the DAT file (auto-detects Logiqx XML or ClrMamePro format)
    let (header, games) = parse_dat_file_auto(&abs_path)?;

    // Determine collection name
    let coll_name = collection_name
        .map(|s| s.to_string())
        .unwrap_or_else(|| header.name.clone());

    println!("  Name: {}", header.name);
    if let Some(ref desc) = header.description {
        println!("  Description: {}", desc);
    }
    if let Some(ref ver) = header.version {
        println!("  Version: {}", ver);
    }
    println!("  Games: {}", games.len());
    println!(
        "  ROMs: {}",
        games.iter().map(|g| g.roms.len()).sum::<usize>()
    );

    // Detect source type
    let source_type = DatSourceType::detect(&header);
    println!("  Detected type: {}", source_type.as_str());

    let db = open_database(data_dir)?;
    let conn = db.conn();

    // Wrap the whole import (collection, version, node, games, ROMs) in one
    // transaction: a mid-import failure rolls back cleanly instead of leaving
    // orphaned partial rows, and the per-row inserts commit once rather than
    // once each (a large speed-up on big DATs such as MAME).
    let tx = conn.unchecked_transaction()?;

    // Get or create collection
    let collection = match collections::get_collection_by_name(conn, &coll_name)? {
        Some(c) => {
            println!("\nAdding to existing collection: {}", c.name);
            c
        }
        None => {
            println!("\nCreating new collection: {}", coll_name);
            let _id = collections::create_collection(conn, &coll_name, source_type.as_str())?;
            collections::get_collection_by_name(conn, &coll_name)?
                .ok_or_else(|| anyhow::anyhow!("Failed to create collection"))?
        }
    };

    // Get version string
    let version = header.version.clone().unwrap_or_else(|| {
        // Use current date as version if none specified
        chrono_lite_version()
    });

    // Add version (activating it)
    let path_str = abs_path.to_string_lossy();
    let version_id = collections::add_version(conn, collection.id, &version, &path_str, true)?;

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
            dats::create_rom(
                conn,
                game_id,
                &rom.name,
                rom.size as i64,
                rom.sha1.as_deref(),
                rom.md5.as_deref(),
                rom.crc32.as_deref(),
                rom.status.as_str(),
                rom.merge.as_deref(),
            )?;
            rom_count += 1;
        }
    }

    tx.commit()?;

    println!();
    println!("Imported {} games with {} ROMs", game_count, rom_count);
    println!("Version '{}' is now active", version);
    println!();
    println!("Run 'cat198x scan' to match files against this DAT.");

    Ok(())
}

fn remove_dat(target: &str, all_versions: bool, data_dir: Option<PathBuf>) -> Result<()> {
    let db = open_database(data_dir)?;
    let conn = db.conn();

    // Try to find collection by name first
    let collection = collections::get_collection_by_name(conn, target)?;

    if let Some(coll) = collection {
        if all_versions {
            // Remove entire collection
            let version_count = collections::count_versions(conn, coll.id)?;
            collections::remove_collection(conn, coll.id)?;
            println!(
                "Removed collection '{}' ({} version{})",
                coll.name,
                version_count,
                if version_count == 1 { "" } else { "s" }
            );
        } else {
            // Remove only the active version
            let active = collections::get_active_version(conn, coll.id)?;
            if let Some(version) = active {
                let (deleted, _) = collections::remove_version(conn, version.id)?;
                if deleted {
                    println!(
                        "Removed version '{}' from '{}'",
                        version.version, coll.name
                    );

                    // Check if there are remaining versions
                    let remaining = collections::list_versions(conn, coll.id)?;
                    if remaining.is_empty() {
                        // No versions left, remove the collection too
                        collections::remove_collection(conn, coll.id)?;
                        println!("  (collection had no remaining versions, removed)");
                    } else if !remaining.iter().any(|v| v.is_active) {
                        // Activate the most recent remaining version
                        let newest = &remaining[0]; // Already sorted by imported_at DESC
                        collections::activate_version(conn, coll.id, &newest.version)?;
                        println!("  Activated version '{}' as the new active version", newest.version);
                    }
                }
            } else {
                println!("Collection '{}' has no active version to remove.", coll.name);
                println!("Use --all-versions to remove the entire collection.");
            }
        }
    } else {
        // Target might be a specific version string like "CollectionName:version"
        if let Some((coll_name, ver_name)) = target.split_once(':') {
            let coll = collections::get_collection_by_name(conn, coll_name)?;
            if let Some(c) = coll {
                let version = collections::get_version_by_name(conn, c.id, ver_name)?;
                if let Some(v) = version {
                    let (deleted, was_active) = collections::remove_version(conn, v.id)?;
                    if deleted {
                        println!("Removed version '{}' from '{}'", ver_name, coll_name);

                        // If it was active, activate another version
                        if was_active {
                            let remaining = collections::list_versions(conn, c.id)?;
                            if remaining.is_empty() {
                                collections::remove_collection(conn, c.id)?;
                                println!("  (collection had no remaining versions, removed)");
                            } else {
                                let newest = &remaining[0];
                                collections::activate_version(conn, c.id, &newest.version)?;
                                println!(
                                    "  Activated version '{}' as the new active version",
                                    newest.version
                                );
                            }
                        }
                    }
                } else {
                    anyhow::bail!("Version '{}' not found in collection '{}'", ver_name, coll_name);
                }
            } else {
                anyhow::bail!("Collection '{}' not found", coll_name);
            }
        } else {
            anyhow::bail!(
                "Collection '{}' not found.\n\nUse 'cat198x dat list' to see available collections.\nTo remove a specific version, use: cat198x dat remove \"Collection Name:version\"",
                target
            );
        }
    }

    Ok(())
}

fn list_dats(all: bool, data_dir: Option<PathBuf>) -> Result<()> {
    let db = open_database(data_dir)?;
    let conn = db.conn();

    let colls = collections::list_collections(conn)?;

    if colls.is_empty() {
        println!("No DATs imported yet.");
        println!();
        println!("Import a DAT file with:");
        println!("  cat198x dat add <path>");
        return Ok(());
    }

    if all {
        println!("All DAT versions:");
    } else {
        println!("Active DATs:");
    }
    println!();

    for coll in &colls {
        let versions = collections::list_versions(conn, coll.id)?;

        if all {
            // Show all versions
            println!("{}  [{}]", coll.name, coll.source_type);
            for ver in &versions {
                let active_marker = if ver.is_active { " (active)" } else { "" };
                let (game_count, rom_count) = dats::count_games_and_roms(conn, ver.id)?;
                println!(
                    "    {} - {} games, {} ROMs{}",
                    ver.version, game_count, rom_count, active_marker
                );
            }
            println!();
        } else {
            // Show only active version
            if let Some(active) = versions.iter().find(|v| v.is_active) {
                let (game_count, rom_count) = dats::count_games_and_roms(conn, active.id)?;
                println!(
                    "{}  v{}  [{} games, {} ROMs]",
                    coll.name, active.version, game_count, rom_count
                );
            }
        }
    }

    if !all {
        println!();
        println!("Use 'cat198x dat list --all' to see all versions.");
    }

    Ok(())
}

fn activate_version(
    collection: &str,
    version: &str,
    data_dir: Option<PathBuf>,
) -> Result<()> {
    let db = open_database(data_dir)?;
    let conn = db.conn();

    let coll = collections::get_collection_by_name(conn, collection)?
        .ok_or_else(|| anyhow::anyhow!("Collection not found: {}", collection))?;

    let success = collections::activate_version(conn, coll.id, version)?;

    if success {
        println!("Activated version '{}' for '{}'", version, collection);
    } else {
        println!("Version '{}' not found in '{}'", version, collection);
        println!();
        println!("Available versions:");
        let versions = collections::list_versions(conn, coll.id)?;
        for ver in &versions {
            let marker = if ver.is_active { " (active)" } else { "" };
            println!("  {}{}", ver.version, marker);
        }
    }

    Ok(())
}

fn diff_versions(
    collection: &str,
    from: Option<&str>,
    to: Option<&str>,
    data_dir: Option<PathBuf>,
) -> Result<()> {
    use std::collections::HashSet;

    let db = open_database(data_dir)?;
    let conn = db.conn();

    // Find the collection
    let coll = collections::get_collection_by_name(conn, collection)?
        .ok_or_else(|| anyhow::anyhow!("Collection '{}' not found", collection))?;

    // Get all versions for this collection
    let versions = collections::list_versions(conn, coll.id)?;
    if versions.len() < 2 && (from.is_none() || to.is_none()) {
        anyhow::bail!(
            "Collection '{}' only has {} version(s). Need at least 2 versions to diff.",
            collection,
            versions.len()
        );
    }

    // Determine the "from" version
    let from_version = if let Some(from_str) = from {
        versions
            .iter()
            .find(|v| v.version == from_str)
            .ok_or_else(|| anyhow::anyhow!("Version '{}' not found", from_str))?
    } else {
        // Find the second most recent (previous to active)
        let active_idx = versions.iter().position(|v| v.is_active);
        if let Some(idx) = active_idx {
            if idx + 1 < versions.len() {
                &versions[idx + 1]
            } else {
                anyhow::bail!("No previous version available to compare against");
            }
        } else if versions.len() >= 2 {
            &versions[1] // Second most recent
        } else {
            anyhow::bail!("No previous version available to compare against");
        }
    };

    // Determine the "to" version
    let to_version = if let Some(to_str) = to {
        versions
            .iter()
            .find(|v| v.version == to_str)
            .ok_or_else(|| anyhow::anyhow!("Version '{}' not found", to_str))?
    } else {
        // Use active version, or most recent
        versions
            .iter()
            .find(|v| v.is_active)
            .or(versions.first())
            .ok_or_else(|| anyhow::anyhow!("No version found"))?
    };

    println!("Comparing versions of '{}':", collection);
    println!("  From: {} ({})", from_version.version, from_version.imported_at);
    println!("  To:   {} ({})", to_version.version, to_version.imported_at);
    println!();

    // Get games from both versions
    let from_games = dats::get_games_for_version(conn, from_version.id)?;
    let to_games = dats::get_games_for_version(conn, to_version.id)?;

    let from_game_names: HashSet<_> = from_games.iter().map(|g| &g.name).collect();
    let to_game_names: HashSet<_> = to_games.iter().map(|g| &g.name).collect();

    // Calculate game changes
    let added_games: Vec<_> = to_game_names.difference(&from_game_names).collect();
    let removed_games: Vec<_> = from_game_names.difference(&to_game_names).collect();

    // Get ROMs and their SHA1s from both versions
    let from_roms = dats::get_roms_for_version(conn, from_version.id)?;
    let to_roms = dats::get_roms_for_version(conn, to_version.id)?;

    // Build sets of SHA1 hashes for comparison
    let from_sha1s: HashSet<_> = from_roms
        .iter()
        .filter_map(|(_, r)| r.sha1.as_ref())
        .collect();
    let to_sha1s: HashSet<_> = to_roms
        .iter()
        .filter_map(|(_, r)| r.sha1.as_ref())
        .collect();

    let new_sha1s: Vec<_> = to_sha1s.difference(&from_sha1s).collect();
    let removed_sha1s: Vec<_> = from_sha1s.difference(&to_sha1s).collect();

    // Print summary
    println!("Games:");
    println!("  {} → {} ({}{})",
        from_games.len(),
        to_games.len(),
        if to_games.len() >= from_games.len() { "+" } else { "" },
        to_games.len() as i64 - from_games.len() as i64
    );

    println!("ROMs (unique SHA1s):");
    println!("  {} → {} ({}{})",
        from_sha1s.len(),
        to_sha1s.len(),
        if to_sha1s.len() >= from_sha1s.len() { "+" } else { "" },
        to_sha1s.len() as i64 - from_sha1s.len() as i64
    );

    println!();

    // Print added games (up to 20)
    if !added_games.is_empty() {
        println!("Added games ({}):", added_games.len());
        for (i, name) in added_games.iter().take(20).enumerate() {
            println!("  + {}", name);
            if i == 19 && added_games.len() > 20 {
                println!("  ... and {} more", added_games.len() - 20);
            }
        }
        println!();
    }

    // Print removed games (up to 20)
    if !removed_games.is_empty() {
        println!("Removed games ({}):", removed_games.len());
        for (i, name) in removed_games.iter().take(20).enumerate() {
            println!("  - {}", name);
            if i == 19 && removed_games.len() > 20 {
                println!("  ... and {} more", removed_games.len() - 20);
            }
        }
        println!();
    }

    // Print ROM hash changes summary
    if !new_sha1s.is_empty() || !removed_sha1s.is_empty() {
        println!("ROM changes:");
        println!("  {} new ROM hashes", new_sha1s.len());
        println!("  {} removed ROM hashes", removed_sha1s.len());
    }

    if added_games.is_empty() && removed_games.is_empty() && new_sha1s.is_empty() && removed_sha1s.is_empty() {
        println!("No differences found between versions.");
    }

    Ok(())
}

fn list_versions(collection: &str, data_dir: Option<PathBuf>) -> Result<()> {
    let db = open_database(data_dir)?;
    let conn = db.conn();

    let coll = collections::get_collection_by_name(conn, collection)?
        .ok_or_else(|| anyhow::anyhow!("Collection '{}' not found", collection))?;

    let versions = collections::list_versions(conn, coll.id)?;

    if versions.is_empty() {
        println!("No versions found for '{}'", collection);
        return Ok(());
    }

    println!("Versions of '{}' ({} total):", collection, versions.len());
    println!();

    for ver in &versions {
        let active_marker = if ver.is_active { " *" } else { "" };
        println!("  {}{}", ver.version, active_marker);

        // Get game/ROM counts for this version
        let games = dats::get_games_for_version(conn, ver.id)?;
        let roms = dats::get_roms_for_version(conn, ver.id)?;

        println!("    Games: {}, ROMs: {}", games.len(), roms.len());
        println!("    Imported: {}", ver.imported_at);
        println!("    DAT: {}", ver.dat_path);
        println!();
    }

    if versions.iter().any(|v| v.is_active) {
        println!("(* = active version)");
    }

    Ok(())
}

/// Generate a simple version string from current date (YYYYMMDD)
fn chrono_lite_version() -> String {
    use chrono::{Datelike, Local};
    let now = Local::now();
    format!("{:04}{:02}{:02}", now.year(), now.month(), now.day())
}

/// Upgrade a collection: add new DAT version and deactivate the old one
fn upgrade_dat(
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
    let collection = collections::get_collection_by_name(conn, &coll_name)?
        .ok_or_else(|| {
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
            dats::create_rom(
                conn,
                game_id,
                &rom.name,
                rom.size as i64,
                rom.sha1.as_deref(),
                rom.md5.as_deref(),
                rom.crc32.as_deref(),
                rom.status.as_str(),
                rom.merge.as_deref(),
            )?;
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
