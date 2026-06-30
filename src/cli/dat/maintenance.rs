use anyhow::Result;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::cli::open_database;
use crate::dat::parse_dat_file_auto;
use crate::db::{collections, dats};

use super::collect_dat_files;

/// Re-point registrations whose recorded DAT file no longer exists, by finding a
/// same-named DAT under `search_dir`. Matching is by file name, which stays
/// stable when a DAT is moved or a pack is reorganised (e.g. Downloads to
/// DatRoot). A unique match updates the recorded path; an absent or ambiguous
/// one is reported and left untouched. Versions whose file is still present are
/// skipped.
pub(super) fn relink_dats(search_dir: &Path, data_dir: Option<PathBuf>) -> Result<()> {
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

/// Re-parse each collection's active DAT and correct names corrupted by an
/// earlier parser that dropped the text around XML entities in DAT headers
/// (e.g. "Commodore C64 - Games - Shoot&apos;em Up" stored as "em Up"). Only
/// the collection name and its root DAT node name/path are rewritten - games,
/// ROMs, versions, and scan data are untouched, since game names always came
/// from correctly-unescaped XML attributes. A collection whose DAT file is
/// missing is left alone (it can't be re-parsed); a corrected name that would
/// collide with a different existing collection is reported and skipped rather
/// than merging two catalogues under one name.
pub(super) fn repair_names(data_dir: Option<PathBuf>) -> Result<()> {
    let db = open_database(data_dir)?;
    let conn = db.conn();

    let mut fixed = 0usize;
    let mut unchanged = 0usize;
    let mut conflicts = 0usize;
    let mut missing = 0usize;

    for collection in collections::list_collections(conn)? {
        let Some(version) = collections::get_active_version(conn, collection.id)? else {
            continue;
        };
        if !Path::new(&version.dat_path).is_file() {
            // Can't re-derive the name without the DAT; leave it untouched.
            missing += 1;
            continue;
        }

        let header = match parse_dat_file_auto(Path::new(&version.dat_path)) {
            Ok((header, _games)) => header,
            Err(e) => {
                println!("  skip      '{}': parse failed ({})", collection.name, e);
                continue;
            }
        };
        let correct = header.name.trim();
        if correct.is_empty() || correct == collection.name {
            unchanged += 1;
            continue;
        }

        if let Some(other) = collections::get_collection_by_name(conn, correct)?
            && other.id != collection.id
        {
            println!(
                "  conflict  '{}' -> '{}' (name already used by another collection)",
                collection.name, correct
            );
            conflicts += 1;
            continue;
        }

        let tx = conn.unchecked_transaction()?;
        collections::rename_collection(conn, collection.id, correct)?;
        for v in collections::list_versions(conn, collection.id)? {
            dats::rename_dat_node(conn, v.id, correct)?;
        }
        tx.commit()?;
        println!("  fixed     '{}' -> '{}'", collection.name, correct);
        fixed += 1;
    }

    println!();
    if fixed == 0 && conflicts == 0 && missing == 0 {
        println!(
            "All {} collection name(s) already correct; nothing to repair.",
            unchanged
        );
    } else {
        println!(
            "Repaired {fixed} name(s); {unchanged} already correct, {conflicts} conflict(s), {missing} with a missing DAT file."
        );
        if missing > 0 {
            println!(
                "Tip: 'cat198x dat relink <dir>' can re-point missing DAT files, then re-run repair-names."
            );
        }
    }
    Ok(())
}
