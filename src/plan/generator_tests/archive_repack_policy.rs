use super::*;

#[test]
fn shared_content_is_copied_to_each_destination_not_moved() {
    let db = setup_db();
    let conn = db.conn();
    // One physical file's content (BBB) belongs to two distinct games — two
    // destinations. It is held once, in ToSort (at neither destination).
    let coll = collections::create_collection(conn, "Shared Coll", "tosec").unwrap();
    let vid = collections::add_version(conn, coll, "v1", "/dats/s.dat", true).unwrap();
    let node = dats::create_node(conn, vid, None, "Shared Coll", "dat", "SET/Sys").unwrap();
    let g1 = dats::create_game(conn, node, "GameA", None, None, false, false, false).unwrap();
    dats::create_rom(conn, g1, "a.rom", 10, Some("BBB"), None, None, "good", None).unwrap();
    let g2 = dats::create_game(conn, node, "GameB", None, None, false, false, false).unwrap();
    dats::create_rom(conn, g2, "b.rom", 10, Some("BBB"), None, None, "good", None).unwrap();
    conn.execute("INSERT INTO files (sha1, size) VALUES ('BBB', 10)", [])
        .unwrap();
    conn.execute(
        "INSERT INTO sources (id, path, case_sensitive, disposition)
     VALUES (200, '/lib/ToSort/SET', 0, 'consume')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO file_locations (sha1, source_id, path, archive_path)
     VALUES ('BBB', 200, 'Sys/shared.rom', NULL)",
        [],
    )
    .unwrap();

    let plan = generate_plan_filtered(
        conn,
        &PlanOptions {
            default_dest: Some("/lib/ROMs".to_string()),
            default_format: OutputFormat::Loose,
            ..Default::default()
        },
    )
    .unwrap();

    // Both distinct entries get a real copy; the shared source is never moved
    // or deleted, so neither destination can be stranded.
    assert_eq!(
        plan.summary.move_count, 0,
        "shared content is copied, not moved"
    );
    assert_eq!(
        plan.summary.delete_count, 0,
        "a shared source is never deleted"
    );
    assert_eq!(
        plan.summary.copy_count, 2,
        "a real copy for each distinct destination"
    );
}

#[test]
fn disk_is_planned_loose_in_a_machine_folder_even_for_a_zip_set() {
    let db = setup_db();
    let conn = db.conn();
    // A CHD (<disk>) in a zip-format set must still be placed loose at
    // <dest>/<game>/<name>.chd — never packed into an archive.
    let coll = collections::create_collection(conn, "MAME CHDs", "mame").unwrap();
    let vid = collections::add_version(conn, coll, "v1", "/dats/chd.dat", true).unwrap();
    let node = dats::create_node(conn, vid, None, "MAME CHDs", "dat", "MAME").unwrap();
    let g = dats::create_game(conn, node, "azumanga", None, None, false, false, false).unwrap();
    // A disk: name without extension, sha1 = the CHD's internal hash.
    dats::create_disk(conn, g, "gdl-0018", Some("DDD"), None, "good", None).unwrap();
    conn.execute("INSERT INTO files (sha1, size) VALUES ('DDD', 4096)", [])
        .unwrap();
    conn.execute(
        "INSERT INTO sources (id, path, case_sensitive) VALUES (300, '/lib/ToSort/MAME', 0)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO file_locations (sha1, source_id, path, archive_path)
     VALUES ('DDD', 300, 'MAME CHDs (merged)/azumanga/gdl-0018.chd', NULL)",
        [],
    )
    .unwrap();

    let plan = generate_plan_filtered(
        conn,
        &PlanOptions {
            default_dest: Some("/lib/ROMs".to_string()),
            // Zip is the set format — the disk must ignore it and stay loose.
            default_format: OutputFormat::Zip,
            ..Default::default()
        },
    )
    .unwrap();

    // No archive is built for a disk.
    assert_eq!(plan.summary.repack_count, 0, "a CHD is never packed");
    // It is copied loose to <dest>/MAME/<game>/<name>.chd.
    let copies: Vec<String> = plan
        .operations
        .iter()
        .filter_map(|op| match &op.kind {
            OperationKind::Copy { dest, .. } => Some(dest.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        copies,
        vec!["/lib/ROMs/MAME/azumanga/gdl-0018.chd".to_string()]
    );
}

#[test]
fn shared_detection_matches_crc_only_arcade_content() {
    // Arcade DATs (MAME / FinalBurn Neo) are CRC-only: their ROMs have a NULL
    // sha1 and match held content by CRC32 + size. A SHA1-only shared check
    // missed them, so a container several games depend on read as unshared and
    // became eligible for a whole-archive relocate. Both detectors must see it.
    let db = setup_db();
    let conn = db.conn();
    let coll = collections::create_collection(conn, "Arcade", "mame").unwrap();
    let vid = collections::add_version(conn, coll, "v1", "/dats/a.dat", true).unwrap();
    let node = dats::create_node(conn, vid, None, "Arcade", "dat", "ARCADE").unwrap();

    // Two distinct games whose ROM is the same content, declared CRC-only
    // (sha1 = None) as arcade DATs do.
    let parent = dats::create_game(conn, node, "2010", None, None, false, false, false).unwrap();
    dats::create_rom(
        conn,
        parent,
        "p.rom",
        100,
        None,
        None,
        Some("AABBCCDD"),
        "good",
        None,
    )
    .unwrap();
    let clone = dats::create_game(
        conn,
        node,
        "2010p1",
        None,
        Some("2010"),
        false,
        false,
        false,
    )
    .unwrap();
    dats::create_rom(
        conn,
        clone,
        "p.rom",
        100,
        None,
        None,
        Some("AABBCCDD"),
        "good",
        None,
    )
    .unwrap();

    // One held file (real sha1) carrying that CRC32/size, inside one archive.
    conn.execute(
        "INSERT INTO files (sha1, crc32, size) VALUES ('FILESHA', 'AABBCCDD', 100)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO sources (id, path, case_sensitive) VALUES (500, '/lib/ToSort/ARCADE', 0)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO file_locations (sha1, source_id, path, archive_path)
     VALUES ('FILESHA', 500, '2010.zip', 'p.rom')",
        [],
    )
    .unwrap();

    let shared = compute_shared_content(conn).unwrap();
    assert!(
        shared.contains("FILESHA"),
        "CRC-only content shared across two games must be flagged shared"
    );

    let containers = compute_shared_containers(conn).unwrap();
    assert!(
        containers.contains("/lib/ToSort/ARCADE/2010.zip"),
        "a container sourcing two games by CRC32 must be flagged shared (repack, not relocate)"
    );
}

/// A parent/clone pair where the clone holds one inherited (merge-tagged) ROM
/// shared with the parent plus one of its own. The same fixture drives both
/// merge modes, asserting only the split filter changes placement.
fn setup_parent_clone_fixture(conn: &Connection) {
    let coll = collections::create_collection(conn, "Arcade", "mame").unwrap();
    let vid = collections::add_version(conn, coll, "v1", "/dats/mame.dat", true).unwrap();
    let node = dats::create_node(conn, vid, None, "Arcade", "dat", "ARCADE").unwrap();

    // Parent: owns shared.rom (AAA), no merge tag.
    let parent = dats::create_game(conn, node, "puckman", None, None, false, false, false).unwrap();
    dats::create_rom(
        conn,
        parent,
        "shared.rom",
        10,
        Some("AAA"),
        None,
        None,
        "good",
        None,
    )
    .unwrap();

    // Clone of puckman: shared.rom is inherited (merge-tagged → lives in the
    // parent under split); clone.rom (BBB) is its own unique ROM.
    let clone = dats::create_game(
        conn,
        node,
        "pacmanm",
        None,
        Some("puckman"),
        false,
        false,
        false,
    )
    .unwrap();
    dats::create_rom(
        conn,
        clone,
        "shared.rom",
        10,
        Some("AAA"),
        None,
        None,
        "good",
        Some("shared.rom"),
    )
    .unwrap();
    dats::create_rom(
        conn,
        clone,
        "clone.rom",
        10,
        Some("BBB"),
        None,
        None,
        "good",
        None,
    )
    .unwrap();

    conn.execute(
        "INSERT INTO files (sha1, size) VALUES ('AAA', 10), ('BBB', 10)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO sources (id, path, case_sensitive) VALUES (400, '/lib/ToSort/ARCADE', 0)",
        [],
    )
    .unwrap();
    // Both ROMs held loose in ToSort.
    conn.execute(
        "INSERT INTO file_locations (sha1, source_id, path, archive_path) VALUES
        ('AAA', 400, 'shared.rom', NULL),
        ('BBB', 400, 'clone.rom', NULL)",
        [],
    )
    .unwrap();
}

/// Map each game's planned archive to the sorted canonical entry names it
/// will hold — read from the repack sources' `entry_name`. Zip is the arcade
/// target, so split/non-merged are compared on archive *contents*.
fn repack_entries(plan: &Plan) -> BTreeMap<String, Vec<String>> {
    let mut by_dest: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for op in &plan.operations {
        if let OperationKind::Repack { sources, dest, .. } = &op.kind {
            let mut entries: Vec<String> = sources
                .iter()
                .filter_map(|s| s.entry_name.clone())
                .collect();
            entries.sort();
            by_dest.insert(dest.clone(), entries);
        }
    }
    by_dest
}

#[test]
fn split_mode_drops_a_clones_inherited_rom_from_its_archive() {
    let db = setup_db();
    let conn = db.conn();
    setup_parent_clone_fixture(conn);

    // Zip + split — the chosen arcade layout. The clone's archive must hold
    // only its own unique ROM; the inherited (merge-tagged) shared.rom lives
    // in the parent's archive, not the clone's.
    let plan = generate_plan_filtered(
        conn,
        &PlanOptions {
            default_dest: Some("/lib/ROMs".to_string()),
            default_format: OutputFormat::Zip,
            default_merge_mode: MergeMode::Split,
            ..Default::default()
        },
    )
    .unwrap();

    let entries = repack_entries(&plan);
    assert_eq!(
        entries.get("/lib/ROMs/ARCADE/pacmanm.zip"),
        Some(&vec!["clone.rom".to_string()]),
        "split: the clone archive holds only its own unique ROM"
    );
    assert_eq!(
        entries.get("/lib/ROMs/ARCADE/puckman.zip"),
        Some(&vec!["shared.rom".to_string()]),
        "split: the inherited ROM lives in the parent archive"
    );
}

#[test]
fn non_merged_mode_keeps_a_clones_inherited_rom_in_its_archive() {
    let db = setup_db();
    let conn = db.conn();
    setup_parent_clone_fixture(conn);

    // Default merge mode (NonMerged): every ROM the DAT lists per game is
    // placed, so the clone's archive carries its own copy of the inherited
    // shared.rom alongside its unique ROM.
    let plan = generate_plan_filtered(
        conn,
        &PlanOptions {
            default_dest: Some("/lib/ROMs".to_string()),
            default_format: OutputFormat::Zip,
            ..Default::default()
        },
    )
    .unwrap();

    let entries = repack_entries(&plan);
    assert_eq!(
        entries.get("/lib/ROMs/ARCADE/pacmanm.zip"),
        Some(&vec!["clone.rom".to_string(), "shared.rom".to_string()]),
        "non-merged: the clone archive carries its own copy of the inherited ROM"
    );
}

#[test]
fn shared_archive_content_is_repacked_to_each_game_not_consumed() {
    let db = setup_db();
    let conn = db.conn();
    // Content CCC belongs to two distinct games in a zip-format set, held once
    // as a loose file in ToSort.
    let node = add_collection_node(conn, "Shared Zip", "tosec", "/dats/z.dat", "SET/Sys");
    add_rom(conn, node, "GA", "r.rom", "CCC");
    add_rom(conn, node, "GB", "r.rom", "CCC");
    add_file(conn, "CCC", 10);
    add_source(conn, 201, "/lib/ToSort/SET", Some(Disposition::Consume));
    add_location(conn, "CCC", 201, "Sys/shared.rom", None);
    db_config::set_output_format(conn, "SET", "zip").unwrap();

    let plan = plan_with_default_dest(conn, OutputFormat::Loose);

    // Each game's archive is built by copying; the shared loose source is
    // neither consumed by a repack nor removed as a duplicate container.
    assert_eq!(
        plan.summary.repack_count, 2,
        "an archive built for each game"
    );
    assert_eq!(plan.summary.delete_count, 0, "shared source never deleted");
    let none_consume_source = plan.operations.iter().all(|op| match &op.kind {
        OperationKind::Repack { move_sources, .. } => !*move_sources,
        _ => true,
    });
    assert!(
        none_consume_source,
        "shared repacks must not consume their source"
    );
}

#[test]
fn shared_container_is_repacked_per_game_not_relocated_whole() {
    let db = setup_db();
    let conn = db.conn();
    // One archive (bundle.zip) holds ROMs for two distinct games — a
    // multi-game container. Each game's ROM is a different entry/SHA1.
    let node = add_collection_node(conn, "Bundle Coll", "tosec", "/dats/b.dat", "SET/Sys");
    add_rom(conn, node, "GameOne", "a.rom", "AAA");
    add_rom(conn, node, "GameTwo", "b.rom", "BBB");
    add_file(conn, "AAA", 10);
    add_file(conn, "BBB", 10);
    add_source(conn, 210, "/lib/ToSort/SET", Some(Disposition::Consume));
    // Both ROMs live as entries inside the SAME archive file.
    add_location(conn, "AAA", 210, "bundle.zip", Some("a.rom"));
    add_location(conn, "BBB", 210, "bundle.zip", Some("b.rom"));
    db_config::set_output_format(conn, "SET", "zip").unwrap();

    let plan = plan_with_default_dest(conn, OutputFormat::Loose);

    // The shared container is repacked per game (extracting each game's own
    // entry), never relocated whole (which would strand the other game).
    let relocates = plan
        .operations
        .iter()
        .filter(|op| matches!(op.kind, OperationKind::Relocate { .. }))
        .count();
    assert_eq!(
        relocates, 0,
        "a multi-game container is never relocated whole"
    );
    assert_eq!(
        plan.summary.repack_count, 2,
        "each game repacks its own entry"
    );
    // Once *both* games are repacked, every entry the container held survives
    // in a game archive, so the consume container is drained — exactly once,
    // despite feeding two games. The verify-before-delete net is the guard at
    // apply time: it removes the container only after confirming each entry
    // survives elsewhere, so the order (drain emitted after all repacks) and
    // the net together make this safe.
    assert_eq!(
        plan.summary.delete_count, 1,
        "the fully-consolidated shared container is drained, once"
    );
    let drained: Vec<_> = plan
        .operations
        .iter()
        .filter_map(|op| match &op.kind {
            OperationKind::Delete { path, reason, .. } => Some((path.clone(), reason.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(drained.len(), 1);
    assert_eq!(drained[0].0, "/lib/ToSort/SET/bundle.zip");
    assert!(
        drained[0].1.starts_with("consolidated into "),
        "reason names where the content went: {:?}",
        drained[0].1
    );
}

#[test]
fn single_game_consume_container_drains_when_a_shared_entry_forces_a_repack() {
    let db = setup_db();
    let conn = db.conn();
    // The real CD-image case: g1.zip, in a CONSUME staging source, holds
    // GameOne in full — its own ROM plus a ROM whose content (CCC) is shared
    // with GameTwo (a common .cue/.sub). The shared entry makes GameOne
    // `game_shared`, which blocks a whole-file relocate and forces a rebuild.
    // The container is then drained — earlier this was the stranded case.
    let node = add_collection_node(conn, "ISO Coll", "tosec", "/dats/i.dat", "SET/Sys");
    let g1 = add_game(conn, node, "GameOne");
    add_rom_to_game(conn, g1, "own.rom", "AAA");
    add_rom_to_game(conn, g1, "common.rom", "CCC");
    let g2 = add_game(conn, node, "GameTwo");
    add_rom_to_game(conn, g2, "other.rom", "BBB");
    add_rom_to_game(conn, g2, "common.rom", "CCC");
    add_file(conn, "AAA", 10);
    add_file(conn, "BBB", 10);
    add_file(conn, "CCC", 10);
    add_source(conn, 220, "/lib/ToSort/SET", Some(Disposition::Consume));
    add_location(conn, "AAA", 220, "g1.zip", Some("own.rom"));
    add_location(conn, "CCC", 220, "g1.zip", Some("common.rom"));
    add_location(conn, "BBB", 220, "g2.zip", Some("other.rom"));
    add_location(conn, "CCC", 220, "g2.zip", Some("common.rom"));
    db_config::set_output_format(conn, "SET", "zip").unwrap();

    let plan = plan_with_default_dest(conn, OutputFormat::Loose);

    // GameOne's source container is drained (its content is now in GameOne's
    // archive). The shared CCC entry — which earlier flagged the container as
    // "shared" and stranded it — no longer blocks the drain, because safety is
    // the verify-before-delete net's job, not a plan-time guess.
    let drained: Vec<&str> = plan
        .operations
        .iter()
        .filter_map(|op| match &op.kind {
            OperationKind::Delete { path, .. } => Some(path.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        drained.contains(&"/lib/ToSort/SET/g1.zip"),
        "GameOne's single-game consume container is drained: {drained:?}"
    );
}

#[test]
fn complete_container_found_among_many_shared_only_containers() {
    let db = setup_db();
    let conn = db.conn();
    // A game whose BIOS ROM is held in many containers (as a Neo-Geo BIOS
    // would be) but whose clone-specific ROM lives in just one — only that
    // container is complete. The planner must find it by the rarest entry
    // rather than scanning every BIOS-bearing container (the merged-arcade
    // quadratic that hung Q7). The BIOS is single-game here, so it is not
    // "shared content" and the build path is unconstrained.
    let coll = collections::create_collection(conn, "Arcade", "mame").unwrap();
    let vid = collections::add_version(conn, coll, "v1", "/dats/a.dat", true).unwrap();
    let node = dats::create_node(conn, vid, None, "Arcade", "dat", "SET/Sys").unwrap();
    let g = dats::create_game(conn, node, "neoclone", None, None, false, false, false).unwrap();
    dats::create_rom(
        conn,
        g,
        "bios.rom",
        10,
        Some("B105"),
        None,
        None,
        "good",
        None,
    )
    .unwrap();
    dats::create_rom(
        conn,
        g,
        "clone.rom",
        10,
        Some("C10E"),
        None,
        None,
        "good",
        None,
    )
    .unwrap();
    conn.execute(
        "INSERT INTO files (sha1, size) VALUES ('B105', 10), ('C10E', 10)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO sources (id, path, case_sensitive) VALUES (400, '/lib/ToSort/SET', 0)",
        [],
    )
    .unwrap();
    // The one complete container holds both ROMs.
    conn.execute(
        "INSERT INTO file_locations (sha1, source_id, path, archive_path) VALUES
        ('B105', 400, 'Sys/neoclone.zip', 'bios.rom'),
        ('C10E', 400, 'Sys/neoclone.zip', 'clone.rom')",
        [],
    )
    .unwrap();
    // The BIOS ROM is also present in 50 other (BIOS-only) containers.
    for i in 0..50 {
        conn.execute(
            &format!(
                "INSERT INTO file_locations (sha1, source_id, path, archive_path)
             VALUES ('B105', 400, 'Sys/other{i}.zip', 'bios.rom')"
            ),
            [],
        )
        .unwrap();
    }
    db_config::set_output_format(conn, "SET", "zip").unwrap();

    // Copy mode: no relocates/deletes, just the build — so the assertion
    // isolates which container the planner chose to build from.
    let plan = generate_plan_filtered(
        conn,
        &PlanOptions {
            default_dest: Some("/lib/ROMs".to_string()),
            default_format: OutputFormat::Loose,
            ..Default::default()
        },
    )
    .unwrap();

    // Exactly one archive built, sourced entirely from the one complete
    // container — never from a BIOS-only container (which lacks clone.rom).
    assert_eq!(plan.summary.repack_count, 1);
    let sources: Vec<String> = plan
        .operations
        .iter()
        .filter_map(|op| match &op.kind {
            OperationKind::Repack { sources, .. } => Some(sources.clone()),
            _ => None,
        })
        .flatten()
        .map(|s| s.path)
        .collect();
    assert!(!sources.is_empty());
    assert!(
        sources
            .iter()
            .all(|p| p == "/lib/ToSort/SET/Sys/neoclone.zip"),
        "repack must build from the one complete container, got {sources:?}"
    );
}

#[test]
fn archive_complete_staged_copy_is_relocated_not_repacked() {
    let db = setup_db();
    let conn = db.conn();
    // Only a staged ToSort copy exists; the library does not hold this game.
    let node = add_collection_node(conn, "Test Coll", "tosec", "/dats/test.dat", "SET/Sys");
    add_rom(conn, node, "Game", "game.rom", "AAA");
    add_file(conn, "AAA", 10);
    add_source(conn, 102, "/lib/ToSort/SET", Some(Disposition::Consume));
    add_location(conn, "AAA", 102, "Sys/Game.zip", Some("game.rom"));
    db_config::set_output_format(conn, "SET", "zip").unwrap();

    let plan = plan_with_default_dest(conn, OutputFormat::Loose);

    // A complete staged archive is relocated whole to its canonical path —
    // an instant rename — rather than rebuilt by repacking its entries.
    assert_eq!(
        plan.summary.repack_count, 0,
        "the staged zip is moved as-is, not rebuilt"
    );
    let relocates: Vec<_> = plan
        .operations
        .iter()
        .filter_map(|op| match &op.kind {
            OperationKind::Relocate { source, dest, .. } => Some((source.clone(), dest.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        relocates,
        vec![(
            "/lib/ToSort/SET/Sys/Game.zip".to_string(),
            "/lib/ROMs/SET/Sys/Game.zip".to_string(),
        )]
    );
}

#[test]
fn loose_staged_file_is_repacked_not_renamed_to_archive() {
    let db = setup_db();
    let conn = db.conn();
    // A complete game held only as a loose .tap under ToSort, in a zip set.
    let node = add_collection_node(conn, "Test Coll", "tosec", "/dats/test.dat", "SET/Sys");
    add_rom(conn, node, "Game", "game.tap", "AAA");
    add_file(conn, "AAA", 10);
    add_source(conn, 102, "/lib/ToSort/SET", Some(Disposition::Consume));
    // Loose file (archive_path NULL): NOT an archive in the target format.
    add_location(conn, "AAA", 102, "Sys/game.tap", None);
    db_config::set_output_format(conn, "SET", "zip").unwrap();

    let plan = plan_with_default_dest(conn, OutputFormat::Loose);

    // Renaming a loose .tap to .zip would mint a file whose extension lies
    // about its contents — the loose ROM must be repacked into a real zip.
    let relocates = plan
        .operations
        .iter()
        .filter(|op| matches!(op.kind, OperationKind::Relocate { .. }))
        .count();
    assert_eq!(
        relocates, 0,
        "a loose file is never relocated to an archive"
    );
    assert_eq!(
        plan.summary.repack_count, 1,
        "the loose .tap is repacked into Game.zip"
    );
    let dest = plan.operations.iter().find_map(|op| match &op.kind {
        OperationKind::Repack { dest, .. } => Some(dest.clone()),
        _ => None,
    });
    assert_eq!(dest.as_deref(), Some("/lib/ROMs/SET/Sys/Game.zip"));
}
