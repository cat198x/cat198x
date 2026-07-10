use std::collections::HashSet;

use super::*;
use crate::config::{MergeMode, OutputFormat};
use crate::db::Database;
use crate::db::dats;

fn setup_db() -> Database {
    Database::open_in_memory().unwrap()
}

fn setup_parent_clone_fixture(conn: &Connection) {
    let coll = collections::create_collection(conn, "Arcade", "mame").unwrap();
    let vid = collections::add_version(conn, coll, "v1", "/dats/mame.dat", true).unwrap();
    let node = dats::create_node(conn, vid, None, "Arcade", "dat", "ARCADE").unwrap();

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
    conn.execute(
        "INSERT INTO file_locations (sha1, source_id, path, archive_path) VALUES
                ('AAA', 400, 'shared.rom', NULL),
                ('BBB', 400, 'clone.rom', NULL)",
        [],
    )
    .unwrap();
}

#[test]
fn desired_state_assigns_a_split_inherited_rom_to_the_parent_archive() {
    let db = setup_db();
    let conn = db.conn();
    setup_parent_clone_fixture(conn);

    let interesting: HashSet<String> = ["AAA".to_string(), "BBB".to_string()].into();
    let state = compute_desired_state(
        conn,
        &PlanOptions {
            default_dest: Some("/lib/ROMs".to_string()),
            default_format: OutputFormat::Zip,
            default_merge_mode: MergeMode::Split,
            ..Default::default()
        },
        &interesting,
    )
    .unwrap();

    assert!(state.dest_paths.contains("/lib/ROMs/ARCADE/puckman.zip"));
    assert!(state.dest_paths.contains("/lib/ROMs/ARCADE/pacmanm.zip"));
    assert_eq!(
        state.archive_homes.get("AAA"),
        Some(&["/lib/ROMs/ARCADE/puckman.zip".to_string()].into()),
        "inherited content's canonical archive is the parent's"
    );
    assert_eq!(
        state.archive_homes.get("BBB"),
        Some(&["/lib/ROMs/ARCADE/pacmanm.zip".to_string()].into()),
        "the clone's own ROM belongs in the clone's archive"
    );
}

#[test]
fn desired_state_archive_homes_only_for_interesting_content() {
    let db = setup_db();
    let conn = db.conn();
    setup_parent_clone_fixture(conn);

    let interesting: HashSet<String> = ["BBB".to_string()].into();
    let state = compute_desired_state(
        conn,
        &PlanOptions {
            default_dest: Some("/lib/ROMs".to_string()),
            default_format: OutputFormat::Zip,
            default_merge_mode: MergeMode::Split,
            ..Default::default()
        },
        &interesting,
    )
    .unwrap();

    assert!(state.archive_homes.contains_key("BBB"));
    assert!(
        !state.archive_homes.contains_key("AAA"),
        "uninteresting content is not indexed"
    );
    assert!(state.dest_paths.contains("/lib/ROMs/ARCADE/puckman.zip"));
}

#[test]
fn desired_state_records_loose_paths_and_no_archive_homes() {
    let db = setup_db();
    let conn = db.conn();
    let coll = collections::create_collection(conn, "Tapes", "tosec").unwrap();
    let vid = collections::add_version(conn, coll, "v1", "/dats/tapes.dat", true).unwrap();
    let node = dats::create_node(conn, vid, None, "Tapes", "dat", "Tapes").unwrap();
    let solo = dats::create_game(conn, node, "Solo", None, None, false, false, false).unwrap();
    dats::create_rom(
        conn,
        solo,
        "solo.tap",
        10,
        Some("AAA"),
        None,
        None,
        "good",
        None,
    )
    .unwrap();
    let duo = dats::create_game(conn, node, "Duo", None, None, false, false, false).unwrap();
    dats::create_rom(
        conn,
        duo,
        "duo-a.tap",
        10,
        Some("BBB"),
        None,
        None,
        "good",
        None,
    )
    .unwrap();
    dats::create_rom(
        conn,
        duo,
        "duo-b.tap",
        10,
        Some("CCC"),
        None,
        None,
        "good",
        None,
    )
    .unwrap();
    conn.execute(
        "INSERT INTO files (sha1, size) VALUES ('AAA',10),('BBB',10),('CCC',10)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO sources (id, path, case_sensitive) VALUES (500, '/lib/ToSort/Tapes', 0)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO file_locations (sha1, source_id, path, archive_path) VALUES
                ('AAA',500,'solo.tap',NULL),('BBB',500,'duo-a.tap',NULL),('CCC',500,'duo-b.tap',NULL)",
        [],
    )
    .unwrap();

    let interesting: HashSet<String> =
        ["AAA".to_string(), "BBB".to_string(), "CCC".to_string()].into();
    let state = compute_desired_state(
        conn,
        &PlanOptions {
            default_dest: Some("/lib/ROMs".to_string()),
            default_format: OutputFormat::Loose,
            ..Default::default()
        },
        &interesting,
    )
    .unwrap();

    assert!(state.dest_paths.contains("/lib/ROMs/Tapes/solo.tap"));
    assert!(state.dest_paths.contains("/lib/ROMs/Tapes/Duo/duo-a.tap"));
    assert!(state.dest_paths.contains("/lib/ROMs/Tapes/Duo/duo-b.tap"));
    assert!(
        state.archive_homes.is_empty(),
        "a loose collection contributes no archive homes"
    );
}
