//! File scanning command with parallel processing and resume support

mod planning;
mod processing;

use anyhow::{Context, Result};
use indicatif::{ProgressBar, ProgressStyle};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Files are hashed and committed in batches of this size rather than hashing
/// the whole source into memory and writing once at the end. Reading files over
/// a flaky network mount is the slow, failure-prone phase, so committing every
/// batch bounds what a dropped or interrupted scan loses to one batch — at a few
/// thousand files per minute that is well under a minute of work, and every
/// committed batch survives. The incremental-scan resume logic then re-runs only
/// the files no batch has recorded.
const BATCH_SIZE: usize = 2000;

use crate::db::files::{self, Source};

use super::open_database;
use planning::{plan_source_files, resolve_walk_root, select_sources};
use processing::process_batch;

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
    let sources = select_sources(conn, source.as_deref())?;

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
    let walk_root = resolve_walk_root(source_path, subtree)?;

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

    let file_plan = plan_source_files(conn, source, source_path, &walk_root, full)?;
    let total_to_scan = file_plan.total_to_scan();
    let skipped = file_plan.skipped;

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
    for batch in file_plan.files_to_scan.chunks(BATCH_SIZE) {
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

/// Set up a Ctrl+C handler for graceful interruption
fn ctrlc_handler<F: Fn() + Send + 'static>(handler: F) -> Result<()> {
    ctrlc::set_handler(handler).context("Failed to set Ctrl+C handler")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::source_selector::source_matches;
    use planning::parse_sqlite_datetime;

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
