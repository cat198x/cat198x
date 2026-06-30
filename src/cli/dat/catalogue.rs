mod diff;

use anyhow::Result;
use std::path::PathBuf;

use crate::cli::open_database;
use crate::db::{collections, dats};

pub(super) use diff::diff_versions;

pub(super) fn remove_dat(
    target: &str,
    all_versions: bool,
    data_dir: Option<PathBuf>,
) -> Result<()> {
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
                    println!("Removed version '{}' from '{}'", version.version, coll.name);

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
                        println!(
                            "  Activated version '{}' as the new active version",
                            newest.version
                        );
                    }
                }
            } else {
                println!(
                    "Collection '{}' has no active version to remove.",
                    coll.name
                );
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
                    anyhow::bail!(
                        "Version '{}' not found in collection '{}'",
                        ver_name,
                        coll_name
                    );
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

pub(super) fn list_dats(all: bool, data_dir: Option<PathBuf>) -> Result<()> {
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

pub(super) fn activate_version(
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

pub(super) fn list_versions(collection: &str, data_dir: Option<PathBuf>) -> Result<()> {
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
