use std::path::Path;

use anyhow::Result;
use rusqlite::Connection;

use super::batches::{flush_placement_batch, flush_repack_batch};
use super::persistence::persist_apply_run;
use super::serial::run_serial_operation;
use super::{ApplyEvent, ApplyOptions, ApplyOutcome};
use crate::db::files::Source;
use crate::plan::executor::{PlacementJob, PlacementKind, RepackJob};
use crate::plan::{OperationKind, OperationLog, Plan};

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
        flush_placement_batch(
            &mut self.placement_batch,
            self.opts.jobs,
            self.plan,
            &mut self.op_log,
            self.conn,
            self.sources,
            self.total_ops,
            &mut self.success_count,
            &mut self.error_count,
            self.on_event,
        );
        flush_repack_batch(
            &mut self.repack_batch,
            self.opts.jobs,
            self.plan,
            &mut self.op_log,
            self.conn,
            self.sources,
            self.total_ops,
            &mut self.success_count,
            &mut self.error_count,
            self.on_event,
        );
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
