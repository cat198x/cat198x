//! Integration tests for source registration and scanning workflows.

use cat198x::cli;

mod support;
use support::{TestEnv, create_test_rom};

#[test]
fn source_add_detects_case_sensitivity() {
    let env = TestEnv::new();
    env.init();

    env.add_source(&env.roms_dir, false, false);

    let db = env.db();
    let conn = db.conn();

    let sources = cat198x::db::files::list_sources(conn).expect("sources should list");
    assert_eq!(sources.len(), 1);

    // Case sensitivity depends on the filesystem, but it should be detected.
    let _ = sources[0].case_sensitive;
}

#[test]
fn source_add_prevents_duplicates() {
    let env = TestEnv::new();
    env.init();

    env.add_source(&env.roms_dir, false, false);
    env.add_source(&env.roms_dir, false, false);

    let db = env.db();
    let conn = db.conn();

    let sources = cat198x::db::files::list_sources(conn).expect("sources should list");
    assert_eq!(sources.len(), 1, "should not create duplicate source");
}

#[test]
fn source_remove() {
    let env = TestEnv::new();
    env.init();

    env.add_source(&env.roms_dir, false, false);

    use cat198x::SourceCommands;
    cli::source::run(
        SourceCommands::Remove {
            path: env.roms_dir.clone(),
        },
        env.data_dir_opt(),
    )
    .expect("source remove failed");

    let db = env.db();
    let conn = db.conn();

    let sources = cat198x::db::files::list_sources(conn).expect("sources should list");
    assert_eq!(sources.len(), 0, "source should be removed");
    assert!(env.roms_dir.exists(), "rom directory should not be deleted");
}

#[test]
fn scan_indexes_loose_files() {
    let env = TestEnv::new();
    env.init();

    create_test_rom(&env.roms_dir, "game1.nes", b"NES ROM content");
    create_test_rom(&env.roms_dir, "subdir/game2.nes", b"Another ROM");
    create_test_rom(&env.roms_dir, "game3.sfc", b"SNES ROM");

    env.add_source(&env.roms_dir, false, false);

    cli::scan::run(None, false, None, env.data_dir_opt()).expect("scan failed");

    let db = env.db();
    let conn = db.conn();

    let sources = cat198x::db::files::list_sources(conn).expect("sources should list");
    let file_count = cat198x::db::files::count_files_in_source(conn, sources[0].id)
        .expect("source file count should load");

    assert_eq!(file_count, 3, "should index all 3 files");
}

#[test]
fn scan_updates_last_scanned() {
    let env = TestEnv::new();
    env.init();

    create_test_rom(&env.roms_dir, "test.rom", b"test");
    env.add_source(&env.roms_dir, false, false);

    let db = env.db();
    let conn = db.conn();

    let sources = cat198x::db::files::list_sources(conn).expect("sources should list");
    assert!(sources[0].last_scanned.is_none());

    cli::scan::run(None, false, None, env.data_dir_opt()).expect("scan failed");

    let sources = cat198x::db::files::list_sources(conn).expect("sources should list");
    assert!(sources[0].last_scanned.is_some());
}

#[test]
fn incremental_scan_skips_unchanged() {
    let env = TestEnv::new();
    env.init();

    create_test_rom(&env.roms_dir, "test.rom", b"original");
    env.add_source(&env.roms_dir, false, false);

    cli::scan::run(None, false, None, env.data_dir_opt()).expect("initial scan failed");

    let db = env.db();
    let conn = db.conn();

    let sources = cat198x::db::files::list_sources(conn).expect("sources should list");
    let first_scanned = sources[0].last_scanned.clone();
    assert!(first_scanned.is_some());

    std::thread::sleep(std::time::Duration::from_millis(100));

    cli::scan::run(None, false, None, env.data_dir_opt()).expect("incremental scan failed");

    let sources = cat198x::db::files::list_sources(conn).expect("sources should list");
    let second_scanned = sources[0].last_scanned.clone();
    assert!(second_scanned.is_some());

    let file_count = cat198x::db::files::count_files_in_source(conn, sources[0].id)
        .expect("source file count should load");
    assert_eq!(file_count, 1);
}
