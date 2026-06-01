//! File scanning command with parallel processing and resume support

use anyhow::{Context, Result};
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::SystemTime;
use walkdir::WalkDir;

use crate::db::files::{self, Source};
use crate::scanner::archive::{hash_archive_entries, ArchiveType};
use crate::scanner::hasher::{hash_file_with_header_detection, FileHashes};
use crate::util::truncate_path;

use super::open_database;

/// Result of hashing a single file or archive
#[derive(Debug)]
enum ScanResult {
    /// A loose file with its hashes
    LooseFile {
        relative_path: String,
        /// Full-file hashes (the true bytes on disk; the dedup identity).
        hashes: FileHashes,
        /// Headerless SHA1, set only when a header was detected and stripped.
        sha1_no_header: Option<String>,
        /// Header that was detected and skipped (for info only)
        header_skipped: Option<String>,
    },
    /// An archive with multiple entries
    Archive {
        relative_path: String,
        entries: Vec<ArchiveEntry>,
    },
    /// Failed to process the file
    Error {
        relative_path: String,
        error: String,
    },
}

/// A single entry from an archive
#[derive(Debug)]
struct ArchiveEntry {
    name: String,
    hashes: FileHashes,
}

/// Run the scan command
pub fn run(source: Option<Vec<String>>, full: bool, data_dir: Option<PathBuf>) -> Result<()> {
    let db = open_database(data_dir)?;
    let conn = db.conn();

    // Get sources to scan
    let sources = if let Some(paths) = &source {
        // Filter to specific sources
        let all_sources = files::list_sources(conn)?;
        all_sources
            .into_iter()
            .filter(|s| paths.iter().any(|p| s.path.contains(p)))
            .collect()
    } else {
        files::list_sources(conn)?
    };

    if sources.is_empty() {
        println!("No sources to scan.");
        println!();
        println!("Add a source directory with:");
        println!("  cat198x source add <path>");
        return Ok(());
    }

    println!(
        "Scanning {} source{}...",
        sources.len(),
        if sources.len() == 1 { "" } else { "s" }
    );
    if full {
        println!("  (full rescan - rehashing all files)");
    }
    println!();

    let mut total_files = 0;
    let mut total_entries = 0;
    let mut skipped_files = 0;

    for source in &sources {
        let (files, entries, skipped) = scan_source(conn, source, full)?;
        total_files += files;
        total_entries += entries;
        skipped_files += skipped;
    }

    println!();
    if skipped_files > 0 {
        println!(
            "Scan complete: {} files ({} skipped), {} archive entries",
            total_files, skipped_files, total_entries
        );
    } else {
        println!(
            "Scan complete: {} files, {} archive entries",
            total_files, total_entries
        );
    }

    Ok(())
}

/// Scan a single source directory with parallel hashing
fn scan_source(
    conn: &rusqlite::Connection,
    source: &Source,
    full: bool,
) -> Result<(usize, usize, usize)> {
    println!("Scanning: {}", source.path);

    let source_path = Path::new(&source.path);
    if !source_path.exists() {
        println!("  Warning: Source path does not exist, skipping");
        return Ok((0, 0, 0));
    }

    // Parse last_scanned timestamp for incremental scan
    let last_scanned = if full {
        None
    } else {
        source.last_scanned.as_ref().and_then(|ts| {
            // Parse SQLite datetime format: "YYYY-MM-DD HH:MM:SS"
            parse_sqlite_datetime(ts)
        })
    };

    // Single pass: collect all files, then partition into to-scan and skipped
    // Follow symlinks so users can symlink ROM folders from external drives
    let all_files: Vec<PathBuf> = WalkDir::new(source_path)
        .follow_links(true)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .collect();

    let total_files_in_source = all_files.len();

    // Filter to files that need scanning (modified since last scan, or full rescan)
    let files_to_scan: Vec<PathBuf> = if full {
        all_files
    } else {
        all_files
            .into_iter()
            .filter(|path| {
                // For incremental scan, check if file was modified since last scan
                if let Some(threshold) = last_scanned
                    && let Ok(metadata) = std::fs::metadata(path)
                        && let Ok(modified) = metadata.modified() {
                            return modified > threshold;
                        }
                // If we can't determine modification time, scan it
                true
            })
            .collect()
    };

    let total_to_scan = files_to_scan.len();
    let skipped = total_files_in_source - total_to_scan;

    if total_to_scan == 0 {
        println!("  No new or modified files to scan");
        files::update_source_scanned(conn, source.id)?;
        return Ok((0, 0, skipped));
    }

    if skipped > 0 {
        println!("  {} files to scan ({} unchanged)", total_to_scan, skipped);
    }

    let pb = ProgressBar::new(total_to_scan as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("  [{bar:40.cyan/blue}] {pos}/{len} {msg}")
            .expect("progress-bar template is a valid literal")
            .progress_chars("=>-"),
    );

    // For tracking progress across threads
    let processed_count = Arc::new(AtomicUsize::new(0));
    let interrupted = Arc::new(AtomicBool::new(false));

    // Set up Ctrl+C handler for graceful interruption
    let interrupted_clone = interrupted.clone();
    let _ = ctrlc_handler(move || {
        interrupted_clone.store(true, Ordering::SeqCst);
    });

    // Parallel hashing phase
    let results: Vec<ScanResult> = files_to_scan
        .par_iter()
        .map(|file_path| {
            // Check for interruption
            if interrupted.load(Ordering::SeqCst) {
                return ScanResult::Error {
                    relative_path: String::new(),
                    error: "Interrupted".to_string(),
                };
            }

            let relative_path = file_path
                .strip_prefix(source_path)
                .unwrap_or(file_path)
                .to_string_lossy()
                .to_string();

            // Update progress
            let count = processed_count.fetch_add(1, Ordering::SeqCst);
            if count.is_multiple_of(10) {
                pb.set_position(count as u64);
                pb.set_message(truncate_path(&relative_path, 30));
            }

            // Check if it's an archive
            if ArchiveType::from_path(file_path).is_some() {
                match hash_archive_entries(file_path) {
                    Ok(entries) => {
                        let archive_entries: Vec<ArchiveEntry> = entries
                            .into_iter()
                            .filter_map(|e| {
                                e.hashes.map(|h| ArchiveEntry {
                                    name: e.name,
                                    hashes: h,
                                })
                            })
                            .collect();
                        ScanResult::Archive {
                            relative_path,
                            entries: archive_entries,
                        }
                    }
                    Err(e) => ScanResult::Error {
                        relative_path,
                        error: e.to_string(),
                    },
                }
            } else {
                // Hash loose file with header detection
                match hash_file_with_header_detection(file_path) {
                    Ok(result) => {
                        // Identity is the full-file hash (the true bytes on
                        // disk); the headerless SHA1 is kept alongside so the
                        // file can also match headerless DATs (No-Intro).
                        // Discarding the full hash, as before, made headered
                        // files unmatchable against headered DATs.
                        let sha1_no_header =
                            result.headerless.as_ref().map(|h| h.sha1.clone());
                        let header_skipped = if result.headerless.is_some() {
                            result.header.map(|h| h.format.name().to_string())
                        } else {
                            None
                        };
                        ScanResult::LooseFile {
                            relative_path,
                            hashes: result.full,
                            sha1_no_header,
                            header_skipped,
                        }
                    }
                    Err(e) => ScanResult::Error {
                        relative_path,
                        error: e.to_string(),
                    },
                }
            }
        })
        .collect();

    pb.set_position(total_to_scan as u64);

    // Check if we were interrupted
    if interrupted.load(Ordering::SeqCst) {
        pb.finish_with_message("interrupted");
        println!("  Scan interrupted. Progress saved - run scan again to resume.");
        // Don't update last_scanned so we'll rescan on next run
        return Ok((0, 0, 0));
    }

    pb.set_message("writing to database...");

    // Sequential database write phase
    let mut processed_files = 0;
    let mut processed_entries = 0;
    let mut errors = Vec::new();

    let mut headers_skipped = 0;

    // One transaction for the whole write-back phase: a DB error part-way
    // through rolls back rather than leaving the catalogue half-updated, and
    // the many per-file upserts commit once instead of once each.
    let tx = conn.unchecked_transaction()?;

    for result in results {
        match result {
            ScanResult::LooseFile { relative_path, hashes, sha1_no_header, header_skipped } => {
                files::upsert_file(
                    conn,
                    &hashes.sha1,
                    sha1_no_header.as_deref(),
                    Some(&hashes.md5),
                    Some(&hashes.crc32),
                    hashes.size as i64,
                )?;
                files::upsert_file_location(
                    conn,
                    &hashes.sha1,
                    source.id,
                    &relative_path,
                    None,
                )?;
                processed_files += 1;
                if header_skipped.is_some() {
                    headers_skipped += 1;
                }
            }
            ScanResult::Archive { relative_path, entries } => {
                for entry in entries {
                    files::upsert_file(
                        conn,
                        &entry.hashes.sha1,
                        None, // archive entries aren't header-detected
                        Some(&entry.hashes.md5),
                        Some(&entry.hashes.crc32),
                        entry.hashes.size as i64,
                    )?;
                    files::upsert_file_location(
                        conn,
                        &entry.hashes.sha1,
                        source.id,
                        &relative_path,
                        Some(&entry.name),
                    )?;
                    processed_entries += 1;
                }
                processed_files += 1;
            }
            ScanResult::Error { relative_path, error } => {
                if !error.is_empty() && error != "Interrupted" {
                    errors.push((relative_path, error));
                }
            }
        }
    }

    pb.finish_with_message("done");

    // Report errors
    for (path, error) in &errors {
        println!("  Warning: {}: {}", path, error);
    }

    // Update source last_scanned
    files::update_source_scanned(conn, source.id)?;

    tx.commit()?;

    if headers_skipped > 0 {
        println!(
            "  {} files, {} archive entries ({} headers skipped)",
            processed_files, processed_entries, headers_skipped
        );
    } else {
        println!(
            "  {} files, {} archive entries",
            processed_files, processed_entries
        );
    }

    Ok((processed_files, processed_entries, skipped))
}

/// Parse SQLite datetime format to SystemTime
fn parse_sqlite_datetime(s: &str) -> Option<SystemTime> {
    use chrono::NaiveDateTime;

    // Format: "YYYY-MM-DD HH:MM:SS"
    NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S")
        .ok()
        .and_then(|dt| {
            dt.and_utc()
                .timestamp()
                .try_into()
                .ok()
                .map(|secs| SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(secs))
        })
}

/// Set up a Ctrl+C handler for graceful interruption
fn ctrlc_handler<F: Fn() + Send + 'static>(handler: F) -> Result<()> {
    ctrlc::set_handler(handler).context("Failed to set Ctrl+C handler")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_sqlite_datetime() {
        let dt = parse_sqlite_datetime("2024-01-15 10:30:45");
        assert!(dt.is_some());
    }

    #[test]
    fn test_parse_sqlite_datetime_invalid() {
        let dt = parse_sqlite_datetime("invalid");
        assert!(dt.is_none());
    }
}
