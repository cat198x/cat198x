//! `reclaim` command — free space by deleting a source's files whose every
//! content is already held in another source.
//!
//! The motivating case: after a reorg moves a set into the library, the staging
//! input (e.g. `ToSort/…`) is left holding archives whose every ROM now lives in
//! the library too. Those husks are pure redundancy — deleting them frees the
//! space without losing a byte, because each content survives in the other source.
//!
//! Safety model (hard delete has no undo):
//! - **Dry-run by default.** A plain `reclaim` only reports; `--execute` deletes.
//! - **Cross-source only.** A file is reclaimable only when every content it holds
//!   is *also* catalogued in a *different* source — so the deleted copy is never
//!   the last one. A source's own unique content is never reclaimed.
//! - **Existence-verified delete.** Before removing a file, each of its contents'
//!   external copies is confirmed to physically exist on disk (not just in the
//!   catalogue), so a stale "held elsewhere" record can't cause data loss.
//! - **Journaled.** Each run writes an audit log of what it removed.

use anyhow::{Context, Result};
use std::path::PathBuf;

use crate::db::files::{self, resolve_in_sources};

use super::{get_data_dir, open_database};

mod analysis;

use analysis::{ReclaimTarget, compute_reclaimable, partition_by_disposition, source_matches};

/// Confirm every content of `target` has an external copy that physically exists
/// on disk — the existence-verified-delete net. Returns false (skip) if any
/// external copy is missing, so a stale catalogue record can't cause loss.
fn external_copies_present(
    conn: &rusqlite::Connection,
    sources: &[files::Source],
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
            if std::path::Path::new(&abs).exists() {
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

/// Run the reclaim command.
pub fn run(selector: Option<String>, execute: bool, data_dir: Option<PathBuf>) -> Result<()> {
    let selector = selector.context(
        "reclaim needs a --source selector (a source id or a path substring) — \
         the source whose redundant files to reclaim",
    )?;

    let db = open_database(data_dir.clone())?;
    let conn = db.conn();
    let sources = files::list_sources(conn)?;

    let matched: Vec<&files::Source> = sources
        .iter()
        .filter(|s| source_matches(s, &selector))
        .collect();
    if matched.is_empty() {
        println!("No source matches '{}'.", selector);
        return Ok(());
    }

    // A preserve source must never be emptied because a copy exists elsewhere;
    // reclaim only operates on consume sources.
    let (reclaimable, preserved) = partition_by_disposition(&matched);
    for s in &preserved {
        println!(
            "  Skipping '{}' — it is a preserve source; reclaim removes content a tree alone may hold.",
            s.path
        );
    }
    if reclaimable.is_empty() {
        println!("Nothing to reclaim: the matched source(s) are all preserve.");
        return Ok(());
    }

    let mut all: Vec<(i64, ReclaimTarget)> = Vec::new();
    for s in &reclaimable {
        for t in compute_reclaimable(conn, s.id)? {
            all.push((s.id, t));
        }
    }

    if all.is_empty() {
        println!("Nothing to reclaim: no fully-redundant files in the matched source(s).");
        return Ok(());
    }

    let total_bytes: i64 = all.iter().map(|(_, t)| t.bytes).sum();
    let loose = all.iter().filter(|(_, t)| !t.is_archive).count();
    let archives = all.len() - loose;

    println!(
        "Reclaimable: {} archive(s) + {} loose file(s), {} — every content is held in another source.",
        archives,
        loose,
        format_bytes(total_bytes.max(0) as u64)
    );

    if !execute {
        for (_, t) in all.iter().take(20) {
            println!("  would remove  {}", t.full_path);
        }
        if all.len() > 20 {
            println!("  … and {} more", all.len() - 20);
        }
        println!();
        println!("Dry run — nothing deleted. Re-run with --execute to free the space.");
        return Ok(());
    }

    // --execute: existence-verified hard delete, journaled.
    let mut removed: Vec<String> = Vec::new();
    let mut freed: i64 = 0;
    let mut skipped = 0usize;
    for (source_id, t) in &all {
        if !external_copies_present(conn, &sources, *source_id, t)? {
            eprintln!("  SKIP (external copy missing on disk): {}", t.full_path);
            skipped += 1;
            continue;
        }
        match std::fs::remove_file(&t.full_path) {
            Ok(()) => {
                // Drop the catalogue rows for the removed file.
                if let Some((sid, rel)) = resolve_in_sources(&sources, &t.full_path) {
                    conn.execute(
                        "DELETE FROM file_locations WHERE source_id = ?1 AND path = ?2",
                        rusqlite::params![sid, rel],
                    )?;
                }
                removed.push(t.full_path.clone());
                freed += t.bytes;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if let Some((sid, rel)) = resolve_in_sources(&sources, &t.full_path) {
                    conn.execute(
                        "DELETE FROM file_locations WHERE source_id = ?1 AND path = ?2",
                        rusqlite::params![sid, rel],
                    )?;
                }
            }
            Err(e) => eprintln!("  ERROR deleting {}: {:#}", t.full_path, e),
        }
    }

    // Journal the run for audit (hard delete is irreversible).
    let logs_dir = get_data_dir(data_dir)?.join("objects/reclaim-logs");
    std::fs::create_dir_all(&logs_dir).ok();
    let log_path = logs_dir.join(format!("reclaim-{}.txt", removed.len()));
    std::fs::write(&log_path, removed.join("\n")).ok();

    println!();
    println!(
        "Reclaimed {} file(s), freed {}{}.",
        removed.len(),
        format_bytes(freed.max(0) as u64),
        if skipped > 0 {
            format!(" ({} skipped — external copy missing)", skipped)
        } else {
            String::new()
        }
    );
    println!("Audit log: {}", log_path.display());
    Ok(())
}

/// Format bytes as a human-readable string.
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
