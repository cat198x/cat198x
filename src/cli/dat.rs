//! DAT file management commands

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

use crate::DatCommands;
use crate::dat::{DatSourceType, parse_dat_file_auto};
use crate::db::{Database, collections, dats};

use super::{fetch, open_database};

/// Run a DAT subcommand
pub fn run(cmd: DatCommands, data_dir: Option<PathBuf>) -> Result<()> {
    match cmd {
        DatCommands::Add {
            path,
            collection,
            recursive,
        } => {
            if recursive {
                add_dats_recursive(&path, collection.as_deref(), data_dir)
            } else {
                add_dat(&path, collection.as_deref(), data_dir)
            }
        }
        DatCommands::Remove {
            target,
            all_versions,
        } => remove_dat(&target, all_versions, data_dir),
        DatCommands::Relink { dir } => relink_dats(&dir, data_dir),
        DatCommands::List { all } => list_dats(all, data_dir),
        DatCommands::Activate {
            collection,
            version,
        } => activate_version(&collection, &version, data_dir),
        DatCommands::Diff {
            collection,
            from,
            to,
        } => diff_versions(&collection, from.as_deref(), to.as_deref(), data_dir),
        DatCommands::Versions { collection } => list_versions(&collection, data_dir),
        DatCommands::Fetch {
            source,
            url,
            output,
            list,
        } => fetch::run(source.as_deref(), url.as_deref(), output, list, data_dir),
        DatCommands::Upgrade { path, collection } => {
            upgrade_dat(&path, collection.as_deref(), data_dir)
        }
    }
}

fn add_dat(path: &Path, collection_name: Option<&str>, data_dir: Option<PathBuf>) -> Result<()> {
    let db = open_database(data_dir)?;
    // A single-file add has no recursive-add root, so no hierarchy is inferred;
    // the node falls back to the flat collection name.
    import_dat_file(&db, path, collection_name, false, None)?;
    Ok(())
}

/// Outcome of importing a single DAT file.
enum ImportOutcome {
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
fn import_dat_file(
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
    // catalogue safe — already-present DATs are reported, not errors.
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
fn add_dats_recursive(
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
/// `/`-joined string — the collection's place in the library tree.
///
/// `root/Acorn/BBC/Magazines/Laserbug/x.dat` under `root` yields
/// `Some("Acorn/BBC/Magazines/Laserbug")`. A DAT sitting directly in `root`
/// yields `None` (no hierarchy to infer — the import falls back to the flat
/// collection name). The separator is always `/` so stored paths are stable
/// across platforms.
fn relative_hierarchy(file: &Path, root: &Path) -> Option<String> {
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

/// Collect every `.dat`/`.xml` file under `dir`, sorted for stable output.
fn collect_dat_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = WalkDir::new(dir)
        .follow_links(true)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .filter(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("dat") || e.eq_ignore_ascii_case("xml"))
                .unwrap_or(false)
        })
        .collect();
    files.sort();
    files
}

/// Re-point registrations whose recorded DAT file no longer exists, by finding a
/// same-named DAT under `search_dir`. Matching is by file name, which stays
/// stable when a DAT is moved or a pack is reorganised (e.g. Downloads →
/// DatRoot). A unique match updates the recorded path; an absent or ambiguous
/// one is reported and left untouched. Versions whose file is still present are
/// skipped.
fn relink_dats(search_dir: &Path, data_dir: Option<PathBuf>) -> Result<()> {
    if !search_dir.is_dir() {
        anyhow::bail!("--relink expects a directory: {}", search_dir.display());
    }

    // Index candidate DATs under search_dir by file name.
    let mut by_name: HashMap<String, Vec<PathBuf>> = HashMap::new();
    for file in collect_dat_files(search_dir) {
        if let Some(name) = file.file_name().and_then(|n| n.to_str()) {
            by_name.entry(name.to_string()).or_default().push(file);
        }
    }

    let db = open_database(data_dir)?;
    let conn = db.conn();

    let mut relinked = 0usize;
    let mut still_missing = 0usize;
    let mut ambiguous = 0usize;

    for collection in collections::list_collections(conn)? {
        for version in collections::list_versions(conn, collection.id)? {
            // Only act on registrations whose recorded file is gone.
            if Path::new(&version.dat_path).is_file() {
                continue;
            }

            let basename = Path::new(&version.dat_path)
                .file_name()
                .and_then(|n| n.to_str());

            match basename.and_then(|n| by_name.get(n)).map(Vec::as_slice) {
                Some([found]) => {
                    let new_path = found.to_string_lossy();
                    collections::update_dat_path(conn, version.id, &new_path)?;
                    println!(
                        "  relinked  {} v{}  ->  {}",
                        collection.name, version.version, new_path
                    );
                    relinked += 1;
                }
                Some(multiple) => {
                    println!(
                        "  ambiguous {} v{}: {} files named '{}' under the search dir",
                        collection.name,
                        version.version,
                        multiple.len(),
                        basename.unwrap_or_default()
                    );
                    ambiguous += 1;
                }
                None => {
                    println!(
                        "  missing   {} v{}: no '{}' under {}",
                        collection.name,
                        version.version,
                        basename.unwrap_or("?"),
                        search_dir.display()
                    );
                    still_missing += 1;
                }
            }
        }
    }

    println!();
    if relinked == 0 && still_missing == 0 && ambiguous == 0 {
        println!("All registered DAT files are present; nothing to relink.");
    } else {
        println!(
            "Relinked {}, {} still missing, {} ambiguous.",
            relinked, still_missing, ambiguous
        );
    }
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

fn activate_version(collection: &str, version: &str, data_dir: Option<PathBuf>) -> Result<()> {
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
    println!(
        "  From: {} ({})",
        from_version.version, from_version.imported_at
    );
    println!(
        "  To:   {} ({})",
        to_version.version, to_version.imported_at
    );
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
    println!(
        "  {} → {} ({}{})",
        from_games.len(),
        to_games.len(),
        if to_games.len() >= from_games.len() {
            "+"
        } else {
            ""
        },
        to_games.len() as i64 - from_games.len() as i64
    );

    println!("ROMs (unique SHA1s):");
    println!(
        "  {} → {} ({}{})",
        from_sha1s.len(),
        to_sha1s.len(),
        if to_sha1s.len() >= from_sha1s.len() {
            "+"
        } else {
            ""
        },
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

    if added_games.is_empty()
        && removed_games.is_empty()
        && new_sha1s.is_empty()
        && removed_sha1s.is_empty()
    {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn collect_dat_files_finds_dat_and_xml_recursively_and_ignores_others() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let root = dir.path();

        fs::write(root.join("a.dat"), "x").expect("write a.dat");
        fs::write(root.join("b.DAT"), "x").expect("write b.DAT"); // case-insensitive
        fs::create_dir(root.join("nested")).expect("mkdir nested");
        fs::write(root.join("nested/c.xml"), "x").expect("write c.xml");
        fs::write(root.join("notes.txt"), "x").expect("write notes.txt"); // ignored
        fs::write(root.join("archive.zip"), "x").expect("write archive.zip"); // ignored

        let found = collect_dat_files(root);
        let names: Vec<String> = found
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();

        assert_eq!(found.len(), 3, "expected 3 DAT/XML files, got {names:?}");
        assert!(names.contains(&"a.dat".to_string()));
        assert!(names.contains(&"b.DAT".to_string()));
        assert!(names.contains(&"c.xml".to_string()));
        assert!(!names.iter().any(|n| n == "notes.txt" || n == "archive.zip"));
    }

    #[test]
    fn collect_dat_files_on_empty_dir_returns_nothing() {
        let dir = tempfile::tempdir().expect("create temp dir");
        assert!(collect_dat_files(dir.path()).is_empty());
    }

    const MINIMAL_DAT: &str = r#"<?xml version="1.0"?>
<datafile>
  <header>
    <name>Test Collection</name>
    <version>2020-01-01</version>
  </header>
  <game name="Game One">
    <description>Game One</description>
    <rom name="game one.rom" size="1000" sha1="ABC123"/>
  </game>
</datafile>"#;

    #[test]
    fn import_dat_file_is_idempotent() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let dat_path = dir.path().join("Test Collection.dat");
        fs::write(&dat_path, MINIMAL_DAT).expect("write dat");

        let db = Database::open_in_memory().expect("open db");

        // First import adds the version.
        let first = import_dat_file(&db, &dat_path, None, true, None).expect("first import");
        assert!(
            matches!(first, ImportOutcome::Added { games: 1, .. }),
            "first import should add one game"
        );

        // Re-importing the same version is a reported no-op, not a UNIQUE error.
        let second = import_dat_file(&db, &dat_path, None, true, None).expect("second import");
        assert!(
            matches!(second, ImportOutcome::AlreadyPresent),
            "re-import should skip as already present"
        );

        // And no duplicate version row was created.
        let conn = db.conn();
        let coll = collections::get_collection_by_name(conn, "Test Collection")
            .expect("query collection")
            .expect("collection exists");
        assert_eq!(
            collections::count_versions(conn, coll.id).expect("count versions"),
            1,
            "exactly one version should exist after a repeated import"
        );
    }

    #[test]
    fn relative_hierarchy_derives_nested_path() {
        let root = Path::new("/dats/TOSEC-PIX");
        let file = Path::new("/dats/TOSEC-PIX/Acorn/BBC/Magazines/Laserbug/x.dat");
        assert_eq!(
            relative_hierarchy(file, root),
            Some("Acorn/BBC/Magazines/Laserbug".to_string())
        );
    }

    #[test]
    fn relative_hierarchy_is_none_at_root() {
        let root = Path::new("/dats/TOSEC-PIX");
        let file = Path::new("/dats/TOSEC-PIX/flat.dat");
        assert_eq!(relative_hierarchy(file, root), None);
    }

    #[test]
    fn relative_hierarchy_is_none_when_unrelated_to_root() {
        let root = Path::new("/dats/TOSEC-PIX");
        let file = Path::new("/elsewhere/x.dat");
        assert_eq!(relative_hierarchy(file, root), None);
    }

    /// Read the single DAT node's stored `path` for a collection version.
    fn node_path_for(db: &Database, collection: &str) -> String {
        let conn = db.conn();
        conn.query_row(
            "SELECT n.path FROM dat_nodes n
             JOIN collection_versions cv ON n.version_id = cv.id
             JOIN collections c ON cv.collection_id = c.id
             WHERE c.name = ?",
            [collection],
            |row| row.get::<_, String>(0),
        )
        .expect("query node path")
    }

    #[test]
    fn recursive_add_records_relative_hierarchy_on_the_node() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let nested = dir.path().join("Acorn/BBC/Magazines/Laserbug");
        fs::create_dir_all(&nested).expect("mkdir nested");
        fs::write(nested.join("coll.dat"), MINIMAL_DAT).expect("write dat");

        let db = Database::open_in_memory().expect("open db");
        let file = nested.join("coll.dat");
        let rel = relative_hierarchy(&file, dir.path());
        import_dat_file(&db, &file, None, true, rel.as_deref()).expect("import");

        assert_eq!(
            node_path_for(&db, "Test Collection"),
            "Acorn/BBC/Magazines/Laserbug",
            "the node path should carry the directory relative to the add root"
        );
    }

    #[test]
    fn single_add_node_path_falls_back_to_collection_name() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let dat_path = dir.path().join("coll.dat");
        fs::write(&dat_path, MINIMAL_DAT).expect("write dat");

        let db = Database::open_in_memory().expect("open db");
        import_dat_file(&db, &dat_path, None, true, None).expect("import");

        assert_eq!(
            node_path_for(&db, "Test Collection"),
            "Test Collection",
            "with no hierarchy the node path falls back to the flat collection name"
        );
    }
}
