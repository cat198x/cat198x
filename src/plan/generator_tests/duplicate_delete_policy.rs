use super::*;

#[test]
fn dedup_never_deletes_a_placed_library_copy() {
    let db = setup_db();
    let conn = db.conn();
    // One single-ROM game whose content is held at three places: the canonical
    // destination (already correct), a *second* library path — a sibling
    // placement, as a merged-set clone would have (one DAT game, so not
    // flagged as shared content) — and a stray copy under ToSort.
    let node = add_collection_node(conn, "Merge Coll", "mame", "/dats/m.dat", "SET/Sys");
    add_rom(conn, node, "Game", "shared.bin", "SSS");
    add_file(conn, "SSS", 10);
    add_source(conn, 1, "/lib/ROMs/SET/Sys", Some(Disposition::Preserve));
    add_source(
        conn,
        2,
        "/lib/ROMs/SET/Sys/clone",
        Some(Disposition::Preserve),
    );
    add_source(conn, 3, "/lib/ToSort/SET", Some(Disposition::Consume));
    add_location(conn, "SSS", 1, "shared.bin", None);
    add_location(conn, "SSS", 2, "shared.bin", None);
    add_location(conn, "SSS", 3, "shared.bin", None);

    let plan = plan_with_default_dest(conn, OutputFormat::Loose);

    // Only the ToSort stray is deleted; both library copies are left in place.
    let deleted: Vec<_> = plan
        .operations
        .iter()
        .filter_map(|op| match &op.kind {
            OperationKind::Delete { path, .. } => Some(path.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        deleted,
        vec!["/lib/ToSort/SET/shared.bin".to_string()],
        "a placed library copy must never be deleted as a duplicate"
    );
}

#[test]
fn loose_duplicate_is_deleted_canonical_kept_in_place() {
    let db = setup_db();
    let conn = db.conn();
    setup_dup_fixture(conn, false);

    let plan = plan_with_default_dest(conn, OutputFormat::Loose);

    // The library copy at /lib/ROMs/SET/Sys/game.rom is already correct, so
    // no move; the ToSort copy is an exact-content duplicate and is deleted
    // (its bytes are preserved by the canonical copy).
    assert_eq!(
        plan.summary.move_count, 0,
        "canonical copy already in place"
    );
    assert_eq!(plan.summary.copy_count, 0);
    assert_eq!(
        plan.summary.quarantine_count, 0,
        "dups are deleted, not quarantined"
    );
    assert_eq!(plan.summary.delete_count, 1, "ToSort dup deleted");
    let deleted: Vec<_> = plan
        .operations
        .iter()
        .filter_map(|op| match &op.kind {
            OperationKind::Delete { path, reason, .. } => Some((path.clone(), reason.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(deleted.len(), 1);
    assert_eq!(deleted[0].0, "/lib/ToSort/SET/Sys/game.rom");
    // The delete records why it is safe: the canonical copy it keeps. Every
    // planner delete is a dedup, so the reason names a surviving path.
    assert!(
        deleted[0].1.starts_with("exact duplicate — kept ") && deleted[0].1.contains("game.rom"),
        "reason names the kept copy: {:?}",
        deleted[0].1
    );
}

#[test]
fn loose_duplicate_left_untouched_for_a_preserve_source() {
    let db = setup_db();
    let conn = db.conn();
    setup_dup_fixture(conn, false);
    // A preserve source never loses content, so its exact-content duplicate
    // is left in place rather than deleted.
    files::set_source_disposition(conn, "/lib/ToSort/SET", Disposition::Preserve).unwrap();

    let plan = plan_with_default_dest(conn, OutputFormat::Loose);
    assert_eq!(plan.summary.delete_count, 0, "copy mode deletes nothing");
    assert_eq!(plan.summary.quarantine_count, 0);
}

#[test]
fn archive_duplicate_container_is_deleted() {
    let db = setup_db();
    let conn = db.conn();
    setup_dup_fixture(conn, true);

    let plan = plan_with_default_dest(conn, OutputFormat::Loose);

    // The complete archive already sits at /lib/ROMs/SET/Sys/Game.zip, so
    // nothing is built; the ToSort .zip is a duplicate container and deleted.
    assert_eq!(
        plan.summary.repack_count, 0,
        "canonical archive already at dest"
    );
    assert_eq!(plan.summary.delete_count, 1);
    let deleted: Vec<_> = plan
        .operations
        .iter()
        .filter_map(|op| match &op.kind {
            OperationKind::Delete { path, .. } => Some(path.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(deleted, vec!["/lib/ToSort/SET/Sys/Game.zip".to_string()]);
}

#[test]
fn preserve_loose_is_consolidated_into_an_archive_in_the_same_tree() {
    let db = setup_db();
    let conn = db.conn();
    let coll = collections::create_collection(conn, "Lib Coll", "tosec").unwrap();
    let vid = collections::add_version(conn, coll, "v1", "/dats/l.dat", true).unwrap();
    let node = dats::create_node(conn, vid, None, "Lib Coll", "dat", "SET/Sys").unwrap();
    let game = dats::create_game(conn, node, "Game", None, None, false, false, false).unwrap();
    dats::create_rom(
        conn,
        game,
        "game.rom",
        10,
        Some("AAA"),
        None,
        None,
        "good",
        None,
    )
    .unwrap();
    conn.execute("INSERT INTO files (sha1, size) VALUES ('AAA', 10)", [])
        .unwrap();

    // The library destination is itself a preserve source, and the loose ROM
    // already lives inside it. Consolidating it into Game.zip keeps the content
    // in the same tree, so the loose original may be consumed.
    conn.execute(
        "INSERT INTO sources (id, path, case_sensitive, disposition)
     VALUES (101, '/lib/ROMs/SET/Sys', 0, 'preserve')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO file_locations (sha1, source_id, path, archive_path)
     VALUES ('AAA', 101, 'game.rom', NULL)",
        [],
    )
    .unwrap();
    db_config::set_output_format(conn, "SET", "zip").unwrap();

    let plan = generate_plan_filtered(
        conn,
        &PlanOptions {
            default_dest: Some("/lib/ROMs".to_string()),
            default_format: OutputFormat::Loose, // overridden to zip per-set
            ..Default::default()
        },
    )
    .unwrap();

    // One repack builds the canonical Game.zip, and because the archive lands
    // in the same preserve tree, it consumes the loose source (move_sources) —
    // the loose original is not left behind. No separate delete is emitted.
    assert_eq!(plan.summary.repack_count, 1, "the loose ROM is archived");
    assert_eq!(
        plan.summary.delete_count, 0,
        "consumed by the repack, not deleted"
    );
    let consumes_loose = plan.operations.iter().any(|op| {
        matches!(
            &op.kind,
            OperationKind::Repack { move_sources: true, dest, .. } if dest.ends_with("Game.zip")
        )
    });
    assert!(
        consumes_loose,
        "loose→archive consolidation within a preserve tree consumes the loose source"
    );
}
