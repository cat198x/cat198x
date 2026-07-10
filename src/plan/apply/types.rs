use std::path::PathBuf;

use crate::plan::{OperationKind, OperationStatus};

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
    pub(super) fn of(kind: &OperationKind) -> Self {
        match kind {
            OperationKind::Copy {
                source, dest, size, ..
            } => OpView {
                verb: "COPY",
                from: source.path.clone(),
                to: Some(dest.clone()),
                file_count: None,
                bytes: *size,
                reason: None,
            },
            OperationKind::Move {
                source, dest, size, ..
            } => OpView {
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
            OperationKind::Delete { path, reason, .. } => OpView {
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
