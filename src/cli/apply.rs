//! Apply command implementation

use anyhow::{Context, Result};
use std::fs;

use crate::db::quarantine::QuarantineReason;
use crate::plan::executor::{
    RepackJob, RepackOutcome, check_disk_space, execute_copy, execute_move, execute_relocate,
    execute_repacks_concurrent, execute_rollback_move, extract_from_archive,
};
use crate::plan::{OperationKind, OperationLog, OperationStatus, compute_state_hash};
use crate::util::truncate_path;

use super::quarantine::move_to_quarantine;

use super::{open_database, plan::load_latest_plan};

/// Run the apply command
pub fn run(
    dry_run: bool,
    skip_space_check: bool,
    skip_repack: bool,
    jobs: usize,
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

    // Check if plan has any pending operations
    let pending_count = plan
        .operations
        .iter()
        .filter(|op| op.status == OperationStatus::Pending)
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
                    && matches!(op.kind, OperationKind::Repack { .. })
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

    // Create operation log (only if not dry run)
    let mut op_log = if !dry_run {
        Some(OperationLog::new(plan.state_hash.clone()))
    } else {
        None
    };

    let mut success_count = 0;
    let mut error_count = 0;

    // Source roots, listed once, used to keep the catalogue in step with each
    // file operation (so a re-plan converges without a re-scan).
    let sources = crate::db::files::list_sources(db.conn())?;

    // Consecutive pending repacks accumulate here and run concurrently —
    // they're latency-bound over a network mount, so overlapping them is the
    // wall-clock win. Any other pending operation flushes the batch first, so
    // ordering between repacks and everything else is exactly serial apply's.
    let mut repack_batch: Vec<RepackJob> = Vec::new();

    for i in 0..plan.operations.len() {
        {
            let op = &plan.operations[i];
            if op.status != OperationStatus::Pending {
                continue; // Skip already completed or failed operations
            }

            if let OperationKind::Repack {
                sources: repack_sources,
                dest,
                format,
                move_sources,
                ..
            } = &op.kind
            {
                // Leave repacks pending for a later pass when deferred, so the
                // cheap operations land first and the recompression can run
                // separately.
                if skip_repack {
                    continue;
                }
                if !dry_run {
                    repack_batch.push(RepackJob {
                        plan_index: i,
                        operation_id: op.id,
                        sources: repack_sources.clone(),
                        dest: dest.clone(),
                        format: format.clone(),
                        move_sources: *move_sources,
                    });
                    continue;
                }
            }
        }

        // A non-repack operation: complete the batched repacks before it runs,
        // preserving the plan's ordering between repacks and other operations.
        flush_repack_batch(
            &mut repack_batch,
            jobs,
            &mut plan,
            &mut op_log,
            db.conn(),
            &sources,
            total_ops,
            &mut success_count,
            &mut error_count,
        );

        let op = &mut plan.operations[i];
        match &op.kind {
            OperationKind::Copy { source, dest, .. } => {
                println!(
                    "[{}/{}] COPY {} -> {}",
                    i + 1,
                    total_ops,
                    truncate_path(&source.path, 40),
                    truncate_path(dest, 40)
                );

                if dry_run {
                    success_count += 1;
                    continue;
                }

                let result = execute_copy(
                    &source.path,
                    source.archive_path.as_deref(),
                    dest,
                    &source.sha1,
                );
                let success = result.is_ok();

                // Log the operation
                if let Some(ref mut log) = op_log {
                    log.log_copy(op.id, &source.path, dest, &source.sha1, success);
                }

                match result {
                    Ok(()) => {
                        op.status = OperationStatus::Completed;
                        success_count += 1;
                    }
                    Err(e) => {
                        eprintln!("  ERROR: {:#}", e);
                        op.status = OperationStatus::Failed;
                        error_count += 1;
                    }
                }
            }
            OperationKind::Move { source, dest, .. } => {
                println!(
                    "[{}/{}] MOVE {} -> {}",
                    i + 1,
                    total_ops,
                    truncate_path(&source.path, 40),
                    truncate_path(dest, 40)
                );

                if dry_run {
                    success_count += 1;
                    continue;
                }

                let result = execute_move(
                    &source.path,
                    source.archive_path.as_deref(),
                    dest,
                    &source.sha1,
                );
                let success = result.is_ok();

                // Log the operation
                if let Some(ref mut log) = op_log {
                    log.log_move(op.id, &source.path, dest, &source.sha1, success);
                }

                match result {
                    Ok(()) => {
                        op.status = OperationStatus::Completed;
                        success_count += 1;
                    }
                    Err(e) => {
                        eprintln!("  ERROR: {:#}", e);
                        op.status = OperationStatus::Failed;
                        error_count += 1;
                    }
                }
            }
            OperationKind::Relocate { source, dest, .. } => {
                println!(
                    "[{}/{}] RELOCATE {} -> {}",
                    i + 1,
                    total_ops,
                    truncate_path(source, 40),
                    truncate_path(dest, 40)
                );

                if dry_run {
                    success_count += 1;
                    continue;
                }

                let result = execute_relocate(source, dest);
                let success = result.is_ok();

                if let Some(ref mut log) = op_log {
                    log.log_relocate(op.id, source, dest, success);
                }

                match result {
                    Ok(()) => {
                        op.status = OperationStatus::Completed;
                        success_count += 1;
                    }
                    Err(e) => {
                        eprintln!("  ERROR: {:#}", e);
                        op.status = OperationStatus::Failed;
                        error_count += 1;
                    }
                }
            }
            OperationKind::Repack { sources, dest, .. } => {
                // Reachable only on a dry run: live repacks were batched above
                // for concurrent execution and never fall through to here.
                println!(
                    "[{}/{}] REPACK ({} files) -> {}",
                    i + 1,
                    total_ops,
                    sources.len(),
                    truncate_path(dest, 40)
                );
                success_count += 1;
                continue;
            }
            OperationKind::Delete { path } => {
                println!(
                    "[{}/{}] DELETE {}",
                    i + 1,
                    total_ops,
                    truncate_path(path, 40)
                );

                if dry_run {
                    success_count += 1;
                    continue;
                }

                match fs::remove_file(path) {
                    Ok(()) => {
                        op.status = OperationStatus::Completed;
                        success_count += 1;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        // File already gone, consider success
                        op.status = OperationStatus::Completed;
                        success_count += 1;
                        println!("  (already deleted)");
                    }
                    Err(e) => {
                        eprintln!("  ERROR: {:#}", e);
                        op.status = OperationStatus::Failed;
                        error_count += 1;
                    }
                }
            }
            OperationKind::Quarantine {
                path,
                sha1,
                size,
                reason,
                collection,
            } => {
                println!(
                    "[{}/{}] QUARANTINE {}",
                    i + 1,
                    total_ops,
                    truncate_path(path, 40)
                );

                if dry_run {
                    success_count += 1;
                    continue;
                }

                let reason_enum =
                    QuarantineReason::parse(reason).unwrap_or(QuarantineReason::PathChanged);

                let result = move_to_quarantine(
                    path,
                    sha1,
                    *size as i64,
                    reason_enum,
                    collection.as_deref(),
                    data_dir.clone(),
                );

                // Journal the quarantine so it can be rolled back: its reverse
                // is a Move restoring the original from the quarantine store.
                // Without this, a quarantine was silently irreversible.
                if let Some(ref mut log) = op_log {
                    let quarantine_path = result.as_deref().unwrap_or("");
                    log.log_quarantine(op.id, path, quarantine_path, sha1, result.is_ok());
                }

                match result {
                    Ok(_) => {
                        op.status = OperationStatus::Completed;
                        success_count += 1;
                    }
                    Err(e) => {
                        eprintln!("  ERROR: {:#}", e);
                        op.status = OperationStatus::Failed;
                        error_count += 1;
                    }
                }
            }
        }

        // Keep the catalogue in step with what just happened on disk, so a
        // re-plan converges without a re-scan. Catalogue-local and cheap; a
        // failure here doesn't undo the file operation that already succeeded.
        if !dry_run
            && op.status == OperationStatus::Completed
            && let Err(e) = sync_catalogue_after(db.conn(), &sources, &op.kind)
        {
            eprintln!("  warning: catalogue not updated for op {}: {}", op.id, e);
        }
    }

    // Repacks at the tail of the plan (the common case) are still batched.
    flush_repack_batch(
        &mut repack_batch,
        jobs,
        &mut plan,
        &mut op_log,
        db.conn(),
        &sources,
        total_ops,
        &mut success_count,
        &mut error_count,
    );

    // Save updated plan and operation log
    if !dry_run {
        let plan_json = serde_json::to_string_pretty(&plan).context("Failed to serialize plan")?;
        fs::write(&plan_path, &plan_json).context("Failed to update plan file")?;

        // Save operation log
        if let Some(mut log) = op_log {
            log.complete();
            let logs_dir = plan_path
                .parent()
                .and_then(|p| p.parent())
                .map(|p| p.join("logs"))
                .unwrap_or_else(|| std::path::PathBuf::from("objects/logs"));
            let log_path = log.save(&logs_dir)?;
            println!();
            println!("Operation log saved to: {}", log_path.display());
        }
    }

    println!();
    println!(
        "Complete: {} succeeded, {} failed",
        success_count, error_count
    );

    if error_count > 0 {
        println!();
        println!("Some operations failed. Run 'cat198x apply' again to retry.");
    }

    Ok(())
}

/// Execute the accumulated repack batch concurrently, then drain it.
///
/// Workers do the file operations; everything stateful happens here on the
/// calling thread as each outcome streams in — journal entry, plan status,
/// catalogue sync — in completion order. That keeps the rollback log append
/// order consistent with what actually happened on disk and never shares the
/// (non-`Sync`) database connection across threads.
#[allow(clippy::too_many_arguments)]
fn flush_repack_batch(
    batch: &mut Vec<RepackJob>,
    workers: usize,
    plan: &mut crate::plan::Plan,
    op_log: &mut Option<OperationLog>,
    conn: &rusqlite::Connection,
    sources: &[crate::db::files::Source],
    total_ops: usize,
    success_count: &mut usize,
    error_count: &mut usize,
) {
    if batch.is_empty() {
        return;
    }
    let jobs = std::mem::take(batch);
    if jobs.len() > 1 && workers > 1 {
        println!(
            "Repacking {} archive(s), {} in flight...",
            jobs.len(),
            workers.min(jobs.len())
        );
    }

    execute_repacks_concurrent(jobs, workers, |outcome: RepackOutcome| {
        let RepackOutcome { job, result } = outcome;
        println!(
            "[{}/{}] REPACK ({} files) -> {}",
            job.plan_index + 1,
            total_ops,
            job.sources.len(),
            truncate_path(&job.dest, 40)
        );

        // Log the operation. A move-mode repack reports the loose sources it
        // consumed so the reverse can extract them back out.
        if let Some(log) = op_log {
            let source_paths: Vec<String> = job.sources.iter().map(|s| s.path.clone()).collect();
            let consumed = result.as_deref().unwrap_or(&[]);
            log.log_repack(
                job.operation_id,
                &source_paths,
                &job.dest,
                consumed,
                result.is_ok(),
            );
        }

        let op = &mut plan.operations[job.plan_index];
        match result {
            Ok(_) => {
                op.status = OperationStatus::Completed;
                *success_count += 1;

                // Keep the catalogue in step, as the serial path does per-op.
                if let Err(e) = sync_catalogue_after(conn, sources, &op.kind) {
                    eprintln!("  warning: catalogue not updated for op {}: {}", op.id, e);
                }
            }
            Err(e) => {
                eprintln!("  ERROR: {:#}", e);
                op.status = OperationStatus::Failed;
                *error_count += 1;
            }
        }
    });
}

/// Update the file catalogue to match a completed operation, so a re-plan
/// converges without a re-scan: a move/relocate updates the file's recorded
/// location, a quarantine/delete removes it, a copy/repack records the new copy.
/// Paths outside any registered source can't be recorded (the file simply leaves
/// the catalogue's view); the library destination is a source, so the common
/// cases resolve.
fn sync_catalogue_after(
    conn: &rusqlite::Connection,
    sources: &[crate::db::files::Source],
    kind: &OperationKind,
) -> Result<()> {
    use crate::db::files;
    match kind {
        OperationKind::Move { source, dest, .. } => {
            if source.archive_path.is_some() {
                // A copy extracted from an archive to a loose dest; source kept.
                if let Some((nsrc, nrel)) = files::resolve_in_sources(sources, dest) {
                    files::upsert_file_location(conn, &source.sha1, nsrc, &nrel, None)?;
                }
            } else {
                relocate_or_drop(conn, sources, &source.path, dest)?;
            }
        }
        OperationKind::Relocate { source, dest, .. } => {
            relocate_or_drop(conn, sources, source, dest)?;
        }
        OperationKind::Copy { source, dest, .. } => {
            if let Some((nsrc, nrel)) = files::resolve_in_sources(sources, dest) {
                files::upsert_file_location(conn, &source.sha1, nsrc, &nrel, None)?;
            }
        }
        OperationKind::Repack {
            sources: entries,
            dest,
            move_sources,
            ..
        } => {
            if let Some((nsrc, nrel)) = files::resolve_in_sources(sources, dest) {
                for e in entries {
                    files::upsert_file_location(
                        conn,
                        &e.sha1,
                        nsrc,
                        &nrel,
                        e.entry_name.as_deref(),
                    )?;
                }
            }
            // Move mode deleted the loose sources on disk; drop their catalogued
            // locations too (archive-member sources are left in place).
            if *move_sources {
                for e in entries {
                    if e.archive_path.is_none()
                        && let Some((src, rel)) = files::resolve_in_sources(sources, &e.path)
                    {
                        files::remove_locations_at(conn, src, &rel)?;
                    }
                }
            }
        }
        OperationKind::Quarantine { path, .. } | OperationKind::Delete { path } => {
            if let Some((src, rel)) = files::resolve_in_sources(sources, path) {
                files::remove_locations_at(conn, src, &rel)?;
            }
        }
    }
    Ok(())
}

/// Move a file's catalogued location(s) from `old_abs` to `new_abs`, or drop them
/// if the destination is outside every registered source.
fn relocate_or_drop(
    conn: &rusqlite::Connection,
    sources: &[crate::db::files::Source],
    old_abs: &str,
    new_abs: &str,
) -> Result<()> {
    use crate::db::files;
    match (
        files::resolve_in_sources(sources, old_abs),
        files::resolve_in_sources(sources, new_abs),
    ) {
        (Some((osrc, orel)), Some((nsrc, nrel))) => {
            files::relocate_locations(conn, osrc, &orel, nsrc, &nrel)?;
        }
        (Some((osrc, orel)), None) => {
            files::remove_locations_at(conn, osrc, &orel)?;
        }
        _ => {}
    }
    Ok(())
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
