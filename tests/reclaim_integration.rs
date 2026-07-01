//! Integration tests for reclaim workflows.

use std::fs;

use cat198x::cli;

mod support;
use support::{TestEnv, create_test_rom, create_test_zip_entries};

#[test]
fn reclaim_execute_removes_redundant_consume_source_file() {
    let env = TestEnv::new();
    env.init();

    let staging_dir = env.temp_dir.path().join("staging");
    let library_dir = env.temp_dir.path().join("library");
    let staging_file = create_test_rom(&staging_dir, "redundant.rom", b"same bytes");
    let library_file = create_test_rom(&library_dir, "copy.rom", b"same bytes");
    let staging_file_canonical = fs::canonicalize(&staging_file)
        .expect("staging file should canonicalize")
        .to_string_lossy()
        .into_owned();
    let sha1 = cat198x::scanner::hasher::hash_file(&library_file)
        .expect("library file should hash")
        .sha1;

    env.add_source(&staging_dir, false, true);
    env.add_source(&library_dir, true, false);

    cli::scan::run(None, false, None, env.data_dir_opt()).expect("scan failed");

    let staging_id = env.source_id(&staging_dir, "staging source exists");

    cli::reclaim::run(Some(staging_id.to_string()), true, env.data_dir_opt())
        .expect("reclaim execute failed");

    assert!(!staging_file.exists(), "redundant staging file is deleted");
    assert!(library_file.exists(), "verified survivor remains on disk");

    let db = env.db();
    let conn = db.conn();
    let locations =
        cat198x::db::files::get_file_locations(conn, &sha1).expect("file locations should load");
    assert_eq!(locations.len(), 1);
    assert_eq!(locations[0].path, "copy.rom");

    assert_eq!(env.single_reclaim_log(), staging_file_canonical);
}

#[test]
fn reclaim_execute_refuses_preserve_source() {
    let env = TestEnv::new();
    env.init();

    let preserve_dir = env.temp_dir.path().join("master");
    let library_dir = env.temp_dir.path().join("library");
    let preserve_file = create_test_rom(&preserve_dir, "redundant.rom", b"same bytes");
    let library_file = create_test_rom(&library_dir, "copy.rom", b"same bytes");
    let sha1 = cat198x::scanner::hasher::hash_file(&library_file)
        .expect("library file should hash")
        .sha1;

    env.add_source(&preserve_dir, true, false);
    env.add_source(&library_dir, true, false);

    cli::scan::run(None, false, None, env.data_dir_opt()).expect("scan failed");

    let preserve_id = env.source_id(&preserve_dir, "preserve source exists");

    cli::reclaim::run(Some(preserve_id.to_string()), true, env.data_dir_opt())
        .expect("reclaim execute failed");

    assert!(
        preserve_file.exists(),
        "preserve source file is left untouched"
    );
    assert!(library_file.exists(), "survivor remains on disk");

    let db = env.db();
    let conn = db.conn();
    let locations =
        cat198x::db::files::get_file_locations(conn, &sha1).expect("file locations should load");
    assert_eq!(locations.len(), 2, "catalogue keeps both preserve copies");

    let logs_dir = env.data_dir.join("objects/reclaim-logs");
    assert!(!logs_dir.exists(), "refused reclaim writes no audit log");
}

#[test]
fn reclaim_execute_removes_redundant_archive_container() {
    use sha1::Digest;

    let env = TestEnv::new();
    env.init();

    let staging_dir = env.temp_dir.path().join("staging");
    let library_dir = env.temp_dir.path().join("library");
    fs::create_dir_all(&staging_dir).expect("failed to create staging dir");
    fs::create_dir_all(&library_dir).expect("failed to create library dir");

    let entries: &[(&str, &[u8])] = &[("a.rom", b"alpha"), ("b.rom", b"beta")];
    let staging_archive = create_test_zip_entries(&staging_dir, "redundant.zip", entries);
    let library_archive = create_test_zip_entries(&library_dir, "canonical.zip", entries);
    let staging_archive_canonical = fs::canonicalize(&staging_archive)
        .expect("staging archive should canonicalize")
        .to_string_lossy()
        .into_owned();
    let sha1s = entries
        .iter()
        .map(|(_, content)| cat198x::util::hex_upper(sha1::Sha1::digest(content)))
        .collect::<Vec<_>>();

    env.add_source(&staging_dir, false, true);
    env.add_source(&library_dir, true, false);

    cli::scan::run(None, false, None, env.data_dir_opt()).expect("scan failed");

    let staging_id = env.source_id(&staging_dir, "staging source exists");
    let library_id = env.source_id(&library_dir, "library source exists");

    cli::reclaim::run(Some(staging_id.to_string()), true, env.data_dir_opt())
        .expect("reclaim execute failed");

    assert!(
        !staging_archive.exists(),
        "redundant staging archive is deleted"
    );
    assert!(library_archive.exists(), "library archive remains on disk");

    let db = env.db();
    let conn = db.conn();
    for (sha1, (entry_name, _)) in sha1s.iter().zip(entries.iter()) {
        let locations =
            cat198x::db::files::get_file_locations(conn, sha1).expect("file locations should load");
        assert_eq!(locations.len(), 1);
        assert_eq!(locations[0].source_id, library_id);
        assert_eq!(locations[0].path, "canonical.zip");
        assert_eq!(locations[0].archive_path.as_deref(), Some(*entry_name));
    }

    assert_eq!(env.single_reclaim_log(), staging_archive_canonical);
}
