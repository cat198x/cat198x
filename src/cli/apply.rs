//! Apply command implementation

mod rollback;

use anyhow::Result;

pub use rollback::run_rollback;

use crate::plan::executor::check_disk_space;
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

#[cfg(test)]
#[path = "apply_tests.rs"]
mod tests;
