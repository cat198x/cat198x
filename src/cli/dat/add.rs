use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use crate::cli::open_database;
use crate::dat::{DatSourceType, parse_dat_file_auto};
use crate::db::{Database, collections, dats};

use super::{chrono_lite_version, collect_dat_files, insert_dat_entry};

pub(super) fn add_dat(
    path: &Path,
    collection_name: Option<&str>,
    data_dir: Option<PathBuf>,
) -> Result<()> {
    let db = open_database(data_dir)?;
    // A single-file add has no recursive-add root, so no hierarchy is inferred;
    // the node falls back to the flat collection name.
    import_dat_file(&db, path, collection_name, false, None)?;
    Ok(())
}

/// Outcome of importing a single DAT file.
pub(super) enum ImportOutcome {
    /// A new version was imported.
    Added { games: usize, roms: usize },
    /// This exact version was already present; nothing changed.
    AlreadyPresent,
}

/// Import a single DAT file into an already-open database.
///
/// Returns what happened: a new version was [`Added`](ImportOutcome::Added), or
/// the same version was [`AlreadyPresent`](ImportOutcome::AlreadyPresent) and the
/// import was skipped. Re-adding an unchanged DAT is therefore a no-op rather than
/// a `UNIQUE` constraint error, which makes a recursive add over a pack that
/// overlaps the catalogue safe to repeat.
///
/// With `quiet`, the per-file progress chatter is suppressed so callers (such as
/// recursive add) can print their own summary. Each call commits its own
/// transaction, so one bad DAT in a batch does not roll back the DATs imported
/// before it.
pub(super) fn import_dat_file(
    db: &Database,
    path: &Path,
    collection_name: Option<&str>,
    quiet: bool,
    rel_path: Option<&str>,
) -> Result<ImportOutcome> {
    let abs_path =
        std::fs::canonicalize(path).with_context(|| format!("Cannot resolve path: {:?}", path))?;

    if !abs_path.is_file() {
        anyhow::bail!("Path is not a file: {}", abs_path.display());
    }

    if !quiet {
        println!("Parsing DAT file: {}", abs_path.display());
    }

    // Parse the DAT file (auto-detects Logiqx XML or ClrMamePro format)
    let (header, games) = parse_dat_file_auto(&abs_path)?;

    // Determine collection name
    let coll_name = collection_name
        .map(|s| s.to_string())
        .unwrap_or_else(|| header.name.clone());

    if !quiet {
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
    }

    // Detect source type
    let source_type = DatSourceType::detect(&header);
    if !quiet {
        println!("  Detected type: {}", source_type.as_str());
    }

    let conn = db.conn();

    // Version string (the DAT's own version, or today's date as a fallback).
    let version = header.version.clone().unwrap_or_else(chrono_lite_version);

    // Idempotency: if this exact version is already present for the collection,
    // skip rather than fail on the UNIQUE(collection_id, version) constraint.
    // This makes re-running a recursive add over a pack that overlaps the
    // catalogue safe - already-present DATs are reported, not errors.
    if let Some(existing) = collections::get_collection_by_name(conn, &coll_name)?
        && collections::get_version_by_name(conn, existing.id, &version)?.is_some()
    {
        if !quiet {
            println!();
            println!(
                "Version '{}' of '{}' is already present; nothing to do.",
                version, coll_name
            );
        }
        return Ok(ImportOutcome::AlreadyPresent);
    }

    // Wrap the whole import (collection, version, node, games, ROMs) in one
    // transaction: a mid-import failure rolls back cleanly instead of leaving
    // orphaned partial rows, and the per-row inserts commit once rather than
    // once each (a large speed-up on big DATs such as MAME).
    let tx = conn.unchecked_transaction()?;

    // Get or create collection
    let collection = match collections::get_collection_by_name(conn, &coll_name)? {
        Some(c) => {
            if !quiet {
                println!("\nAdding to existing collection: {}", c.name);
            }
            c
        }
        None => {
            if !quiet {
                println!("\nCreating new collection: {}", coll_name);
            }
            let _id = collections::create_collection(conn, &coll_name, source_type.as_str())?;
            collections::get_collection_by_name(conn, &coll_name)?
                .ok_or_else(|| anyhow::anyhow!("Failed to create collection"))?
        }
    };

    // Add version (activating it)
    let path_str = abs_path.to_string_lossy();
    let version_id = collections::add_version(conn, collection.id, &version, &path_str, true)?;

    // Create the DAT node. Its `path` carries the collection's place in the
    // library tree: the directory of the DAT relative to the recursive-add root
    // (e.g. "Acorn/BBC/Magazines/Laserbug") when known, falling back to the flat
    // collection name for a single-file add or a DAT sitting at the add root.
    // The destination builder reads this path to lay files out hierarchically.
    let node_path = rel_path.unwrap_or(header.name.as_str());
    let node_id = dats::create_node(conn, version_id, None, &header.name, "dat", node_path)?;

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

    if !quiet {
        println!();
        println!("Imported {} games with {} ROMs", game_count, rom_count);
        println!("Version '{}' is now active", version);
        println!();
        println!("Run 'cat198x scan' to match files against this DAT.");
    }

    Ok(ImportOutcome::Added {
        games: game_count,
        roms: rom_count,
    })
}

/// Add every `.dat`/`.xml` file found under `dir` (recursively).
///
/// The DB is opened once and each DAT is imported in its own transaction, so a
/// single malformed DAT is reported and skipped without losing the rest of the
/// batch. `--collection` is intentionally ignored here: each DAT names its own
/// collection from its header.
pub(super) fn add_dats_recursive(
    dir: &Path,
    collection_name: Option<&str>,
    data_dir: Option<PathBuf>,
) -> Result<()> {
    if !dir.is_dir() {
        anyhow::bail!(
            "--recursive expects a directory, but this is not one: {}",
            dir.display()
        );
    }
    if collection_name.is_some() {
        println!("Note: --collection is ignored with --recursive; each DAT names its own.");
    }

    let dat_files = collect_dat_files(dir);
    if dat_files.is_empty() {
        println!("No .dat or .xml files found under {}", dir.display());
        return Ok(());
    }

    println!(
        "Found {} DAT file(s) under {}",
        dat_files.len(),
        dir.display()
    );

    let db = open_database(data_dir)?;

    let mut added = 0usize;
    let mut skipped = 0usize;
    let mut games_total = 0usize;
    let mut roms_total = 0usize;
    let mut failures: Vec<(PathBuf, String)> = Vec::new();

    for (idx, file) in dat_files.iter().enumerate() {
        let rel = relative_hierarchy(file, dir);
        match import_dat_file(&db, file, None, true, rel.as_deref()) {
            Ok(ImportOutcome::Added { games, roms }) => {
                added += 1;
                games_total += games;
                roms_total += roms;
                let name = file
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| file.display().to_string());
                println!(
                    "  [{}/{}] {} ({} games)",
                    idx + 1,
                    dat_files.len(),
                    name,
                    games
                );
            }
            Ok(ImportOutcome::AlreadyPresent) => skipped += 1,
            Err(e) => failures.push((file.clone(), e.to_string())),
        }
    }

    println!();
    println!(
        "Added {} DAT file(s): {} games, {} ROMs.",
        added, games_total, roms_total
    );
    if skipped > 0 {
        println!("{} already present, skipped.", skipped);
    }
    if !failures.is_empty() {
        println!("{} file(s) failed:", failures.len());
        for (file, err) in &failures {
            println!("  {}: {}", file.display(), err);
        }
    }
    println!();
    println!("Run 'cat198x scan' to match files against these DATs.");

    Ok(())
}

/// The directory of `file` relative to the recursive-add `root`, as a
/// `/`-joined string - the collection's place in the library tree.
///
/// `root/Acorn/BBC/Magazines/Laserbug/x.dat` under `root` yields
/// `Some("Acorn/BBC/Magazines/Laserbug")`. A DAT sitting directly in `root`
/// yields `None` (no hierarchy to infer - the import falls back to the flat
/// collection name). The separator is always `/` so stored paths are stable
/// across platforms.
pub(super) fn relative_hierarchy(file: &Path, root: &Path) -> Option<String> {
    let rel = file.strip_prefix(root).ok()?;
    let dir = rel.parent()?;
    let segments: Vec<String> = dir
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    if segments.is_empty() {
        None
    } else {
        Some(segments.join("/"))
    }
}
