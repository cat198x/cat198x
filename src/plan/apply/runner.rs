use std::path::Path;

use anyhow::Result;
use rusqlite::Connection;

use super::catalogue::sync_catalogue_after;
use super::persistence::persist_apply_run;
use super::serial::run_serial_operation;
use super::{ApplyEvent, ApplyOptions, ApplyOutcome, OpView};
use crate::db::files::Source;
use crate::plan::executor::{
    PlacementEvent, PlacementJob, PlacementKind, PlacementOutcome, RepackEvent, RepackJob,
    RepackOutcome, execute_placements_concurrent, execute_repacks_concurrent,
};
use crate::plan::{OperationKind, OperationLog, OperationStatus, Plan};

pub(super) struct ApplyRunner<'a> {
    conn: &'a Connection,
    plan: &'a mut Plan,
    plan_path: &'a Path,
    sources: &'a [Source],
    opts: &'a ApplyOptions,
    on_event: &'a mut dyn FnMut(ApplyEvent),
    total_ops: usize,
    op_log: Option<OperationLog>,
    success_count: usize,
    error_count: usize,
    refused_count: usize,
    placement_batch: Vec<PlacementJob>,
    repack_batch: Vec<RepackJob>,
}

impl<'a> ApplyRunner<'a> {
    pub(super) fn new(
        conn: &'a Connection,
        plan: &'a mut Plan,
        plan_path: &'a Path,
        sources: &'a [Source],
        opts: &'a ApplyOptions,
        on_event: &'a mut dyn FnMut(ApplyEvent),
    ) -> Self {
        let total_ops = plan.operations.len();
        let op_log = if opts.dry_run {
            None
        } else {
            Some(OperationLog::new(plan.state_hash.clone()))
        };

        Self {
            conn,
            plan,
            plan_path,
            sources,
            opts,
            on_event,
            total_ops,
            op_log,
            success_count: 0,
            error_count: 0,
            refused_count: 0,
            placement_batch: Vec::new(),
            repack_batch: Vec::new(),
        }
    }

    pub(super) fn run(mut self) -> Result<ApplyOutcome> {
        for index in 0..self.plan.operations.len() {
            {
                let op = &self.plan.operations[index];
                // Skip completed and (sticky) refused operations. A retryable
                // Failed op IS re-attempted, so a run interrupted by a dropped
                // mount recovers by applying again — the whole point of issue #47.
                if !op.status.is_remaining_work() {
                    continue;
                }

                // Deferred repacks stay pending for a later pass, in both dry and
                // real runs, so the cheap operations land first.
                if self.opts.skip_repack && matches!(op.kind, OperationKind::Repack { .. }) {
                    continue;
                }

                // A real run accumulates the parallelisable operations into their
                // batches and runs them concurrently. (A dry run falls through to
                // the serial path below, which tallies each op without touching disk.)
                if !self.opts.dry_run {
                    match &op.kind {
                        OperationKind::Repack {
                            sources: repack_sources,
                            dest,
                            format,
                            move_sources,
                            size,
                        } => {
                            self.repack_batch.push(RepackJob {
                                plan_index: index,
                                operation_id: op.id,
                                sources: repack_sources.clone(),
                                dest: dest.clone(),
                                format: format.clone(),
                                move_sources: *move_sources,
                                size: *size,
                            });
                            continue;
                        }
                        OperationKind::Copy {
                            source,
                            dest,
                            placement,
                            ..
                        } => {
                            self.placement_batch.push(PlacementJob {
                                plan_index: index,
                                operation_id: op.id,
                                kind: PlacementKind::Copy {
                                    source: source.clone(),
                                    dest: dest.clone(),
                                    placement: placement.clone(),
                                },
                            });
                            continue;
                        }
                        OperationKind::Move {
                            source,
                            dest,
                            placement,
                            ..
                        } => {
                            self.placement_batch.push(PlacementJob {
                                plan_index: index,
                                operation_id: op.id,
                                kind: PlacementKind::Move {
                                    source: source.clone(),
                                    dest: dest.clone(),
                                    placement: placement.clone(),
                                },
                            });
                            continue;
                        }
                        OperationKind::Relocate { source, dest, .. } => {
                            self.placement_batch.push(PlacementJob {
                                plan_index: index,
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

            // A serial operation (delete/quarantine, or any op on a dry run):
            // complete both concurrent batches before it runs, so a placement
            // that creates a surviving copy lands before the delete that depends on it.
            self.flush_batches();
            self.run_serial(index);
        }

        // Placements and repacks at the tail of the plan (the common case) are
        // still batched — drain both.
        self.flush_batches();

        let Self {
            plan,
            plan_path,
            opts,
            op_log,
            success_count,
            error_count,
            refused_count,
            ..
        } = self;
        let log_path = persist_apply_run(plan, plan_path, opts.dry_run, op_log)?;

        Ok(ApplyOutcome {
            success_count,
            error_count,
            refused_count,
            log_path,
        })
    }

    fn flush_batches(&mut self) {
        self.flush_placement_batch();
        self.flush_repack_batch();
    }

    /// Execute the accumulated placement batch (copy/move/relocate)
    /// concurrently, then drain it. Workers do the file operations, while
    /// everything stateful stays on this thread.
    fn flush_placement_batch(&mut self) {
        if self.placement_batch.is_empty() {
            return;
        }
        let jobs = std::mem::take(&mut self.placement_batch);

        execute_placements_concurrent(jobs, self.opts.jobs, |event| match event {
            PlacementEvent::Started { slot, plan_index } => {
                (self.on_event)(ApplyEvent::OpStarted {
                    index: plan_index,
                    total: self.total_ops,
                    slot: Some(slot),
                    op: OpView::of(&self.plan.operations[plan_index].kind),
                });
            }
            PlacementEvent::Finished { slot, outcome } => {
                let PlacementOutcome { job, result } = outcome;
                let view = OpView::of(&self.plan.operations[job.plan_index].kind);

                if let Some(log) = self.op_log.as_mut() {
                    let success = result.is_ok();
                    match &job.kind {
                        PlacementKind::Copy { source, dest, .. } => log.log_copy(
                            job.operation_id,
                            &source.path,
                            dest,
                            &source.sha1,
                            success,
                        ),
                        PlacementKind::Move { source, dest, .. } => log.log_move(
                            job.operation_id,
                            &source.path,
                            dest,
                            &source.sha1,
                            success,
                        ),
                        PlacementKind::Relocate { source, dest } => {
                            log.log_relocate(job.operation_id, source, dest, success)
                        }
                    }
                }

                let mut detail = None;
                match result {
                    Ok(()) => {
                        let op_id = {
                            let op = &mut self.plan.operations[job.plan_index];
                            op.status = OperationStatus::Completed;
                            op.id
                        };
                        self.success_count += 1;

                        if let Err(e) = sync_catalogue_after(
                            self.conn,
                            self.sources,
                            &self.plan.operations[job.plan_index].kind,
                        ) {
                            (self.on_event)(ApplyEvent::CatalogueWarning {
                                op_id,
                                message: e.to_string(),
                            });
                        }
                    }
                    Err(e) => {
                        let message = format!("{e:#}");
                        (self.on_event)(ApplyEvent::OpFailed {
                            index: job.plan_index,
                            message: message.clone(),
                        });
                        detail = Some(message);
                        self.plan.operations[job.plan_index].status = OperationStatus::Failed;
                        self.error_count += 1;
                    }
                }

                (self.on_event)(ApplyEvent::OpFinished {
                    index: job.plan_index,
                    slot: Some(slot),
                    op: view,
                    status: self.plan.operations[job.plan_index].status,
                    detail,
                });
            }
        });
    }

    /// Execute the accumulated repack batch concurrently, then drain it.
    /// Journal entries, plan status updates, catalogue sync, and progress events
    /// stay on the runner thread as each worker outcome streams in.
    fn flush_repack_batch(&mut self) {
        if self.repack_batch.is_empty() {
            return;
        }
        let jobs = std::mem::take(&mut self.repack_batch);
        if jobs.len() > 1 && self.opts.jobs > 1 {
            (self.on_event)(ApplyEvent::RepackBatchStarted {
                count: jobs.len(),
                in_flight: self.opts.jobs.min(jobs.len()),
            });
        }

        let repack_view = |sources_len: usize, dest: &str, size: u64| OpView {
            verb: "REPACK",
            from: dest.to_string(),
            to: None,
            file_count: Some(sources_len),
            bytes: size,
            reason: None,
        };

        execute_repacks_concurrent(jobs, self.opts.jobs, |event| match event {
            RepackEvent::Started { slot, plan_index } => {
                (self.on_event)(ApplyEvent::OpStarted {
                    index: plan_index,
                    total: self.total_ops,
                    slot: Some(slot),
                    op: OpView::of(&self.plan.operations[plan_index].kind),
                });
            }
            RepackEvent::Finished { slot, outcome } => {
                let RepackOutcome { job, result } = outcome;
                let view = repack_view(job.sources.len(), &job.dest, job.size);

                if let Some(log) = self.op_log.as_mut() {
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
                match result {
                    Ok(_) => {
                        let op_id = {
                            let op = &mut self.plan.operations[job.plan_index];
                            op.status = OperationStatus::Completed;
                            op.id
                        };
                        self.success_count += 1;

                        if let Err(e) = sync_catalogue_after(
                            self.conn,
                            self.sources,
                            &self.plan.operations[job.plan_index].kind,
                        ) {
                            (self.on_event)(ApplyEvent::CatalogueWarning {
                                op_id,
                                message: e.to_string(),
                            });
                        }
                    }
                    Err(e) => {
                        let message = format!("{e:#}");
                        (self.on_event)(ApplyEvent::OpFailed {
                            index: job.plan_index,
                            message: message.clone(),
                        });
                        detail = Some(message);
                        self.plan.operations[job.plan_index].status = OperationStatus::Failed;
                        self.error_count += 1;
                    }
                }

                (self.on_event)(ApplyEvent::OpFinished {
                    index: job.plan_index,
                    slot: Some(slot),
                    op: view,
                    status: self.plan.operations[job.plan_index].status,
                    detail,
                });
            }
        });
    }

    fn run_serial(&mut self, index: usize) {
        let counts = run_serial_operation(
            index,
            self.total_ops,
            &mut self.plan.operations[index],
            self.conn,
            self.sources,
            self.opts,
            &mut self.op_log,
            self.on_event,
        );
        self.success_count += counts.success;
        self.error_count += counts.error;
        self.refused_count += counts.refused;
    }
}
