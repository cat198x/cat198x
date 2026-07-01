use anyhow::Result;
use std::path::{Path, PathBuf};

use crate::cli::get_data_dir;
use crate::db::files::{self, Source};

use super::analysis::ReclaimTarget;

pub(super) struct ExecutionReport {
    pub(super) removed_count: usize,
    pub(super) freed_bytes: i64,
    pub(super) skipped: usize,
    pub(super) log_path: PathBuf,
}

/// Verify external copies, delete reclaimable files, update catalogue rows, and
/// write an audit log for the irreversible hard-delete operation.
pub(super) fn execute_reclaim(
    conn: &rusqlite::Connection,
    sources: &[Source],
    targets: &[(i64, ReclaimTarget)],
    data_dir: Option<PathBuf>,
) -> Result<ExecutionReport> {
    let mut removed: Vec<String> = Vec::new();
    let mut freed_bytes: i64 = 0;
    let mut skipped = 0usize;

    for (source_id, target) in targets {
        if !external_copies_present(conn, sources, *source_id, target)? {
            eprintln!(
                "  SKIP (external copy missing on disk): {}",
                target.full_path
            );
            skipped += 1;
            continue;
        }
        match std::fs::remove_file(&target.full_path) {
            Ok(()) => {
                remove_catalogue_rows(conn, sources, &target.full_path)?;
                removed.push(target.full_path.clone());
                freed_bytes += target.bytes;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                remove_catalogue_rows(conn, sources, &target.full_path)?;
            }
            Err(e) => eprintln!("  ERROR deleting {}: {:#}", target.full_path, e),
        }
    }

    // Journal the run for audit (hard delete is irreversible).
    let logs_dir = get_data_dir(data_dir)?.join("objects/reclaim-logs");
    std::fs::create_dir_all(&logs_dir).ok();
    let log_path = logs_dir.join(format!("reclaim-{}.txt", removed.len()));
    std::fs::write(&log_path, removed.join("\n")).ok();

    Ok(ExecutionReport {
        removed_count: removed.len(),
        freed_bytes,
        skipped,
        log_path,
    })
}

/// Confirm every content of `target` has an external copy that physically exists
/// on disk — the existence-verified-delete net. Returns false (skip) if any
/// external copy is missing, so a stale catalogue record can't cause loss.
fn external_copies_present(
    conn: &rusqlite::Connection,
    sources: &[Source],
    source_id: i64,
    target: &ReclaimTarget,
) -> Result<bool> {
    for sha1 in &target.sha1s {
        let locs = files::get_file_locations(conn, sha1)?;
        let mut ok = false;
        for l in locs {
            if l.source_id == source_id {
                continue; // a copy in the source we're reclaiming doesn't count
            }
            let root = sources
                .iter()
                .find(|s| s.id == l.source_id)
                .map(|s| s.path.trim_end_matches('/').to_string());
            let Some(root) = root else { continue };
            let abs = format!("{}/{}", root, l.path);
            if Path::new(&abs).exists() {
                ok = true;
                break;
            }
        }
        if !ok {
            return Ok(false);
        }
    }
    Ok(true)
}

fn remove_catalogue_rows(
    conn: &rusqlite::Connection,
    sources: &[Source],
    full_path: &str,
) -> Result<()> {
    if let Some((sid, rel)) = files::resolve_in_sources(sources, full_path) {
        files::remove_locations_at(conn, sid, &rel)?;
    }
    Ok(())
}
