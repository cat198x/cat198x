use anyhow::Result;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::db::quarantine as db_quarantine;
use crate::util::format_bytes;

use super::open_database;

/// Restore quarantined files back to a source directory.
pub(super) fn run(
    collection: Option<String>,
    target: Option<PathBuf>,
    yes: bool,
    data_dir: Option<PathBuf>,
) -> Result<()> {
    let db = open_database(data_dir.clone())?;
    let conn = db.conn();
    let quarantine_dir = super::super::config::resolve_quarantine_dir(data_dir)?;

    let entries = if let Some(ref pattern) = collection {
        db_quarantine::list_entries_by_collection(conn, pattern)?
    } else {
        db_quarantine::list_entries(conn)?
    };

    if entries.is_empty() {
        println!("No files to restore.");
        return Ok(());
    }

    let target_dir = match target {
        Some(t) => t,
        None => {
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

        let filename = Path::new(&entry.original_path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(&entry.quarantine_path);

        let dest_path = target_dir.join(filename);

        if dest_path.exists() {
            eprintln!("Skipping {} - file already exists at destination", filename);
            errors += 1;
            continue;
        }

        if source_path.exists() {
            if let Err(e) = fs::rename(&source_path, &dest_path) {
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

    if restored > 0 {
        println!();
        println!("Run 'cat198x scan' to update the file catalog.");
    }

    Ok(())
}
