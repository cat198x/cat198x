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

use crate::db::files;
use crate::util::format_bytes;

use super::open_database;

mod analysis;
mod execution;

use super::source_selector::source_matches;
use analysis::{analyze_reclaimable, partition_by_disposition};
use execution::execute_reclaim;

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

    let report = analyze_reclaimable(conn, &reclaimable)?;
    if report.targets.is_empty() {
        println!("Nothing to reclaim: no fully-redundant files in the matched source(s).");
        return Ok(());
    }

    println!(
        "Reclaimable: {} archive(s) + {} loose file(s), {} — every content is held in another source.",
        report.archive_count,
        report.loose_count,
        format_bytes(report.total_bytes.max(0) as u64)
    );

    if !execute {
        for (_, t) in report.targets.iter().take(20) {
            println!("  would remove  {}", t.full_path);
        }
        if report.targets.len() > 20 {
            println!("  … and {} more", report.targets.len() - 20);
        }
        println!();
        println!("Dry run — nothing deleted. Re-run with --execute to free the space.");
        return Ok(());
    }

    let report = execute_reclaim(conn, &sources, &report.targets, data_dir)?;

    println!();
    println!(
        "Reclaimed {} file(s), freed {}{}.",
        report.removed_count,
        format_bytes(report.freed_bytes.max(0) as u64),
        if report.skipped > 0 {
            format!(" ({} skipped — external copy missing)", report.skipped)
        } else {
            String::new()
        }
    );
    println!("Audit log: {}", report.log_path.display());
    Ok(())
}
