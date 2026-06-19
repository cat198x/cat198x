//! Apply command implementation

use anyhow::{Context, Result};
use std::fs;

use crate::plan::executor::{
    check_disk_space, execute_relocate, execute_rollback_move, extract_from_archive,
};
use crate::plan::{ApplyEvent, ApplyOptions, OperationStatus, apply_plan, compute_state_hash};
use crate::util::truncate_path;

use super::{open_database, plan::load_latest_plan};

/// Run the apply command
pub fn run(
    dry_run: bool,
    skip_space_check: bool,
    skip_repack: bool,
    jobs: usize,
    prune_empty: bool,
    data_dir: Option<std::path::PathBuf>,
) -> Result<()> {
    // Load the most recent plan
    let (mut plan, plan_path) = match load_latest_plan(data_dir.clone())? {
        Some(p) => p,
        None => {
            println!("No plan found. Run 'cat198x plan' first to generate a plan.");
            return Ok(());
        }
    };

    // Verify plan is not stale
    let db = open_database(data_dir.clone())?;
    let current_hash = compute_state_hash(db.conn())?;

    // A plan with operations already applied is mid-flight: its own completed
    // operations updated the catalogue (and so the state hash) by design, so the
    // drift is expected and we resume rather than reject. The stale check only
    // guards a fresh plan — one whose every operation is still pending — against
    // a catalogue that moved underneath it (e.g. a scan) since it was generated.
    let plan_started = plan
        .operations
        .iter()
        .any(|op| op.status != OperationStatus::Pending);

    if !plan_started && current_hash != plan.state_hash {
        println!("Plan is stale! The database state has changed since the plan was generated.");
        println!();
        println!("Run 'cat198x plan' to generate a new plan.");
        return Ok(());
    }

    // Check disk space before proceeding (unless skipped)
    if !skip_space_check && let Err(e) = check_disk_space(&plan) {
        println!("Disk space check failed:");
        println!("  {}", e);
        println!();
        println!("Free up disk space or use --skip-space-check to proceed anyway.");
        return Ok(());
    }

    // Remaining work is fresh pending ops plus retryable failed ones, so a
    // re-apply after a dropped mount picks up where it left off.
    let pending_count = plan
        .operations
        .iter()
        .filter(|op| op.status.is_remaining_work())
        .count();

    if pending_count == 0 {
        println!("No pending operations in plan.");
        return Ok(());
    }

    let total_ops = plan.operations.len();
    println!(
        "Applying plan: {} operations ({} pending)",
        total_ops, pending_count
    );

    // Deferring repacks runs the cheap operations (relocates, quarantines) now
    // and leaves the expensive read-and-recompress repacks pending for a later
    // pass. Resumable: a subsequent `apply` (without --skip-repack) picks them up.
    if skip_repack {
        let deferred = plan
            .operations
            .iter()
            .filter(|op| {
                op.status == OperationStatus::Pending
                    && matches!(op.kind, crate::plan::OperationKind::Repack { .. })
            })
            .count();
        if deferred > 0 {
            println!(
                "Deferring {} repack operation(s); run `cat198x apply` again to complete them.",
                deferred
            );
        }
    }
    println!();

    if dry_run {
        println!("DRY RUN - no files will be modified");
        println!();
    }

    // Source roots, listed once, used to keep the catalogue in step with each
    // file operation (so a re-plan converges without a re-scan).
    let sources = crate::db::files::list_sources(db.conn())?;
    let quarantine_dir = super::config::resolve_quarantine_dir(data_dir.clone())?;

    // Drive the library apply engine. Its progress events become exactly the
    // console output this command has always produced; the engine itself prints
    // nothing, so the UI and MCP surfaces can render the same run differently.
    let outcome = apply_plan(
        db.conn(),
        &mut plan,
        &plan_path,
        &sources,
        &ApplyOptions {
            dry_run,
            skip_repack,
            jobs,
            quarantine_dir,
        },
        &mut |event| print_event(&event),
    )?;

    if let Some(log_path) = &outcome.log_path {
        println!();
        println!("Operation log saved to: {}", log_path.display());
    }

    println!();
    print!(
        "Complete: {} succeeded, {} failed",
        outcome.success_count, outcome.error_count
    );
    if outcome.refused_count > 0 {
        print!(", {} refused (safety)", outcome.refused_count);
    }
    println!();

    if outcome.error_count > 0 {
        println!();
        println!(
            "Some operations failed (e.g. a dropped mount). Run 'cat198x apply' again to retry them."
        );
    }
    if outcome.refused_count > 0 {
        println!();
        println!(
            "{} operation(s) were refused by the safety net and will not be retried; \
             regenerate the plan with 'cat198x plan' if the catalogue has since changed.",
            outcome.refused_count
        );
    }

    // Self-clean: with --prune-empty, remove the directories the move-tidy left
    // empty under the source roots. Done once here rather than per operation —
    // over a network mount a per-op emptiness check would add a round trip to
    // every operation, and an archive-entry move never removes its source
    // container, so a folder only truly empties once its last whole file is gone.
    // Only ever uses fs::remove_dir, which refuses a non-empty directory.
    if prune_empty && !dry_run {
        let roots: Vec<std::path::PathBuf> = sources
            .iter()
            .map(|s| std::path::PathBuf::from(&s.path))
            .collect();
        let report = crate::cli::prune::prune_sources(
            &roots,
            &crate::cli::prune::PruneOptions {
                remove: true,
                ignore_os_junk: false,
            },
        )?;
        println!();
        if report.dirs.is_empty() {
            println!("Prune: no empty directories under the source roots.");
        } else {
            println!(
                "Prune: removed {} empty director{} left by the tidy.",
                report.dirs.len(),
                if report.dirs.len() == 1 { "y" } else { "ies" }
            );
        }
    }

    Ok(())
}

/// Render an apply progress event as the console line `apply` has always shown.
/// Errors, refusals, and warnings go to stderr; everything else to stdout.
fn print_event(event: &ApplyEvent) {
    match event {
        // The CLI prints one line per op as it starts; the slot lane and the
        // paired OpFinished are for live displays, so it ignores them here.
        ApplyEvent::OpFinished { .. } => {}
        ApplyEvent::OpStarted {
            index, total, op, ..
        } => {
            let n = index + 1;
            match (op.file_count, &op.to) {
                (Some(count), _) => println!(
                    "[{}/{}] {} ({} files) -> {}",
                    n,
                    total,
                    op.verb,
                    count,
                    truncate_path(&op.from, 40)
                ),
                (None, Some(to)) => println!(
                    "[{}/{}] {} {} -> {}",
                    n,
                    total,
                    op.verb,
                    truncate_path(&op.from, 40),
                    truncate_path(to, 40)
                ),
                (None, None) => {
                    println!(
                        "[{}/{}] {} {}",
                        n,
                        total,
                        op.verb,
                        truncate_path(&op.from, 40)
                    )
                }
            }
        }
        ApplyEvent::AlreadyGone { .. } => println!("  (already deleted)"),
        ApplyEvent::DeleteRefused { path, .. } => eprintln!(
            "  REFUSED: no surviving copy of {} found on disk — not deleting",
            truncate_path(path, 40)
        ),
        ApplyEvent::DeleteVerifyError { message, .. } => {
            eprintln!(
                "  ERROR verifying surviving copy — not deleting: {}",
                message
            )
        }
        ApplyEvent::OpFailed { message, .. } => eprintln!("  ERROR: {}", message),
        ApplyEvent::CatalogueWarning { op_id, message } => {
            eprintln!(
                "  warning: catalogue not updated for op {}: {}",
                op_id, message
            )
        }
        ApplyEvent::RepackBatchStarted { count, in_flight } => {
            println!("Repacking {} archive(s), {} in flight...", count, in_flight)
        }
    }
}

/// Run the rollback command
pub fn run_rollback(
    dry_run: bool,
    continue_rollback: bool,
    data_dir: Option<std::path::PathBuf>,
) -> Result<()> {
    use crate::plan::{LogStatus, LoggedOperation, OperationLog};

    // Find the most recent operation log
    let data_dir_path = super::get_data_dir(data_dir)?;
    let logs_dir = data_dir_path.join("objects/logs");

    if !logs_dir.exists() {
        println!("No operation logs found. Nothing to rollback.");
        return Ok(());
    }

    // Find the most recent log file
    let mut latest: Option<(std::path::PathBuf, std::time::SystemTime)> = None;
    for entry in fs::read_dir(&logs_dir)? {
        let entry = entry?;
        let path = entry.path();

        if path.extension().map(|e| e == "json").unwrap_or(false)
            && let Ok(metadata) = entry.metadata()
            && let Ok(modified) = metadata.modified()
        {
            match &latest {
                None => latest = Some((path, modified)),
                Some((_, prev_time)) if modified > *prev_time => latest = Some((path, modified)),
                _ => {}
            }
        }
    }

    let log_path = match latest {
        Some((path, _)) => path,
        None => {
            println!("No operation logs found. Nothing to rollback.");
            return Ok(());
        }
    };

    let mut log = OperationLog::load(&log_path)?;

    println!("Rollback log: {}", log_path.display());
    println!("Plan hash: {}", log.plan_hash);
    println!();

    // Collect indices of entries that need rollback
    let indices_to_rollback: Vec<usize> = log
        .entries
        .iter()
        .enumerate()
        .filter(|(_, e)| {
            if continue_rollback {
                // On continue, only retry failed rollbacks or completed (not yet rolled back)
                e.status == LogStatus::Completed || e.status == LogStatus::Failed
            } else {
                // Normal rollback: only rollback completed operations
                e.status == LogStatus::Completed
            }
        })
        .filter(|(_, e)| e.reverse.is_some())
        .map(|(idx, _)| idx)
        .collect();

    if indices_to_rollback.is_empty() {
        println!("No operations to rollback.");
        return Ok(());
    }

    println!(
        "Rolling back {} operations{}...",
        indices_to_rollback.len(),
        if continue_rollback {
            " (continue mode)"
        } else {
            ""
        }
    );
    println!();

    if dry_run {
        println!("DRY RUN - no files will be modified");
        println!();
    }

    let mut success_count = 0;
    let mut error_count = 0;

    // Process in reverse order (last operation first)
    for idx in indices_to_rollback.into_iter().rev() {
        let entry = &log.entries[idx];
        let reverse_op = entry
            .reverse
            .clone()
            .expect("filtered to entries with reverse ops");
        let operation_id = entry.operation_id;

        match reverse_op {
            LoggedOperation::Delete { ref path } => {
                println!("[{}] DELETE {}", operation_id, truncate_path(path, 50));

                if dry_run {
                    success_count += 1;
                    continue;
                }

                match fs::remove_file(path) {
                    Ok(()) => {
                        log.entries[idx].status = LogStatus::RolledBack;
                        success_count += 1;
                    }
                    Err(e) => {
                        // File might not exist (already deleted, etc.)
                        if e.kind() == std::io::ErrorKind::NotFound {
                            log.entries[idx].status = LogStatus::RolledBack;
                            success_count += 1;
                            println!("  (already deleted)");
                        } else {
                            eprintln!("  ERROR: {:#}", e);
                            log.entries[idx].status = LogStatus::Failed;
                            error_count += 1;
                        }
                    }
                }
            }
            LoggedOperation::Move {
                ref source,
                ref dest,
                ref sha1,
            } => {
                println!(
                    "[{}] MOVE {} -> {}",
                    operation_id,
                    truncate_path(source, 30),
                    truncate_path(dest, 30)
                );

                if dry_run {
                    success_count += 1;
                    continue;
                }

                // Move the file back (source is current location, dest is original location)
                match execute_rollback_move(source, dest, sha1) {
                    Ok(()) => {
                        log.entries[idx].status = LogStatus::RolledBack;
                        success_count += 1;
                    }
                    Err(e) => {
                        eprintln!("  ERROR: {:#}", e);
                        log.entries[idx].status = LogStatus::Failed;
                        error_count += 1;
                    }
                }
            }
            LoggedOperation::Relocate {
                ref source,
                ref dest,
            } => {
                println!(
                    "[{}] RELOCATE {} -> {}",
                    operation_id,
                    truncate_path(source, 30),
                    truncate_path(dest, 30)
                );

                if dry_run {
                    success_count += 1;
                    continue;
                }

                // Reverse is itself a relocate (source is current, dest is original).
                match execute_relocate(source, dest) {
                    Ok(()) => {
                        log.entries[idx].status = LogStatus::RolledBack;
                        success_count += 1;
                    }
                    Err(e) => {
                        eprintln!("  ERROR: {:#}", e);
                        log.entries[idx].status = LogStatus::Failed;
                        error_count += 1;
                    }
                }
            }
            LoggedOperation::Copy { ref dest, .. } => {
                // The reverse of COPY should be DELETE, but handle this case just in case
                println!(
                    "[{}] DELETE {} (reverse of copy)",
                    operation_id,
                    truncate_path(dest, 50)
                );

                if dry_run {
                    success_count += 1;
                    continue;
                }

                match fs::remove_file(dest) {
                    Ok(()) => {
                        log.entries[idx].status = LogStatus::RolledBack;
                        success_count += 1;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        log.entries[idx].status = LogStatus::RolledBack;
                        success_count += 1;
                        println!("  (already deleted)");
                    }
                    Err(e) => {
                        eprintln!("  ERROR: {:#}", e);
                        log.entries[idx].status = LogStatus::Failed;
                        error_count += 1;
                    }
                }
            }
            LoggedOperation::Repack { ref dest, .. } => {
                // Reverse of REPACK is DELETE the created archive
                println!(
                    "[{}] DELETE {} (reverse of repack)",
                    operation_id,
                    truncate_path(dest, 50)
                );

                if dry_run {
                    success_count += 1;
                    continue;
                }

                match fs::remove_file(dest) {
                    Ok(()) => {
                        log.entries[idx].status = LogStatus::RolledBack;
                        success_count += 1;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        log.entries[idx].status = LogStatus::RolledBack;
                        success_count += 1;
                        println!("  (already deleted)");
                    }
                    Err(e) => {
                        eprintln!("  ERROR: {:#}", e);
                        log.entries[idx].status = LogStatus::Failed;
                        error_count += 1;
                    }
                }
            }
            LoggedOperation::UnpackRepack {
                ref dest,
                ref restore,
            } => {
                // Reverse of a move-mode repack: extract each consumed source
                // back out of the archive, then delete the archive.
                println!(
                    "[{}] UNPACK {} ({} source(s) restored)",
                    operation_id,
                    truncate_path(dest, 40),
                    restore.len()
                );

                if dry_run {
                    success_count += 1;
                    continue;
                }

                // Restore every source first; only delete the archive once they
                // are all safely back, so a failure leaves the sources recoverable.
                let result = restore
                    .iter()
                    .try_for_each(|(entry_name, path)| extract_from_archive(dest, entry_name, path))
                    .and_then(|()| match fs::remove_file(dest) {
                        Ok(()) => Ok(()),
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                        Err(e) => Err(e.into()),
                    });

                match result {
                    Ok(()) => {
                        log.entries[idx].status = LogStatus::RolledBack;
                        success_count += 1;
                    }
                    Err(e) => {
                        eprintln!("  ERROR: {:#}", e);
                        log.entries[idx].status = LogStatus::Failed;
                        error_count += 1;
                    }
                }
            }
            LoggedOperation::Quarantine { .. } => {
                // A quarantine never appears as a reverse op — its reverse is a
                // Move (restore from quarantine), handled by the Move arm above.
                // Present only for match exhaustiveness.
                eprintln!(
                    "[{}] unexpected quarantine reverse op, skipping",
                    operation_id
                );
                error_count += 1;
            }
        }
    }

    // Save updated log
    if !dry_run {
        let json = serde_json::to_string_pretty(&log).context("Failed to serialize log")?;
        fs::write(&log_path, &json).context("Failed to update log file")?;
    }

    println!();
    println!(
        "Rollback complete: {} succeeded, {} failed",
        success_count, error_count
    );

    if error_count > 0 {
        println!();
        println!(
            "Some rollback operations failed. Run 'cat198x apply --rollback --continue' to retry."
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::db::Database;
    use crate::db::files::{
        Disposition, add_source, get_source_by_path, set_source_disposition, upsert_file,
        upsert_file_location,
    };
    use crate::plan::executor::delete_has_surviving_copy;

    // A delete is allowed only while a surviving copy physically exists; once the
    // other copy is gone, the same record must no longer authorise the delete.
    // The staging source is `consume`, so a copy in the library (another tree)
    // does authorise emptying it.
    #[test]
    fn delete_refused_when_no_surviving_copy_on_disk() {
        let tosort = tempfile::TempDir::new().unwrap();
        let library = tempfile::TempDir::new().unwrap();
        let tosort_root = tosort.path().to_str().unwrap();
        let library_root = library.path().to_str().unwrap();

        // The same content exists physically in both the staging source and the
        // library, and is catalogued in both.
        std::fs::write(tosort.path().join("game.zip"), b"content").unwrap();
        std::fs::write(library.path().join("game.zip"), b"content").unwrap();

        let db = Database::open_in_memory().unwrap();
        let conn = db.conn();
        add_source(conn, tosort_root, false).unwrap();
        add_source(conn, library_root, false).unwrap();
        // ToSort is staging — consume — so a cross-tree library copy counts.
        set_source_disposition(conn, tosort_root, Disposition::Consume).unwrap();
        upsert_file(conn, "AAAA", None, None, None, 7).unwrap();
        let ts = get_source_by_path(conn, tosort_root).unwrap().unwrap();
        let lib = get_source_by_path(conn, library_root).unwrap().unwrap();
        upsert_file_location(conn, "AAAA", ts.id, "game.zip", None).unwrap();
        upsert_file_location(conn, "AAAA", lib.id, "game.zip", None).unwrap();

        let sources = crate::db::files::list_sources(conn).unwrap();
        let tosort_abs = format!("{}/game.zip", tosort_root);

        // Library copy present on disk → safe to delete the staging copy.
        assert!(delete_has_surviving_copy(conn, &sources, &tosort_abs).unwrap());

        // Library copy gone on disk (stale catalogue record) → refuse the delete.
        std::fs::remove_file(library.path().join("game.zip")).unwrap();
        assert!(!delete_has_surviving_copy(conn, &sources, &tosort_abs).unwrap());
    }

    // A `preserve` source must not be emptied because a copy exists in another
    // tree: only a same-tree copy authorises the delete. The default disposition
    // is preserve, so this is the safe baseline.
    #[test]
    fn delete_of_preserve_file_refused_when_only_copy_is_in_another_tree() {
        let master = tempfile::TempDir::new().unwrap();
        let library = tempfile::TempDir::new().unwrap();
        let master_root = master.path().to_str().unwrap();
        let library_root = library.path().to_str().unwrap();

        // The same content sits in a preserve reference master and in the library.
        std::fs::write(master.path().join("game.zip"), b"content").unwrap();
        std::fs::write(library.path().join("game.zip"), b"content").unwrap();

        let db = Database::open_in_memory().unwrap();
        let conn = db.conn();
        add_source(conn, master_root, false).unwrap(); // defaults to preserve
        add_source(conn, library_root, false).unwrap();
        upsert_file(conn, "AAAA", None, None, None, 7).unwrap();
        let master_src = get_source_by_path(conn, master_root).unwrap().unwrap();
        let lib = get_source_by_path(conn, library_root).unwrap().unwrap();
        upsert_file_location(conn, "AAAA", master_src.id, "game.zip", None).unwrap();
        upsert_file_location(conn, "AAAA", lib.id, "game.zip", None).unwrap();

        let sources = crate::db::files::list_sources(conn).unwrap();
        let master_abs = format!("{}/game.zip", master_root);

        // A library copy in a different tree does NOT authorise emptying the
        // preserve master — that would lose content the master's tree held.
        assert!(!delete_has_surviving_copy(conn, &sources, &master_abs).unwrap());
    }

    // Within a single preserve tree, a duplicate (the same content at a second
    // path) may be dropped — the content survives in the same tree.
    #[test]
    fn delete_of_preserve_file_allowed_when_a_same_tree_copy_survives() {
        let master = tempfile::TempDir::new().unwrap();
        let master_root = master.path().to_str().unwrap();

        // The same content sits at two paths within the one preserve tree.
        std::fs::write(master.path().join("dup.zip"), b"content").unwrap();
        std::fs::write(master.path().join("canonical.zip"), b"content").unwrap();

        let db = Database::open_in_memory().unwrap();
        let conn = db.conn();
        add_source(conn, master_root, false).unwrap(); // preserve
        upsert_file(conn, "AAAA", None, None, None, 7).unwrap();
        let src = get_source_by_path(conn, master_root).unwrap().unwrap();
        upsert_file_location(conn, "AAAA", src.id, "dup.zip", None).unwrap();
        upsert_file_location(conn, "AAAA", src.id, "canonical.zip", None).unwrap();

        let sources = crate::db::files::list_sources(conn).unwrap();
        let dup_abs = format!("{}/dup.zip", master_root);

        // The canonical copy in the same tree survives → dropping the duplicate
        // loses nothing. Once that same-tree copy is gone, the delete is refused.
        assert!(delete_has_surviving_copy(conn, &sources, &dup_abs).unwrap());
        std::fs::remove_file(master.path().join("canonical.zip")).unwrap();
        assert!(!delete_has_surviving_copy(conn, &sources, &dup_abs).unwrap());
    }

    // A path whose contents aren't catalogued can't be reasoned about — refuse.
    #[test]
    fn delete_refused_for_uncatalogued_path() {
        let tosort = tempfile::TempDir::new().unwrap();
        let root = tosort.path().to_str().unwrap();
        let db = Database::open_in_memory().unwrap();
        let conn = db.conn();
        add_source(conn, root, false).unwrap();
        let sources = crate::db::files::list_sources(conn).unwrap();
        let abs = format!("{}/unknown.zip", root);
        assert!(!delete_has_surviving_copy(conn, &sources, &abs).unwrap());
    }
}
