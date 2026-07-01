use anyhow::Result;
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use walkdir::WalkDir;

use crate::db::files::{self, Source};

use super::super::source_selector::source_matches;

pub(super) struct SourceFilePlan {
    pub(super) files_to_scan: Vec<PathBuf>,
    pub(super) skipped: usize,
}

impl SourceFilePlan {
    pub(super) fn total_to_scan(&self) -> usize {
        self.files_to_scan.len()
    }
}

/// Select sources to scan from optional `--source` selectors.
pub(super) fn select_sources(
    conn: &rusqlite::Connection,
    selectors: Option<&[String]>,
) -> Result<Vec<Source>> {
    let all_sources = files::list_sources(conn)?;
    Ok(match selectors {
        Some(selectors) => {
            // A purely numeric selector is a source id and matches exactly;
            // anything else matches as a path substring. The id form exists
            // because substring selection cannot always isolate a source - one
            // source's path may be a prefix of another's, and digits inside a
            // path can collide with id-like selectors.
            all_sources
                .into_iter()
                .filter(|source| selectors.iter().any(|sel| source_matches(source, sel)))
                .collect()
        }
        None => all_sources,
    })
}

/// Resolve and validate the filesystem walk root for a source/subtree scan.
pub(super) fn resolve_walk_root(source_path: &Path, subtree: Option<&str>) -> Result<PathBuf> {
    match subtree {
        Some(sub) => {
            // Keep the walk inside the source. `Path::starts_with` is lexical and
            // would not catch `..` (it compares components, not resolved paths),
            // so reject any subtree that is not a plain relative descent.
            use std::path::Component;
            let valid = Path::new(sub)
                .components()
                .all(|c| matches!(c, Component::Normal(_) | Component::CurDir));
            if sub.is_empty() || !valid {
                anyhow::bail!("--path {sub:?} escapes the source root");
            }
            Ok(source_path.join(sub))
        }
        None => Ok(source_path.to_path_buf()),
    }
}

/// Build the set of files that need hashing for this scan.
pub(super) fn plan_source_files(
    conn: &rusqlite::Connection,
    source: &Source,
    source_path: &Path,
    walk_root: &Path,
    full: bool,
) -> Result<SourceFilePlan> {
    // Parse last_scanned timestamp for incremental scan.
    let last_scanned = if full {
        None
    } else {
        source.last_scanned.as_ref().and_then(|ts| {
            // Parse SQLite datetime format: "YYYY-MM-DD HH:MM:SS"
            parse_sqlite_datetime(ts)
        })
    };

    // Single pass: collect all files, then partition into to-scan and skipped.
    // Follow symlinks so users can symlink ROM folders from external drives.
    let all_files: Vec<PathBuf> = WalkDir::new(walk_root)
        .follow_links(true)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .collect();

    let total_files_in_source = all_files.len();

    // Filter to files that need scanning: modified since last scan, or full rescan.
    let files_to_scan: Vec<PathBuf> = if full {
        all_files
    } else {
        // An incremental scan must also catch files that are on disk but absent
        // from the catalogue - added with an older mtime, or left behind when a
        // previous scan was interrupted before its write phase. Treating
        // uncatalogued files as always-scan makes incremental scans self-healing
        // and resumable.
        let known = files::catalogued_paths(conn, source.id)?;
        all_files
            .into_iter()
            .filter(|path| {
                let relative = path
                    .strip_prefix(source_path)
                    .unwrap_or(path)
                    .to_string_lossy();
                // Never catalogued here yet - always scan.
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
                // If we cannot determine modification time, scan it.
                true
            })
            .collect()
    };

    let skipped = total_files_in_source - files_to_scan.len();

    Ok(SourceFilePlan {
        files_to_scan,
        skipped,
    })
}

/// Parse SQLite datetime format to SystemTime.
pub(super) fn parse_sqlite_datetime(s: &str) -> Option<SystemTime> {
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
