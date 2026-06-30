use anyhow::Result;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::db::quarantine as db_quarantine;
use crate::util::format_bytes;

use super::open_database;

/// Permanently delete quarantined files.
pub(super) fn run(collection: Option<String>, yes: bool, data_dir: Option<PathBuf>) -> Result<()> {
    let db = open_database(data_dir.clone())?;
    let conn = db.conn();
    let quarantine_dir = super::super::config::resolve_quarantine_dir(data_dir)?;

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
fn quarantine_file_holds_content(file_path: &Path, expected_sha1: &str) -> Result<bool> {
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
