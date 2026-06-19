//! The shared operation surface.
//!
//! Each Cat198x operation is defined once here as a typed request → response, so
//! the CLI, the `cat198x mcp` server, and the Tauri UI are thin adapters over one
//! audited core rather than parallel implementations. See
//! `decisions/agent-native-surface-and-ui.md`: any action an adapter offers is,
//! by construction, an operation every other adapter can invoke.
//!
//! Functions here are **silent** — they return data and never print — so the
//! adapter owns all output. That is load-bearing for the MCP stdio server, whose
//! stdout is the JSON-RPC transport: a stray `println!` in an operation would
//! corrupt the protocol stream.
//!
//! This is the read-only foundation — collection status, the saved plan-as-diff,
//! and collection/source listings, the operations the first UI slice needs.
//! Mutating operations (apply, reclaim, clean-superseded) join it behind a
//! structured-progress-event design in a follow-up.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;
use rusqlite::Connection;
use serde::Serialize;

use crate::db::dats::{self, MergeMode};
use crate::db::{collections, files};
use crate::plan::executor::check_disk_space;
use crate::plan::{ApplyEvent, ApplyOptions, Plan, apply_plan, compute_state_hash};

/// Completeness of one collection against its active DAT.
#[derive(Debug, Clone, Serialize)]
pub struct CollectionStatus {
    pub name: String,
    /// The active version label, or `None` when the collection has no active version.
    pub version: Option<String>,
    pub total_games: usize,
    pub total_roms: usize,
    pub have_roms: usize,
    pub missing_roms: usize,
    pub completion_pct: f64,
    pub nodump_roms: usize,
    pub bios_sets: usize,
    pub device_sets: usize,
}

/// A registered collection.
#[derive(Debug, Clone, Serialize)]
pub struct CollectionInfo {
    pub name: String,
    pub source_type: String,
    pub has_active_version: bool,
    /// The collection's full library path — the whole tree it sits in, e.g.
    /// `TOSEC/Acorn/Archimedes/Games/[ADF]` — set by recursive `dat add`. Falls
    /// back to the collection name when it has no active version or no recorded
    /// path. A caller groups the catalogue's thousands of collections by walking
    /// this path: the first segment is the set, the rest the manufacturer /
    /// system / category tree beneath it.
    pub node_path: String,
}

/// A registered source directory.
#[derive(Debug, Clone, Serialize)]
pub struct SourceInfo {
    pub id: i64,
    pub path: String,
    pub last_scanned: Option<String>,
}

/// Collection completeness, optionally filtered to one collection by name.
///
/// A collection with no active version is reported with `version: None` and zero
/// counts rather than omitted, so a caller sees every registered collection.
pub fn collection_status(
    conn: &Connection,
    collection: Option<&str>,
    mode: MergeMode,
) -> Result<Vec<CollectionStatus>> {
    let mut out = Vec::new();
    for coll in collections::list_collections(conn)? {
        if let Some(name) = collection
            && coll.name != name
        {
            continue;
        }
        let Some(version) = collections::get_active_version(conn, coll.id)? else {
            out.push(CollectionStatus {
                name: coll.name,
                version: None,
                total_games: 0,
                total_roms: 0,
                have_roms: 0,
                missing_roms: 0,
                completion_pct: 0.0,
                nodump_roms: 0,
                bios_sets: 0,
                device_sets: 0,
            });
            continue;
        };
        // exclude_mechanical by default, matching the `status` command.
        let stats = dats::calculate_merge_mode_stats(conn, version.id, mode, true)?;
        let completion_pct = if stats.total_roms > 0 {
            (stats.have_roms as f64 / stats.total_roms as f64) * 100.0
        } else {
            0.0
        };
        out.push(CollectionStatus {
            name: coll.name,
            version: Some(version.version),
            total_games: stats.total_games,
            total_roms: stats.total_roms,
            have_roms: stats.have_roms,
            missing_roms: stats.total_roms.saturating_sub(stats.have_roms),
            completion_pct,
            nodump_roms: stats.nodump_roms,
            bios_sets: stats.bios_sets,
            device_sets: stats.device_sets,
        });
    }
    Ok(out)
}

/// Every registered collection, with whether it has an active version and the
/// set it rolls up under.
pub fn list_collections(conn: &Connection) -> Result<Vec<CollectionInfo>> {
    let mut out = Vec::new();
    for coll in collections::list_collections(conn)? {
        let version = collections::get_active_version(conn, coll.id)?;
        // Full library path (the tree set by recursive `dat add`); fall back to
        // the collection name when there's no active version or recorded path.
        let node_path = match &version {
            Some(v) => dats::primary_node_path(conn, v.id)?.unwrap_or_else(|| coll.name.clone()),
            None => coll.name.clone(),
        };
        out.push(CollectionInfo {
            name: coll.name,
            source_type: coll.source_type,
            has_active_version: version.is_some(),
            node_path,
        });
    }
    Ok(out)
}

/// Every registered source directory.
pub fn list_sources(conn: &Connection) -> Result<Vec<SourceInfo>> {
    Ok(files::list_sources(conn)?
        .into_iter()
        .map(|s| SourceInfo {
            id: s.id,
            path: s.path,
            last_scanned: s.last_scanned,
        })
        .collect())
}

/// The most recent saved plan — the plan-as-diff the UI renders — or `None` when
/// no plan has been generated. Reads the newest plan JSON under
/// `<data_dir>/objects/plans`; the plan already *is* the diff, so no reconcile
/// model is needed (see the decision record).
pub fn latest_plan(data_dir: &Path) -> Result<Option<Plan>> {
    match newest_plan_file(data_dir)? {
        Some(path) => {
            let contents = std::fs::read_to_string(&path)?;
            Ok(Some(serde_json::from_str(&contents)?))
        }
        None => Ok(None),
    }
}

/// The path of the most recently written plan under `<data_dir>/objects/plans`,
/// or `None` when none exists. Shared by [`latest_plan`] and [`apply`] — the
/// latter needs the path to drive the apply engine.
fn newest_plan_file(data_dir: &Path) -> Result<Option<std::path::PathBuf>> {
    let plans_dir = data_dir.join("objects/plans");
    if !plans_dir.is_dir() {
        return Ok(None);
    }
    let mut latest: Option<(std::path::PathBuf, std::time::SystemTime)> = None;
    for entry in std::fs::read_dir(&plans_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "json")
            && let Ok(modified) = entry.metadata().and_then(|m| m.modified())
        {
            let newer = match &latest {
                Some((_, prev)) => modified > *prev,
                None => true,
            };
            if newer {
                latest = Some((path, modified));
            }
        }
    }
    Ok(latest.map(|(path, _)| path))
}

/// One collection's pending reorganise work, from the saved plan.
#[derive(Debug, Clone, Serialize)]
pub struct PendingItem {
    pub collection: String,
    /// The collection's library path, for rolling up the tree.
    pub node_path: String,
    /// Operations the plan would perform here (copy/move/repack/relocate).
    pub to_write: usize,
    /// Bytes the plan would transfer here.
    pub bytes: u64,
}

/// The reorganise work the saved plan implies, per collection, plus whether the
/// plan has gone stale against the current catalogue.
#[derive(Debug, Clone, Serialize)]
pub struct PendingWork {
    /// `true` when the catalogue has changed since the plan was generated, so the
    /// numbers may be out of date and the plan should be re-run.
    pub stale: bool,
    /// When the underlying plan was generated.
    pub plan_created_at: String,
    /// Collections with at least one pending operation.
    pub items: Vec<PendingItem>,
}

/// The pending reorganise work from the most recent saved plan, or `None` when
/// no plan has been generated.
///
/// This is a *read* of the saved plan's per-collection breakdown — it does not
/// run the planner. The `stale` flag (the saved plan's state hash vs the current
/// catalogue's) tells a caller when those numbers predate the catalogue and the
/// plan should be regenerated. Clean-up work (removals, husks) is not included
/// here; only the additive/reorganise operations the plan carries.
pub fn pending_work(conn: &Connection, data_dir: &Path) -> Result<Option<PendingWork>> {
    let Some(plan) = latest_plan(data_dir)? else {
        return Ok(None);
    };
    let stale = compute_state_hash(conn)? != plan.state_hash;
    let items = plan
        .per_collection
        .iter()
        .filter(|c| c.to_write > 0)
        .map(|c| PendingItem {
            collection: c.name.clone(),
            node_path: c.node_path.clone(),
            to_write: c.to_write,
            bytes: c.bytes,
        })
        .collect();
    Ok(Some(PendingWork {
        stale,
        plan_created_at: plan.created_at,
        items,
    }))
}

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

/// One operation's worth of progress, reported as a plan is applied.
#[derive(Debug, Clone, Serialize)]
pub struct ApplyProgress {
    /// Operations started so far (monotonic; counts each op as it begins).
    pub done: usize,
    /// Total operations in the plan.
    pub total: usize,
    /// The verb of the operation just started (COPY/MOVE/RELOCATE/…).
    pub verb: String,
    /// The operation's source (or, for a repack, the destination archive).
    pub from: String,
    /// The operation's destination, when it has a distinct one.
    pub to: Option<String>,
    /// This operation's size in bytes (a delete has none, so `0`).
    pub bytes: u64,
    /// Cumulative bytes across every operation started so far — a running total
    /// the adapter can show climbing as the apply proceeds.
    pub bytes_done: u64,
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
    let outcome = apply_plan(
        conn,
        &mut plan,
        &plan_path,
        &sources,
        &ApplyOptions {
            dry_run: opts.dry_run,
            skip_repack: false,
            // Placements and repacks are latency-bound over a network mount, so a
            // handful of workers overlaps the round-trips (see
            // decisions/concurrent-apply.md). The library destination is one
            // volume, so this is bounded by the mount, not the CPU.
            jobs: 6,
            quarantine_dir,
        },
        &mut |event| {
            if let ApplyEvent::OpStarted { op, .. } = event {
                *by_kind.entry(op.verb.to_string()).or_default() += 1;
                done += 1;
                bytes_done = bytes_done.saturating_add(op.bytes);
                on_progress(ApplyProgress {
                    done,
                    total: total_ops,
                    verb: op.verb.to_string(),
                    from: op.from.clone(),
                    to: op.to.clone(),
                    bytes: op.bytes,
                    bytes_done,
                });
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Database, collections, dats};

    #[test]
    fn collection_status_reports_completeness_and_no_active_version() {
        let db = Database::open_in_memory().unwrap();
        let conn = db.conn();

        // One collection with an active version holding one of two ROMs…
        let c1 = collections::create_collection(conn, "NES", "nointro").unwrap();
        let v1 = collections::add_version(conn, c1, "v1", "/d/nes.dat", true).unwrap();
        let node = dats::create_node(conn, v1, None, "NES", "dat", "NES").unwrap();
        let g = dats::create_game(conn, node, "Game", None, None, false, false, false).unwrap();
        dats::create_rom(conn, g, "a.nes", 10, Some("AAA"), None, None, "good", None).unwrap();
        dats::create_rom(conn, g, "b.nes", 10, Some("BBB"), None, None, "good", None).unwrap();
        conn.execute("INSERT INTO files (sha1, size) VALUES ('AAA', 10)", [])
            .unwrap();

        // …and one collection with no active version.
        collections::create_collection(conn, "Empty", "nointro").unwrap();

        let all = collection_status(conn, None, MergeMode::NonMerged).unwrap();
        assert_eq!(all.len(), 2);

        let nes = all.iter().find(|s| s.name == "NES").unwrap();
        assert_eq!(nes.version.as_deref(), Some("v1"));
        assert_eq!(nes.total_roms, 2);
        assert_eq!(nes.have_roms, 1);
        assert_eq!(nes.missing_roms, 1);
        assert!((nes.completion_pct - 50.0).abs() < 1e-9);

        let empty = all.iter().find(|s| s.name == "Empty").unwrap();
        assert_eq!(empty.version, None);
        assert_eq!(empty.total_roms, 0);

        // Filtering returns just the requested collection.
        let just_nes = collection_status(conn, Some("NES"), MergeMode::NonMerged).unwrap();
        assert_eq!(just_nes.len(), 1);
        assert_eq!(just_nes[0].name, "NES");
    }

    #[test]
    fn list_collections_reports_the_full_library_path() {
        let db = Database::open_in_memory().unwrap();
        let conn = db.conn();

        // A collection whose library path nests under a set ("TOSEC-PIX/…").
        let c = collections::create_collection(conn, "Acorn BBC - Magazines", "tosec").unwrap();
        let v = collections::add_version(conn, c, "v1", "/d/bbc.dat", true).unwrap();
        dats::create_node(
            conn,
            v,
            None,
            "Magazines",
            "dat",
            "TOSEC-PIX/Acorn/BBC/Magazines",
        )
        .unwrap();

        // A collection with no active version falls back to its own name.
        collections::create_collection(conn, "Loose Coll", "tosec").unwrap();

        let cols = list_collections(conn).unwrap();
        let bbc = cols
            .iter()
            .find(|c| c.name == "Acorn BBC - Magazines")
            .unwrap();
        assert_eq!(
            bbc.node_path, "TOSEC-PIX/Acorn/BBC/Magazines",
            "the full library path is reported, not just the set"
        );
        assert!(bbc.has_active_version);

        let loose = cols.iter().find(|c| c.name == "Loose Coll").unwrap();
        assert_eq!(
            loose.node_path, "Loose Coll",
            "no active version → path is the name"
        );
        assert!(!loose.has_active_version);
    }

    #[test]
    fn latest_plan_is_none_without_a_saved_plan() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(latest_plan(tmp.path()).unwrap().is_none());
    }

    #[test]
    fn latest_plan_reads_the_newest_saved_plan() {
        let tmp = tempfile::tempdir().unwrap();
        let plans = tmp.path().join("objects/plans");
        std::fs::create_dir_all(&plans).unwrap();

        // A round-tripped Plan deserializes back through latest_plan.
        let plan = Plan::new("deadbeefdeadbeef".to_string());
        let json = serde_json::to_string_pretty(&plan).unwrap();
        std::fs::write(plans.join("deadbeefdeadbeef.json"), json).unwrap();

        let loaded = latest_plan(tmp.path()).unwrap().expect("a plan");
        assert_eq!(loaded.state_hash, "deadbeefdeadbeef");
    }

    #[test]
    fn pending_work_rolls_up_the_saved_plans_per_collection() {
        let db = Database::open_in_memory().unwrap();
        let conn = db.conn();
        let tmp = tempfile::tempdir().unwrap();
        let plans = tmp.path().join("objects/plans");
        std::fs::create_dir_all(&plans).unwrap();

        // A plan whose hash matches the (empty) catalogue → not stale.
        let current = compute_state_hash(conn).unwrap();
        let mut plan = Plan::new(current);
        plan.per_collection = vec![
            crate::plan::CollectionPlanStat {
                name: "A".into(),
                node_path: "TOSEC/A".into(),
                to_write: 3,
                already_correct: 0,
                bytes: 30,
            },
            crate::plan::CollectionPlanStat {
                name: "B".into(),
                node_path: "TOSEC/B".into(),
                to_write: 0, // fully placed — excluded
                already_correct: 5,
                bytes: 0,
            },
        ];
        std::fs::write(plans.join("p.json"), serde_json::to_string(&plan).unwrap()).unwrap();

        let pw = pending_work(conn, tmp.path())
            .unwrap()
            .expect("pending work");
        assert!(!pw.stale, "plan hash matches the catalogue");
        assert_eq!(pw.items.len(), 1, "only collections with pending work");
        assert_eq!(pw.items[0].collection, "A");
        assert_eq!(pw.items[0].node_path, "TOSEC/A");
        assert_eq!(pw.items[0].to_write, 3);
        assert_eq!(pw.items[0].bytes, 30);
    }

    #[test]
    fn pending_work_is_none_without_a_plan() {
        let db = Database::open_in_memory().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        assert!(pending_work(db.conn(), tmp.path()).unwrap().is_none());
    }

    #[test]
    fn apply_dry_run_reports_the_plan_without_mutating() {
        let db = Database::open_in_memory().unwrap();
        let conn = db.conn();
        let tmp = tempfile::tempdir().unwrap();
        let plans = tmp.path().join("objects/plans");
        std::fs::create_dir_all(&plans).unwrap();

        let mut plan = crate::plan::Plan::new(compute_state_hash(conn).unwrap());
        let dest = tmp.path().join("a.rom");
        plan.add_copy(
            crate::plan::SourceRef {
                path: "/staging/a.rom".into(),
                archive_path: None,
                sha1: "AAA".into(),
                entry_name: None,
            },
            dest.to_string_lossy().into_owned(),
            10,
        );
        plan.add_delete("/staging/b.rom".into());
        std::fs::write(plans.join("p.json"), serde_json::to_string(&plan).unwrap()).unwrap();

        let report = apply(conn, tmp.path(), ApplyRunOptions::preview())
            .unwrap()
            .expect("a plan");
        assert!(report.dry_run);
        assert!(report.refused.is_none(), "a dry run never refuses");
        assert!(!report.stale, "plan hash matches the (empty) catalogue");
        assert_eq!(report.total_ops, 2);
        assert_eq!(report.pending, 2);
        assert_eq!(
            report.total_bytes, 10,
            "the copy's bytes, from the plan summary"
        );
        // Tallied from the engine's own progress events.
        assert_eq!(report.by_kind.get("COPY"), Some(&1));
        assert_eq!(report.by_kind.get("DELETE"), Some(&1));
        assert_eq!(report.failed, 0);
        assert!(!dest.exists(), "a dry run mutates nothing");
    }

    #[test]
    fn apply_is_none_without_a_plan() {
        let db = Database::open_in_memory().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        assert!(
            apply(db.conn(), tmp.path(), ApplyRunOptions::preview())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn real_apply_moves_the_file_and_writes_a_journal() {
        let db = Database::open_in_memory().unwrap();
        let conn = db.conn();
        let tmp = tempfile::tempdir().unwrap();
        let plans = tmp.path().join("objects/plans");
        std::fs::create_dir_all(&plans).unwrap();

        // A real source file whose content hashes to the plan's recorded sha1
        // (sha1("hello")), so verify-before-delete passes on the move.
        let src = tmp.path().join("staging/a.rom");
        std::fs::create_dir_all(src.parent().unwrap()).unwrap();
        std::fs::write(&src, b"hello").unwrap();
        let dest = tmp.path().join("lib/a.rom");

        let mut plan = crate::plan::Plan::new(compute_state_hash(conn).unwrap());
        plan.add_move(
            crate::plan::SourceRef {
                path: src.to_string_lossy().into_owned(),
                archive_path: None,
                sha1: "aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d".into(),
                entry_name: None,
            },
            dest.to_string_lossy().into_owned(),
            5,
        );
        std::fs::write(plans.join("p.json"), serde_json::to_string(&plan).unwrap()).unwrap();

        let report = apply(
            conn,
            tmp.path(),
            ApplyRunOptions {
                dry_run: false,
                skip_space_check: false,
            },
        )
        .unwrap()
        .expect("a plan");

        assert!(
            report.refused.is_none(),
            "a fresh in-fit plan is not refused"
        );
        assert_eq!(report.succeeded, 1);
        assert_eq!(report.failed, 0);
        assert_eq!(std::fs::read(&dest).unwrap(), b"hello", "moved into place");
        assert!(!src.exists(), "a move frees the source");
        // The rollback journal lands under objects/logs alongside objects/plans.
        let logs = tmp.path().join("objects/logs");
        let journal_written = logs.is_dir()
            && std::fs::read_dir(&logs)
                .unwrap()
                .any(|e| e.unwrap().path().extension().is_some_and(|x| x == "json"));
        assert!(journal_written, "a real apply writes a rollback journal");
    }

    #[test]
    fn real_apply_refuses_a_stale_plan_without_touching_anything() {
        let db = Database::open_in_memory().unwrap();
        let conn = db.conn();
        let tmp = tempfile::tempdir().unwrap();
        let plans = tmp.path().join("objects/plans");
        std::fs::create_dir_all(&plans).unwrap();

        let src = tmp.path().join("staging/a.rom");
        std::fs::create_dir_all(src.parent().unwrap()).unwrap();
        std::fs::write(&src, b"hello").unwrap();
        let dest = tmp.path().join("lib/a.rom");

        // A plan whose state hash does NOT match the current catalogue, with every
        // operation still pending → stale and not started → must be refused.
        let mut plan = crate::plan::Plan::new("stale-hash-that-will-not-match".into());
        plan.add_move(
            crate::plan::SourceRef {
                path: src.to_string_lossy().into_owned(),
                archive_path: None,
                sha1: "aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d".into(),
                entry_name: None,
            },
            dest.to_string_lossy().into_owned(),
            5,
        );
        std::fs::write(plans.join("p.json"), serde_json::to_string(&plan).unwrap()).unwrap();

        let report = apply(
            conn,
            tmp.path(),
            ApplyRunOptions {
                dry_run: false,
                skip_space_check: false,
            },
        )
        .unwrap()
        .expect("a plan");

        assert!(report.stale, "the plan hash does not match the catalogue");
        assert!(report.refused.is_some(), "a stale fresh plan is refused");
        assert_eq!(report.succeeded, 0);
        assert_eq!(report.failed, 0);
        // Nothing moved: the source is untouched, the destination never created,
        // and no rollback journal was written.
        assert!(src.exists(), "the source is untouched");
        assert!(!dest.exists(), "the destination is never created");
        assert!(
            !tmp.path().join("objects/logs").exists(),
            "no journal — nothing ran"
        );
    }

    #[test]
    fn apply_streaming_reports_progress_per_operation() {
        let db = Database::open_in_memory().unwrap();
        let conn = db.conn();
        let tmp = tempfile::tempdir().unwrap();
        let plans = tmp.path().join("objects/plans");
        std::fs::create_dir_all(&plans).unwrap();

        let mut plan = crate::plan::Plan::new(compute_state_hash(conn).unwrap());
        plan.add_copy(
            crate::plan::SourceRef {
                path: "/staging/a.rom".into(),
                archive_path: None,
                sha1: "AAA".into(),
                entry_name: None,
            },
            tmp.path().join("a.rom").to_string_lossy().into_owned(),
            10,
        );
        plan.add_delete("/staging/b.rom".into());
        std::fs::write(plans.join("p.json"), serde_json::to_string(&plan).unwrap()).unwrap();

        let mut progress = Vec::new();
        apply_streaming(conn, tmp.path(), ApplyRunOptions::preview(), &mut |p| {
            progress.push(p)
        })
        .unwrap()
        .expect("a plan");

        // One progress callback per operation, monotonic, total carried through.
        assert_eq!(progress.len(), 2);
        assert_eq!((progress[0].done, progress[0].total), (1, 2));
        assert_eq!((progress[1].done, progress[1].total), (2, 2));
        assert_eq!(progress[0].verb, "COPY");
        assert_eq!(progress[1].verb, "DELETE");

        // The copy carries its paths and size; the running byte total accrues it.
        assert_eq!(progress[0].from, "/staging/a.rom");
        assert_eq!(
            progress[0].to.as_deref(),
            Some(tmp.path().join("a.rom").to_string_lossy().as_ref())
        );
        assert_eq!(progress[0].bytes, 10);
        assert_eq!(progress[0].bytes_done, 10);

        // The delete has a path but no destination and no bytes, so the running
        // total holds steady.
        assert_eq!(progress[1].from, "/staging/b.rom");
        assert_eq!(progress[1].to, None);
        assert_eq!(progress[1].bytes, 0);
        assert_eq!(progress[1].bytes_done, 10);
    }
}
