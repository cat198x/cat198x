use anyhow::{Context, Result};
use std::fs;

use crate::cli::get_data_dir;
use crate::plan::executor::{
    execute_relocate, execute_repack, execute_rollback_move, extract_from_archive,
};
use crate::plan::{LogStatus, LoggedOperation, OperationLog, SourceRef};
use crate::util::truncate_path;

/// Run the rollback command
pub fn run_rollback(
    dry_run: bool,
    continue_rollback: bool,
    data_dir: Option<std::path::PathBuf>,
) -> Result<()> {
    // Find the most recent operation log
    let data_dir_path = get_data_dir(data_dir)?;
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
            LoggedOperation::RebuildContainer {
                ref container,
                ref format,
                ref entries,
            } => {
                // Reverse of a container-drain delete: rebuild the staging
                // container by extracting each entry back out of the destination
                // it was repacked into, verifying SHA1. This deletes nothing — the
                // destinations are removed by the repacks' own reverses, which run
                // after this (reverse plan order), so every destination still
                // exists here.
                println!(
                    "[{}] REBUILD {} ({} entry/entries)",
                    operation_id,
                    truncate_path(container, 40),
                    entries.len()
                );

                if dry_run {
                    success_count += 1;
                    continue;
                }

                // Each entry becomes an archive-member source pulled from its
                // destination, named back to its in-container name. execute_repack
                // verifies every entry's SHA1 before finalising and removes the
                // partial archive on mismatch, so a corrupt destination can't yield
                // a silently wrong container.
                let sources: Vec<SourceRef> = entries
                    .iter()
                    .map(|e| SourceRef {
                        path: e.dest.clone(),
                        archive_path: Some(e.dest_entry.clone()),
                        sha1: e.sha1.clone(),
                        entry_name: Some(e.container_entry.clone()),
                    })
                    .collect();

                match execute_repack(&sources, container, format, false) {
                    Ok(_) => {
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
