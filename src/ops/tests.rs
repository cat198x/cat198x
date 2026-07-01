use super::*;
use crate::db::dats::MergeMode;
use crate::db::{Database, collections, dats};
use crate::plan::{Plan, compute_state_hash};

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
    plan.add_delete(
        "/staging/b.rom".into(),
        "exact duplicate — kept /lib/b.rom".into(),
    );
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
    plan.add_delete(
        "/staging/b.rom".into(),
        "exact duplicate — kept /lib/b.rom".into(),
    );
    std::fs::write(plans.join("p.json"), serde_json::to_string(&plan).unwrap()).unwrap();

    let mut progress = Vec::new();
    apply_streaming(conn, tmp.path(), ApplyRunOptions::preview(), &mut |p| {
        progress.push(p)
    })
    .unwrap()
    .expect("a plan");

    // Every op emits a start then a finish, both slotless on a dry run. The
    // finishes carry the count, the running bytes, and an "ok" log outcome.
    let finishes: Vec<_> = progress.iter().filter(|p| p.finished).collect();
    assert_eq!(finishes.len(), 2, "one finish per operation");
    assert!(
        progress
            .iter()
            .all(|p| p.slot.is_none() && p.bytes_total == 10)
    );

    assert_eq!((finishes[0].done, finishes[0].total), (1, 2));
    assert_eq!(finishes[0].verb, "COPY");
    assert_eq!(finishes[0].from, "/staging/a.rom");
    assert_eq!(
        finishes[0].to.as_deref(),
        Some(tmp.path().join("a.rom").to_string_lossy().as_ref())
    );
    assert_eq!(finishes[0].bytes, 10);
    assert_eq!(finishes[0].bytes_done, 10, "the copy's bytes are banked");
    assert_eq!(finishes[0].outcome.as_deref(), Some("ok"));

    // The delete is logged too (no longer the silent gap), with no bytes.
    assert_eq!((finishes[1].done, finishes[1].total), (2, 2));
    assert_eq!(finishes[1].verb, "DELETE");
    assert_eq!(finishes[1].from, "/staging/b.rom");
    assert_eq!(finishes[1].to, None);
    assert_eq!(finishes[1].bytes, 0);
    assert_eq!(finishes[1].bytes_done, 10);
    assert_eq!(finishes[1].outcome.as_deref(), Some("ok"));
}

// A real apply runs placements concurrently: each reports a start (occupying a
// worker slot, bytes not yet counted) and a finish (freeing the slot, bytes
// banked), so the processed total never runs ahead of the disk.
#[test]
fn real_apply_reports_slotted_start_and_finish_per_placement() {
    let db = Database::open_in_memory().unwrap();
    let conn = db.conn();
    let tmp = tempfile::tempdir().unwrap();
    let plans = tmp.path().join("objects/plans");
    std::fs::create_dir_all(&plans).unwrap();

    let mut plan = crate::plan::Plan::new(compute_state_hash(conn).unwrap());
    let src = tmp.path().join("a.rom");
    std::fs::write(&src, b"hello").unwrap(); // sha1 aaf4c6…, 5 bytes
    plan.add_move(
        crate::plan::SourceRef {
            path: src.to_string_lossy().into_owned(),
            archive_path: None,
            sha1: "aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d".into(),
            entry_name: None,
        },
        tmp.path().join("lib/a.rom").to_string_lossy().into_owned(),
        5,
    );
    std::fs::write(plans.join("p.json"), serde_json::to_string(&plan).unwrap()).unwrap();

    let mut progress = Vec::new();
    apply_streaming(
        conn,
        tmp.path(),
        ApplyRunOptions {
            dry_run: false,
            skip_space_check: false,
        },
        &mut |p| progress.push(p),
    )
    .unwrap()
    .expect("a plan");

    // A start (slotted, processed still 0) then a finish (slot freed, bytes
    // banked) — the move's 5 bytes count only once it has completed.
    let start = progress.iter().find(|p| !p.finished).expect("a start");
    assert!(start.slot.is_some(), "a placement runs in a worker slot");
    assert_eq!(start.verb, "MOVE");
    assert_eq!(start.bytes_done, 0, "bytes not counted while in flight");

    let finish = progress
        .iter()
        .rev()
        .find(|p| p.finished)
        .expect("a finish");
    assert!(finish.slot.is_some());
    assert_eq!(finish.bytes_done, 5, "bytes banked on completion");
    assert_eq!(finish.done, 1);
    // The finish is also a log line naming the op.
    assert_eq!(finish.outcome.as_deref(), Some("ok"));
    assert_eq!(finish.verb, "MOVE");
}

// A failed placement emits a "failed" log line that names the op and carries
// the reason, and its bytes never join the processed total.
#[test]
fn a_failed_placement_emits_a_failed_log_line() {
    let db = Database::open_in_memory().unwrap();
    let conn = db.conn();
    let tmp = tempfile::tempdir().unwrap();
    let plans = tmp.path().join("objects/plans");
    std::fs::create_dir_all(&plans).unwrap();

    let mut plan = crate::plan::Plan::new(compute_state_hash(conn).unwrap());
    // A move whose source does not exist → fails on the copy step.
    plan.add_move(
        crate::plan::SourceRef {
            path: tmp
                .path()
                .join("missing.rom")
                .to_string_lossy()
                .into_owned(),
            archive_path: None,
            sha1: "aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d".into(),
            entry_name: None,
        },
        tmp.path().join("lib/x.rom").to_string_lossy().into_owned(),
        5,
    );
    std::fs::write(plans.join("p.json"), serde_json::to_string(&plan).unwrap()).unwrap();

    let mut progress = Vec::new();
    apply_streaming(
        conn,
        tmp.path(),
        ApplyRunOptions {
            dry_run: false,
            skip_space_check: false,
        },
        &mut |p| progress.push(p),
    )
    .unwrap()
    .expect("a plan");

    let log = progress
        .iter()
        .find(|p| p.outcome.is_some())
        .expect("a log line");
    assert_eq!(log.outcome.as_deref(), Some("failed"));
    assert_eq!(log.verb, "MOVE");
    assert!(log.detail.is_some(), "a failure carries its reason");
    assert_eq!(
        progress.last().unwrap().bytes_done,
        0,
        "a failed op never counts toward processed bytes"
    );
}
