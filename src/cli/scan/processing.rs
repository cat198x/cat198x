use anyhow::Result;
use indicatif::ProgressBar;
use rayon::prelude::*;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::db::files::{self, Source};
use crate::scanner::archive::{ArchiveType, hash_archive_entries};
use crate::scanner::hasher::{FileHashes, hash_file_with_header_detection};
use crate::util::truncate_path;

/// When stderr is not a terminal (piped, redirected, run in the background, or
/// in CI), the indicatif progress bar draws nothing, so the scan would appear to
/// hang. In that case we emit a plain progress line every this many files
/// instead, plus one for the final file.
const PROGRESS_LOG_INTERVAL: usize = 250;

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

/// Tallies from processing one batch, accumulated across batches by the caller.
#[derive(Default)]
pub(super) struct BatchStats {
    pub(super) files: usize,
    pub(super) entries: usize,
    pub(super) headers_skipped: usize,
    /// `(relative_path, error)` for each file that failed to hash.
    pub(super) errors: Vec<(String, String)>,
}

/// Build a CHD's scan hashes from its header and metadata only - never reading
/// the (multi-GB) body. The match identity is the internal header SHA1; size
/// comes from metadata; the container's md5/crc are not meaningful for a CHD and
/// are left empty.
fn chd_hashes(path: &Path) -> Result<FileHashes> {
    let sha1 = crate::scanner::chd::read_chd_sha1(path)?;
    let size = std::fs::metadata(path)?.len();
    Ok(FileHashes {
        sha1,
        md5: String::new(),
        crc32: String::new(),
        size,
    })
}

/// Hash one batch of files in parallel, then commit them in a single
/// transaction. One transaction per batch: a DB error rolls back just this
/// batch, and the per-file upserts commit together rather than once each.
#[allow(clippy::too_many_arguments)]
pub(super) fn process_batch(
    conn: &rusqlite::Connection,
    source: &Source,
    source_path: &Path,
    batch: &[PathBuf],
    processed_count: &AtomicUsize,
    interrupted: &AtomicBool,
    interactive: bool,
    pb: &ProgressBar,
    total_to_scan: usize,
) -> Result<BatchStats> {
    // Parallel hashing phase for this batch
    let results: Vec<ScanResult> = batch
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
            let done = processed_count.fetch_add(1, Ordering::SeqCst) + 1;
            if interactive {
                if done.is_multiple_of(10) {
                    pb.set_position(done as u64);
                    pb.set_message(truncate_path(&relative_path, 30));
                }
            } else if done.is_multiple_of(PROGRESS_LOG_INTERVAL) || done == total_to_scan {
                println!(
                    "  hashed {}/{} ({}%)",
                    done,
                    total_to_scan,
                    done * 100 / total_to_scan
                );
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
            } else if crate::scanner::chd::is_chd_path(file_path) {
                // A CHD's identity is its *internal* logical-data SHA1 from the
                // header, which is what <disk> DAT entries reference - not the
                // hash of the .chd file's bytes. Read only the 124-byte header,
                // never the (multi-GB) body: the internal SHA1 is the match key,
                // size comes from metadata, and the container's md5/crc aren't
                // meaningful for a CHD. An unreadable header surfaces as a scan
                // error rather than a silently unmatchable (file-hashed) CHD.
                match chd_hashes(file_path) {
                    Ok(hashes) => ScanResult::LooseFile {
                        relative_path,
                        hashes,
                        sha1_no_header: None,
                        header_skipped: None,
                    },
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
                        let sha1_no_header = result.headerless.as_ref().map(|h| h.sha1.clone());
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

    // Sequential database write phase for this batch
    let mut stats = BatchStats::default();

    let tx = conn.unchecked_transaction()?;

    for result in results {
        match result {
            ScanResult::LooseFile {
                relative_path,
                hashes,
                sha1_no_header,
                header_skipped,
            } => {
                files::upsert_file(
                    conn,
                    &hashes.sha1,
                    sha1_no_header.as_deref(),
                    Some(&hashes.md5),
                    Some(&hashes.crc32),
                    hashes.size as i64,
                )?;
                files::upsert_file_location(conn, &hashes.sha1, source.id, &relative_path, None)?;
                stats.files += 1;
                if header_skipped.is_some() {
                    stats.headers_skipped += 1;
                }
            }
            ScanResult::Archive {
                relative_path,
                entries,
            } => {
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
                    stats.entries += 1;
                }
                stats.files += 1;
            }
            ScanResult::Error {
                relative_path,
                error,
            } => {
                if !error.is_empty() && error != "Interrupted" {
                    stats.errors.push((relative_path, error));
                }
            }
        }
    }

    tx.commit()?;

    Ok(stats)
}
