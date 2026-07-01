use anyhow::Result;
use std::path::PathBuf;

use crate::cli::audit_log::write_hard_delete_log;
use crate::db::files::{self, Source};
use crate::plan::executor::delete_has_surviving_copy;

use super::analysis::Candidate;

pub(super) struct ExecutionReport {
    pub(super) removed_count: usize,
    pub(super) freed_bytes: i64,
    pub(super) skipped: usize,
    pub(super) log_path: PathBuf,
}

/// Verify survivors, delete redundant loose files, update catalogue rows, and
/// write an audit log for the irreversible hard-delete operation.
pub(super) fn execute_cleanup(
    conn: &rusqlite::Connection,
    sources: &[Source],
    targets: &[Candidate],
    data_dir: Option<PathBuf>,
) -> Result<ExecutionReport> {
    let mut removed: Vec<String> = Vec::new();
    let mut freed_bytes: i64 = 0;
    let mut skipped = 0usize;

    for c in targets {
        match delete_has_surviving_copy(conn, sources, &c.abs_path) {
            Ok(true) => {}
            Ok(false) => {
                eprintln!(
                    "  SKIP (no surviving copy verified on disk): {}",
                    c.abs_path
                );
                skipped += 1;
                continue;
            }
            Err(e) => {
                eprintln!("  SKIP (verify failed: {:#}): {}", e, c.abs_path);
                skipped += 1;
                continue;
            }
        }
        // A successful delete, or a file already gone, both drop the catalogue
        // row (the file has left the tracked sources either way).
        let gone = match std::fs::remove_file(&c.abs_path) {
            Ok(()) => true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
            Err(e) => {
                eprintln!("  ERROR deleting {}: {:#}", c.abs_path, e);
                false
            }
        };
        if gone {
            if let Some((sid, rel)) = files::resolve_in_sources(sources, &c.abs_path) {
                files::remove_locations_at(conn, sid, &rel)?;
            }
            removed.push(c.abs_path.clone());
            freed_bytes += c.size;
        }
    }

    let log_path = write_hard_delete_log(
        data_dir,
        "clean-superseded-logs",
        "clean-superseded",
        &removed,
    )?;

    Ok(ExecutionReport {
        removed_count: removed.len(),
        freed_bytes,
        skipped,
        log_path,
    })
}
