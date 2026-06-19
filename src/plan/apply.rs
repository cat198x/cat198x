//! Plan application: the orchestration that carries out a plan's operations.
//!
//! This is the loop that walks a plan's operations — copy / move / relocate /
//! repack / delete / quarantine — driving the verified file primitives in
//! [`crate::plan::executor`], journaling each to the rollback log, and keeping
//! the catalogue in step so a re-plan converges without a re-scan. Repacks are
//! batched and run concurrently (they're latency-bound over a network mount).
//!
//! It holds no output concerns: progress is reported through an [`ApplyEvent`]
//! callback, so the `apply` CLI prints, the UI streams a progress bar, and the
//! MCP surface stays silent — each adapter decides how to render the same run.
//! That keeps this engine drivable from every 198x surface, exactly as the
//! safety model requires ("the execution engine lives in the library").

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rusqlite::Connection;

use crate::db::files::{self, Source};
use crate::db::quarantine::QuarantineReason;
use crate::plan::executor::{
    PlacementEvent, PlacementJob, PlacementKind, PlacementOutcome, RepackEvent, RepackJob,
    RepackOutcome, delete_has_surviving_copy, execute_copy, execute_move,
    execute_placements_concurrent, execute_quarantine, execute_relocate,
    execute_repacks_concurrent,
};
use crate::plan::{OperationKind, OperationLog, OperationStatus, Plan};

/// How to apply a plan. Staleness and disk-space pre-checks stay with the
/// caller (they're user-facing gates); this is the execution itself.
pub struct ApplyOptions {
    /// Report what would happen without touching any file.
    pub dry_run: bool,
    /// Leave repack operations pending for a later pass (cheap ops land first).
    pub skip_repack: bool,
    /// Concurrent repack workers.
    pub jobs: usize,
    /// The resolved quarantine store location (the caller resolves config vs
    /// default and passes it in, so this engine needs no config layer).
    pub quarantine_dir: PathBuf,
}

/// A display-agnostic view of an operation about to run — full paths and counts,
/// no truncation or formatting (that's the adapter's job).
#[derive(Debug, Clone)]
pub struct OpView {
    pub verb: &'static str,
    /// The primary path: the source for copy/move/relocate, the path for
    /// delete/quarantine, the destination archive for a repack.
    pub from: String,
    /// The destination, for the operations that have a distinct one.
    pub to: Option<String>,
    /// The number of files folded into a repack.
    pub file_count: Option<usize>,
    /// The operation's size in bytes (a delete has none, so `0`). Adapters show
    /// it as a per-op figure and accumulate it into a running total.
    pub bytes: u64,
    /// Why this op is safe to do, when it carries a reason: a dedup delete names
    /// the canonical copy it keeps; a quarantine names what flagged it. `None`
    /// for ops whose intent is evident from the verb and paths.
    pub reason: Option<String>,
}

/// A progress event emitted as a plan is applied. The library never prints;
/// adapters turn these into console lines, UI updates, or summaries.
#[derive(Debug, Clone)]
pub enum ApplyEvent {
    /// An operation is starting (or, on a dry run, would run). `slot` is the
    /// worker lane running it for a concurrent placement (`0..jobs`), or `None`
    /// for a serial operation (delete/quarantine) and on a dry run.
    OpStarted {
        index: usize,
        total: usize,
        slot: Option<usize>,
        op: OpView,
    },
    /// An operation has finished. Pairs with an earlier `OpStarted` of the same
    /// `index`/`slot`. Every operation emits exactly one of these — so a caller
    /// banks processed bytes only on completion, frees the worker slot, and logs
    /// the outcome (the `op` view names it). `status` is the terminal state
    /// (`Completed`/`Failed`/`Refused`); `detail` carries the error or refusal
    /// reason when not completed.
    OpFinished {
        index: usize,
        slot: Option<usize>,
        op: OpView,
        status: OperationStatus,
        detail: Option<String>,
    },
    /// A delete or copy/repack reverse whose target was already gone — an
    /// idempotent success, not a failure.
    AlreadyGone { index: usize },
    /// A delete refused because no surviving copy of its content exists on disk.
    DeleteRefused { index: usize, path: String },
    /// A delete refused because its surviving-copy check itself errored.
    DeleteVerifyError { index: usize, message: String },
    /// An operation failed.
    OpFailed { index: usize, message: String },
    /// The catalogue couldn't be updated after a completed op (non-fatal).
    CatalogueWarning { op_id: u64, message: String },
    /// A concurrent repack batch is starting.
    RepackBatchStarted { count: usize, in_flight: usize },
}

/// The result of applying a plan.
pub struct ApplyOutcome {
    pub success_count: usize,
    /// Retryable failures (a later `apply` re-attempts these).
    pub error_count: usize,
    /// Operations a safety check declined — sticky, not retried. Kept apart from
    /// `error_count` so an adapter can tell "drove off a flaky mount, run again"
    /// from "the safety net refused this and re-running won't change it".
    pub refused_count: usize,
    /// Where the rollback journal was written (absent on a dry run).
    pub log_path: Option<PathBuf>,
}

impl OpView {
    fn of(kind: &OperationKind) -> Self {
        match kind {
            OperationKind::Copy { source, dest, size } => OpView {
                verb: "COPY",
                from: source.path.clone(),
                to: Some(dest.clone()),
                file_count: None,
                bytes: *size,
                reason: None,
            },
            OperationKind::Move { source, dest, size } => OpView {
                verb: "MOVE",
                from: source.path.clone(),
                to: Some(dest.clone()),
                file_count: None,
                bytes: *size,
                reason: None,
            },
            OperationKind::Relocate { source, dest, size } => OpView {
                verb: "RELOCATE",
                from: source.clone(),
                to: Some(dest.clone()),
                file_count: None,
                bytes: *size,
                reason: None,
            },
            OperationKind::Repack {
                sources,
                dest,
                size,
                ..
            } => OpView {
                verb: "REPACK",
                from: dest.clone(),
                to: None,
                file_count: Some(sources.len()),
                bytes: *size,
                reason: None,
            },
            OperationKind::Delete { path, reason } => OpView {
                verb: "DELETE",
                from: path.clone(),
                to: None,
                file_count: None,
                bytes: 0,
                reason: (!reason.is_empty()).then(|| reason.clone()),
            },
            OperationKind::Quarantine {
                path, size, reason, ..
            } => OpView {
                verb: "QUARANTINE",
                from: path.clone(),
                to: None,
                file_count: None,
                bytes: *size,
                reason: Some(reason.clone()),
            },
        }
    }
}

/// Apply a plan's pending operations, reporting progress through `on_event`.
///
/// The plan's per-operation status is updated in place and (on a real run) the
/// plan file and rollback journal are written, so a re-run resumes rather than
/// repeats. `sources` is the registered source list, used to keep the catalogue
/// in step with each file operation.
pub fn apply_plan(
    conn: &Connection,
    plan: &mut Plan,
    plan_path: &Path,
    sources: &[Source],
    opts: &ApplyOptions,
    on_event: &mut dyn FnMut(ApplyEvent),
) -> Result<ApplyOutcome> {
    let total_ops = plan.operations.len();

    // Create operation log (only if not dry run)
    let mut op_log = if !opts.dry_run {
        Some(OperationLog::new(plan.state_hash.clone()))
    } else {
        None
    };

    let mut success_count = 0;
    let mut error_count = 0;
    let mut refused_count = 0;

    // Placement (copy/move/relocate) and repack operations accumulate here and
    // run concurrently — both are latency-bound over a network mount, so
    // overlapping them is the wall-clock win. A serial operation (delete /
    // quarantine) flushes both batches first, so the one ordering that matters —
    // a placement that creates a surviving copy lands before the delete that
    // relies on it — is preserved exactly as serial apply's.
    let mut placement_batch: Vec<PlacementJob> = Vec::new();
    let mut repack_batch: Vec<RepackJob> = Vec::new();

    for i in 0..plan.operations.len() {
        {
            let op = &plan.operations[i];
            // Skip completed and (sticky) refused operations. A retryable Failed
            // op IS re-attempted, so a run interrupted by a dropped mount recovers
            // by applying again — the whole point of issue #47.
            if !op.status.is_remaining_work() {
                continue;
            }

            // Deferred repacks stay pending for a later pass, in both dry and real
            // runs, so the cheap operations land first.
            if opts.skip_repack && matches!(op.kind, OperationKind::Repack { .. }) {
                continue;
            }

            // A real run accumulates the parallelisable operations into their
            // batches and runs them concurrently. (A dry run falls through to the
            // serial path below, which tallies each op without touching disk.)
            if !opts.dry_run {
                match &op.kind {
                    OperationKind::Repack {
                        sources: repack_sources,
                        dest,
                        format,
                        move_sources,
                        size,
                    } => {
                        repack_batch.push(RepackJob {
                            plan_index: i,
                            operation_id: op.id,
                            sources: repack_sources.clone(),
                            dest: dest.clone(),
                            format: format.clone(),
                            move_sources: *move_sources,
                            size: *size,
                        });
                        continue;
                    }
                    OperationKind::Copy { source, dest, .. } => {
                        placement_batch.push(PlacementJob {
                            plan_index: i,
                            operation_id: op.id,
                            kind: PlacementKind::Copy {
                                source: source.clone(),
                                dest: dest.clone(),
                            },
                        });
                        continue;
                    }
                    OperationKind::Move { source, dest, .. } => {
                        placement_batch.push(PlacementJob {
                            plan_index: i,
                            operation_id: op.id,
                            kind: PlacementKind::Move {
                                source: source.clone(),
                                dest: dest.clone(),
                            },
                        });
                        continue;
                    }
                    OperationKind::Relocate { source, dest, .. } => {
                        placement_batch.push(PlacementJob {
                            plan_index: i,
                            operation_id: op.id,
                            kind: PlacementKind::Relocate {
                                source: source.clone(),
                                dest: dest.clone(),
                            },
                        });
                        continue;
                    }
                    // Delete / Quarantine are serial — they fall through.
                    OperationKind::Delete { .. } | OperationKind::Quarantine { .. } => {}
                }
            }
        }

        // A serial operation (delete/quarantine, or any op on a dry run): complete
        // both concurrent batches before it runs, so a placement that creates a
        // surviving copy lands before the delete that depends on it.
        flush_placement_batch(
            &mut placement_batch,
            opts.jobs,
            plan,
            &mut op_log,
            conn,
            sources,
            total_ops,
            &mut success_count,
            &mut error_count,
            on_event,
        );
        flush_repack_batch(
            &mut repack_batch,
            opts.jobs,
            plan,
            &mut op_log,
            conn,
            sources,
            total_ops,
            &mut success_count,
            &mut error_count,
            on_event,
        );

        let op = &mut plan.operations[i];
        // A serial op (delete/quarantine, or any op on a dry run) runs on this
        // thread, so it has no worker slot.
        on_event(ApplyEvent::OpStarted {
            index: i,
            total: total_ops,
            slot: None,
            op: OpView::of(&op.kind),
        });

        // A dry run performs nothing and leaves the op pending, but still reports a
        // (notional) completion so the preview tallies every op uniformly.
        if opts.dry_run {
            success_count += 1;
            on_event(ApplyEvent::OpFinished {
                index: i,
                slot: None,
                op: OpView::of(&op.kind),
                status: OperationStatus::Completed,
                detail: None,
            });
            continue;
        }

        // The op's terminal state and (when not completed) the reason, set by the
        // arm below and reported once at the tail.
        let mut detail: Option<String> = None;

        match &op.kind {
            // Copy/Move/Relocate/Repack run in their concurrent batches and never
            // reach here in a real run; these arms are a defensive fallback.
            OperationKind::Copy { source, dest, .. } => {
                let result = execute_copy(
                    &source.path,
                    source.archive_path.as_deref(),
                    dest,
                    &source.sha1,
                );
                if let Some(ref mut log) = op_log {
                    log.log_copy(op.id, &source.path, dest, &source.sha1, result.is_ok());
                }
                match result {
                    Ok(()) => {
                        op.status = OperationStatus::Completed;
                        success_count += 1;
                    }
                    Err(e) => {
                        let message = format!("{e:#}");
                        on_event(ApplyEvent::OpFailed {
                            index: i,
                            message: message.clone(),
                        });
                        detail = Some(message);
                        op.status = OperationStatus::Failed;
                        error_count += 1;
                    }
                }
            }
            OperationKind::Move { source, dest, .. } => {
                let result = execute_move(
                    &source.path,
                    source.archive_path.as_deref(),
                    dest,
                    &source.sha1,
                );
                if let Some(ref mut log) = op_log {
                    log.log_move(op.id, &source.path, dest, &source.sha1, result.is_ok());
                }
                match result {
                    Ok(()) => {
                        op.status = OperationStatus::Completed;
                        success_count += 1;
                    }
                    Err(e) => {
                        let message = format!("{e:#}");
                        on_event(ApplyEvent::OpFailed {
                            index: i,
                            message: message.clone(),
                        });
                        detail = Some(message);
                        op.status = OperationStatus::Failed;
                        error_count += 1;
                    }
                }
            }
            OperationKind::Relocate { source, dest, .. } => {
                let result = execute_relocate(source, dest);
                if let Some(ref mut log) = op_log {
                    log.log_relocate(op.id, source, dest, result.is_ok());
                }
                match result {
                    Ok(()) => {
                        op.status = OperationStatus::Completed;
                        success_count += 1;
                    }
                    Err(e) => {
                        let message = format!("{e:#}");
                        on_event(ApplyEvent::OpFailed {
                            index: i,
                            message: message.clone(),
                        });
                        detail = Some(message);
                        op.status = OperationStatus::Failed;
                        error_count += 1;
                    }
                }
            }
            OperationKind::Repack { .. } => {
                // Unreachable: live repacks are batched. Mark failed defensively
                // rather than silently miscount, should a batching bug send one here.
                detail = Some("internal: repack reached the serial path".to_string());
                op.status = OperationStatus::Failed;
                error_count += 1;
            }
            OperationKind::Delete { path, .. } => {
                // Verify-before-delete: a plan deletes a file only because its
                // content is held elsewhere, but never destroy the last copy on
                // a stale record. Refuse if no surviving copy physically exists.
                match delete_has_surviving_copy(conn, sources, path) {
                    // Refused (sticky): the safety net declined this delete; only a
                    // fresh plan should revisit it. Skip the removal entirely.
                    Ok(false) => {
                        on_event(ApplyEvent::DeleteRefused {
                            index: i,
                            path: path.clone(),
                        });
                        detail = Some("no surviving copy on disk".to_string());
                        op.status = OperationStatus::Refused;
                        refused_count += 1;
                    }
                    Err(e) => {
                        let message = format!("{e:#}");
                        on_event(ApplyEvent::DeleteVerifyError {
                            index: i,
                            message: message.clone(),
                        });
                        detail = Some(message);
                        op.status = OperationStatus::Refused;
                        refused_count += 1;
                    }
                    // Safe to remove.
                    Ok(true) => match fs::remove_file(path) {
                        Ok(()) => {
                            op.status = OperationStatus::Completed;
                            success_count += 1;
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                            // Already gone — an idempotent success.
                            op.status = OperationStatus::Completed;
                            success_count += 1;
                            on_event(ApplyEvent::AlreadyGone { index: i });
                        }
                        Err(e) => {
                            let message = format!("{e:#}");
                            on_event(ApplyEvent::OpFailed {
                                index: i,
                                message: message.clone(),
                            });
                            detail = Some(message);
                            op.status = OperationStatus::Failed;
                            error_count += 1;
                        }
                    },
                }
            }
            OperationKind::Quarantine {
                path,
                sha1,
                size,
                reason,
                collection,
            } => {
                let reason_enum =
                    QuarantineReason::parse(reason).unwrap_or(QuarantineReason::PathChanged);

                let result = execute_quarantine(
                    conn,
                    path,
                    sha1,
                    *size as i64,
                    reason_enum,
                    collection.as_deref(),
                    &opts.quarantine_dir,
                );

                // Journal the quarantine so it can be rolled back: its reverse
                // is a Move restoring the original from the quarantine store.
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
                        let message = format!("{e:#}");
                        on_event(ApplyEvent::OpFailed {
                            index: i,
                            message: message.clone(),
                        });
                        detail = Some(message);
                        op.status = OperationStatus::Failed;
                        error_count += 1;
                    }
                }
            }
        }

        // Keep the catalogue in step with what just happened on disk, so a
        // re-plan converges without a re-scan. Catalogue-local and cheap; a
        // failure here doesn't undo the file operation that already succeeded.
        if op.status == OperationStatus::Completed
            && let Err(e) = sync_catalogue_after(conn, sources, &op.kind)
        {
            on_event(ApplyEvent::CatalogueWarning {
                op_id: op.id,
                message: e.to_string(),
            });
        }

        // Report the op's terminal state once — counted, logged, slot-free.
        on_event(ApplyEvent::OpFinished {
            index: i,
            slot: None,
            op: OpView::of(&op.kind),
            status: op.status,
            detail,
        });
    }

    // Placements and repacks at the tail of the plan (the common case) are still
    // batched — drain both.
    flush_placement_batch(
        &mut placement_batch,
        opts.jobs,
        plan,
        &mut op_log,
        conn,
        sources,
        total_ops,
        &mut success_count,
        &mut error_count,
        on_event,
    );
    flush_repack_batch(
        &mut repack_batch,
        opts.jobs,
        plan,
        &mut op_log,
        conn,
        sources,
        total_ops,
        &mut success_count,
        &mut error_count,
        on_event,
    );

    // Save updated plan and operation log
    let mut log_path = None;
    if !opts.dry_run {
        let plan_json = serde_json::to_string_pretty(&plan).context("Failed to serialize plan")?;
        fs::write(plan_path, &plan_json).context("Failed to update plan file")?;

        if let Some(mut log) = op_log {
            log.complete();
            let logs_dir = plan_path
                .parent()
                .and_then(|p| p.parent())
                .map(|p| p.join("logs"))
                .unwrap_or_else(|| PathBuf::from("objects/logs"));
            log_path = Some(log.save(&logs_dir)?);
        }
    }

    Ok(ApplyOutcome {
        success_count,
        error_count,
        refused_count,
        log_path,
    })
}

/// Execute the accumulated placement batch (copy/move/relocate) concurrently,
/// then drain it.
///
/// The same shape as [`flush_repack_batch`]: workers do the file operations,
/// while everything stateful — the rollback-journal entry, the plan status, the
/// catalogue sync, and the progress event — happens here on the calling thread
/// as each outcome streams in, in completion order. The non-`Sync` database
/// connection never leaves this thread.
#[allow(clippy::too_many_arguments)]
fn flush_placement_batch(
    batch: &mut Vec<PlacementJob>,
    workers: usize,
    plan: &mut Plan,
    op_log: &mut Option<OperationLog>,
    conn: &Connection,
    sources: &[Source],
    total_ops: usize,
    success_count: &mut usize,
    error_count: &mut usize,
    on_event: &mut dyn FnMut(ApplyEvent),
) {
    if batch.is_empty() {
        return;
    }
    let jobs = std::mem::take(batch);

    execute_placements_concurrent(jobs, workers, |event| match event {
        // A worker picked up a job: surface it in that worker's slot. The op view
        // (verb, paths, bytes) comes from the plan op itself.
        PlacementEvent::Started { slot, plan_index } => {
            on_event(ApplyEvent::OpStarted {
                index: plan_index,
                total: total_ops,
                slot: Some(slot),
                op: OpView::of(&plan.operations[plan_index].kind),
            });
        }
        // A job finished: journal it, update status + catalogue, free the slot.
        PlacementEvent::Finished { slot, outcome } => {
            let PlacementOutcome { job, result } = outcome;
            let view = OpView::of(&plan.operations[job.plan_index].kind);

            if let Some(log) = op_log {
                let success = result.is_ok();
                match &job.kind {
                    PlacementKind::Copy { source, dest } => {
                        log.log_copy(job.operation_id, &source.path, dest, &source.sha1, success)
                    }
                    PlacementKind::Move { source, dest } => {
                        log.log_move(job.operation_id, &source.path, dest, &source.sha1, success)
                    }
                    PlacementKind::Relocate { source, dest } => {
                        log.log_relocate(job.operation_id, source, dest, success)
                    }
                }
            }

            let mut detail = None;
            let op = &mut plan.operations[job.plan_index];
            match result {
                Ok(()) => {
                    op.status = OperationStatus::Completed;
                    *success_count += 1;

                    // Keep the catalogue in step, as the serial path does per-op.
                    if let Err(e) = sync_catalogue_after(conn, sources, &op.kind) {
                        on_event(ApplyEvent::CatalogueWarning {
                            op_id: op.id,
                            message: e.to_string(),
                        });
                    }
                }
                Err(e) => {
                    let message = format!("{e:#}");
                    on_event(ApplyEvent::OpFailed {
                        index: job.plan_index,
                        message: message.clone(),
                    });
                    detail = Some(message);
                    op.status = OperationStatus::Failed;
                    *error_count += 1;
                }
            }

            on_event(ApplyEvent::OpFinished {
                index: job.plan_index,
                slot: Some(slot),
                op: view,
                status: op.status,
                detail,
            });
        }
    });
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
    plan: &mut Plan,
    op_log: &mut Option<OperationLog>,
    conn: &Connection,
    sources: &[Source],
    total_ops: usize,
    success_count: &mut usize,
    error_count: &mut usize,
    on_event: &mut dyn FnMut(ApplyEvent),
) {
    if batch.is_empty() {
        return;
    }
    let jobs = std::mem::take(batch);
    if jobs.len() > 1 && workers > 1 {
        on_event(ApplyEvent::RepackBatchStarted {
            count: jobs.len(),
            in_flight: workers.min(jobs.len()),
        });
    }

    // A repack op view (verb/paths/size), from a plan index or a job.
    let repack_view = |sources_len: usize, dest: &str, size: u64| OpView {
        verb: "REPACK",
        from: dest.to_string(),
        to: None,
        file_count: Some(sources_len),
        bytes: size,
        reason: None,
    };

    execute_repacks_concurrent(jobs, workers, |event| match event {
        // A worker picked up a repack: surface it in that worker's slot.
        RepackEvent::Started { slot, plan_index } => {
            let job_view = OpView::of(&plan.operations[plan_index].kind);
            on_event(ApplyEvent::OpStarted {
                index: plan_index,
                total: total_ops,
                slot: Some(slot),
                op: job_view,
            });
        }
        // A repack finished: journal it, update status + catalogue, free the slot.
        RepackEvent::Finished { slot, outcome } => {
            let RepackOutcome { job, result } = outcome;
            let view = repack_view(job.sources.len(), &job.dest, job.size);

            // Log the operation. A move-mode repack reports the loose sources it
            // consumed so the reverse can extract them back out.
            if let Some(log) = op_log {
                let source_paths: Vec<String> =
                    job.sources.iter().map(|s| s.path.clone()).collect();
                let consumed = result.as_deref().unwrap_or(&[]);
                log.log_repack(
                    job.operation_id,
                    &source_paths,
                    &job.dest,
                    consumed,
                    result.is_ok(),
                );
            }

            let mut detail = None;
            let op = &mut plan.operations[job.plan_index];
            match result {
                Ok(_) => {
                    op.status = OperationStatus::Completed;
                    *success_count += 1;

                    // Keep the catalogue in step, as the serial path does per-op.
                    if let Err(e) = sync_catalogue_after(conn, sources, &op.kind) {
                        on_event(ApplyEvent::CatalogueWarning {
                            op_id: op.id,
                            message: e.to_string(),
                        });
                    }
                }
                Err(e) => {
                    let message = format!("{e:#}");
                    on_event(ApplyEvent::OpFailed {
                        index: job.plan_index,
                        message: message.clone(),
                    });
                    detail = Some(message);
                    op.status = OperationStatus::Failed;
                    *error_count += 1;
                }
            }

            on_event(ApplyEvent::OpFinished {
                index: job.plan_index,
                slot: Some(slot),
                op: view,
                status: op.status,
                detail,
            });
        }
    });
}

/// Update the file catalogue to match a completed operation, so a re-plan
/// converges without a re-scan: a move/relocate updates the file's recorded
/// location, a quarantine/delete removes it, a copy/repack records the new copy.
/// Paths outside any registered source can't be recorded (the file simply leaves
/// the catalogue's view); the library destination is a source, so the common
/// cases resolve.
fn sync_catalogue_after(conn: &Connection, sources: &[Source], kind: &OperationKind) -> Result<()> {
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
        OperationKind::Quarantine { path, .. } | OperationKind::Delete { path, .. } => {
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
    conn: &Connection,
    sources: &[Source],
    old_abs: &str,
    new_abs: &str,
) -> Result<()> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use crate::plan::{Plan, SourceRef};

    fn loose(path: &str, sha1: &str) -> SourceRef {
        SourceRef {
            path: path.to_string(),
            archive_path: None,
            sha1: sha1.to_string(),
            entry_name: None,
        }
    }

    fn opts(dry_run: bool, quarantine_dir: PathBuf) -> ApplyOptions {
        ApplyOptions {
            dry_run,
            skip_repack: false,
            jobs: 1,
            quarantine_dir,
        }
    }

    #[test]
    fn dry_run_touches_nothing_and_leaves_the_op_pending() {
        let db = Database::open_in_memory().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("out.bin");

        let mut plan = Plan::new("statehash".to_string());
        plan.add_copy(
            loose(
                "/does/not/exist.bin",
                "aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d",
            ),
            dest.to_string_lossy().into_owned(),
            5,
        );

        let mut events = Vec::new();
        let outcome = apply_plan(
            db.conn(),
            &mut plan,
            &tmp.path().join("plan.json"),
            &[],
            &opts(true, tmp.path().join("q")),
            &mut |e| events.push(e),
        )
        .unwrap();

        assert_eq!(outcome.success_count, 1);
        assert_eq!(outcome.error_count, 0);
        assert!(outcome.log_path.is_none(), "a dry run writes no journal");
        assert!(!dest.exists(), "a dry run copies nothing");
        assert!(matches!(events[0], ApplyEvent::OpStarted { .. }));
        // Left pending, so a real apply still runs it.
        assert_eq!(plan.operations[0].status, OperationStatus::Pending);
    }

    #[test]
    fn real_run_copies_the_file_and_writes_a_journal() {
        let db = Database::open_in_memory().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let in_path = tmp.path().join("in.bin");
        std::fs::write(&in_path, b"hello").unwrap();
        let dest = tmp.path().join("lib/out.bin");

        let mut plan = Plan::new("statehash".to_string());
        plan.add_copy(
            // sha1("hello")
            loose(
                in_path.to_str().unwrap(),
                "aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d",
            ),
            dest.to_string_lossy().into_owned(),
            5,
        );

        // Plan under objects/plans so the journal lands in objects/logs.
        let plans_dir = tmp.path().join("objects/plans");
        std::fs::create_dir_all(&plans_dir).unwrap();
        let plan_path = plans_dir.join("plan.json");

        let outcome = apply_plan(
            db.conn(),
            &mut plan,
            &plan_path,
            &[],
            &opts(false, tmp.path().join("q")),
            &mut |_| {},
        )
        .unwrap();

        assert_eq!(outcome.success_count, 1);
        assert_eq!(outcome.error_count, 0);
        assert_eq!(std::fs::read(&dest).unwrap(), b"hello", "file copied");
        assert_eq!(plan.operations[0].status, OperationStatus::Completed);
        assert!(outcome.log_path.unwrap().exists(), "journal written");
        assert!(plan_path.exists(), "updated plan written");
    }

    // Issue #47: an op that fails from a transient I/O error (here, a source not
    // yet present — as when a mount drops mid-run) is `Failed`, and a later apply
    // re-attempts it. Once the source is readable, the retry completes it.
    #[test]
    fn a_failed_op_is_retried_on_a_later_apply() {
        let db = Database::open_in_memory().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let plans_dir = tmp.path().join("objects/plans");
        std::fs::create_dir_all(&plans_dir).unwrap();
        let plan_path = plans_dir.join("plan.json");

        let src = tmp.path().join("in.bin");
        let dest = tmp.path().join("lib/out.bin");
        let mut plan = Plan::new("statehash".to_string());
        // sha1("hello"); the source does not exist yet.
        plan.add_move(
            loose(
                src.to_str().unwrap(),
                "aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d",
            ),
            dest.to_string_lossy().into_owned(),
            5,
        );

        // First apply: source missing → the op fails, but retryably.
        let first = apply_plan(
            db.conn(),
            &mut plan,
            &plan_path,
            &[],
            &opts(false, tmp.path().join("q")),
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(first.error_count, 1);
        assert_eq!(first.refused_count, 0, "an I/O failure is not a refusal");
        assert_eq!(plan.operations[0].status, OperationStatus::Failed);
        assert!(!dest.exists());

        // The source appears (mount back); a second apply retries the Failed op.
        std::fs::write(&src, b"hello").unwrap();
        let second = apply_plan(
            db.conn(),
            &mut plan,
            &plan_path,
            &[],
            &opts(false, tmp.path().join("q")),
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(
            second.success_count, 1,
            "the failed op is retried and completes"
        );
        assert_eq!(plan.operations[0].status, OperationStatus::Completed);
        assert_eq!(std::fs::read(&dest).unwrap(), b"hello");
    }

    // A delete the safety net refuses is `Refused` (sticky): a later apply skips
    // it rather than blindly retrying — only regenerating the plan should revisit.
    #[test]
    fn a_refused_delete_is_sticky_and_not_retried() {
        use crate::db::files::{add_source, list_sources};

        let db = Database::open_in_memory().unwrap();
        let conn = db.conn();
        let tmp = tempfile::tempdir().unwrap();
        let plans_dir = tmp.path().join("objects/plans");
        std::fs::create_dir_all(&plans_dir).unwrap();
        let plan_path = plans_dir.join("plan.json");

        // A registered source holding an uncatalogued file — a delete of it can't
        // be proven safe (no known surviving copy), so it's refused.
        let root = tmp.path().to_str().unwrap();
        add_source(conn, root, false).unwrap();
        let victim = tmp.path().join("orphan.bin");
        std::fs::write(&victim, b"data").unwrap();
        let sources = list_sources(conn).unwrap();

        let mut plan = Plan::new("statehash".to_string());
        plan.add_delete(
            victim.to_string_lossy().into_owned(),
            "exact duplicate — kept elsewhere".into(),
        );

        let first = apply_plan(
            conn,
            &mut plan,
            &plan_path,
            &sources,
            &opts(false, tmp.path().join("q")),
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(first.refused_count, 1, "uncatalogued delete is refused");
        assert_eq!(first.error_count, 0, "a refusal is not a retryable failure");
        assert_eq!(plan.operations[0].status, OperationStatus::Refused);
        assert!(victim.exists(), "refused → nothing deleted");

        // A second apply skips the sticky Refused op entirely.
        let second = apply_plan(
            conn,
            &mut plan,
            &plan_path,
            &sources,
            &opts(false, tmp.path().join("q")),
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(second.success_count, 0);
        assert_eq!(second.error_count, 0);
        assert_eq!(
            second.refused_count, 0,
            "the refused op was skipped, not re-evaluated"
        );
        assert_eq!(plan.operations[0].status, OperationStatus::Refused);
        assert!(victim.exists());
    }

    fn opts_jobs(jobs: usize, quarantine_dir: PathBuf) -> ApplyOptions {
        ApplyOptions {
            dry_run: false,
            skip_repack: false,
            jobs,
            quarantine_dir,
        }
    }

    // A batch of placements runs concurrently and every one lands, is journaled,
    // and reports exactly one progress event — the worker pool integrated into
    // apply_plan.
    #[test]
    fn concurrent_placements_all_complete_and_journal() {
        let db = Database::open_in_memory().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let plans_dir = tmp.path().join("objects/plans");
        std::fs::create_dir_all(&plans_dir).unwrap();
        let plan_path = plans_dir.join("plan.json");

        let mut plan = Plan::new("statehash".to_string());
        let mut dests = Vec::new();
        for i in 0..6 {
            let src = tmp.path().join(format!("in-{i}.bin"));
            std::fs::write(&src, b"hello").unwrap(); // sha1 aaf4c6…
            let dest = tmp.path().join(format!("lib/out-{i}.bin"));
            dests.push(dest.clone());
            plan.add_copy(
                loose(
                    src.to_str().unwrap(),
                    "aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d",
                ),
                dest.to_string_lossy().into_owned(),
                5,
            );
        }

        let mut started = 0;
        let outcome = apply_plan(
            db.conn(),
            &mut plan,
            &plan_path,
            &[],
            &opts_jobs(4, tmp.path().join("q")),
            &mut |e| {
                if matches!(e, ApplyEvent::OpStarted { .. }) {
                    started += 1;
                }
            },
        )
        .unwrap();

        assert_eq!(outcome.success_count, 6);
        assert_eq!(outcome.error_count, 0);
        assert_eq!(started, 6, "one progress event per op");
        for dest in &dests {
            assert_eq!(std::fs::read(dest).unwrap(), b"hello", "every copy landed");
        }
        assert!(
            plan.operations
                .iter()
                .all(|o| o.status == OperationStatus::Completed)
        );
        assert!(outcome.log_path.unwrap().exists(), "journal written");
    }

    // The placement batch flushes before a serial delete, so a placement that
    // creates a surviving copy always lands before the delete that relies on it.
    #[test]
    fn placements_flush_before_a_serial_delete() {
        let db = Database::open_in_memory().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let plans_dir = tmp.path().join("objects/plans");
        std::fs::create_dir_all(&plans_dir).unwrap();
        let plan_path = plans_dir.join("plan.json");

        let mut plan = Plan::new("statehash".to_string());
        for i in 0..4 {
            let src = tmp.path().join(format!("in-{i}.bin"));
            std::fs::write(&src, b"hello").unwrap();
            plan.add_copy(
                loose(
                    src.to_str().unwrap(),
                    "aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d",
                ),
                tmp.path()
                    .join(format!("out-{i}.bin"))
                    .to_string_lossy()
                    .into_owned(),
                5,
            );
        }
        // An uncatalogued delete (refused) — present only to mark the boundary.
        plan.add_delete(
            tmp.path().join("nope.bin").to_string_lossy().into_owned(),
            "exact duplicate — kept elsewhere".into(),
        );

        let mut verbs: Vec<&'static str> = Vec::new();
        apply_plan(
            db.conn(),
            &mut plan,
            &plan_path,
            &[],
            &opts_jobs(4, tmp.path().join("q")),
            &mut |e| {
                if let ApplyEvent::OpStarted { op, .. } = e {
                    verbs.push(op.verb);
                }
            },
        )
        .unwrap();

        let delete_pos = verbs.iter().position(|v| *v == "DELETE").expect("a delete");
        assert_eq!(verbs.iter().filter(|v| **v == "COPY").count(), 4);
        assert!(
            verbs[..delete_pos].iter().all(|v| *v == "COPY"),
            "all placements flush before the serial delete: {verbs:?}"
        );
    }
}
