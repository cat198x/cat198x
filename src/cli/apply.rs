//! Apply command implementation

use anyhow::{Context, Result};
use std::fs;

use crate::db::quarantine::QuarantineReason;
use crate::plan::executor::{
    check_disk_space, execute_copy, execute_move, execute_repack, execute_rollback_move,
};
use crate::plan::{compute_state_hash, OperationKind, OperationLog, OperationStatus};
use crate::util::truncate_path;

use super::quarantine::move_to_quarantine;

use super::{open_database, plan::load_latest_plan};

/// Run the apply command
pub fn run(dry_run: bool, skip_space_check: bool, data_dir: Option<std::path::PathBuf>) -> Result<()> {
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

    if current_hash != plan.state_hash {
        println!("Plan is stale! The database state has changed since the plan was generated.");
        println!();
        println!("Run 'cat198x plan' to generate a new plan.");
        return Ok(());
    }

    // Check disk space before proceeding (unless skipped)
    if !skip_space_check
        && let Err(e) = check_disk_space(&plan) {
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

    for (i, op) in plan.operations.iter_mut().enumerate() {
        if op.status != OperationStatus::Pending {
            continue; // Skip already completed or failed operations
        }

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

                let result =
                    execute_copy(&source.path, source.archive_path.as_deref(), dest, &source.sha1);
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
                        eprintln!("  ERROR: {}", e);
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
                        eprintln!("  ERROR: {}", e);
                        op.status = OperationStatus::Failed;
                        error_count += 1;
                    }
                }
            }
            OperationKind::Repack {
                sources,
                dest,
                format,
            } => {
                println!(
                    "[{}/{}] REPACK ({} files) -> {}",
                    i + 1,
                    total_ops,
                    sources.len(),
                    truncate_path(dest, 40)
                );

                if dry_run {
                    success_count += 1;
                    continue;
                }

                let result = execute_repack(sources, dest, format);
                let success = result.is_ok();

                // Log the operation
                if let Some(ref mut log) = op_log {
                    let source_paths: Vec<String> =
                        sources.iter().map(|s| s.path.clone()).collect();
                    log.log_repack(op.id, &source_paths, dest, success);
                }

                match result {
                    Ok(()) => {
                        op.status = OperationStatus::Completed;
                        success_count += 1;
                    }
                    Err(e) => {
                        eprintln!("  ERROR: {}", e);
                        op.status = OperationStatus::Failed;
                        error_count += 1;
                    }
                }
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
                        eprintln!("  ERROR: {}", e);
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

                let reason_enum = QuarantineReason::parse(reason)
                    .unwrap_or(QuarantineReason::PathChanged);

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
                        eprintln!("  ERROR: {}", e);
                        op.status = OperationStatus::Failed;
                        error_count += 1;
                    }
                }
            }
        }
    }

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

/// Run the rollback command
pub fn run_rollback(dry_run: bool, continue_rollback: bool, data_dir: Option<std::path::PathBuf>) -> Result<()> {
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
                && let Ok(modified) = metadata.modified() {
                    match &latest {
                        None => latest = Some((path, modified)),
                        Some((_, prev_time)) if modified > *prev_time => {
                            latest = Some((path, modified))
                        }
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
        if continue_rollback { " (continue mode)" } else { "" }
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
        let reverse_op = entry.reverse.clone().expect("filtered to entries with reverse ops");
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
                            eprintln!("  ERROR: {}", e);
                            log.entries[idx].status = LogStatus::Failed;
                            error_count += 1;
                        }
                    }
                }
            }
            LoggedOperation::Move { ref source, ref dest, ref sha1 } => {
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
                        eprintln!("  ERROR: {}", e);
                        log.entries[idx].status = LogStatus::Failed;
                        error_count += 1;
                    }
                }
            }
            LoggedOperation::Copy { ref dest, .. } => {
                // The reverse of COPY should be DELETE, but handle this case just in case
                println!("[{}] DELETE {} (reverse of copy)", operation_id, truncate_path(dest, 50));

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
                        eprintln!("  ERROR: {}", e);
                        log.entries[idx].status = LogStatus::Failed;
                        error_count += 1;
                    }
                }
            }
            LoggedOperation::Repack { ref dest, .. } => {
                // Reverse of REPACK is DELETE the created archive
                println!("[{}] DELETE {} (reverse of repack)", operation_id, truncate_path(dest, 50));

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
                        eprintln!("  ERROR: {}", e);
                        log.entries[idx].status = LogStatus::Failed;
                        error_count += 1;
                    }
                }
            }
            LoggedOperation::Quarantine { .. } => {
                // A quarantine never appears as a reverse op — its reverse is a
                // Move (restore from quarantine), handled by the Move arm above.
                // Present only for match exhaustiveness.
                eprintln!("[{}] unexpected quarantine reverse op, skipping", operation_id);
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
        println!("Some rollback operations failed. Run 'cat198x apply --rollback --continue' to retry.");
    }

    Ok(())
}
