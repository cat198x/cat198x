use std::fs;

use rusqlite::Connection;

use super::catalogue::sync_catalogue_after;
use super::{ApplyEvent, ApplyOptions, OpView};
use crate::db::files::Source;
use crate::db::quarantine::QuarantineReason;
use crate::plan::executor::{
    delete_has_surviving_copy, execute_copy, execute_move, execute_quarantine, execute_relocate,
};
use crate::plan::{Operation, OperationKind, OperationLog, OperationStatus};

#[derive(Default)]
pub(super) struct SerialCounts {
    pub(super) success: usize,
    pub(super) error: usize,
    pub(super) refused: usize,
}

/// Run one serial operation: delete/quarantine in a real run, or any operation
/// in a dry run. Parallel-capable operations only reach this path as a defensive
/// fallback because real runs batch them before this helper is called.
#[allow(clippy::too_many_arguments)]
pub(super) fn run_serial_operation(
    index: usize,
    total_ops: usize,
    op: &mut Operation,
    conn: &Connection,
    sources: &[Source],
    opts: &ApplyOptions,
    op_log: &mut Option<OperationLog>,
    on_event: &mut dyn FnMut(ApplyEvent),
) -> SerialCounts {
    // A serial op (delete/quarantine, or any op on a dry run) runs on this
    // thread, so it has no worker slot.
    on_event(ApplyEvent::OpStarted {
        index,
        total: total_ops,
        slot: None,
        op: OpView::of(&op.kind),
    });

    // A dry run performs nothing and leaves the op pending, but still reports a
    // (notional) completion so the preview tallies every op uniformly.
    if opts.dry_run {
        on_event(ApplyEvent::OpFinished {
            index,
            slot: None,
            op: OpView::of(&op.kind),
            status: OperationStatus::Completed,
            detail: None,
        });
        return SerialCounts {
            success: 1,
            ..Default::default()
        };
    }

    // The op's terminal state and (when not completed) the reason, set by the
    // arm below and reported once at the tail.
    let mut detail: Option<String> = None;
    let mut counts = SerialCounts::default();

    match &op.kind {
        // Copy/Move/Relocate/Repack run in their concurrent batches and never
        // reach here in a real run; these arms are a defensive fallback.
        OperationKind::Copy {
            source,
            dest,
            placement,
            ..
        } => {
            let result = execute_copy(
                &source.path,
                source.archive_path.as_deref(),
                dest,
                &source.sha1,
                placement,
            );
            if let Some(log) = op_log.as_mut() {
                log.log_copy(op.id, &source.path, dest, &source.sha1, result.is_ok());
            }
            match result {
                Ok(()) => {
                    op.status = OperationStatus::Completed;
                    counts.success += 1;
                }
                Err(e) => {
                    let message = format!("{e:#}");
                    on_event(ApplyEvent::OpFailed {
                        index,
                        message: message.clone(),
                    });
                    detail = Some(message);
                    op.status = OperationStatus::Failed;
                    counts.error += 1;
                }
            }
        }
        OperationKind::Move {
            source,
            dest,
            placement,
            ..
        } => {
            let result = execute_move(
                &source.path,
                source.archive_path.as_deref(),
                dest,
                &source.sha1,
                placement,
            );
            if let Some(log) = op_log.as_mut() {
                log.log_move(op.id, &source.path, dest, &source.sha1, result.is_ok());
            }
            match result {
                Ok(()) => {
                    op.status = OperationStatus::Completed;
                    counts.success += 1;
                }
                Err(e) => {
                    let message = format!("{e:#}");
                    on_event(ApplyEvent::OpFailed {
                        index,
                        message: message.clone(),
                    });
                    detail = Some(message);
                    op.status = OperationStatus::Failed;
                    counts.error += 1;
                }
            }
        }
        OperationKind::Relocate { source, dest, .. } => {
            let result = execute_relocate(source, dest);
            if let Some(log) = op_log.as_mut() {
                log.log_relocate(op.id, source, dest, result.is_ok());
            }
            match result {
                Ok(()) => {
                    op.status = OperationStatus::Completed;
                    counts.success += 1;
                }
                Err(e) => {
                    let message = format!("{e:#}");
                    on_event(ApplyEvent::OpFailed {
                        index,
                        message: message.clone(),
                    });
                    detail = Some(message);
                    op.status = OperationStatus::Failed;
                    counts.error += 1;
                }
            }
        }
        OperationKind::Repack { .. } => {
            // Unreachable: live repacks are batched. Mark failed defensively
            // rather than silently miscount, should a batching bug send one here.
            detail = Some("internal: repack reached the serial path".to_string());
            op.status = OperationStatus::Failed;
            counts.error += 1;
        }
        OperationKind::Delete { path, rebuild, .. } => {
            // Verify-before-delete: a plan deletes a file only because its
            // content is held elsewhere, but never destroy the last copy on
            // a stale record. Refuse if no surviving copy physically exists.
            match delete_has_surviving_copy(conn, sources, path) {
                // Refused (sticky): the safety net declined this delete; only a
                // fresh plan should revisit it. Skip the removal entirely.
                Ok(false) => {
                    on_event(ApplyEvent::DeleteRefused {
                        index,
                        path: path.clone(),
                    });
                    detail = Some("no surviving copy on disk".to_string());
                    op.status = OperationStatus::Refused;
                    counts.refused += 1;
                }
                Err(e) => {
                    let message = format!("{e:#}");
                    on_event(ApplyEvent::DeleteVerifyError {
                        index,
                        message: message.clone(),
                    });
                    detail = Some(message);
                    op.status = OperationStatus::Refused;
                    counts.refused += 1;
                }
                // Safe to remove.
                Ok(true) => match fs::remove_file(path) {
                    Ok(()) => {
                        // A container-drain delete journals a reverse that
                        // rebuilds the container from the destinations its
                        // entries were repacked into — but only because *this*
                        // run did the removal (the already-gone arm below does
                        // not, since a prior run's log owns that reversal).
                        if let (Some(log), Some(rebuild)) = (op_log.as_mut(), rebuild) {
                            log.log_container_drain(
                                op.id,
                                path,
                                &rebuild.format,
                                &rebuild.entries,
                                true,
                            );
                        }
                        op.status = OperationStatus::Completed;
                        counts.success += 1;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        // Already gone — an idempotent success.
                        op.status = OperationStatus::Completed;
                        counts.success += 1;
                        on_event(ApplyEvent::AlreadyGone { index });
                    }
                    Err(e) => {
                        let message = format!("{e:#}");
                        on_event(ApplyEvent::OpFailed {
                            index,
                            message: message.clone(),
                        });
                        detail = Some(message);
                        op.status = OperationStatus::Failed;
                        counts.error += 1;
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
            if let Some(log) = op_log.as_mut() {
                let quarantine_path = result.as_deref().unwrap_or("");
                log.log_quarantine(op.id, path, quarantine_path, sha1, result.is_ok());
            }

            match result {
                Ok(_) => {
                    op.status = OperationStatus::Completed;
                    counts.success += 1;
                }
                Err(e) => {
                    let message = format!("{e:#}");
                    on_event(ApplyEvent::OpFailed {
                        index,
                        message: message.clone(),
                    });
                    detail = Some(message);
                    op.status = OperationStatus::Failed;
                    counts.error += 1;
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
        index,
        slot: None,
        op: OpView::of(&op.kind),
        status: op.status,
        detail,
    });

    counts
}
