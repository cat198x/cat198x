//! File scanning command with parallel processing and resume support

use anyhow::{Context, Result};
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::SystemTime;
use walkdir::WalkDir;

/// When stderr is not a terminal (piped, redirected, run in the background, or
/// in CI), the indicatif progress bar draws nothing, so the scan would appear to
/// hang. In that case we emit a plain progress line every this many files
/// instead, plus one for the final file.
const PROGRESS_LOG_INTERVAL: usize = 250;

/// Files are hashed and committed in batches of this size rather than hashing
/// the whole source into memory and writing once at the end. Reading files over
/// a flaky network mount is the slow, failure-prone phase, so committing every
/// batch bounds what a dropped or interrupted scan loses to one batch — at a few
/// thousand files per minute that is well under a minute of work, and every
/// committed batch survives. The incremental-scan resume logic then re-runs only
/// the files no batch has recorded.
const BATCH_SIZE: usize = 2000;

use crate::db::files::{self, Source};
use crate::scanner::archive::{ArchiveType, hash_archive_entries};
use crate::scanner::hasher::{FileHashes, hash_file_with_header_detection};
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

/// Build a CHD's scan hashes from its header and metadata only — never reading
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

/// Whether a `--source` selector picks this source: a purely numeric selector
/// is a source id and matches exactly; anything else matches as a path
/// substring.
fn source_matches(source: &files::Source, selector: &str) -> bool {
    match selector.parse::<i64>() {
        Ok(id) => source.id == id,
        Err(_) => source.path.contains(selector),
    }
}

/// Run the scan command
pub fn run(
    source: Option<Vec<String>>,
    full: bool,
    subtree: Option<String>,
    data_dir: Option<PathBuf>,
) -> Result<()> {
    let db = open_database(data_dir)?;
    let conn = db.conn();

    // Get sources to scan
    let sources = if let Some(selectors) = &source {
        // Filter to specific sources. A purely numeric selector is a source id
        // and matches exactly; anything else matches as a path substring. The
        // id form exists because substring selection cannot always isolate a
        // source — one source's path may be a prefix of another's (e.g.
        // `ToSort/MAME` and `ToSort/MAME 0.288 …`), and digits inside a path
        // (the `28` in `0.288`) collide with id-like selectors.
        let all_sources = files::list_sources(conn)?;
        all_sources
            .into_iter()
            .filter(|s| selectors.iter().any(|sel| source_matches(s, sel)))
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

    // A subtree only makes sense against one source — its meaning is ambiguous
    // across several, and chunked scanning targets one big source at a time.
    if subtree.is_some() && sources.len() != 1 {
        anyhow::bail!(
            "--path scans a subtree of a single source; narrow with --source (matched {} sources)",
            sources.len()
        );
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
        let (files, entries, skipped) = scan_source(conn, source, full, subtree.as_deref())?;
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

/// Scan a single source directory with parallel hashing.
///
/// When `subtree` is set, only that subdirectory (relative to the source root)
/// is walked, but files are still catalogued under the source with paths
/// relative to its root. This lets a huge source on a slow mount be scanned in
/// bounded chunks — one walk per subtree completes and commits instead of one
/// unbounded walk of the whole tree. A subtree scan is partial by definition, so
/// it never stamps `last_scanned` (that would falsely mark the whole source
/// done); the resume logic still picks up the rest on later runs.
fn scan_source(
    conn: &rusqlite::Connection,
    source: &Source,
    full: bool,
    subtree: Option<&str>,
) -> Result<(usize, usize, usize)> {
    let source_path = Path::new(&source.path);

    // Resolve and validate the walk root: the source itself, or a subtree of it.
    let walk_root = match subtree {
        Some(sub) => {
            // Keep the walk inside the source. `Path::starts_with` is lexical and
            // wouldn't catch `..` (it compares components, not resolved paths), so
            // reject any subtree that isn't a plain relative descent — no absolute
            // path, no `..`, no leading `/`.
            use std::path::Component;
            let valid = Path::new(sub)
                .components()
                .all(|c| matches!(c, Component::Normal(_) | Component::CurDir));
            if sub.is_empty() || !valid {
                anyhow::bail!("--path {sub:?} escapes the source root");
            }
            source_path.join(sub)
        }
        None => source_path.to_path_buf(),
    };

    match subtree {
        Some(sub) => println!("Scanning: {} (subtree {})", source.path, sub),
        None => println!("Scanning: {}", source.path),
    }

    if !walk_root.exists() {
        let what = if subtree.is_some() {
            "Subtree"
        } else {
            "Source path"
        };
        println!("  Warning: {what} does not exist, skipping");
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
    let all_files: Vec<PathBuf> = WalkDir::new(&walk_root)
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
        // An incremental scan must also catch files that are on disk but absent
        // from the catalogue — added with an older mtime, or left behind when a
        // previous scan was interrupted before its write phase. Without this, a
        // partial scan that still stamped `last_scanned` would strand the rest
        // forever (their mtime predates the stamp), and a dropped scan over a
        // flaky mount could never resume. Treating uncatalogued files as
        // always-scan makes incremental scans self-healing and resumable.
        let known = files::catalogued_paths(conn, source.id)?;
        all_files
            .into_iter()
            .filter(|path| {
                let relative = path
                    .strip_prefix(source_path)
                    .unwrap_or(path)
                    .to_string_lossy();
                // Never catalogued here yet — always scan.
                if !known.contains(relative.as_ref()) {
                    return true;
                }
                // Already catalogued: scan only if modified since last scan.
                if let Some(threshold) = last_scanned
                    && let Ok(metadata) = std::fs::metadata(path)
                    && let Ok(modified) = metadata.modified()
                {
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
        // A subtree scan covers only part of the source, so it must not stamp
        // the source as fully scanned.
        if subtree.is_none() {
            files::update_source_scanned(conn, source.id)?;
        }
        return Ok((0, 0, skipped));
    }

    if skipped > 0 {
        println!("  {} files to scan ({} unchanged)", total_to_scan, skipped);
    }

    // A terminal gets the live progress bar; anything else (pipe, redirect,
    // background, CI) gets periodic textual progress lines instead, because the
    // bar is invisible there and the scan would otherwise look frozen.
    let interactive = std::io::stderr().is_terminal();
    let pb = if interactive {
        let bar = ProgressBar::new(total_to_scan as u64);
        bar.set_style(
            ProgressStyle::default_bar()
                .template("  [{bar:40.cyan/blue}] {pos}/{len} {msg}")
                .expect("progress-bar template is a valid literal")
                .progress_chars("=>-"),
        );
        bar
    } else {
        println!("  hashing {} files...", total_to_scan);
        ProgressBar::hidden()
    };

    // For tracking progress across threads
    let processed_count = Arc::new(AtomicUsize::new(0));
    let interrupted = Arc::new(AtomicBool::new(false));

    // Set up Ctrl+C handler for graceful interruption
    let interrupted_clone = interrupted.clone();
    let _ = ctrlc_handler(move || {
        interrupted_clone.store(true, Ordering::SeqCst);
    });

    let mut processed_files = 0;
    let mut processed_entries = 0;
    let mut headers_skipped = 0;
    let mut errors: Vec<(String, String)> = Vec::new();

    // Hash and commit in batches so a dropped or interrupted scan keeps every
    // completed batch instead of losing the whole run (see BATCH_SIZE).
    for batch in files_to_scan.chunks(BATCH_SIZE) {
        if interrupted.load(Ordering::SeqCst) {
            break;
        }
        let stats = process_batch(
            conn,
            source,
            source_path,
            batch,
            &processed_count,
            &interrupted,
            interactive,
            &pb,
            total_to_scan,
        )?;
        processed_files += stats.files;
        processed_entries += stats.entries;
        headers_skipped += stats.headers_skipped;
        errors.extend(stats.errors);
    }

    pb.set_position(processed_count.load(Ordering::SeqCst) as u64);

    // Report per-file errors surfaced while hashing the batches.
    for (path, error) in &errors {
        println!("  Warning: {}: {}", path, error);
    }

    // An interrupted scan keeps its committed batches but must not stamp
    // last_scanned: the files it never reached have to be picked up next run,
    // which the resume logic handles by treating uncatalogued files as new.
    if interrupted.load(Ordering::SeqCst) {
        pb.finish_with_message("interrupted");
        println!(
            "  Scan interrupted after {} files. Progress saved — run scan again to resume.",
            processed_files
        );
        return Ok((processed_files, processed_entries, skipped));
    }

    pb.finish_with_message("done");

    // Update source last_scanned (only on a fully completed scan of the whole
    // source — a subtree scan is partial and must not stamp it).
    if subtree.is_none() {
        files::update_source_scanned(conn, source.id)?;
    }

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

/// Tallies from processing one batch, accumulated across batches by the caller.
#[derive(Default)]
struct BatchStats {
    files: usize,
    entries: usize,
    headers_skipped: usize,
    /// `(relative_path, error)` for each file that failed to hash.
    errors: Vec<(String, String)>,
}

/// Hash one batch of files in parallel, then commit them in a single
/// transaction. One transaction per batch: a DB error rolls back just this
/// batch, and the per-file upserts commit together rather than once each.
#[allow(clippy::too_many_arguments)]
fn process_batch(
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
                // header, which is what <disk> DAT entries reference — not the
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

    fn source_with(id: i64, path: &str) -> files::Source {
        files::Source {
            id,
            path: path.to_string(),
            case_sensitive: false,
            added_at: String::new(),
            last_scanned: None,
            disposition: files::Disposition::Preserve,
        }
    }

    #[test]
    fn source_matches_numeric_selector_by_id_only() {
        // The regression this guards: source 31's path contains the digits
        // "28" (inside "0.288"), so a substring match for the id selector
        // "28" used to pick the wrong source — and never the intended one.
        let mame = source_with(28, "/Volumes/Data/ToSort/MAME");
        let sl = source_with(
            31,
            "/Volumes/Data/ToSort/MAME 0.288 Software List ROMs (merged)",
        );

        assert!(source_matches(&mame, "28"));
        assert!(!source_matches(&sl, "28"));
        assert!(source_matches(&sl, "31"));
        assert!(!source_matches(&mame, "31"));
    }

    #[test]
    fn source_matches_non_numeric_selector_by_path_substring() {
        let mame = source_with(28, "/Volumes/Data/ToSort/MAME");
        assert!(source_matches(&mame, "ToSort/MAME"));
        assert!(!source_matches(&mame, "Library/ROMs"));
    }

    #[test]
    fn scan_catalogues_files_then_resumes_uncatalogued() {
        use crate::db::Database;
        use crate::db::files::{add_source, catalogued_paths, get_source_by_path};

        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.rom"), b"alpha").unwrap();
        std::fs::write(dir.path().join("b.rom"), b"bravo").unwrap();
        let root = dir.path().to_str().unwrap();

        let db = Database::open_in_memory().unwrap();
        let conn = db.conn();
        add_source(conn, root, false).unwrap();
        let source = get_source_by_path(conn, root).unwrap().unwrap();

        // A full scan catalogues every file via the batch path.
        let (files, _entries, _skipped) = scan_source(conn, &source, false, None).unwrap();
        assert_eq!(files, 2);
        assert_eq!(catalogued_paths(conn, source.id).unwrap().len(), 2);

        // Force last_scanned into the future so the modified-since filter would
        // skip every file. A newly added, still-uncatalogued file must be
        // scanned anyway — this is the resume guarantee, independent of mtime.
        conn.execute(
            "UPDATE sources SET last_scanned = '2999-01-01 00:00:00' WHERE id = ?",
            [source.id],
        )
        .unwrap();
        std::fs::write(dir.path().join("c.rom"), b"charlie").unwrap();

        let source = get_source_by_path(conn, root).unwrap().unwrap();
        let (files2, _entries2, skipped2) = scan_source(conn, &source, false, None).unwrap();
        assert_eq!(files2, 1, "only the uncatalogued newcomer is hashed");
        assert_eq!(skipped2, 2, "the two already-catalogued files are skipped");
        let paths = catalogued_paths(conn, source.id).unwrap();
        assert_eq!(paths.len(), 3);
        assert!(paths.contains("c.rom"));
    }

    #[test]
    fn scan_subtree_catalogues_only_that_subtree_and_keeps_source_relative_paths() {
        use crate::db::Database;
        use crate::db::files::{add_source, catalogued_paths, get_source_by_path};

        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("Sinclair")).unwrap();
        std::fs::create_dir_all(dir.path().join("Atari")).unwrap();
        std::fs::write(dir.path().join("Sinclair/game.rom"), b"spectrum").unwrap();
        std::fs::write(dir.path().join("Atari/game.rom"), b"atari").unwrap();
        let root = dir.path().to_str().unwrap();

        let db = Database::open_in_memory().unwrap();
        let conn = db.conn();
        add_source(conn, root, false).unwrap();
        let source = get_source_by_path(conn, root).unwrap().unwrap();

        // Scanning the Sinclair subtree catalogues only its file, under a path
        // relative to the source root — and does not stamp last_scanned.
        let (files, _entries, _skipped) =
            scan_source(conn, &source, false, Some("Sinclair")).unwrap();
        assert_eq!(files, 1);
        let paths = catalogued_paths(conn, source.id).unwrap();
        assert_eq!(paths.len(), 1);
        assert!(paths.contains("Sinclair/game.rom"));
        assert!(
            get_source_by_path(conn, root)
                .unwrap()
                .unwrap()
                .last_scanned
                .is_none(),
            "a subtree scan must not stamp the source as fully scanned"
        );

        // A second subtree adds to the same source's catalogue.
        let (files2, _e2, _s2) = scan_source(conn, &source, false, Some("Atari")).unwrap();
        assert_eq!(files2, 1);
        let paths = catalogued_paths(conn, source.id).unwrap();
        assert_eq!(paths.len(), 2);
        assert!(paths.contains("Atari/game.rom"));
    }

    #[test]
    fn scan_subtree_escaping_source_root_is_rejected() {
        use crate::db::Database;
        use crate::db::files::{add_source, get_source_by_path};

        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let db = Database::open_in_memory().unwrap();
        let conn = db.conn();
        add_source(conn, root, false).unwrap();
        let source = get_source_by_path(conn, root).unwrap().unwrap();

        let err = scan_source(conn, &source, false, Some("../escape")).unwrap_err();
        assert!(err.to_string().contains("escapes the source root"));
    }
}
