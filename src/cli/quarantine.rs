//! Quarantine command implementations
//!
//! The quarantine is a holding area for files that are no longer needed
//! at their current location but shouldn't be immediately deleted.

use anyhow::Result;
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;

use crate::QuarantineCommands;
use crate::db::quarantine as db_quarantine;

use super::open_database;

/// Run the quarantine command
pub fn run(cmd: QuarantineCommands, data_dir: Option<PathBuf>) -> Result<()> {
    match cmd {
        QuarantineCommands::Status {
            collection,
            detailed,
        } => run_status(collection, detailed, data_dir),
        QuarantineCommands::Prune { collection, yes } => run_prune(collection, yes, data_dir),
        QuarantineCommands::Restore {
            collection,
            target,
            yes,
        } => run_restore(collection, target, yes, data_dir),
    }
}

/// Show quarantine status and contents
fn run_status(collection: Option<String>, detailed: bool, data_dir: Option<PathBuf>) -> Result<()> {
    let db = open_database(data_dir.clone())?;
    let conn = db.conn();

    let entries = if let Some(ref pattern) = collection {
        db_quarantine::list_entries_by_collection(conn, pattern)?
    } else {
        db_quarantine::list_entries(conn)?
    };

    if entries.is_empty() {
        println!("Quarantine is empty.");
        return Ok(());
    }

    let total_size = db_quarantine::total_size(conn)?;
    let count = entries.len();

    println!(
        "Quarantine: {} files, {}",
        count,
        format_bytes(total_size as u64)
    );
    println!();

    // Show summary by collection
    let by_collection = db_quarantine::summary_by_collection(conn)?;
    if by_collection.len() > 1 || by_collection.iter().any(|(c, _, _)| c.is_some()) {
        println!("By collection:");
        for (coll, cnt, size) in &by_collection {
            let name = coll.as_deref().unwrap_or("(unknown)");
            println!(
                "  {} ··· {} files, {}",
                name,
                cnt,
                format_bytes(*size as u64)
            );
        }
        println!();
    }

    // Show summary by reason
    let by_reason = db_quarantine::summary_by_reason(conn)?;
    println!("By reason:");
    for (reason, cnt, size) in &by_reason {
        println!(
            "  {} ··· {} files, {}",
            reason.description(),
            cnt,
            format_bytes(*size as u64)
        );
    }

    if detailed {
        println!();
        println!("Files:");
        for entry in &entries {
            println!(
                "  {} ({}) - {}",
                truncate_path(&entry.original_path, 50),
                format_bytes(entry.size as u64),
                entry.reason.description()
            );
        }
    }

    println!();
    println!("Use 'cat198x quarantine prune' to permanently delete.");
    println!("Use 'cat198x quarantine restore' to move back to sources.");

    Ok(())
}

/// Permanently delete quarantined files
fn run_prune(collection: Option<String>, yes: bool, data_dir: Option<PathBuf>) -> Result<()> {
    let db = open_database(data_dir.clone())?;
    let conn = db.conn();
    let quarantine_dir = super::config::resolve_quarantine_dir(data_dir)?;

    let entries = if let Some(ref pattern) = collection {
        db_quarantine::list_entries_by_collection(conn, pattern)?
    } else {
        db_quarantine::list_entries(conn)?
    };

    if entries.is_empty() {
        println!("No files to prune.");
        return Ok(());
    }

    let total_size: i64 = entries.iter().map(|e| e.size).sum();

    println!(
        "Will permanently delete {} files ({})",
        entries.len(),
        format_bytes(total_size as u64)
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

    let mut deleted = 0;
    let mut errors = 0;

    for entry in &entries {
        let file_path = quarantine_dir.join(&entry.quarantine_path);

        // A permanent delete must not destroy the wrong bytes: re-hash the
        // quarantined file and confirm it still holds the recorded SHA1 before
        // unlinking. If it was replaced or tampered with, refuse.
        if file_path.exists() {
            match quarantine_file_holds_content(&file_path, &entry.sha1) {
                Ok(true) => {
                    if let Err(e) = fs::remove_file(&file_path) {
                        eprintln!("Failed to delete {}: {}", file_path.display(), e);
                        errors += 1;
                        continue;
                    }
                }
                Ok(false) => {
                    eprintln!(
                        "Skipping {}: recorded content {} not present — not deleting",
                        file_path.display(),
                        entry.sha1
                    );
                    errors += 1;
                    continue;
                }
                Err(e) => {
                    eprintln!(
                        "Failed to hash {} — not deleting: {}",
                        file_path.display(),
                        e
                    );
                    errors += 1;
                    continue;
                }
            }
        }

        // Remove from database
        if let Err(e) = db_quarantine::remove_entry(conn, entry.id) {
            eprintln!("Failed to remove database entry: {}", e);
            errors += 1;
            continue;
        }

        deleted += 1;
    }

    println!();
    println!("Pruned {} files, {} errors", deleted, errors);

    Ok(())
}

/// Check whether a quarantined file still holds the recorded content hash.
///
/// Quarantine entries record the *content* SHA1 — for a loose file that is the
/// file's own byte hash, but for an archived file (`.zip`/`.7z`) it is the hash
/// of the ROM *inside* the archive, never the archive's byte hash. Verifying
/// with a whole-file hash alone would therefore always fail for archived
/// entries (the same file-byte-vs-content mismatch fixed for CHDs). Accept a
/// match against the file's byte hash *or* any archive entry's content hash.
fn quarantine_file_holds_content(file_path: &std::path::Path, expected_sha1: &str) -> Result<bool> {
    // Loose file: the recorded hash is the file's own byte hash.
    let file_hash = crate::scanner::hasher::hash_file(file_path)?;
    if file_hash.sha1.eq_ignore_ascii_case(expected_sha1) {
        return Ok(true);
    }

    // Archived file: the recorded hash is the content of an entry inside it.
    if crate::scanner::archive::ArchiveType::from_path(file_path).is_some() {
        let entries = crate::scanner::archive::hash_archive_entries(file_path)?;
        if entries.iter().any(|e| {
            e.hashes
                .as_ref()
                .is_some_and(|h| h.sha1.eq_ignore_ascii_case(expected_sha1))
        }) {
            return Ok(true);
        }
    }

    Ok(false)
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

/// Format bytes as human-readable string
fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} bytes", bytes)
    }
}

/// Truncate a path for display
fn truncate_path(path: &str, max_len: usize) -> String {
    if path.len() <= max_len {
        path.to_string()
    } else {
        format!("...{}", &path[path.len() - max_len + 3..])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_bytes() {
        assert_eq!(format_bytes(0), "0 bytes");
        assert_eq!(format_bytes(512), "512 bytes");
        assert_eq!(format_bytes(1024), "1.00 KB");
        assert_eq!(format_bytes(1536), "1.50 KB");
        assert_eq!(format_bytes(1048576), "1.00 MB");
        assert_eq!(format_bytes(1073741824), "1.00 GB");
    }

    #[test]
    fn test_truncate_path() {
        assert_eq!(truncate_path("/short/path", 50), "/short/path");
        let long = "/very/long/path/that/exceeds/the/maximum/length/allowed";
        let truncated = truncate_path(long, 30);
        assert!(truncated.starts_with("..."));
        assert_eq!(truncated.len(), 30);
    }

    #[test]
    fn test_holds_content_loose_file_byte_hash() {
        let temp = tempfile::TempDir::new().unwrap();
        let f = temp.path().join("game.rom");
        std::fs::write(&f, b"loose rom bytes").unwrap();
        let sha1 = crate::scanner::hasher::hash_file(&f).unwrap().sha1;

        assert!(quarantine_file_holds_content(&f, &sha1).unwrap());
        assert!(
            !quarantine_file_holds_content(&f, "0000000000000000000000000000000000000000").unwrap()
        );
    }

    #[test]
    fn test_holds_content_archived_entry_hash() {
        let temp = tempfile::TempDir::new().unwrap();

        // The recorded content hash is the ROM *inside* the zip, not the zip's
        // own byte hash — derive it from a loose copy of the entry content.
        let content = b"the rom inside the archive";
        let loose = temp.path().join("inner.rom");
        std::fs::write(&loose, content).unwrap();
        let entry_sha1 = crate::scanner::hasher::hash_file(&loose).unwrap().sha1;

        let zip_path = temp.path().join("quarantined.zip");
        let file = std::fs::File::create(&zip_path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file("inner.rom", options).unwrap();
        zip.write_all(content).unwrap();
        zip.finish().unwrap();

        // The zip's byte hash must differ from the recorded entry hash, so the
        // old whole-file check would have wrongly refused to prune it.
        let zip_byte_sha1 = crate::scanner::hasher::hash_file(&zip_path).unwrap().sha1;
        assert_ne!(zip_byte_sha1, entry_sha1);

        assert!(quarantine_file_holds_content(&zip_path, &entry_sha1).unwrap());
        assert!(
            !quarantine_file_holds_content(&zip_path, "0000000000000000000000000000000000000000")
                .unwrap()
        );
    }

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
