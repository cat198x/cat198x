//! Plan application: the orchestration that carries out a plan's operations.
//!
//! This is the loop that walks a plan's operations -- copy / move / relocate /
//! repack / delete / quarantine -- driving the verified file primitives in
//! [`crate::plan::executor`], journaling each to the rollback log, and keeping
//! the catalogue in step so a re-plan converges without a re-scan. Repacks are
//! batched and run concurrently (they're latency-bound over a network mount).
//!
//! It holds no output concerns: progress is reported through an [`ApplyEvent`]
//! callback, so the `apply` CLI prints, the UI streams a progress bar, and the
//! MCP surface stays silent -- each adapter decides how to render the same run.
//! That keeps this engine drivable from every 198x surface, exactly as the
//! safety model requires ("the execution engine lives in the library").

use std::path::Path;

use anyhow::Result;
use rusqlite::Connection;

use crate::db::files::Source;
use crate::plan::Plan;

mod catalogue;
mod persistence;
mod runner;
mod serial;
mod types;

use runner::ApplyRunner;
pub use types::{ApplyEvent, ApplyOptions, ApplyOutcome, OpView};

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
    ApplyRunner::new(conn, plan, plan_path, sources, opts, on_event).run()
}

#[cfg(test)]
#[path = "apply_tests.rs"]
mod tests;
