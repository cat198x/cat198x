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

use std::path::Path;

use anyhow::Result;
use rusqlite::Connection;

use crate::db::files::Source;
use crate::plan::executor::{PlacementJob, PlacementKind, RepackJob};
use crate::plan::{OperationKind, OperationLog, Plan};

mod batches;
mod catalogue;
mod persistence;
mod serial;
mod types;

use batches::{flush_placement_batch, flush_repack_batch};
use persistence::persist_apply_run;
use serial::run_serial_operation;
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
                    OperationKind::Copy {
                        source,
                        dest,
                        placement,
                        ..
                    } => {
                        placement_batch.push(PlacementJob {
                            plan_index: i,
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
                        placement_batch.push(PlacementJob {
                            plan_index: i,
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

        let counts = run_serial_operation(
            i,
            total_ops,
            &mut plan.operations[i],
            conn,
            sources,
            opts,
            &mut op_log,
            on_event,
        );
        success_count += counts.success;
        error_count += counts.error;
        refused_count += counts.refused;
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

    let log_path = persist_apply_run(plan, plan_path, opts.dry_run, op_log)?;

    Ok(ApplyOutcome {
        success_count,
        error_count,
        refused_count,
        log_path,
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::db::Database;
    use crate::plan::executor::delete_has_surviving_copy;
    use crate::plan::{OperationStatus, Plan, SourceRef};

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

    // The safety guarantee behind draining a consume staging container: the
    // verify-before-delete net treats a container at the entry level — it removes
    // the container only once *every* entry it held is confirmed surviving (here,
    // in the rebuilt library archive). Drop one survivor and the net refuses,
    // proving a container another game still needs is never lost.
    #[test]
    fn draining_a_container_is_permitted_only_when_every_entry_survives() {
        use crate::db::files::{Disposition, Source};

        let db = Database::open_in_memory().unwrap();
        let conn = db.conn();
        let tmp = tempfile::tempdir().unwrap();

        // The rebuilt library archive exists on disk (the net checks the survivor
        // physically exists); the staging container need not — it's the candidate.
        let lib = tmp.path().join("lib");
        std::fs::create_dir_all(&lib).unwrap();
        let game_zip = lib.join("game.zip");
        std::fs::write(&game_zip, b"rebuilt archive").unwrap();

        conn.execute(
            "INSERT INTO files (sha1, size) VALUES ('AAA',10),('BBB',10)",
            [],
        )
        .unwrap();
        // The staging container holds two entries; the rebuilt archive holds the
        // same two (as a repack's catalogue sync would record).
        conn.execute(
            "INSERT INTO sources (id, path, case_sensitive, disposition)
             VALUES (1, '/tosort', 0, 'consume'), (2, ?1, 0, 'preserve')",
            [lib.to_str().unwrap()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO file_locations (sha1, source_id, path, archive_path) VALUES
                ('AAA', 1, 'bundle.zip', 'a.bin'),
                ('BBB', 1, 'bundle.zip', 'b.cue'),
                ('AAA', 2, 'game.zip', 'a.bin'),
                ('BBB', 2, 'game.zip', 'b.cue')",
            [],
        )
        .unwrap();

        let now = "2026-01-01".to_string();
        let sources = vec![
            Source {
                id: 1,
                path: "/tosort".into(),
                case_sensitive: false,
                added_at: now.clone(),
                last_scanned: None,
                disposition: Disposition::Consume,
            },
            Source {
                id: 2,
                path: lib.to_str().unwrap().into(),
                case_sensitive: false,
                added_at: now,
                last_scanned: None,
                disposition: Disposition::Preserve,
            },
        ];

        // Both entries survive in the rebuilt archive on disk → the drain is safe.
        assert!(
            delete_has_surviving_copy(conn, &sources, "/tosort/bundle.zip").unwrap(),
            "every entry survives in the library archive, so the container drains"
        );

        // Drop one entry's surviving location — now b.cue lives only in the
        // container. The net must refuse: removing it would lose that content.
        conn.execute(
            "DELETE FROM file_locations WHERE sha1='BBB' AND source_id=2",
            [],
        )
        .unwrap();
        assert!(
            !delete_has_surviving_copy(conn, &sources, "/tosort/bundle.zip").unwrap(),
            "an entry held only by the container blocks the drain"
        );
    }
}
