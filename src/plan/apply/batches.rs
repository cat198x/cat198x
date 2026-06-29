use rusqlite::Connection;

use super::{ApplyEvent, OpView, sync_catalogue_after};
use crate::db::files::Source;
use crate::plan::executor::{
    PlacementEvent, PlacementJob, PlacementKind, PlacementOutcome, RepackEvent, RepackJob,
    RepackOutcome, execute_placements_concurrent, execute_repacks_concurrent,
};
use crate::plan::{OperationLog, OperationStatus, Plan};

/// Execute the accumulated placement batch (copy/move/relocate) concurrently,
/// then drain it.
///
/// The same shape as [`flush_repack_batch`]: workers do the file operations,
/// while everything stateful — the rollback-journal entry, the plan status, the
/// catalogue sync, and the progress event — happens here on the calling thread
/// as each outcome streams in, in completion order. The non-`Sync` database
/// connection never leaves this thread.
#[allow(clippy::too_many_arguments)]
pub(super) fn flush_placement_batch(
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
                    PlacementKind::Copy { source, dest, .. } => {
                        log.log_copy(job.operation_id, &source.path, dest, &source.sha1, success)
                    }
                    PlacementKind::Move { source, dest, .. } => {
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
pub(super) fn flush_repack_batch(
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
