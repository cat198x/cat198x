//! Quarantine command implementations
//!
//! The quarantine is a holding area for files that are no longer needed
//! at their current location but shouldn't be immediately deleted.

mod prune;
mod status;

use anyhow::Result;
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;

use crate::cli::args::QuarantineCommands;
use crate::db::quarantine as db_quarantine;
use crate::util::format_bytes;

use super::open_database;

/// Run the quarantine command
pub fn run(cmd: QuarantineCommands, data_dir: Option<PathBuf>) -> Result<()> {
    match cmd {
        QuarantineCommands::Status {
            collection,
            detailed,
        } => status::run(collection, detailed, data_dir),
        QuarantineCommands::Prune { collection, yes } => prune::run(collection, yes, data_dir),
        QuarantineCommands::Restore {
            collection,
            target,
            yes,
        } => run_restore(collection, target, yes, data_dir),
    }
}

/// Restore quarantined files back to a source directory
fn run_restore(
    collection: Option<String>,
    target: Option<PathBuf>,
    yes: bool,
    data_dir: Option<PathBuf>,
) -> Result<()> {
    let db = open_database(data_dir.clone())?;
    let conn = db.conn();
    let quarantine_dir = super::config::resolve_quarantine_dir(data_dir)?;

    let entries = if let Some(ref pattern) = collection {
        db_quarantine::list_entries_by_collection(conn, pattern)?
    } else {
        db_quarantine::list_entries(conn)?
    };

    if entries.is_empty() {
        println!("No files to restore.");
        return Ok(());
    }

    // Determine target directory
    let target_dir = match target {
        Some(t) => t,
        None => {
            // Try to get first source directory
            let sources = crate::db::files::list_sources(conn)?;
            if sources.is_empty() {
                anyhow::bail!(
                    "No target directory specified and no sources registered.\n\
                     Use --target <path> to specify where to restore files."
                );
            }
            PathBuf::from(&sources[0].path)
        }
    };

    if !target_dir.exists() {
        anyhow::bail!("Target directory does not exist: {}", target_dir.display());
    }

    let total_size: i64 = entries.iter().map(|e| e.size).sum();

    println!(
        "Will restore {} files ({}) to {}",
        entries.len(),
        format_bytes(total_size as u64),
        target_dir.display()
    );

    if !yes {
        print!("Continue? [y/N] ");
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;

        if !input.trim().eq_ignore_ascii_case("y") {
            println!("Aborted.");
            return Ok(());
        }
    }

    let mut restored = 0;
    let mut errors = 0;

    for entry in &entries {
        let source_path = quarantine_dir.join(&entry.quarantine_path);

        // Use original filename for restoration
        let filename = std::path::Path::new(&entry.original_path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(&entry.quarantine_path);

        let dest_path = target_dir.join(filename);

        // Check for conflicts
        if dest_path.exists() {
            eprintln!("Skipping {} - file already exists at destination", filename);
            errors += 1;
            continue;
        }

        // Move the file
        if source_path.exists() {
            if let Err(e) = fs::rename(&source_path, &dest_path) {
                // If rename fails (cross-device), try copy + delete
                if let Err(e2) = fs::copy(&source_path, &dest_path) {
                    eprintln!("Failed to restore {}: {} / {}", filename, e, e2);
                    errors += 1;
                    continue;
                }
                if let Err(e) = fs::remove_file(&source_path) {
                    eprintln!("Warning: Failed to remove source after copy: {}", e);
                }
            }
        } else {
            eprintln!(
                "Skipping {} - quarantine file not found",
                entry.quarantine_path
            );
            errors += 1;
            continue;
        }

        // Remove from database
        if let Err(e) = db_quarantine::remove_entry(conn, entry.id) {
            eprintln!("Warning: Failed to remove database entry: {}", e);
        }

        restored += 1;
    }

    println!();
    println!(
        "Restored {} files to {}, {} errors",
        restored,
        target_dir.display(),
        errors
    );

    // Remind user to rescan
    if restored > 0 {
        println!();
        println!("Run 'cat198x scan' to update the file catalog.");
    }

    Ok(())
}

/// Move a file to quarantine
///
/// This is called from the apply workflow when a file needs to be quarantined.
pub fn move_to_quarantine(
    file_path: &str,
    sha1: &str,
    size: i64,
    reason: db_quarantine::QuarantineReason,
    collection_name: Option<&str>,
    data_dir: Option<PathBuf>,
) -> Result<String> {
    // Resolve the store location here (config vs default) and open the
    // connection; the file move + catalogue entry are the library primitive.
    let quarantine_dir = super::config::resolve_quarantine_dir(data_dir.clone())?;
    let db = open_database(data_dir)?;
    crate::plan::executor::execute_quarantine(
        db.conn(),
        file_path,
        sha1,
        size,
        reason,
        collection_name,
        &quarantine_dir,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_move_to_quarantine_refuses_to_overwrite() {
        let temp = tempfile::TempDir::new().unwrap();
        let data_dir = temp.path().join("data");
        let sha1 = "ABCDEF0123456789ABCDEF0123456789ABCDEF01";

        // Cat198x must be initialised so the quarantine DB exists.
        crate::cli::init::run(None, Some(data_dir.clone())).unwrap();

        // First file is quarantined under a full-SHA1 filename.
        let f1 = temp.path().join("game.rom");
        std::fs::write(&f1, b"first").unwrap();
        move_to_quarantine(
            f1.to_str().unwrap(),
            sha1,
            5,
            db_quarantine::QuarantineReason::SetRemoved,
            None,
            Some(data_dir.clone()),
        )
        .unwrap();
        let qfile = data_dir
            .join("quarantine")
            .join(format!("{}_game.rom", sha1));
        assert!(qfile.exists(), "quarantined under the full-SHA1 name");
        let original = std::fs::read(&qfile).unwrap();

        // A different file mapping to the same quarantine path must be refused,
        // not silently clobbered, and its source left in place.
        let f2 = temp.path().join("game.rom");
        std::fs::write(&f2, b"second-and-different").unwrap();
        let result = move_to_quarantine(
            f2.to_str().unwrap(),
            sha1,
            20,
            db_quarantine::QuarantineReason::SetRemoved,
            None,
            Some(data_dir.clone()),
        );
        assert!(
            result.is_err(),
            "must refuse to overwrite an existing quarantine file"
        );
        assert_eq!(
            std::fs::read(&qfile).unwrap(),
            original,
            "existing quarantine file untouched"
        );
        assert!(
            f2.exists(),
            "source left in place when quarantine is refused"
        );
    }
}
