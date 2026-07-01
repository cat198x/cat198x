use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;
use rusqlite::Connection;
use serde::Serialize;

use crate::db::files;
use crate::ops::plans::newest_plan_file;
use crate::plan::executor::check_disk_space;
use crate::plan::{ApplyEvent, ApplyOptions, Plan, apply_plan, compute_state_hash};

/// Worker concurrency for a UI/MCP-driven apply. Placements and repacks are
/// latency-bound over a network mount, so a handful of workers overlaps the
/// round-trips (see decisions/concurrent-apply.md); the destination is one
/// volume, so this is bounded by the mount, not the CPU. The CLI tunes this via
/// `--jobs`; the adapters here use a fixed sensible default.
const APPLY_JOBS: usize = 6;

/// How to run an apply through the ops surface: the dry-run switch plus the one
/// gate override a real apply needs. The staleness gate has no override (a stale
/// plan must be regenerated, never forced).
#[derive(Debug, Clone, Copy)]
pub struct ApplyRunOptions {
    /// Report what would happen without touching any file.
    pub dry_run: bool,
    /// Apply even when the destination volume looks too small. Ignored on a dry
    /// run, which never mutates and so never blocks on a gate.
    pub skip_space_check: bool,
}

impl ApplyRunOptions {
    /// The dry-run preview: mutates nothing and never blocks on a gate (it only
    /// *reports* staleness and disk readiness for the adapter to show).
    pub fn preview() -> Self {
        Self {
            dry_run: true,
            skip_space_check: false,
        }
    }
}

/// Readiness to apply the latest plan, plus the work it would do — what the UI's
/// dry-run preview shows, and what a real apply returns once it has run.
#[derive(Debug, Clone, Serialize)]
pub struct ApplyReport {
    /// The plan predates the current catalogue, so it should be regenerated.
    pub stale: bool,
    /// The destination volumes have room for the plan's transfers.
    pub disk_ok: bool,
    /// When `disk_ok` is false, the "need X, have Y" detail from the check.
    pub disk_detail: Option<String>,
    pub total_ops: usize,
    pub pending: usize,
    /// Bytes the plan would transfer (copy/move), from the plan summary — the
    /// figure a confirm gate states before a real apply ("move ~X").
    pub total_bytes: u64,
    /// Operation count by kind (copy/move/relocate/repack/delete/quarantine),
    /// tallied from the apply engine's own progress events.
    pub by_kind: BTreeMap<String, usize>,
    /// Whether this was a dry run.
    pub dry_run: bool,
    /// When a real apply was refused by a gate (a stale plan, or insufficient
    /// disk without an override), the human-readable reason — and nothing was
    /// touched. `None` when the apply ran, and always `None` for a dry run
    /// (which only reports the gate flags above, never blocks on them).
    pub refused: Option<String>,
    pub succeeded: usize,
    /// Retryable failures — a later apply re-attempts these (e.g. a dropped mount).
    pub failed: usize,
    /// Operations a safety check declined (verify-before-delete). Sticky: not
    /// retried by re-applying. Distinct from `failed` so the UI can say "run again
    /// to resume" only when there is genuinely retryable work.
    pub refused_ops: usize,
}

/// Drive the apply engine over the latest plan and report what it did (or, on a
/// dry run, would do).
///
/// A dry run (`ApplyRunOptions::preview`) performs nothing, reports the
/// apply-time gates (staleness, disk space) the static plan view can't show, and
/// tallies the operations from the engine's [`ApplyEvent`] stream. A real apply
/// (`dry_run: false`) enforces those gates — refusing to mutate a stale or
/// won't-fit plan (see [`ApplyReport::refused`]) — and otherwise carries the plan
/// out. Returns `None` when no plan has been generated. For a live progress bar,
/// use [`apply_streaming`].
pub fn apply(
    conn: &Connection,
    data_dir: &Path,
    opts: ApplyRunOptions,
) -> Result<Option<ApplyReport>> {
    apply_streaming(conn, data_dir, opts, &mut |_| {})
}

/// One progress update as a plan is applied — either an operation *starting* in a
/// worker slot, or one *finishing*. A live display tracks `jobs` concurrent slots
/// from these: occupy a slot on a start, free it on a finish.
#[derive(Debug, Clone, Serialize)]
pub struct ApplyProgress {
    /// Operations completed so far (monotonic).
    pub done: usize,
    /// Total operations in the plan.
    pub total: usize,
    /// Number of worker slots (the configured concurrency), so the display sizes
    /// its grid. A serial op (delete/quarantine) reports `slot: None`.
    pub jobs: usize,
    /// Which worker slot this update concerns, or `None` for a serial op.
    pub slot: Option<usize>,
    /// `true` when the operation finished (free the slot, bank its bytes); `false`
    /// when it started (occupy the slot).
    pub finished: bool,
    /// The verb of the operation (COPY/MOVE/RELOCATE/…). Empty on a finish event.
    pub verb: String,
    /// The operation's source (or, for a repack, the destination archive).
    pub from: String,
    /// The operation's destination, when it has a distinct one.
    pub to: Option<String>,
    /// This operation's size in bytes (a delete has none, so `0`).
    pub bytes: u64,
    /// Bytes of the operations that have **completed** — the processed total, which
    /// never runs ahead of the disk because an in-flight op's bytes join it only
    /// when it finishes.
    pub bytes_done: u64,
    /// The plan's total bytes, so the display can show processed-of-total.
    pub bytes_total: u64,
    /// Set when this update is a **log line** — the terminal result of one
    /// operation: `"ok"`, `"failed"`, or `"refused"`. `None` for a plain
    /// slot/aggregate tick. A failure or refusal carries its reason in `detail`.
    pub outcome: Option<String>,
    /// The error message or refusal reason, for a `failed`/`refused` log line.
    pub detail: Option<String>,
    /// Why the operation is safe to do, when it carries one — a dedup delete names
    /// the canonical copy it keeps, a quarantine names what flagged it. Lets the
    /// display reassure on a mass delete ("kept …"); `None` for self-evident ops.
    pub reason: Option<String>,
}

/// Like [`apply`], but reports each operation's progress through `on_progress`
/// as the engine runs — the hook a UI drives a live progress bar from. A caller
/// that wants only the final report uses [`apply`]. Returns `None` without a
/// plan.
pub fn apply_streaming(
    conn: &Connection,
    data_dir: &Path,
    opts: ApplyRunOptions,
    on_progress: &mut dyn FnMut(ApplyProgress),
) -> Result<Option<ApplyReport>> {
    let Some(plan_path) = newest_plan_file(data_dir)? else {
        return Ok(None);
    };
    let contents = std::fs::read_to_string(&plan_path)?;
    let mut plan: Plan = serde_json::from_str(&contents)?;

    let stale = compute_state_hash(conn)? != plan.state_hash;
    let (disk_ok, disk_detail) = match check_disk_space(&plan) {
        Ok(()) => (true, None),
        Err(e) => (false, Some(e.to_string())),
    };
    let total_ops = plan.operations.len();
    let total_bytes = plan.summary.total_bytes;
    // Remaining work is fresh `Pending` ops plus retryable `Failed` ones — so a
    // plan left part-done by a dropped mount still reports work to do, and the UI
    // offers an apply that resumes it.
    let pending = plan
        .operations
        .iter()
        .filter(|op| op.status.is_remaining_work())
        .count();

    // A real apply enforces the gates the dry-run preview only reports: it refuses
    // to mutate when the plan is stale or won't fit, returning a refusal the
    // adapter surfaces without touching a single file. (A dry run never blocks —
    // it reports the same flags so the UI can show them, then runs the engine in
    // its no-op mode to tally the work.)
    if !opts.dry_run {
        // A *started* plan is mid-flight: its own completed operations moved the
        // catalogue (and so the state hash) by design, so that drift is expected
        // and it resumes. The staleness gate only rejects a fresh plan — one whose
        // every operation is still pending — that the catalogue moved underneath.
        // This mirrors the `apply` CLI exactly.
        let plan_started = plan
            .operations
            .iter()
            .any(|op| op.status != crate::plan::OperationStatus::Pending);
        if stale && !plan_started {
            return Ok(Some(refused_report(
                "Plan is stale: the catalogue changed since it was generated. \
                 Run `cat198x plan` to regenerate it."
                    .to_string(),
                stale,
                disk_ok,
                disk_detail,
                total_ops,
                pending,
                total_bytes,
            )));
        }
        if !disk_ok && !opts.skip_space_check {
            let reason = match &disk_detail {
                Some(detail) => format!("Not enough disk space: {detail}"),
                None => "Not enough disk space for the plan's transfers.".to_string(),
            };
            return Ok(Some(refused_report(
                reason,
                stale,
                disk_ok,
                disk_detail,
                total_ops,
                pending,
                total_bytes,
            )));
        }
    }

    // Resolve the real quarantine store (configured path, or <data_dir>/quarantine).
    // On a dry run quarantine never executes, but resolving it costs nothing and
    // keeps the dry-run and real paths identical.
    let quarantine_dir = crate::config::resolve_quarantine_dir(data_dir)?;

    let sources = files::list_sources(conn)?;
    let mut by_kind: BTreeMap<String, usize> = BTreeMap::new();
    let mut done = 0usize;
    let mut bytes_done = 0u64;

    // Build one progress update. The varying fields are passed in; `total_ops`,
    // `total_bytes`, and the worker count are constant for the run.
    #[allow(clippy::too_many_arguments)]
    let mk = |done,
              slot,
              finished,
              verb: String,
              from: String,
              to,
              bytes,
              bytes_done,
              outcome,
              detail,
              reason| {
        ApplyProgress {
            done,
            total: total_ops,
            jobs: APPLY_JOBS,
            slot,
            finished,
            verb,
            from,
            to,
            bytes,
            bytes_done,
            bytes_total: total_bytes,
            outcome,
            detail,
            reason,
        }
    };

    let outcome = apply_plan(
        conn,
        &mut plan,
        &plan_path,
        &sources,
        &ApplyOptions {
            dry_run: opts.dry_run,
            skip_repack: false,
            jobs: APPLY_JOBS,
            quarantine_dir,
        },
        // Every op emits OpStarted then OpFinished, so the consumer is uniform: a
        // start occupies a worker slot (or shows a serial op) without counting; a
        // finish counts the op, banks its bytes on success, frees the slot, and
        // logs the outcome. No per-op state to stash.
        &mut |event| match event {
            ApplyEvent::OpStarted { op, slot, .. } => {
                *by_kind.entry(op.verb.to_string()).or_default() += 1;
                on_progress(mk(
                    done,
                    slot,
                    false,
                    op.verb.to_string(),
                    op.from.clone(),
                    op.to.clone(),
                    op.bytes,
                    bytes_done,
                    None,
                    None,
                    op.reason.clone(),
                ));
            }
            ApplyEvent::OpFinished {
                slot,
                op,
                status,
                detail,
                ..
            } => {
                done += 1;
                let outcome = match status {
                    crate::plan::OperationStatus::Completed => {
                        bytes_done = bytes_done.saturating_add(op.bytes);
                        "ok"
                    }
                    crate::plan::OperationStatus::Refused => "refused",
                    _ => "failed",
                };
                on_progress(mk(
                    done,
                    slot,
                    true,
                    op.verb.to_string(),
                    op.from.clone(),
                    op.to.clone(),
                    op.bytes,
                    bytes_done,
                    Some(outcome.to_string()),
                    detail,
                    op.reason.clone(),
                ));
            }
            _ => {}
        },
    )?;

    Ok(Some(ApplyReport {
        stale,
        disk_ok,
        disk_detail,
        total_ops,
        pending,
        total_bytes,
        by_kind,
        dry_run: opts.dry_run,
        refused: None,
        succeeded: outcome.success_count,
        failed: outcome.error_count,
        refused_ops: outcome.refused_count,
    }))
}

/// Build the report for a real apply a gate refused: nothing ran, so the work
/// tallies are zero, but the gate flags and plan size are carried through so the
/// adapter can explain the refusal.
fn refused_report(
    reason: String,
    stale: bool,
    disk_ok: bool,
    disk_detail: Option<String>,
    total_ops: usize,
    pending: usize,
    total_bytes: u64,
) -> ApplyReport {
    ApplyReport {
        stale,
        disk_ok,
        disk_detail,
        total_ops,
        pending,
        total_bytes,
        by_kind: BTreeMap::new(),
        dry_run: false,
        refused: Some(reason),
        succeeded: 0,
        failed: 0,
        refused_ops: 0,
    }
}
